// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface staging for handing engine frames to a consumer one hop
//! away — the "one GPU blit into an exportable staging buffer" the plan
//! decides.
//!
//! Why a blit at all: pool allocations are DMA-BUF-flavoured, external
//! device APIs import OPAQUE_FD, and on NVIDIA one allocation cannot
//! export both — `vkGetPhysicalDeviceExternalBufferProperties` reports
//! the two handle types in disjoint `compatibleHandleTypes` sets, so a
//! dual-flavour allocation is spec-invalid (VUID-VkExportMemoryAllocateInfo-
//! handleTypes-00656). The staging buffer is the bridge: OPAQUE_FD,
//! allocated once per surface, refilled by a GPU copy each time a
//! consumer asks.
//!
//! One machine, two residencies ([`SurfaceExportStagingResidency`]).
//! Everything below — the per-surface lifetime, the source resolve, the
//! frame-identity guards, the timeline, the reused recorder, the
//! surface-share publication — is the same whether the consumer reads
//! the staging with the GPU or with the CPU. The only axis that differs
//! is where the memory lives, and it is never inferred: the caller names
//! it, because CPU residency is the compatibility path a consumer opts
//! into deliberately, not one it can fall into.
//!
//! What the blit reads is the surface's own pooled backing whenever it
//! has one — a producer's registered texture is a frames-in-flight
//! transient it keeps overwriting, and a published surface id names an
//! immutable frame (`docs/decisions/surface-id-lifetime-contract.md`).
//! The source is still resolved **per refill**, never cached: a rotating
//! producer re-registers under the same surface id, so a handle resolved
//! at creation can name an allocation that has since rotated out. The
//! staging itself is per-surface-id and spans those re-registrations —
//! which is why it lives in a [`GpuContext`]-owned map beside
//! `texture_cache` rather than on the `TextureRegistration` that
//! rotates out from under it.
//!
//! Ordering: the refill orders against the producer through queue
//! submission order — every producer in this engine submits on the one
//! `GpuContext` queue, and the refill's ALL_COMMANDS barrier makes prior
//! submissions' writes visible. A multi-queue engine would need the
//! refill to wait on the producer's timeline instead.
//!
//! Nothing here assumes the consumer lives in this process. A processor
//! reaching this from its own helper process gets the same staging and
//! the same timeline: [`GpuContext::share_surface_export_staging`]
//! publishes both to the surface-share service once, and the child
//! triggers refills through the escalate ops that wrap the methods
//! below. The bounded host wait after each submit is a convenience for
//! callers already in this process; the exportable timeline is the
//! contract, and it is what a child waits on.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::GpuContext;
use crate::core::error::{Error, Result};
use crate::host_rhi::{HostGpuDeviceExt as _, VulkanAccess, VulkanStage};
use crate::vulkan::rhi::{
    HostVulkanBuffer, HostVulkanTimelineSemaphore, ImageCopyRegion, RhiCommandRecorder,
};
use streamlib_consumer_rhi::{PixelFormat, TextureFormat, VulkanLayout};

/// Bound on the host wait for a staging refill. The copy is a GPU copy
/// of one frame (tens of microseconds); a wait that reaches this bound
/// is a wedged queue, not a slow copy.
const STAGING_REFILL_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

/// Where a staging buffer's memory lives, and therefore who can read it.
///
/// Named by the caller, never inferred from the surface: the GPU
/// adapters deliberately do not offer CPU access, and that asymmetry is
/// how "switch to opt into CPU" is enforced. A residency defaulted or
/// guessed would erode it silently.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SurfaceExportStagingResidency {
    /// DEVICE_LOCAL — an external *device* API (CUDA today) imports the
    /// OPAQUE_FD and reads it on the GPU. The frame never touches host
    /// memory.
    DeviceLocal,
    /// HOST_VISIBLE + HOST_COHERENT — the consumer maps the memory and
    /// reads it with the CPU. The compatibility path, for code that
    /// speaks host memory only.
    HostVisible,
}

impl SurfaceExportStagingResidency {
    /// The suffix distinguishing this residency's surface-share
    /// registration from the other's for the same surface. Two
    /// residencies are two allocations and must never collide on one id.
    fn surface_share_id_suffix(self) -> &'static str {
        match self {
            Self::DeviceLocal => "device-export-staging",
            Self::HostVisible => "cpu-readback-staging",
        }
    }

    /// How this residency reads in an error message.
    fn described(self) -> &'static str {
        match self {
            Self::DeviceLocal => "device-local",
            Self::HostVisible => "host-visible",
        }
    }
}

/// The pixel shape a texture-backed export presents to a DLPack
/// consumer. Restricted to 4-byte color: the refill's geometry guard,
/// the tightly-packed copy region, and the consumer-side tensor layout
/// all express exactly that today — accepting a wider format here would
/// size a staging no refill can fill. BGRA stays BGRA: relabeling those
/// bytes RGBA would silently swap channels.
fn export_pixel_shape_for_texture(format: TextureFormat) -> Result<PixelFormat> {
    match format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => Ok(PixelFormat::Rgba32),
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => Ok(PixelFormat::Bgra32),
        TextureFormat::Rgba16Float | TextureFormat::Rgba32Float => Err(Error::GpuError(format!(
            "device export supports 4-byte color textures today; {format:?} needs the wider \
             tensor layouts no consumer path expresses yet"
        ))),
        TextureFormat::Nv12 => Err(Error::GpuError(
            "device export refuses NV12: it is two planes, and a one-buffer export would drop \
             chroma"
                .into(),
        )),
    }
}

fn export_bytes_per_pixel_for_pixel_format(format: PixelFormat) -> Result<u32> {
    if format.plane_count() > 1 || format == PixelFormat::Unknown {
        return Err(Error::GpuError(format!(
            "device export refuses {format:?}: DLPack expresses one strided linear buffer, and \
             exporting only the first plane would hand out part of the image"
        )));
    }
    Ok(format.bits_per_pixel() / 8)
}

/// The recorder plus the next timeline value, under one lock: the
/// correctness of the strictly-increasing signal values depends on the
/// value being drawn and submitted while this lock is held, so the
/// invariant is structural rather than a convention beside an atomic.
struct RefillSubmission {
    recorder: RhiCommandRecorder,
    next_signal_value: u64,
}

