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
//! One staging per (surface, residency), shared by every holder of that
//! slot — so two consumers reading one surface at one residency map the
//! same allocation, and nothing here arbitrates their overlap. The
//! `try_` ops' `contended` reports a busy *recorder*, not a competing
//! reader: it keeps two copies from interleaving, not two consumers from
//! reading a buffer a third is refilling. The deleted cpu-readback
//! bridge carried the same limitation in its own words; it is restated
//! here because that is where the staging now lives.
//!
//! Nothing here assumes the consumer lives in this process. A processor
//! reaching this from its own helper process gets the same staging and
//! the same timeline: [`GpuContext::share_surface_export_staging`]
//! publishes both to the surface-share service once, and the child
//! triggers refills through the escalate ops that wrap the methods
//! below. The bounded host wait after each submit is a convenience for
//! callers already in this process; the exportable timeline is the
//! contract, and it is what a child waits on.

use std::collections::HashMap;
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
    /// speaks host memory only. Allocated HOST_CACHED where the device
    /// has a cached exportable type, since every consumer of this
    /// residency reads the mapping; write-combined memory elsewhere,
    /// which is slower to read but never unavailable.
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
}

impl std::fmt::Display for SurfaceExportStagingResidency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DeviceLocal => "device-local",
            Self::HostVisible => "host-visible",
        })
    }
}

/// The pixel shape a texture-backed export presents to its consumer.
/// Restricted to single-plane color: the tightly-packed copy region and
/// the consumer-side layout both express one strided buffer. BGRA stays
/// BGRA: relabeling those bytes RGBA would silently swap channels, and
/// the float formats keep their float identity for the same reason —
/// a kernel's HDR output reaches the consumer as float16/float32
/// elements, never as bytes wearing an integer label.
///
/// Residency-neutral, because the constraint is: a staging is one
/// buffer, and both residencies hand out exactly that one buffer.
fn export_pixel_shape_for_texture(format: TextureFormat) -> Result<PixelFormat> {
    match format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => Ok(PixelFormat::Rgba32),
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => Ok(PixelFormat::Bgra32),
        TextureFormat::Rgba16Float => Ok(PixelFormat::Rgba16Float),
        TextureFormat::Rgba32Float => Ok(PixelFormat::Rgba32Float),
        TextureFormat::Nv12 => Err(Error::GpuError(
            "a surface export refuses NV12: it is two planes, and a one-buffer export would drop \
             chroma"
                .into(),
        )),
    }
}

fn export_bytes_per_pixel_for_pixel_format(format: PixelFormat) -> Result<u32> {
    if format.plane_count() > 1 || format == PixelFormat::Unknown {
        return Err(Error::GpuError(format!(
            "a surface export refuses {format:?}: a staging is one buffer, and exporting only \
             the first plane would hand out part of the image"
        )));
    }
    Ok(format.bits_per_pixel() / 8)
}

/// What a staging holds once the copy being submitted lands.
///
/// Passed into the submit rather than written after it, because the write-back
/// guard compares against this and both must be ordered by the same lock.
enum FrameThisStagingHoldsAfterTheCopy<'a> {
    /// A refill: the staging now holds this frame's pixels.
    TheFrameJustReadIn(&'a str),
    /// A write-back: the copy reads the staging, so what it holds is
    /// whatever the refill before it put there.
    WhateverItAlreadyHeld,
}

/// A registration-layout update the copy being submitted makes true.
///
/// Returned by the record step and applied only after the submission
/// succeeds: the barriers that settle the layout execute only then, so
/// recording it earlier would let a failed submit leave the cell naming
/// a layout the image never reached — and the next copy's barrier would
/// name a wrong source.
struct TextureLayoutSettledByThisCopy {
    registration: crate::core::context::TextureRegistration,
    settled_layout: VulkanLayout,
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
    /// format, or the color shape a texture source maps to (its own
    /// float width included).
    pixel_format: Option<PixelFormat>,
    /// Which backing kind the staging was minted over — what the
    /// write-back resolves its destination against, and what
    /// [`Self::writable`] derives from.
    backing_kind_at_mint: SurfaceExportStagingBackingKindAtMint,
    /// The frame id a refill last read into this staging, if any.
    ///
    /// Names a *frame*, not a flag, because the staging is cached per pool
    /// slot and spans the generations that slot publishes: a bool would
    /// let frame `<slot>#7`'s staged pixels be published over `<slot>#8`'s
    /// backing — another frame's picture, in the write direction, which is
    /// what `[surface-id-lifetime-contract]` exists to refuse. A
    /// write-back is an edit *of the frame that was read in*, so it must
    /// name that frame.
    ///
    /// `None` until the first refill: a freshly allocated staging holds
    /// whatever the allocator handed back, and publishing that would
    /// replace a live picture with uninitialised memory.
    frame_last_read_into_this_staging: Mutex<Option<String>>,
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

    /// Whether a consumer may write and copy back — derived from the
    /// backing kind the staging was minted over, so each kind's write
    /// rule lives on its own variant.
    pub fn writable(&self) -> bool {
        match self.backing_kind_at_mint {
            SurfaceExportStagingBackingKindAtMint::RegisteredTexture {
                texture_takes_a_recorded_copy_in,
            } => texture_takes_a_recorded_copy_in,
            SurfaceExportStagingBackingKindAtMint::PooledPixelBuffer {
                pooled_allocation_was_the_only_backing_at_mint,
            } => pooled_allocation_was_the_only_backing_at_mint,
        }
    }