/// One surface's export staging: the OPAQUE_FD buffer a consumer one hop
/// away imports, the exportable timeline that orders refills, and the
/// recorder that reuses one command pool across them.
pub struct SurfaceExportStaging {
    /// The slot-normalized key of the surface this staging exports —
    /// stable across the frames a pool slot publishes, which is what lets
    /// one staging (and the child's one import of it) span them. The
    /// frame a refill serves is named by the id passed to that refill.
    source_surface_key: String,
    /// Where this staging's memory lives. Part of the cache key, so one
    /// surface can carry both residencies at once — a consumer reading on
    /// the GPU and another reading on the CPU are two allocations, never
    /// one reinterpreted.
    residency: SurfaceExportStagingResidency,
    staging_buffer: Arc<HostVulkanBuffer>,
    refill_done_timeline: Arc<HostVulkanTimelineSemaphore>,
    /// One recorder, reused: per-refill pool creation is the exact churn
    /// `docs/learnings/nvidia-opaque-fd-after-swapchain.md` warns about.
    refill_submission: Mutex<RefillSubmission>,
    staging_byte_size: u64,
    surface_width: u32,
    surface_height: u32,
    /// The pixel shape the staging was sized for — the buffer's own
    /// format, or the 4-byte color shape a texture source maps to.
    pixel_format: Option<PixelFormat>,
    /// Whether [`GpuContext::copy_surface_export_staging_back_to_surface`]
    /// can honour a write — true only when the surface's sole backing is
    /// its own pooled allocation.
    writable: bool,
    /// The surface-share id this staging is published under, once it has
    /// been. Registration hands the service a dup of the staging fd and
    /// the timeline fd, so repeating it per frame would leak a pair per
    /// frame; holding the id here makes the publish once-per-staging and
    /// still lets a failed attempt be retried.
    #[cfg(target_os = "linux")]
    surface_share_registration_id: Mutex<Option<String>>,
}

impl SurfaceExportStaging {
    /// Byte size of the staging buffer — the span a DLPack tensor covers.
    pub fn staging_byte_size(&self) -> u64 {
        self.staging_byte_size
    }

    /// Width in pixels of the surface this staging was sized for.
    pub fn surface_width(&self) -> u32 {
        self.surface_width
    }

    /// Height in pixels of the surface this staging was sized for.
    pub fn surface_height(&self) -> u32 {
        self.surface_height
    }

    /// Row pitch in bytes, derived from the staging's own geometry.
    pub fn bytes_per_row(&self) -> u64 {
        self.staging_byte_size / u64::from(self.surface_height.max(1))
    }

    /// The pixel format the source carries, when it is a pixel buffer.
    pub fn pixel_format(&self) -> Option<PixelFormat> {
        self.pixel_format
    }

    /// Whether a consumer may write and copy back.
    pub fn writable(&self) -> bool {
        self.writable
    }

    /// Where this staging's memory lives.
    pub fn residency(&self) -> SurfaceExportStagingResidency {
        self.residency
    }

    /// Borrow the staging allocation (for export or import bookkeeping).
    pub fn staging_buffer(&self) -> &Arc<HostVulkanBuffer> {
        &self.staging_buffer
    }

    /// The timeline a consumer waits on; each refill signals the value
    /// [`GpuContext::refill_surface_export_staging`] returns.
    pub fn refill_done_timeline(&self) -> &Arc<HostVulkanTimelineSemaphore> {
        &self.refill_done_timeline
    }

    /// UUID of the Vulkan device that owns the staging allocation. An
    /// external device API must import onto this GPU; picking any other
    /// reads the wrong memory rather than failing.
    pub fn exporting_device_uuid(&self) -> [u8; 16] {
        self.staging_buffer.vulkan_device().physical_device_uuid()
    }
}

/// What a refill resolved this frame — looked up fresh on every copy so
/// a rotating producer's re-registration is honoured, never a snapshot.
enum ResolvedBlitSource {
    RegisteredTexture(crate::core::context::TextureRegistration),
    PixelBuffer(crate::core::rhi::PixelBuffer),
}

/// The geometry and capability a staging is minted with, read off
/// whichever backing answered for the surface.
struct SurfaceExportStagingShape {
    staging_byte_size: u64,
    surface_width: u32,
    surface_height: u32,
    pixel_format: PixelFormat,
    /// The advertised write-back capability. A snapshot: the write-back
    /// itself re-tests, because a producer can register a texture under
    /// this id after the staging was minted.
    writable: bool,
}

impl GpuContext {
    /// Resolve the current blit source for `surface_id` — the surface's
    /// pooled backing whenever it has one, the registered texture only
    /// for surfaces that have none.
    ///
    /// The pool member is the frame the bag named; a producer's own
    /// registered texture is a frames-in-flight transient holding
    /// whatever that producer has rendered since. Sourcing the transient
    /// hands a consumer a different frame under the id it asked for, so
    /// a producer-internal texture never backs a cross-process export.
    /// Texture-first survives for surfaces with no pooled member — kernel
    /// outputs, whose id↔backing binding is stable.
    /// Both same-process caches are consulted first, in that same
    /// priority order. Either composite lookup below would find them,
    /// but each reaches the surface-share service on the way — and a
    /// miss there is a blocking socket round trip, which for a
    /// texture-only surface would be paid on every refill to learn what
    /// the local pool already knows.
    fn resolve_device_export_source(&self, surface_id: &str) -> Result<ResolvedBlitSource> {
        if let Some(pixel_buffer) = self.pooled_backing_held_in_this_process(surface_id) {
            return Ok(ResolvedBlitSource::PixelBuffer(pixel_buffer));
        }
        if let Some(registration) = self.producer_registered_texture_for_surface_id(surface_id) {
            return Ok(ResolvedBlitSource::RegisteredTexture(registration));
        }
        match self.resolve_pixel_buffer_by_surface_id(surface_id) {
            Ok(pixel_buffer) => Ok(ResolvedBlitSource::PixelBuffer(pixel_buffer)),
            Err(buffer_miss) => {
                match self.resolve_texture_registration_by_surface_id(surface_id, None, 0, 0) {
                    Ok(registration) => Ok(ResolvedBlitSource::RegisteredTexture(registration)),
                    Err(texture_miss) => Err(Error::GpuError(format!(
                        "surface {surface_id} resolves to neither a pixel buffer \
                         ({buffer_miss}) nor a registered texture ({texture_miss})"
                    ))),
                }
            }
        }
    }

    /// The export staging for `surface_id` at `residency`, created on
    /// first ask and cached on this context — it dies with the context and
    /// is evicted by [`Self::unregister_texture`], never by a rotating
    /// re-registration, which the per-refill source resolve absorbs.
    ///
    /// Keyed by residency as well as surface, so asking for a CPU-readable
    /// staging never hands back the device-local one a CUDA consumer is
    /// already importing.
    pub fn surface_export_staging(
        &self,
        surface_id: &str,
        residency: SurfaceExportStagingResidency,
    ) -> Result<Arc<SurfaceExportStaging>> {
        self.refuse_a_retired_frame_id(surface_id)?;
        let source_surface_key = crate::core::rhi::pool_slot_key_of_surface_id(surface_id);
        let cache_key = (source_surface_key.to_string(), residency);
        if let Some(existing) = self.surface_export_stagings.lock().get(&cache_key) {
            return Ok(Arc::clone(existing));
        }

        let shape = match self.resolve_device_export_source(surface_id)? {
            ResolvedBlitSource::RegisteredTexture(registration) => {
                let texture = registration.texture();
                let pixel_format = export_pixel_shape_for_texture(texture.format())?;
                let (surface_width, surface_height) = (texture.width(), texture.height());
                SurfaceExportStagingShape {
                    // 4-byte color by `export_pixel_shape_for_texture`'s
                    // restriction — the same arithmetic the refill guard
                    // and the copy region assume.
                    staging_byte_size: u64::from(surface_width) * u64::from(surface_height) * 4,
                    surface_width,
                    surface_height,
                    pixel_format,
                    writable: false,
                }
            }
            ResolvedBlitSource::PixelBuffer(pixel_buffer) => {
                let pixel_format = pixel_buffer.format();
                export_bytes_per_pixel_for_pixel_format(pixel_format)?;
                let staging_byte_size = pixel_buffer.plane_size(0);
                if staging_byte_size == 0 {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} resolves to a zero-byte plane; nothing to export"
                    )));
                }
                // The write-back protocol belongs to a surface whose
                // only backing is its own pooled allocation. A pool
                // member a producer also published as a registered
                // texture is a frame that producer still owns, and an
                // in-place device edit would land in a live pool slot.
                let pooled_allocation_is_the_only_backing = self
                    .producer_registered_texture_for_surface_id(surface_id)
                    .is_none();
                SurfaceExportStagingShape {
                    staging_byte_size,
                    surface_width: pixel_buffer.width,
                    surface_height: pixel_buffer.height,
                    pixel_format,
                    writable: pooled_allocation_is_the_only_backing,
                }
            }
        };

        let vulkan_device = self.device().vulkan_device();
        // The one line the residency decides. Both are OPAQUE_FD so the
        // check-out and import path is identical either way; only the
        // memory properties differ, and with them who can read it.
        let staging_buffer = Arc::new(match residency {
            SurfaceExportStagingResidency::DeviceLocal => {
                HostVulkanBuffer::new_opaque_fd_export_device_local(
                    vulkan_device,
                    shape.staging_byte_size,
                )?
            }
            SurfaceExportStagingResidency::HostVisible => {
                HostVulkanBuffer::new_opaque_fd_export(vulkan_device, shape.staging_byte_size)?
            }
        });
        let refill_done_timeline = self.create_exportable_timeline_semaphore(0)?;
        let refill_submission = Mutex::new(RefillSubmission {
            recorder: self.create_command_recorder("surface_export_staging_refill")?,
            next_signal_value: 1,
        });

        let staging = Arc::new(SurfaceExportStaging {
            source_surface_key: source_surface_key.to_string(),
            residency,
            staging_buffer,
            refill_done_timeline,
            refill_submission,
            staging_byte_size: shape.staging_byte_size,
            surface_width: shape.surface_width,
            surface_height: shape.surface_height,
            pixel_format: Some(shape.pixel_format),
            writable: shape.writable,
            #[cfg(target_os = "linux")]
            surface_share_registration_id: Mutex::new(None),
        });
        // Double-check under the insert lock: a concurrent asker for the
        // same slot and residency may have published one while this thread
        // was allocating. The loser's freshly-built staging drops here
        // rather than replacing the entry other holders (and the wheel's
        // import memo) key on.
        let mut stagings = self.surface_export_stagings.lock();
        if let Some(published) = stagings.get(&cache_key) {
            return Ok(Arc::clone(published));
        }
        stagings.insert(cache_key, Arc::clone(&staging));
        Ok(staging)
    }

    /// Publish this staging and its refill timeline to the surface-share
    /// service, and answer with the id they are registered under.
    ///
    /// This is how a consumer one process away reaches the export: it
    /// checks the id out, receives the staging's OPAQUE_FD and the
    /// timeline's fd over SCM_RIGHTS, imports or maps the memory, and
    /// waits on the timeline for the value each refill returns. The
    /// published id is derived from the source surface's *and* the
    /// residency, so three registrations never collide — the source is
    /// already registered under its own id with its DMA-BUF planes, and
    /// the two residencies are separate allocations of their own.
    ///
    /// Registers at most once per staging; later calls answer with the
    /// same id.
    ///
    /// Answers with the pixel shape it validated as well as the id, so a
    /// caller building a consumer-side layout does not re-derive — and
    /// cannot disagree about — what this already refused without.
    #[cfg(target_os = "linux")]
    pub fn share_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(String, PixelFormat)> {
        let pixel_format = staging.pixel_format.ok_or_else(|| {
            Error::GpuError(format!(
                "the {} export staging for surface {} carries no pixel shape; a consumer would \
                 have no layout to import it under",
                staging.residency.described(),
                staging.source_surface_key
            ))
        })?;
        let mut registration_id = staging.surface_share_registration_id.lock();
        if let Some(already_registered) = registration_id.as_ref() {
            return Ok((already_registered.clone(), pixel_format));
        }
        let surface_store = self.surface_store().ok_or_else(|| {
            Error::GpuError(
                "this runtime has no surface-share service, so a surface export cannot reach \
                 another process"
                    .into(),
            )
        })?;
        let shared_id = format!(
            "{}-{}",
            staging.source_surface_key,
            staging.residency.surface_share_id_suffix()
        );
        // `host_inner`-direct for the same reason the passthroughs below
        // are: the plugin ABI is not grown a vtable slot for a surface
        // #1715 deletes. A cdylib caller panics at the guard.
        surface_store.host_inner().register_device_export_staging(
            &shared_id,
            &staging.staging_buffer,
            staging.staging_byte_size,
            staging.surface_width,
            staging.surface_height,
            pixel_format,
            &staging.refill_done_timeline,
        )?;
        *registration_id = Some(shared_id.clone());
        Ok((shared_id, pixel_format))
    }

    /// Drop every cached export staging for `surface_id` — both
    /// residencies, because the surface they staged is going away and
    /// neither survives it. Outstanding consumers keep theirs alive
    /// through their own `Arc`s.
    ///
    /// A staging that was published to the surface-share service is released
    /// from it here too: the service refuses duplicate ids, so leaving the
    /// dead entry behind would permanently block a later staging for a reused
    /// surface id from ever being shared — and the service would keep holding
    /// dups of an fd pair nothing refills.
    pub(crate) fn evict_surface_export_stagings(&self, surface_id: &str) {
        let evicted_key = crate::core::rhi::pool_slot_key_of_surface_id(surface_id);
        let evicted_stagings: Vec<Arc<SurfaceExportStaging>> = {
            let mut stagings = self.surface_export_stagings.lock();
            let keys: Vec<_> = stagings
                .keys()
                .filter(|(slot_key, _)| slot_key == evicted_key)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| stagings.remove(&key))
                .collect()
        };
        #[cfg(target_os = "linux")]
        for staging in &evicted_stagings {
            if let Some(shared_id) = staging.surface_share_registration_id.lock().take()
                && let Some(surface_store) = self.surface_store()
            {
                // Best-effort: the service also releases everything with the
                // connection, so a failure here is deferred cleanup, not a leak
                // for the life of the process.
                if let Err(release_failure) = surface_store.host_inner().release(&shared_id) {
                    tracing::debug!(
                        %shared_id,
                        %release_failure,
                        "releasing an evicted surface export staging from surface-share failed"
                    );
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        drop(evicted_stagings);
    }

    /// Record + submit one staging copy and wait (bounded) for it to
    /// land, returning the signalled timeline value.
    ///
    /// The lock/begin/record/submit/wait choreography lives here once:
    /// the signal value must be drawn and submitted under the recorder
    /// lock (strictly-increasing timeline values), the lock must drop
    /// before the host wait, and any record failure must abort the
    /// recording — a recorder left mid-recording refuses every later
    /// `begin`, which would brick this surface's export for the life of
    /// the cache entry.
    /// Takes the recorder it is handed rather than acquiring it, so the
    /// blocking and non-blocking entry points differ only in how they got
    /// one — and neither carries the other's return shape.
    fn submit_staging_copy_and_wait(
        &self,
        staging: &SurfaceExportStaging,
        mut submission: parking_lot::MutexGuard<'_, RefillSubmission>,
        record_copy: impl FnOnce(&mut RhiCommandRecorder) -> Result<()>,
    ) -> Result<u64> {
        let signal_value;
        {
            submission.recorder.begin()?;
            if let Err(record_failure) = record_copy(&mut submission.recorder) {
                submission.recorder.abort_recording();
                return Err(record_failure);
            }
            signal_value = submission.next_signal_value;
            submission.next_signal_value += 1;
            if let Err(submit_failure) = submission
                .recorder
                .submit_signaling_timeline(&staging.refill_done_timeline, signal_value)
            {
                submission.recorder.abort_recording();
                return Err(submit_failure);
            }
            drop(submission);
        }
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        Ok(signal_value)
    }

    /// Refuse a surface whose pool slot this staging was not opened for.
    fn refuse_a_surface_this_staging_does_not_export(
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<()> {
        if crate::core::rhi::pool_slot_key_of_surface_id(surface_id) != staging.source_surface_key {
            return Err(Error::GpuError(format!(
                "surface {surface_id} is not the surface this {} export staging was opened for \
                 ({}); the staged copy would carry another surface's pixels",
                staging.residency.described(),
                staging.source_surface_key
            )));
        }
        Ok(())
    }

    /// Copy `surface_id`'s current pixels into the staging buffer.
    /// Resolves the source fresh — a rotating producer's latest
    /// registration, not a snapshot — and refuses an id whose frame the
    /// producer has recycled, because the slot's bytes are then somebody
    /// else's picture. Returns the signalled timeline value a
    /// cross-process consumer would wait on instead of relying on the
    /// in-process host wait.
    pub fn refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<u64> {
        let source = self.resolve_refill_source(staging, surface_id)?;
        self.submit_staging_copy_and_wait(staging, staging.refill_submission.lock(), |recorder| {
            Self::record_refill(staging, &source, recorder)
        })
    }

    /// [`Self::refill_surface_export_staging`], but answering `Ok(None)`
    /// instead of queueing when another copy already holds this staging's
    /// recorder — what the `try_` escalate op reports as `contended`.
    ///
    /// The guards and the source resolve run first and identically: a
    /// retired frame id or a mismatched staging is an error either way,
    /// never a contention report.
    pub fn try_refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let source = self.resolve_refill_source(staging, surface_id)?;
        let Some(submission) = staging.refill_submission.try_lock() else {
            return Ok(None);
        };
        self.submit_staging_copy_and_wait(staging, submission, |recorder| {
            Self::record_refill(staging, &source, recorder)
        })
        .map(Some)
    }

    /// The guards every refill runs, plus the freshly-resolved source.
    /// Resolves fresh — a rotating producer's latest registration, not a
    /// snapshot — and refuses an id whose frame the producer has
    /// recycled, because the slot's bytes are then somebody else's
    /// picture.
    fn resolve_refill_source(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<ResolvedBlitSource> {
        Self::refuse_a_surface_this_staging_does_not_export(staging, surface_id)?;
        self.refuse_a_retired_frame_id(surface_id)?;
        self.resolve_device_export_source(surface_id)
    }

    /// Record one surface→staging copy. Geometry is re-checked here
    /// rather than at resolve: a re-registration between the two would
    /// otherwise size the copy from a shape the staging no longer has.
    fn record_refill(
        staging: &SurfaceExportStaging,
        source: &ResolvedBlitSource,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<()> {
        match source {
            ResolvedBlitSource::RegisteredTexture(registration) => {
                let texture = registration.texture();
                if u64::from(texture.width()) * u64::from(texture.height()) * 4
                    != staging.staging_byte_size
                {
                    return Err(Error::GpuError(format!(
                        "surface {} was re-registered with different geometry ({}x{}); the \
                         cached staging is sized for {}x{} — resolve the surface again",
                        staging.source_surface_key,
                        texture.width(),
                        texture.height(),
                        staging.surface_width,
                        staging.surface_height,
                    )));
                }
                // The registration's last-known layout is the barrier's
                // source. UNDEFINED (a texture nothing has written yet)
                // cannot be a restore target, so such a texture comes to
                // rest in GENERAL and the registration records that.
                let resting_layout = registration.current_layout();
                let restore_layout = if resting_layout == VulkanLayout::UNDEFINED {
                    VulkanLayout::GENERAL
                } else {
                    resting_layout
                };
                recorder.record_image_barrier(
                    texture,
                    resting_layout,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::ALL_TRANSFER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::TRANSFER_READ,
                )?;
                recorder.record_copy_image_to_buffer(
                    texture,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    staging.staging_buffer.as_ref(),
                    ImageCopyRegion::tightly_packed(texture.width(), texture.height()),
                )?;
                recorder.record_image_barrier(
                    texture,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    restore_layout,
                    VulkanStage::ALL_TRANSFER,
                    VulkanStage::ALL_COMMANDS,
                    VulkanAccess::TRANSFER_READ,
                    VulkanAccess::MEMORY_READ,
                )?;
                registration.update_layout(restore_layout);
                Ok(())
            }
            ResolvedBlitSource::PixelBuffer(pixel_buffer) => {
                if pixel_buffer.plane_size(0) != staging.staging_byte_size {
                    return Err(Error::GpuError(format!(
                        "surface {} now resolves to a {}-byte buffer; the cached staging is \
                         sized for {} — resolve the surface again",
                        staging.source_surface_key,
                        pixel_buffer.plane_size(0),
                        staging.staging_byte_size,
                    )));
                }
                // Visibility for a prior producer's GPU writes: submission
                // order alone doesn't make an earlier submission's buffer
                // writes visible to this TRANSFER_READ.
                recorder.record_buffer_barrier(
                    pixel_buffer,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::ALL_TRANSFER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::TRANSFER_READ,
                )?;
                recorder.record_copy_buffer_to_buffer(
                    pixel_buffer,
                    staging.staging_buffer.as_ref(),
                    staging.staging_byte_size,
                )
            }
        }
    }

    /// Copy a written staging buffer back into its source surface, so an
    /// in-place consumer-side edit is visible to every other holder.
    ///
    /// Only for a surface whose sole backing is its own pooled
    /// allocation. A frame its producer also published as a registered
    /// texture is still the producer's, and a texture-backed export has
    /// no write-back path at all (buffer→image plus the layout dance has
    /// no consumer).
    ///
    /// Tested twice: the staging's `writable` is the capability the
    /// consumer was told about when it opened the export, and the
    /// registration test is re-run here. The two can disagree — a
    /// producer can register a texture over the slot after the export
    /// was opened, and while a recycled *frame id* is refused above, a
    /// registration alone advances no generation.
    ///
    /// That live test narrows the window; it does not close it. Nothing
    /// holds the registration and this write-back together, so a producer
    /// registering between the test and the submit still gets its slot
    /// written. Closing it takes the checkout lease, whose claim is
    /// atomic against pool acquire by construction — which a test here
    /// cannot be.
    pub fn copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<u64> {
        let destination = self.resolve_write_back_destination(staging, surface_id)?;
        self.submit_staging_copy_and_wait(staging, staging.refill_submission.lock(), |recorder| {
            Self::record_write_back(staging, &destination, recorder)
        })
    }

    /// [`Self::copy_surface_export_staging_back_to_surface`], but
    /// answering `Ok(None)` instead of queueing when another copy already
    /// holds this staging's recorder.
    ///
    /// Every refusal above — read-only export, a producer's texture
    /// arriving over the slot, a geometry change — stays an error here.
    /// Only the recorder being busy is a contention report.
    pub fn try_copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let destination = self.resolve_write_back_destination(staging, surface_id)?;
        let Some(submission) = staging.refill_submission.try_lock() else {
            return Ok(None);
        };
        self.submit_staging_copy_and_wait(staging, submission, |recorder| {
            Self::record_write_back(staging, &destination, recorder)
        })
        .map(Some)
    }

    /// The guards every write-back runs, plus the pooled allocation the
    /// staged edit publishes into.
    fn resolve_write_back_destination(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        Self::refuse_a_surface_this_staging_does_not_export(staging, surface_id)?;
        self.refuse_a_retired_frame_id(surface_id)?;
        if !staging.writable {
            return Err(Error::GpuError(format!(
                "surface {surface_id}'s export is read-only: the write-back path belongs to \
                 surfaces whose only backing is their own pooled allocation"
            )));
        }
        if self
            .producer_registered_texture_for_surface_id(surface_id)
            .is_some()
        {
            return Err(Error::GpuError(format!(
                "surface {surface_id} has gained a producer's registered texture since its \
                 export was opened read-write; the pooled allocation now backs someone else's \
                 frame and the staged edit cannot be published into it"
            )));
        }
        let ResolvedBlitSource::PixelBuffer(pixel_buffer) =
            self.resolve_device_export_source(surface_id)?
        else {
            return Err(Error::GpuError(format!(
                "surface {surface_id} no longer resolves to a pooled allocation; the staged \
                 edit has nowhere to be published"
            )));
        };
        if pixel_buffer.plane_size(0) != staging.staging_byte_size {
            return Err(Error::GpuError(format!(
                "surface {surface_id} now resolves to a {}-byte buffer; the staged write is \
                 sized for {} — the edit cannot be published",
                pixel_buffer.plane_size(0),
                staging.staging_byte_size,
            )));
        }
        Ok(pixel_buffer)
    }

    /// Record one staging→surface copy.
    fn record_write_back(
        staging: &SurfaceExportStaging,
        destination: &crate::core::rhi::PixelBuffer,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<()> {
        recorder.record_copy_buffer_to_buffer(
            staging.staging_buffer.as_ref(),
            destination,
            staging.staging_byte_size,
        )?;
        // The published edit must be visible to whoever reads next —
        // downstream GPU consumers and, via the coherent mapping the
        // host wait covers, CPU readers.
        recorder.record_buffer_barrier(
            destination,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::MEMORY_READ,
        )
    }

    /// Export the staging buffer's OPAQUE_FD plus byte size and the
    /// exporting device's UUID — the CUDA import triple. The fd
    /// transfers to the caller.
    pub fn export_surface_export_staging_opaque_fd(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        let fd = staging.staging_buffer.export_opaque_fd_memory()?;
        let uuid = staging
            .staging_buffer
            .vulkan_device()
            .physical_device_uuid();
        Ok((fd, staging.staging_byte_size, uuid))
    }
}

// In-process passthroughs on the limited capability, `host_inner`-direct
// like `create_exportable_timeline_semaphore`: a per-frame export must be
// reachable from `process` without escalating, and the cdylib arm is not
// grown for a surface #1715 deletes — a cdylib caller panics at the
// `host_inner` guard.
impl crate::core::context::GpuContextLimitedAccess {
    /// See [`GpuContext::share_surface_export_staging`].
    #[cfg(target_os = "linux")]
    pub fn share_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(String, PixelFormat)> {
        self.host_inner().share_surface_export_staging(staging)
    }

    /// See [`GpuContext::surface_export_staging`].
    pub fn surface_export_staging(
        &self,
        surface_id: &str,
        residency: SurfaceExportStagingResidency,
    ) -> Result<Arc<SurfaceExportStaging>> {
        self.host_inner()
            .surface_export_staging(surface_id, residency)
    }

    /// See [`GpuContext::refill_surface_export_staging`].
    pub fn refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<u64> {
        self.host_inner()
            .refill_surface_export_staging(staging, surface_id)
    }

    /// See [`GpuContext::try_refill_surface_export_staging`].
    pub fn try_refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        self.host_inner()
            .try_refill_surface_export_staging(staging, surface_id)
    }

    /// See [`GpuContext::copy_surface_export_staging_back_to_surface`].
    pub fn copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<u64> {
        self.host_inner()
            .copy_surface_export_staging_back_to_surface(staging, surface_id)
    }

    /// See [`GpuContext::try_copy_surface_export_staging_back_to_surface`].
    pub fn try_copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        self.host_inner()
            .try_copy_surface_export_staging_back_to_surface(staging, surface_id)
    }

    /// See [`GpuContext::export_surface_export_staging_opaque_fd`].
    pub fn export_surface_export_staging_opaque_fd(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        self.host_inner()
            .export_surface_export_staging_opaque_fd(staging)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::core::rhi::{PixelBuffer, Texture, TextureDescriptor, TextureUsages};

    const SURFACE_WIDTH: u32 = 64;
    const SURFACE_HEIGHT: u32 = 64;
    /// The pixels the bag was published with.
    const PUBLISHED_FRAME_STAMP: u8 = 0x11;
    /// What the producer's ring slot holds once it has come around again.
    const TWO_FRAMES_LATER_STAMP: u8 = 0x77;
    /// What a consumer writes through a writable export.
    const CONSUMER_EDIT_STAMP: u8 = 0x5a;
    /// A kernel output's pixels — texture-backed, no pooled member.
    const KERNEL_OUTPUT_STAMP: u8 = 0x2b;

    fn gpu_context_or_skip() -> Option<GpuContext> {
        match GpuContext::init_for_platform() {
            Ok(gpu) => Some(gpu),
            Err(_) => {
                println!("Skipping - no GPU device available");
                None
            }
        }
    }

    fn pooled_backing_host_mapping(buffer: &PixelBuffer) -> (*mut u8, usize) {
        let base_address = buffer.plane_base_address(0);
        assert!(
            !base_address.is_null(),
            "a pooled buffer must be host-mapped for this fixture to reach its pixels"
        );
        (base_address, buffer.plane_size(0) as usize)
    }

    fn stamp_every_byte_of_pooled_backing(buffer: &PixelBuffer, stamp: u8) {
        let (base_address, byte_count) = pooled_backing_host_mapping(buffer);
        unsafe { std::ptr::write_bytes(base_address, stamp, byte_count) };
    }

    fn read_pooled_backing_bytes(buffer: &PixelBuffer) -> Vec<u8> {
        let (base_address, byte_count) = pooled_backing_host_mapping(buffer);
        unsafe { std::slice::from_raw_parts(base_address, byte_count) }.to_vec()
    }

    fn assert_every_byte_is(bytes: &[u8], expected: u8, asserted_subject: &str) {
        assert!(!bytes.is_empty(), "{asserted_subject} came back empty");
        if let Some((index, found)) = bytes
            .iter()
            .copied()
            .enumerate()
            .find(|(_, byte)| *byte != expected)
        {
            panic!("{asserted_subject}: byte {index} is {found:#04x}, expected {expected:#04x}");
        }
    }

    /// A DEVICE_LOCAL texture a producer renders into and registers under
    /// a surface id — the camera's ring slot and a kernel's output are
    /// both this shape.
    fn create_producer_owned_texture(gpu: &GpuContext) -> Texture {
        let descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
                .with_usage(
                    TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST
                        | TextureUsages::TEXTURE_BINDING
                        | TextureUsages::STORAGE_BINDING,
                );
        gpu.device()
            .create_texture(&descriptor)
            .expect("producer-owned texture")
    }

    /// The staging is DEVICE_LOCAL OPAQUE_FD memory, so the host reaches
    /// its bytes only through a copy into a mapped allocation — the same
    /// hop a CUDA consumer's device-to-host copy makes.
    fn read_device_export_staging_into_host_bytes(
        gpu: &GpuContext,
        staging: &SurfaceExportStaging,
    ) -> Vec<u8> {
        let byte_size = staging.staging_byte_size();
        let host_readback = gpu
            .acquire_storage_buffer(byte_size)
            .expect("host-visible readback allocation");
        let mut recorder = gpu
            .create_command_recorder("device_export_staging_test_readback")
            .expect("readback recorder");
        recorder.begin().expect("begin readback");
        recorder
            .record_copy_buffer_to_buffer(
                staging.staging_buffer().as_ref(),
                &host_readback,
                byte_size,
            )
            .expect("copy the staging into the readback allocation");
        recorder
            .record_buffer_barrier(
                &host_readback,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::HOST,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::HOST_READ,
            )
            .expect("readback host-read barrier");
        recorder.submit_and_wait().expect("submit the readback");

        let mapped = host_readback.mapped_ptr();
        assert!(!mapped.is_null(), "the readback allocation must be mapped");
        unsafe { std::slice::from_raw_parts(mapped, byte_size as usize) }.to_vec()
    }

    /// The device-side write a consumer would make through a writable
    /// export, before asking for it to be published back.
    fn stamp_every_byte_of_device_export_staging(
        gpu: &GpuContext,
        staging: &SurfaceExportStaging,
        stamp: u8,
    ) {
        let byte_size = staging.staging_byte_size();
        let host_upload = gpu
            .acquire_storage_buffer(byte_size)
            .expect("host-visible upload allocation");
        let mapped = host_upload.mapped_ptr();
        assert!(!mapped.is_null(), "the upload allocation must be mapped");
        unsafe { std::ptr::write_bytes(mapped, stamp, byte_size as usize) };

        let mut recorder = gpu
            .create_command_recorder("device_export_staging_test_upload")
            .expect("upload recorder");
        recorder.begin().expect("begin upload");
        recorder
            .record_copy_buffer_to_buffer(
                &host_upload,
                staging.staging_buffer().as_ref(),
                byte_size,
            )
            .expect("copy the edit into the staging");
        recorder.submit_and_wait().expect("submit the edit");
    }

    /// The producer-published shape #1755 is about: one surface id over a
    /// pooled backing *and* a producer-internal ring texture registered
    /// under that same id, which the producer overwrites in place every
    /// `RING_TEXTURE_COUNT` frames. Replicates `camera_source.rs` —
    /// acquire the pool member, register the ring slot under its id,
    /// render into the slot.
    struct RotatingProducerPublishedFrame {
        surface_id: String,
        /// Held for the fixture's life: the pool reclaims a slot when the
        /// last handle drops, and this frame must stay the bag's frame.
        _pooled_backing: PixelBuffer,
        producer_ring_texture: Texture,
        ring_upload_source: PixelBuffer,
    }

    impl RotatingProducerPublishedFrame {
        fn publish(gpu: &GpuContext, stamp: u8) -> Self {
            let (pool_id, pooled_backing) = gpu
                .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
                .expect("acquire the frame's pooled backing");
            let surface_id = pool_id.to_string();
            stamp_every_byte_of_pooled_backing(&pooled_backing, stamp);

            let producer_ring_texture = create_producer_owned_texture(gpu);
            let (_, ring_upload_source) = gpu
                .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
                .expect("acquire the ring's upload source");
            gpu.register_texture_with_layout(
                &surface_id,
                producer_ring_texture.clone(),
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            );

            let published = Self {
                surface_id,
                _pooled_backing: pooled_backing,
                producer_ring_texture,
                ring_upload_source,
            };
            published.advance_ring_texture_to(gpu, stamp);
            published
        }

        /// The producer comes around to this ring slot again: the slot is
        /// rewritten in place, the pooled backing the bag named is not.
        fn advance_ring_texture_to(&self, gpu: &GpuContext, stamp: u8) {
            stamp_every_byte_of_pooled_backing(&self.ring_upload_source, stamp);
            gpu.copy_pixel_buffer_to_texture(
                &self.ring_upload_source,
                &self.producer_ring_texture,
                &self.surface_id,
                SURFACE_WIDTH,
                SURFACE_HEIGHT,
            )
            .expect("overwrite the producer's ring slot");
        }
    }

    /// Ground truth, not view-identity: once the producer has come back
    /// around to the ring slot, a device export refilled for the bag's
    /// surface id still carries the pixels that bag was published with.
    ///
    /// Mental-revert: resolving the export texture-first blits the ring
    /// slot the producer has already overwritten, and this reads
    /// `TWO_FRAMES_LATER_STAMP` — #1755's reproduction, at engine level.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_device_export_reads_the_published_frame_after_the_producer_ring_advances() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let published = RotatingProducerPublishedFrame::publish(&gpu, PUBLISHED_FRAME_STAMP);

        // The consumer takes its export at bag receipt; the producer then
        // runs ahead of it.
        let staging = gpu
            .surface_export_staging(
                &published.surface_id,
                SurfaceExportStagingResidency::DeviceLocal,
            )
            .expect("device-export staging for the published frame");
        published.advance_ring_texture_to(&gpu, TWO_FRAMES_LATER_STAMP);

        gpu.refill_surface_export_staging(&staging, &published.surface_id)
            .expect("refill the device export");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            PUBLISHED_FRAME_STAMP,
            "the device export of a frame the producer has run past",
        );
    }

    /// A frame the producer also published as a registered texture is one
    /// the producer still owns: an in-place device edit would land in a
    /// live pool slot, so the export is read-only.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_dual_backed_export_refuses_an_in_place_edit() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let published = RotatingProducerPublishedFrame::publish(&gpu, PUBLISHED_FRAME_STAMP);
        let staging = gpu
            .surface_export_staging(
                &published.surface_id,
                SurfaceExportStagingResidency::DeviceLocal,
            )
            .expect("device-export staging for the published frame");

        assert!(
            !staging.writable(),
            "a surface backed by both a pool member and a producer's registered texture \
             must export read-only"
        );
        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &published.surface_id)
            .expect_err("a dual-backed export must refuse a write-back");
        assert!(
            refusal.to_string().contains("read-only"),
            "the refusal must say the export is read-only, got: {refusal}"
        );
    }

    /// A surface whose only backing is its pool member keeps the
    /// write-back protocol — the shape every green wheel device-edit test
    /// exercises. GPU-gated: skips when no device is present.
    #[test]
    fn a_pool_only_export_stays_writable_and_publishes_its_edit() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        let surface_id = pool_id.to_string();
        stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the published frame");
        assert!(
            staging.writable(),
            "a surface whose only backing is its pool member keeps the write-back path"
        );

        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill the device export");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            PUBLISHED_FRAME_STAMP,
            "the device export of a pool-only surface",
        );

        stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("publish the device-side edit back to the surface");

        assert_every_byte_is(
            &read_pooled_backing_bytes(&pooled_backing),
            CONSUMER_EDIT_STAMP,
            "the surface's own allocation after the edit was published",
        );
    }

    /// The advertised capability is a snapshot; the write-back is not.
    ///
    /// A pool slot keeps its id across reuse, so a surface that was
    /// pool-only when a consumer opened its export read-write can be
    /// handed back out to a texture-registering producer while that
    /// consumer still holds the staging. The export keeps saying
    /// writable — it is what the consumer was told — and the write-back
    /// refuses anyway.
    ///
    /// Mental-revert: gate the write-back on `staging.writable` alone
    /// and the staged edit lands in the new owner's live pool slot.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_write_back_refuses_once_a_producer_registers_a_texture_over_the_slot() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        let surface_id = pool_id.to_string();
        stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the published frame");
        assert!(
            staging.writable(),
            "the surface was pool-only when the export was opened"
        );
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill the device export");
        stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);

        gpu.register_texture_with_layout(
            &surface_id,
            create_producer_owned_texture(&gpu),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect_err("the write-back must refuse a slot a producer has taken over");
        assert!(
            refusal.to_string().contains("registered texture"),
            "the refusal must name the registration that took the slot, got: {refusal}"
        );
        assert_every_byte_is(
            &read_pooled_backing_bytes(&pooled_backing),
            PUBLISHED_FRAME_STAMP,
            "the pooled allocation after the refused write-back",
        );
    }

    /// Kernel outputs have no pooled member, so the registered texture
    /// stays the export's source — and a texture-backed export has no
    /// write-back path. GPU-gated: skips when no device is present.
    #[test]
    fn a_surface_with_no_pooled_backing_still_exports_its_registered_texture() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let kernel_output_texture = create_producer_owned_texture(&gpu);
        gpu.register_texture_with_layout(
            &surface_id,
            kernel_output_texture.clone(),
            VulkanLayout::UNDEFINED,
        );

        let (_, upload_source) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the kernel output's upload source");
        stamp_every_byte_of_pooled_backing(&upload_source, KERNEL_OUTPUT_STAMP);
        gpu.copy_pixel_buffer_to_texture(
            &upload_source,
            &kernel_output_texture,
            &surface_id,
            SURFACE_WIDTH,
            SURFACE_HEIGHT,
        )
        .expect("render into the kernel output texture");

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the kernel output");
        assert!(
            !staging.writable(),
            "a texture-backed export has no write-back path"
        );

        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill the device export");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            KERNEL_OUTPUT_STAMP,
            "the device export of a surface with no pooled backing",
        );
    }

    /// The bytes of a host-visible staging, read the way its consumer
    /// does: straight off the mapping, with no device-to-host hop. That
    /// this is a plain pointer read *is* the residency's whole point.
    fn read_cpu_readback_staging_through_its_mapping(staging: &SurfaceExportStaging) -> Vec<u8> {
        let mapped = staging.staging_buffer().mapped_ptr();
        assert!(
            !mapped.is_null(),
            "a host-visible staging must be mapped; a null pointer here means the residency \
             fell back to device-local memory no CPU consumer can read"
        );
        unsafe { std::slice::from_raw_parts(mapped, staging.staging_byte_size() as usize) }.to_vec()
    }

    /// The payoff: a CPU consumer reads a texture-backed frame's pixels
    /// off the mapping, with nothing installed on the context.
    ///
    /// Mental-revert: mint this staging `DeviceLocal` and `mapped_ptr()`
    /// is null, which the reader above fails on by name. Verified.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_cpu_readback_of_a_texture_backed_surface_lands_its_pixels_on_the_mapping() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let kernel_output_texture = create_producer_owned_texture(&gpu);
        gpu.register_texture_with_layout(
            &surface_id,
            kernel_output_texture.clone(),
            VulkanLayout::UNDEFINED,
        );

        let (_, upload_source) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the kernel output's upload source");
        stamp_every_byte_of_pooled_backing(&upload_source, KERNEL_OUTPUT_STAMP);
        gpu.copy_pixel_buffer_to_texture(
            &upload_source,
            &kernel_output_texture,
            &surface_id,
            SURFACE_WIDTH,
            SURFACE_HEIGHT,
        )
        .expect("render into the kernel output texture");

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("a cpu-readback staging needs nothing installed");
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill the cpu-readback staging");

        assert_every_byte_is(
            &read_cpu_readback_staging_through_its_mapping(&staging),
            KERNEL_OUTPUT_STAMP,
            "the cpu readback of a texture-backed surface",
        );
    }

    /// A CPU edit written through the mapping publishes back into the
    /// surface's own pooled allocation.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_cpu_edit_through_the_mapping_publishes_back_into_the_pooled_backing() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire a pool-only frame");
        let surface_id = pool_id.to_string();
        stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("a cpu-readback staging for a pool-only frame");
        assert!(
            staging.writable(),
            "a frame whose only backing is its own pooled allocation is writable"
        );
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill before editing");

        let mapped = staging.staging_buffer().mapped_ptr();
        assert!(!mapped.is_null(), "a host-visible staging must be mapped");
        unsafe {
            std::ptr::write_bytes(
                mapped,
                CONSUMER_EDIT_STAMP,
                staging.staging_byte_size() as usize,
            )
        };

        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("publish the CPU edit");
        assert_every_byte_is(
            &read_pooled_backing_bytes(&pooled_backing),
            CONSUMER_EDIT_STAMP,
            "the pooled backing after a CPU edit was published",
        );
    }

    /// The two residencies are two allocations under one surface, and
    /// asking twice at one residency is a cache hit.
    ///
    /// A shared allocation would hand a CUDA consumer host-visible memory
    /// — or a CPU consumer memory it cannot map — depending only on which
    /// of them asked first.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn each_residency_is_its_own_allocation_and_repeats_hit_the_cache() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, _pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire a frame to export both ways");
        let surface_id = pool_id.to_string();

        let host_visible = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("the cpu-readback staging");
        let device_local = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("the device-export staging");

        assert!(
            !Arc::ptr_eq(&host_visible, &device_local),
            "one surface's two residencies must be two allocations"
        );
        assert!(
            !host_visible.staging_buffer().mapped_ptr().is_null(),
            "the host-visible residency is mapped"
        );

        let host_visible_again = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("the cpu-readback staging, again");
        assert!(
            Arc::ptr_eq(&host_visible, &host_visible_again),
            "a second ask at the same residency must hit the cache, not allocate a second \
             staging the first consumer is not refilling"
        );
    }

    /// `try_` answers `contended` while another copy holds the recorder,
    /// and runs the copy once it is free.
    ///
    /// This is what `contended` means now that the capability is the
    /// engine's: not a foreign registry's counter, but work already in
    /// flight against this staging.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_try_copy_answers_contended_only_while_the_recorder_is_held() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, _pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire a frame to read back");
        let surface_id = pool_id.to_string();
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("the cpu-readback staging");

        let held = staging.refill_submission.lock();
        assert!(
            gpu.try_refill_surface_export_staging(&staging, &surface_id)
                .expect("contention is not an error")
                .is_none(),
            "a copy already holding the recorder must read as contended"
        );
        drop(held);

        assert!(
            gpu.try_refill_surface_export_staging(&staging, &surface_id)
                .expect("the refill itself must succeed")
                .is_some(),
            "with the recorder free the try_ path must run the copy, not report contention"
        );
    }

    /// A guard refusal stays an error in the `try_` spelling — never a
    /// contention report, which a caller would retry forever.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_try_copy_reports_a_guard_refusal_as_an_error_and_never_as_contention() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (staged_pool_id, _staged_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame the staging is opened for");
        let (other_pool_id, _other_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire a second, unrelated frame");
        let staged_surface_id = staged_pool_id.to_string();
        let other_surface_id = other_pool_id.to_string();
        assert_ne!(
            crate::core::rhi::pool_slot_key_of_surface_id(&staged_surface_id),
            crate::core::rhi::pool_slot_key_of_surface_id(&other_surface_id),
            "the fixture needs two distinct pool slots to cross the staging against"
        );

        let staging = gpu
            .surface_export_staging(
                &staged_surface_id,
                SurfaceExportStagingResidency::HostVisible,
            )
            .expect("the cpu-readback staging");

        match gpu.try_refill_surface_export_staging(&staging, &other_surface_id) {
            Err(refusal) => assert!(
                refusal.to_string().contains(&other_surface_id),
                "the refusal must name the surface asked for, got: {refusal}"
            ),
            Ok(None) => panic!("a staging opened for another surface is an error, not contention"),
            Ok(Some(_)) => panic!("a staging must never carry another surface's pixels"),
        }
    }
}