    /// Where this staging's memory lives.
    pub fn residency(&self) -> SurfaceExportStagingResidency {
        self.residency
    }

    /// Run `while_held` with this staging's recorder taken, so a test can
    /// drive the contended path the `try_` ops report.
    ///
    /// Two concurrent copies produce this state on their own — that is
    /// what `contended` reports. What a test cannot do without a hook is
    /// make the window deterministic, because no engine path holds the
    /// recorder *across* a call.
    #[cfg(test)]
    pub(crate) fn while_holding_the_refill_recorder_for_a_test<T>(
        &self,
        while_held: impl FnOnce() -> T,
    ) -> T {
        let _held = self.refill_submission.lock();
        while_held()
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

/// Which backing kind the staging was minted over, carrying what that
/// kind's write-back rule needs. The write-back publishes into the same
/// kind: a pool-backed staging's edit lands in the pooled allocation
/// under the pool-member rule, a texture-backed staging's in the
/// registered texture itself, and a surface whose backing kind changed
/// since mint refuses rather than publishing into a different world
/// than the consumer was reading. [`SurfaceExportStaging::writable`]
/// derives from this, so a staging carrying the wrong kind's write rule
/// is unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SurfaceExportStagingBackingKindAtMint {
    /// A registered texture with no pooled member (a kernel output).
    /// Writable exactly when the image can take a recorded copy.
    RegisteredTexture {
        texture_takes_a_recorded_copy_in: bool,
    },
    /// A pool member. Writable only while the pooled allocation is the
    /// surface's sole backing — the `[surface-id-lifetime-contract]`
    /// rule; a snapshot the write-back re-tests.
    PooledPixelBuffer {
        pooled_allocation_was_the_only_backing_at_mint: bool,
    },
}

/// Which copy a texture-backed staging guard protects — it decides the
/// transfer usage the image must carry and how the refusal names the
/// remedy.
#[derive(Clone, Copy)]
enum SurfaceExportStagingTextureCopyDirection {
    RefillIntoStaging,
    WriteBackIntoSurface,
}

impl SurfaceExportStagingTextureCopyDirection {
    /// How the refusal names what was staged.
    fn staged_subject(self) -> &'static str {
        match self {
            Self::RefillIntoStaging => "the cached staging",
            Self::WriteBackIntoSurface => "the staged write",
        }
    }

    /// The remedy clause the refusal ends with.
    fn remedy(self) -> &'static str {
        match self {
            Self::RefillIntoStaging => "resolve the surface again",
            Self::WriteBackIntoSurface => "the edit cannot be published",
        }
    }
}

/// What a refill resolved this frame — looked up fresh on every copy so
/// a rotating producer's re-registration is honoured, never a snapshot.
enum ResolvedBlitSource {
    RegisteredTexture(crate::core::context::TextureRegistration),
    PixelBuffer(crate::core::rhi::PixelBuffer),
}

/// Where a write-back publishes the staged edit — the same backing kind
/// the staging was minted over, re-resolved and re-guarded at publish.
enum ResolvedWriteBackDestination {
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
    backing_kind_at_mint: SurfaceExportStagingBackingKindAtMint,
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
        if let Some(existing) = self
            .surface_export_stagings
            .lock()
            .get(source_surface_key)
            .and_then(|by_residency| by_residency.get(&residency))
        {
            return Ok(Arc::clone(existing));
        }

        let shape = match self.resolve_device_export_source(surface_id)? {
            ResolvedBlitSource::RegisteredTexture(registration) => {
                let texture = registration.texture();
                let pixel_format = export_pixel_shape_for_texture(texture.format())?;
                let export_bytes_per_pixel = export_bytes_per_pixel_for_pixel_format(pixel_format)?;
                let (surface_width, surface_height) = (texture.width(), texture.height());
                SurfaceExportStagingShape {
                    staging_byte_size: u64::from(surface_width)
                        * u64::from(surface_height)
                        * u64::from(export_bytes_per_pixel),
                    surface_width,
                    surface_height,
                    pixel_format,
                    // This arm answers only when no pooled member resolves
                    // ahead of the registration — in-tree that means none
                    // exists, because every producer that publishes a pool
                    // frame acquires it in this process, and cross-process
                    // registrations mint their own handle ids rather than
                    // pool ids. The pool-member rule the buffer arm
                    // computes below therefore has nothing to protect
                    // here; what gates the write-back instead is whether
                    // the image can legally take a recorded copy, which
                    // its usage decided at allocation.
                    backing_kind_at_mint:
                        SurfaceExportStagingBackingKindAtMint::RegisteredTexture {
                            texture_takes_a_recorded_copy_in: texture.supports_transfer_write(),
                        },
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
                    backing_kind_at_mint:
                        SurfaceExportStagingBackingKindAtMint::PooledPixelBuffer {
                            pooled_allocation_was_the_only_backing_at_mint:
                                pooled_allocation_is_the_only_backing,
                        },
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
                HostVulkanBuffer::new_opaque_fd_export_host_cached(
                    vulkan_device,
                    shape.staging_byte_size,
                )?
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
            backing_kind_at_mint: shape.backing_kind_at_mint,
            frame_last_read_into_this_staging: Mutex::new(None),
            #[cfg(target_os = "linux")]
            surface_share_registration_id: Mutex::new(None),
        });
        // Double-check under the insert lock: a concurrent asker for the
        // same slot and residency may have published one while this thread
        // was allocating. The loser's freshly-built staging drops here
        // rather than replacing the entry other holders (and the wheel's
        // import memo) key on.
        let mut stagings = self.surface_export_stagings.lock();
        let by_residency = stagings.entry(source_surface_key.to_string()).or_default();
        if let Some(published) = by_residency.get(&residency) {
            return Ok(Arc::clone(published));
        }
        by_residency.insert(residency, Arc::clone(&staging));
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
                staging.residency, staging.source_surface_key
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
        surface_store.host_inner().register_surface_export_staging(
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
        // One removal takes every residency: they are the surface's inner
        // map, and the surface they stage is going away.
        let evicted = self
            .surface_export_stagings
            .lock()
            .remove(crate::core::rhi::pool_slot_key_of_surface_id(surface_id));
        for staging in evicted.into_iter().flat_map(HashMap::into_values) {
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
    ///
    /// Takes the recorder it is handed rather than acquiring it, so the
    /// blocking and non-blocking entry points differ only in how they got
    /// one — and neither carries the other's return shape.
    fn submit_staging_copy_and_wait(
        staging: &SurfaceExportStaging,
        mut submission: parking_lot::MutexGuard<'_, RefillSubmission>,
        holds_after_this_copy: FrameThisStagingHoldsAfterTheCopy<'_>,
        record_copy: impl FnOnce(
            &mut RhiCommandRecorder,
        ) -> Result<Option<TextureLayoutSettledByThisCopy>>,
    ) -> Result<u64> {
        submission.recorder.begin()?;
        let texture_layout_settled_by_this_copy = match record_copy(&mut submission.recorder) {
            Ok(settled) => settled,
            Err(record_failure) => {
                submission.recorder.abort_recording();
                return Err(record_failure);
            }
        };
        let signal_value = submission.next_signal_value;
        submission.next_signal_value += 1;
        if let Err(submit_failure) = submission
            .recorder
            .submit_signaling_timeline(&staging.refill_done_timeline, signal_value)
        {
            submission.recorder.abort_recording();
            return Err(submit_failure);
        }
        // Under the recorder guard, with the submit that made it true: the
        // guard serialises submission order, and these copies retire in that
        // order on the one queue. Recorded outside it, two refills of the same
        // slot at different generations could land their copies in one order
        // and their bookkeeping in the other — leaving the staging holding one
        // frame while the field named another, which is the write the frame
        // check exists to refuse.
        if let FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id) =
            holds_after_this_copy
        {
            *staging.frame_last_read_into_this_staging.lock() = Some(surface_id.to_string());
        }
        if let Some(settled) = texture_layout_settled_by_this_copy {
            settled.registration.update_layout(settled.settled_layout);
        }
        drop(submission);
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        Ok(signal_value)
    }

    /// The contention contract in one place: only a busy recorder is
    /// `Ok(None)`. Every guard refusal reaches here already as an `Err`,
    /// because the guards run before the lock is even attempted.
    fn try_submit_staging_copy_and_wait(
        staging: &SurfaceExportStaging,
        holds_after_this_copy: FrameThisStagingHoldsAfterTheCopy<'_>,
        record_copy: impl FnOnce(
            &mut RhiCommandRecorder,
        ) -> Result<Option<TextureLayoutSettledByThisCopy>>,
    ) -> Result<Option<u64>> {
        let Some(submission) = staging.refill_submission.try_lock() else {
            return Ok(None);
        };
        Self::submit_staging_copy_and_wait(staging, submission, holds_after_this_copy, record_copy)
            .map(Some)
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
                staging.residency, staging.source_surface_key
            )));
        }
        Ok(())
    }

    /// Refuse a registered texture this staging cannot legally copy with
    /// in `direction` — the guard both texture-arm copies share, run
    /// against the freshly resolved registration so a rotating producer's
    /// re-registration is judged, never a snapshot.
    ///
    /// Three refusals, in order of severity: an image whose usage forbids
    /// the copy (recording it anyway is a Vulkan spec violation the
    /// driver never reports); a format change (two same-size formats
    /// would silently swap channels); a geometry change (the staging is
    /// sized for the old extent).
    fn refuse_a_texture_this_staging_cannot_copy_with(
        staging: &SurfaceExportStaging,
        surface_named: &str,
        texture: &crate::core::rhi::Texture,
        direction: SurfaceExportStagingTextureCopyDirection,
    ) -> Result<()> {
        let copy_is_legal = match direction {
            SurfaceExportStagingTextureCopyDirection::RefillIntoStaging => {
                texture.supports_transfer_read()
            }
            SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface => {
                texture.supports_transfer_write()
            }
        };
        if !copy_is_legal {
            let missing_usage = match direction {
                SurfaceExportStagingTextureCopyDirection::RefillIntoStaging => "copy_src",
                SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface => "copy_dst",
            };
            return Err(Error::GpuError(format!(
                "surface {surface_named}'s texture was allocated without {missing_usage:?} \
                 usage, so no copy may touch it — acquire the texture with that usage to \
                 export it"
            )));
        }
        let export_pixel_format = export_pixel_shape_for_texture(texture.format())?;
        // Format identity, not just byte size: two 4-byte formats (RGBA
        // vs BGRA) match on size, and staging the bytes of one under the
        // label of the other swaps channels.
        if Some(export_pixel_format) != staging.pixel_format {
            return Err(Error::GpuError(format!(
                "surface {surface_named} was re-registered as {:?}; {} presents {:?} — {}",
                export_pixel_format,
                direction.staged_subject(),
                staging.pixel_format,
                direction.remedy(),
            )));
        }
        let export_bytes_per_pixel = export_bytes_per_pixel_for_pixel_format(export_pixel_format)?;
        if u64::from(texture.width())
            * u64::from(texture.height())
            * u64::from(export_bytes_per_pixel)
            != staging.staging_byte_size
        {
            return Err(Error::GpuError(format!(
                "surface {surface_named} was re-registered with different geometry ({}x{}); {} \
                 is sized for {}x{} — {}",
                texture.width(),
                texture.height(),
                direction.staged_subject(),
                staging.surface_width,
                staging.surface_height,
                direction.remedy(),
            )));
        }
        Ok(())
    }

    /// The layouts a registered texture's staged copy transitions
    /// between: its last-known resting layout, and where it comes back
    /// to rest. UNDEFINED (a texture nothing has written yet) cannot be
    /// a restore target, so such a texture comes to rest in GENERAL and
    /// the caller records that on the registration.
    fn resting_and_restore_layouts_of(
        registration: &crate::core::context::TextureRegistration,
    ) -> (VulkanLayout, VulkanLayout) {
        let resting_layout = registration.current_layout();
        let restore_layout = if resting_layout == VulkanLayout::UNDEFINED {
            VulkanLayout::GENERAL
        } else {
            resting_layout
        };
        (resting_layout, restore_layout)
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
        Self::submit_staging_copy_and_wait(
            staging,
            staging.refill_submission.lock(),
            FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id),
            |recorder| Self::record_refill(staging, &source, recorder),
        )
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
        Self::try_submit_staging_copy_and_wait(
            staging,
            FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id),
            |recorder| Self::record_refill(staging, &source, recorder),
        )
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

    /// Record one surface→staging copy, answering the layout update the
    /// submission settles. Geometry is re-checked here rather than at
    /// resolve: a re-registration between the two would otherwise size
    /// the copy from a shape the staging no longer has.
    fn record_refill(
        staging: &SurfaceExportStaging,
        source: &ResolvedBlitSource,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<Option<TextureLayoutSettledByThisCopy>> {
        match source {
            ResolvedBlitSource::RegisteredTexture(registration) => {
                let texture = registration.texture();
                Self::refuse_a_texture_this_staging_cannot_copy_with(
                    staging,
                    &staging.source_surface_key,
                    texture,
                    SurfaceExportStagingTextureCopyDirection::RefillIntoStaging,
                )?;
                let (resting_layout, restore_layout) =
                    Self::resting_and_restore_layouts_of(registration);
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
                Ok(Some(TextureLayoutSettledByThisCopy {
                    registration: registration.clone(),
                    settled_layout: restore_layout,
                }))
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
                )?;
                Ok(None)
            }
        }
    }

    /// Copy a written staging buffer back into its source surface, so an
    /// in-place consumer-side edit is visible to every other holder.
    ///
    /// The destination is the backing kind the staging was minted over:
    /// a pooled allocation for a pool-backed staging — only when it is
    /// the surface's sole backing, because a frame its producer also
    /// published as a registered texture is still the producer's — or
    /// the registered texture itself for a texture-backed staging (a
    /// kernel output), via buffer→image plus the same layout dance the
    /// refill's read direction records.
    ///
    /// The pool arm is tested twice: the staging's `writable` is the
    /// capability the consumer was told about when it opened the export,
    /// and the registration test is re-run here. The two can disagree — a
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
        Self::submit_staging_copy_and_wait(
            staging,
            staging.refill_submission.lock(),
            FrameThisStagingHoldsAfterTheCopy::WhateverItAlreadyHeld,
            |recorder| Self::record_write_back(staging, &destination, recorder),
        )
    }

    /// [`Self::copy_surface_export_staging_back_to_surface`], but
    /// answering `Ok(None)` instead of queueing when another copy already
    /// holds this staging's recorder.
    ///
    /// Every refusal above — read-only export, a producer's texture
    /// arriving over the slot, a backing-kind change, a usage the copy
    /// is illegal for, a format or geometry change — stays an error
    /// here. Only the recorder being busy is a contention report.
    pub fn try_copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let destination = self.resolve_write_back_destination(staging, surface_id)?;
        Self::try_submit_staging_copy_and_wait(
            staging,
            FrameThisStagingHoldsAfterTheCopy::WhateverItAlreadyHeld,
            |recorder| Self::record_write_back(staging, &destination, recorder),
        )
    }

    /// Refuse publishing a staging that holds no frame, or another frame
    /// than the one named — the read-before-write rule both backing
    /// kinds share, because a write-back is an edit *of the frame that
    /// was read in* whichever memory it lands in.
    fn refuse_a_write_of_a_frame_this_staging_does_not_hold(
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<()> {
        match staging.frame_last_read_into_this_staging.lock().as_deref() {
            None => Err(Error::GpuError(format!(
                "surface {surface_id}'s export staging has never been read into; publishing \
                 it would write uninitialised memory over a live frame — read the frame in \
                 before publishing an edit of it"
            ))),
            Some(read_in) if read_in != surface_id => Err(Error::GpuError(format!(
                "surface {surface_id}'s export staging currently holds frame {read_in}; \
                 publishing it would write that frame's pixels over this one — read this \
                 frame in before publishing an edit of it"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// The guards every write-back runs, plus the backing the staged
    /// edit publishes into — the kind the staging was minted over.
    fn resolve_write_back_destination(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<ResolvedWriteBackDestination> {
        Self::refuse_a_surface_this_staging_does_not_export(staging, surface_id)?;
        self.refuse_a_retired_frame_id(surface_id)?;
        match staging.backing_kind_at_mint {
            SurfaceExportStagingBackingKindAtMint::PooledPixelBuffer {
                pooled_allocation_was_the_only_backing_at_mint,
            } => {
                if !pooled_allocation_was_the_only_backing_at_mint {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id}'s export is read-only: this frame is a pool \
                         member its producer still owns, and a pooled allocation publishes an \
                         edit only when it is the surface's sole backing"
                    )));
                }
                if self
                    .producer_registered_texture_for_surface_id(surface_id)
                    .is_some()
                {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} has gained a producer's registered texture since \
                         its export was opened read-write; the pooled allocation now backs \
                         someone else's frame and the staged edit cannot be published into it"
                    )));
                }
                Self::refuse_a_write_of_a_frame_this_staging_does_not_hold(staging, surface_id)?;
                let ResolvedBlitSource::PixelBuffer(pixel_buffer) =
                    self.resolve_device_export_source(surface_id)?
                else {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} no longer resolves to a pooled allocation; the \
                         staged edit has nowhere to be published"
                    )));
                };
                if pixel_buffer.plane_size(0) != staging.staging_byte_size {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} now resolves to a {}-byte buffer; the staged \
                         write is sized for {} — the edit cannot be published",
                        pixel_buffer.plane_size(0),
                        staging.staging_byte_size,
                    )));
                }
                Ok(ResolvedWriteBackDestination::PixelBuffer(pixel_buffer))
            }
            SurfaceExportStagingBackingKindAtMint::RegisteredTexture {
                texture_takes_a_recorded_copy_in,
            } => {
                if !texture_takes_a_recorded_copy_in {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id}'s export is read-only: its texture was allocated \
                         without \"copy_dst\" usage, so no staged edit may be copied into it — \
                         acquire the texture with that usage to write it through an export"
                    )));
                }
                Self::refuse_a_write_of_a_frame_this_staging_does_not_hold(staging, surface_id)?;
                let ResolvedBlitSource::RegisteredTexture(registration) =
                    self.resolve_device_export_source(surface_id)?
                else {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} has gained a pooled backing since its \
                         texture-backed export was opened; the staged edit's destination is no \
                         longer the texture the consumer read — resolve the surface again"
                    )));
                };
                Self::refuse_a_texture_this_staging_cannot_copy_with(
                    staging,
                    surface_id,
                    registration.texture(),
                    SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface,
                )?;
                Ok(ResolvedWriteBackDestination::RegisteredTexture(
                    registration,
                ))
            }
        }
    }

    /// Record one staging→surface copy, into whichever backing kind the
    /// staging was minted over, answering the layout update the
    /// submission settles.
    fn record_write_back(
        staging: &SurfaceExportStaging,
        destination: &ResolvedWriteBackDestination,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<Option<TextureLayoutSettledByThisCopy>> {
        match destination {
            ResolvedWriteBackDestination::PixelBuffer(pixel_buffer) => {
                recorder.record_copy_buffer_to_buffer(
                    staging.staging_buffer.as_ref(),
                    pixel_buffer,
                    staging.staging_byte_size,
                )?;
                // The published edit must be visible to whoever reads next —
                // downstream GPU consumers and, via the coherent mapping the
                // host wait covers, CPU readers.
                recorder.record_buffer_barrier(
                    pixel_buffer,
                    VulkanStage::ALL_TRANSFER,
                    VulkanStage::ALL_COMMANDS,
                    VulkanAccess::TRANSFER_WRITE,
                    VulkanAccess::MEMORY_READ,
                )?;
                Ok(None)
            }
            ResolvedWriteBackDestination::RegisteredTexture(registration) => {
                let texture = registration.texture();
                // The refill's layout dance with the transfer arrow
                // reversed.
                let (resting_layout, restore_layout) =
                    Self::resting_and_restore_layouts_of(registration);
                recorder.record_image_barrier(
                    texture,
                    resting_layout,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::ALL_TRANSFER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::TRANSFER_WRITE,
                )?;
                recorder.record_copy_buffer_to_image(
                    staging.staging_buffer.as_ref(),
                    texture,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    ImageCopyRegion::tightly_packed(texture.width(), texture.height()),
                )?;
                recorder.record_image_barrier(
                    texture,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    restore_layout,
                    VulkanStage::ALL_TRANSFER,
                    VulkanStage::ALL_COMMANDS,
                    VulkanAccess::TRANSFER_WRITE,
                    VulkanAccess::MEMORY_READ,
                )?;
                Ok(Some(TextureLayoutSettledByThisCopy {
                    registration: registration.clone(),
                    settled_layout: restore_layout,
                }))
            }
        }
    }

    /// Export the staging buffer's OPAQUE_FD plus byte size and the
    /// exporting device's UUID — what an importer needs at either
    /// residency, and the CUDA import triple at `DeviceLocal`. The fd
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
    use vulkanalia::vk;

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
    /// stays the export's source — writable, because the pool-member
    /// rule has nothing to protect there. GPU-gated: skips when no
    /// device is present.
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
            staging.writable(),
            "a texture-backed export is writable: a kernel output has no pooled member, so \
             the pool-member rule has nothing to protect"
        );

        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("refill the device export");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            KERNEL_OUTPUT_STAMP,
            "the device export of a surface with no pooled backing",
        );
    }

    /// The scope's write direction: an edit staged over a texture-backed
    /// export publishes back into the registered texture itself, proven
    /// by a second refill reading the edit out of the texture.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_texture_backed_export_publishes_its_edit_back_into_the_texture() {
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
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("read the kernel output into the staging");
        stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("publish the edit into the texture");

        // Round trip: a fresh refill reads the texture, not the staging's
        // leftover bytes, so the edit surviving it proves the texture took
        // the write.
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("re-read the texture after the write-back");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            CONSUMER_EDIT_STAMP,
            "the texture's pixels after a staged edit published back",
        );
    }

    /// The read-before-write rule holds for the texture arm too: a
    /// staging never refilled has nothing to edit, and publishing it
    /// would write allocator garbage over the kernel's output.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_texture_backed_write_back_of_a_never_filled_staging_is_refused() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        gpu.register_texture_with_layout(
            &surface_id,
            create_producer_owned_texture(&gpu),
            VulkanLayout::UNDEFINED,
        );
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the kernel output");
        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect_err("a never-filled texture staging must not publish");
        assert!(
            refusal.to_string().contains("never been read into"),
            "the refusal must name the missing read, got: {refusal}"
        );
    }

    /// The float path's write direction: an rgba16_float edit staged at
    /// 8 bytes per pixel publishes into the texture and survives a fresh
    /// refill — a stride or region error in the wider-than-4-byte
    /// arithmetic would corrupt this round trip, not the mint the sizing
    /// test covers. GPU-gated: skips when no device is present.
    #[test]
    fn a_float_format_edit_round_trips_through_the_texture() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba16Float)
                .with_usage(
                    TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST
                        | TextureUsages::STORAGE_BINDING,
                );
        let float_texture = gpu
            .device()
            .create_texture(&descriptor)
            .expect("rgba16_float kernel output texture");
        gpu.register_texture_with_layout(&surface_id, float_texture, VulkanLayout::UNDEFINED);
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("a float texture's export staging");

        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("read the (undefined) float texture in");
        stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("publish the float edit into the texture");
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("re-read the texture after the write-back");
        assert_every_byte_is(
            &read_device_export_staging_into_host_bytes(&gpu, &staging),
            CONSUMER_EDIT_STAMP,
            "an rgba16_float texture's pixels after a staged edit published back",
        );
    }

    /// A rotating producer re-registering the slot with a different
    /// format or geometry is judged at the copy, not trusted from mint:
    /// a same-size format swap would relabel channels, and a new extent
    /// would mis-size the copy region. GPU-gated: skips when no device
    /// is present.
    #[test]
    fn a_re_registration_with_a_different_shape_is_refused_at_the_copy() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        gpu.register_texture_with_layout(
            &surface_id,
            create_producer_owned_texture(&gpu),
            VulkanLayout::UNDEFINED,
        );
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the rgba8 registration");

        // Same byte size, different channel order — the case a
        // size-only guard would silently relabel.
        let bgra_descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Bgra8Unorm)
                .with_usage(TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
        gpu.register_texture_with_layout(
            &surface_id,
            gpu.device()
                .create_texture(&bgra_descriptor)
                .expect("the re-registered bgra texture"),
            VulkanLayout::UNDEFINED,
        );
        let format_refusal = gpu
            .refill_surface_export_staging(&staging, &surface_id)
            .expect_err("a same-size format swap must refuse");
        assert!(
            format_refusal.to_string().contains("re-registered as"),
            "the refusal must name the format change, got: {format_refusal}"
        );

        // Same format, different extent — the copy region would misfit.
        let smaller_descriptor = TextureDescriptor::new(
            SURFACE_WIDTH / 2,
            SURFACE_HEIGHT / 2,
            TextureFormat::Rgba8Unorm,
        )
        .with_usage(TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
        gpu.register_texture_with_layout(
            &surface_id,
            gpu.device()
                .create_texture(&smaller_descriptor)
                .expect("the re-registered smaller texture"),
            VulkanLayout::UNDEFINED,
        );
        let geometry_refusal = gpu
            .refill_surface_export_staging(&staging, &surface_id)
            .expect_err("a geometry change must refuse");
        assert!(
            geometry_refusal.to_string().contains("different geometry"),
            "the refusal must name the geometry change, got: {geometry_refusal}"
        );
    }

    /// Recording a copy against an image whose usage forbids it is a
    /// Vulkan spec violation the driver never reports — the refill
    /// refuses by name instead. GPU-gated: skips when no device is
    /// present.
    #[test]
    fn a_refill_of_a_texture_without_copy_src_usage_is_refused() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::TEXTURE_BINDING);
        let sampled_only_texture = gpu
            .device()
            .create_texture(&descriptor)
            .expect("sampled-only texture");
        gpu.register_texture_with_layout(
            &surface_id,
            sampled_only_texture,
            VulkanLayout::UNDEFINED,
        );
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("the staging mints from format and geometry alone");
        let refusal = gpu
            .refill_surface_export_staging(&staging, &surface_id)
            .expect_err("a copy out of a sampled-only image must refuse");
        assert!(
            refusal.to_string().contains("copy_src"),
            "the refusal must name the missing usage, got: {refusal}"
        );
    }

    /// The write direction's twin: an image without COPY_DST advertises
    /// a read-only export and refuses a publish by naming the usage.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_write_back_into_a_texture_without_copy_dst_usage_is_refused() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC);
        let readable_only_texture = gpu
            .device()
            .create_texture(&descriptor)
            .expect("readable-only texture");
        gpu.register_texture_with_layout(
            &surface_id,
            readable_only_texture,
            VulkanLayout::UNDEFINED,
        );
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("device-export staging for the readable-only texture");
        assert!(
            !staging.writable(),
            "an image that cannot take a recorded copy must advertise a read-only export"
        );
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("the read direction needs only copy_src");
        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect_err("a copy into a copy_dst-less image must refuse");
        assert!(
            refusal.to_string().contains("copy_dst"),
            "the refusal must name the missing usage, got: {refusal}"
        );
    }

    /// A float kernel output sizes its staging by its own pixel width
    /// and presents its float identity — the HDR shape the scope exists
    /// for. GPU-gated: skips when no device is present.
    #[test]
    fn a_float_format_texture_sizes_its_staging_by_its_own_pixel_width() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let surface_id = uuid::Uuid::new_v4().to_string();
        let descriptor =
            TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba16Float)
                .with_usage(
                    TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST
                        | TextureUsages::STORAGE_BINDING,
                );
        let float_texture = gpu
            .device()
            .create_texture(&descriptor)
            .expect("rgba16_float kernel output texture");
        gpu.register_texture_with_layout(&surface_id, float_texture, VulkanLayout::UNDEFINED);

        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
            .expect("a float texture's export staging");
        assert_eq!(
            staging.staging_byte_size(),
            u64::from(SURFACE_WIDTH) * u64::from(SURFACE_HEIGHT) * 8,
            "an rgba16_float staging spans 8 bytes per pixel"
        );
        assert_eq!(
            staging.pixel_format(),
            Some(PixelFormat::Rgba16Float),
            "the staged shape keeps its float identity"
        );
        assert!(staging.writable(), "a texture-backed export is writable");
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

        // Every consumer of this residency reads the mapping, so it must
        // come off the cached pool wherever the device has one. On a
        // device without, it degrades to the write-combined pool — slower
        // to read, never refused, and never a third residency.
        let vulkan_device = gpu.device().vulkan_device();
        let host_visible_memory_type_index = host_visible
            .staging_buffer()
            .vma_allocation_memory_type_index()
            .expect("the host-visible staging states its memory type index");
        let memory_properties = vulkan_device.allocator().get_memory_properties();
        let host_visible_flags =
            memory_properties.memory_types[host_visible_memory_type_index as usize].property_flags;
        if vulkan_device.opaque_fd_buffer_pool_host_cached().is_some() {
            assert!(
                host_visible_flags.contains(vk::MemoryPropertyFlags::HOST_CACHED),
                "a device with a cached exportable type must mint the HostVisible \
                 residency from the cached pool"
            );
        }
        assert!(
            host_visible_flags.contains(
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
            ),
            "the child-side OPAQUE_FD import requires HOST_VISIBLE | HOST_COHERENT, and \
             nothing on this path flushes or invalidates"
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

    /// A staging nobody has read a frame into holds whatever the
    /// allocator handed back. Publishing it would replace a live frame's
    /// picture with uninitialised memory.
    ///
    /// Reachable only because this change made it so: at main the op
    /// answered "no bridge registered" and touched nothing.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_write_back_of_a_never_filled_staging_is_refused_before_it_touches_the_frame() {
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
            .expect("a freshly minted cpu-readback staging");

        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect_err("a never-filled staging has no edit to publish");
        assert!(
            refusal.to_string().contains("never been read into"),
            "the refusal must name why, got: {refusal}"
        );

        // The frame is untouched — the refusal came before any submit.
        assert_every_byte_is(
            &read_pooled_backing_bytes(&pooled_backing),
            PUBLISHED_FRAME_STAMP,
            "the live frame after a refused write-back",
        );

        // And the guard lifts once the frame has actually been read in.
        gpu.refill_surface_export_staging(&staging, &surface_id)
            .expect("read the frame into the staging");
        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("a filled staging may publish its edit");
    }

    /// One staging spans every frame its pool slot publishes, so the
    /// write-back must name the frame that was read in — not merely
    /// *some* currently-valid frame.
    ///
    /// The gap this closes is narrow, because `refuse_a_retired_frame_id`
    /// already catches a write-back naming the *older* frame. What it
    /// cannot catch is the other order: read `<slot>#N` in, let the slot
    /// cycle, then publish against the now-current `<slot>#N+1` — every
    /// earlier guard passes and frame N's pixels land on frame N+1's
    /// backing. That is another frame's picture in the write direction,
    /// which `[surface-id-lifetime-contract]` refuses.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_write_back_naming_a_different_frame_than_was_read_in_is_refused() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (first_pool_id, first_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame that gets read in");
        let read_in_surface_id = first_pool_id.to_string();
        let slot = crate::core::rhi::pool_slot_key_of_surface_id(&read_in_surface_id).to_string();
        stamp_every_byte_of_pooled_backing(&first_backing, PUBLISHED_FRAME_STAMP);

        let staging = gpu
            .surface_export_staging(
                &read_in_surface_id,
                SurfaceExportStagingResidency::HostVisible,
            )
            .expect("the cpu-readback staging");
        gpu.refill_surface_export_staging(&staging, &read_in_surface_id)
            .expect("read this frame in");

        // Let the slot cycle: release it, then take it back. The staging
        // is cached per slot, so the next generation gets this very one.
        drop(first_backing);
        let mut recycled = None;
        let mut _held_elsewhere = Vec::new();
        for _ in 0..8 {
            let (pool_id, backing) = gpu
                .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
                .expect("re-acquire from the pool");
            if crate::core::rhi::pool_slot_key_of_surface_id(&pool_id.to_string()) == slot {
                recycled = Some((pool_id.to_string(), backing));
                break;
            }
            _held_elsewhere.push(backing);
        }
        let Some((next_frame_surface_id, next_backing)) = recycled else {
            println!(
                "a_write_back_naming_a_different_frame_than_was_read_in_is_refused: the pool \
                 never handed slot {slot} back — skipping"
            );
            return;
        };
        assert_ne!(
            next_frame_surface_id, read_in_surface_id,
            "recycling the slot must publish a new frame id"
        );
        stamp_every_byte_of_pooled_backing(&next_backing, TWO_FRAMES_LATER_STAMP);

        let refusal = gpu
            .copy_surface_export_staging_back_to_surface(&staging, &next_frame_surface_id)
            .expect_err("a staging holding the previous frame must not publish over this one");
        assert!(
            refusal.to_string().contains(&read_in_surface_id),
            "the refusal must name the frame the staging actually holds, got: {refusal}"
        );
        assert_every_byte_is(
            &read_pooled_backing_bytes(&next_backing),
            TWO_FRAMES_LATER_STAMP,
            "the new frame's backing after a refused cross-frame write-back",
        );
    }

    /// What the staging holds and what it says it holds are written under
    /// one lock, so they cannot be ordered against each other.
    ///
    /// Two threads refilling one staging both take the recorder, and the
    /// copies retire in the order the guard granted it. Recording the frame
    /// id outside that guard let the two orders diverge: the staging would
    /// hold one frame while the field named another, and a write-back for
    /// the named frame would then pass the identity check and publish the
    /// other frame's pixels. The field is written under the guard now, so
    /// whichever copy submitted last is also the one the field names.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn concurrent_refills_leave_the_staging_naming_the_frame_it_actually_holds() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (pool_id, pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire a frame to refill from");
        let surface_id = pool_id.to_string();
        stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);
        let staging = gpu
            .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
            .expect("the cpu-readback staging");

        // Both threads refill the same staging, repeatedly, through the very
        // guard the bookkeeping now rides.
        let gpu = Arc::new(gpu);
        let both_are_ready = Arc::new(std::sync::Barrier::new(2));
        let refillers: Vec<_> = (0..2)
            .map(|_| {
                let gpu = Arc::clone(&gpu);
                let staging = Arc::clone(&staging);
                let surface_id = surface_id.clone();
                let both_are_ready = Arc::clone(&both_are_ready);
                std::thread::spawn(move || {
                    both_are_ready.wait();
                    for _ in 0..16 {
                        gpu.refill_surface_export_staging(&staging, &surface_id)
                            .expect("a refill of a live frame succeeds");
                    }
                })
            })
            .collect();
        for refiller in refillers {
            refiller.join().expect("both refillers finish");
        }

        // Every refill named this frame, so the field must too — and the
        // write-back it gates must be accepted rather than refused as
        // holding somebody else's frame.
        assert_eq!(
            staging.frame_last_read_into_this_staging.lock().as_deref(),
            Some(surface_id.as_str()),
            "the staging must name the frame its last copy read in"
        );
        gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
            .expect("a staging naming this frame may publish back into it");
    }
}
