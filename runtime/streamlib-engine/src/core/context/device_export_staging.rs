// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface staging for handing engine frames to an external device
//! consumer (CUDA today) — the "one GPU blit into an exportable staging
//! buffer" the plan decides.
//!
//! Why a blit at all: pool allocations are DMA-BUF-flavoured, external
//! device APIs import OPAQUE_FD, and on NVIDIA one allocation cannot
//! export both — `vkGetPhysicalDeviceExternalBufferProperties` reports
//! the two handle types in disjoint `compatibleHandleTypes` sets, so a
//! dual-flavour allocation is spec-invalid (VUID-VkExportMemoryAllocateInfo-
//! handleTypes-00656). The staging buffer is the bridge: OPAQUE_FD
//! DEVICE_LOCAL, allocated once per surface, refilled by a VRAM-side
//! copy each time a consumer asks.
//!
//! The blit source is resolved **per refill**, never cached: rotating
//! producers re-register a different texture under the same surface id
//! every frame (the camera's ring does exactly this), so a source
//! resolved at creation silently blits the previous cycle's frame. The
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
//! the same timeline: [`GpuContext::share_device_export_staging`]
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

/// Bound on the host wait for a staging refill. The copy is VRAM→VRAM
/// (tens of microseconds); a wait that reaches this bound is a wedged
/// queue, not a slow copy.
const STAGING_REFILL_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

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

/// One surface's device-export staging: the OPAQUE_FD buffer an external
/// device consumer imports, the exportable timeline that orders refills,
/// and the recorder that reuses one command pool across them.
pub struct SurfaceDeviceExportStaging {
    surface_id: String,
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
    /// Whether [`GpuContext::copy_device_export_staging_back_to_surface`]
    /// can honour a write — true only for buffer-backed sources today.
    writable: bool,
    /// The surface-share id this staging is published under, once it has
    /// been. Registration hands the service a dup of the staging fd and
    /// the timeline fd, so repeating it per frame would leak a pair per
    /// frame; holding the id here makes the publish once-per-staging and
    /// still lets a failed attempt be retried.
    #[cfg(target_os = "linux")]
    surface_share_registration_id: Mutex<Option<String>>,
}

impl SurfaceDeviceExportStaging {
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

    /// Whether a device consumer may write and copy back.
    pub fn writable(&self) -> bool {
        self.writable
    }

    /// Borrow the staging allocation (for export or import bookkeeping).
    pub fn staging_buffer(&self) -> &Arc<HostVulkanBuffer> {
        &self.staging_buffer
    }

    /// The timeline a consumer waits on; each refill signals the value
    /// [`GpuContext::refill_device_export_staging`] returns.
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

impl GpuContext {
    /// Resolve the current blit source for `surface_id`, texture-first —
    /// the registered texture is the frame's device-resident truth; the
    /// pooled pixel buffer is a derived host-visible copy and the
    /// fallback for textureless producers.
    fn resolve_device_export_source(&self, surface_id: &str) -> Result<ResolvedBlitSource> {
        match self.resolve_texture_registration_by_surface_id(surface_id, None, 0, 0) {
            Ok(registration) => Ok(ResolvedBlitSource::RegisteredTexture(registration)),
            Err(texture_miss) => match self.resolve_pixel_buffer_by_surface_id(surface_id) {
                Ok(pixel_buffer) => Ok(ResolvedBlitSource::PixelBuffer(pixel_buffer)),
                Err(buffer_miss) => Err(Error::GpuError(format!(
                    "surface {surface_id} resolves to neither a registered texture \
                     ({texture_miss}) nor a pixel buffer ({buffer_miss})"
                ))),
            },
        }
    }

    /// The device-export staging for `surface_id`, created on first ask
    /// and cached on this context — it dies with the context and is
    /// evicted by [`Self::unregister_texture`], never by a rotating
    /// re-registration, which the per-refill source resolve absorbs.
    pub fn surface_device_export_staging(
        &self,
        surface_id: &str,
    ) -> Result<Arc<SurfaceDeviceExportStaging>> {
        if let Some(existing) = self.device_export_stagings.lock().get(surface_id) {
            return Ok(Arc::clone(existing));
        }

        let (staging_byte_size, surface_width, surface_height, pixel_format, writable) =
            match self.resolve_device_export_source(surface_id)? {
                ResolvedBlitSource::RegisteredTexture(registration) => {
                    let texture = registration.texture();
                    let pixel_shape = export_pixel_shape_for_texture(texture.format())?;
                    let (width, height) = (texture.width(), texture.height());
                    // 4-byte color by `export_pixel_shape_for_texture`'s
                    // restriction — the same arithmetic the refill guard and
                    // the copy region assume.
                    let byte_size = u64::from(width) * u64::from(height) * 4;
                    (byte_size, width, height, Some(pixel_shape), false)
                }
                ResolvedBlitSource::PixelBuffer(pixel_buffer) => {
                    let format = pixel_buffer.format();
                    export_bytes_per_pixel_for_pixel_format(format)?;
                    let byte_size = pixel_buffer.plane_size(0);
                    if byte_size == 0 {
                        return Err(Error::GpuError(format!(
                            "surface {surface_id} resolves to a zero-byte plane; nothing to export"
                        )));
                    }
                    (
                        byte_size,
                        pixel_buffer.width,
                        pixel_buffer.height,
                        Some(format),
                        true,
                    )
                }
            };

        let vulkan_device = self.device().vulkan_device();
        let staging_buffer = Arc::new(HostVulkanBuffer::new_opaque_fd_export_device_local(
            vulkan_device,
            staging_byte_size,
        )?);
        let refill_done_timeline = self.create_exportable_timeline_semaphore(0)?;
        let refill_submission = Mutex::new(RefillSubmission {
            recorder: self.create_command_recorder("device_export_refill")?,
            next_signal_value: 1,
        });

        let staging = Arc::new(SurfaceDeviceExportStaging {
            surface_id: surface_id.to_string(),
            staging_buffer,
            refill_done_timeline,
            refill_submission,
            staging_byte_size,
            surface_width,
            surface_height,
            pixel_format,
            writable,
            #[cfg(target_os = "linux")]
            surface_share_registration_id: Mutex::new(None),
        });
        // Double-check under the insert lock: a concurrent asker for the
        // same surface_id may have published one while this thread was
        // allocating. The loser's freshly-built staging drops here rather
        // than replacing the entry other holders (and the wheel's
        // CUDA-import memo) key on.
        let mut stagings = self.device_export_stagings.lock();
        if let Some(published) = stagings.get(surface_id) {
            return Ok(Arc::clone(published));
        }
        stagings.insert(surface_id.to_string(), Arc::clone(&staging));
        Ok(staging)
    }

    /// Publish this staging and its refill timeline to the surface-share
    /// service, and answer with the id they are registered under.
    ///
    /// This is how a consumer one process away reaches the export: it
    /// checks the id out, receives the staging's OPAQUE_FD and the
    /// timeline's fd over SCM_RIGHTS, imports the memory into its own
    /// device API, and waits on the timeline for the value each refill
    /// returns. The published id is derived from the source surface's so
    /// the two never collide — the source is already registered under
    /// its own id with its DMA-BUF planes, which is a different
    /// allocation in a different external-handle flavour.
    ///
    /// Registers at most once per staging; later calls answer with the
    /// same id.
    #[cfg(target_os = "linux")]
    pub fn share_device_export_staging(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<String> {
        let mut registration_id = staging.surface_share_registration_id.lock();
        if let Some(already_registered) = registration_id.as_ref() {
            return Ok(already_registered.clone());
        }
        let surface_store = self.surface_store().ok_or_else(|| {
            Error::GpuError(
                "this runtime has no surface-share service, so a device export cannot reach \
                 another process"
                    .into(),
            )
        })?;
        let pixel_format = staging.pixel_format.ok_or_else(|| {
            Error::GpuError(format!(
                "the device-export staging for surface {} carries no pixel shape; a consumer \
                 would have no layout to import it under",
                staging.surface_id
            ))
        })?;
        let shared_id = format!("{}-device-export-staging", staging.surface_id);
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
        Ok(shared_id)
    }

    /// Drop the cached device-export staging for `surface_id`, if any.
    /// Outstanding consumers keep theirs alive through their own `Arc`s.
    pub(crate) fn evict_device_export_staging(&self, surface_id: &str) {
        self.device_export_stagings.lock().remove(surface_id);
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
    fn submit_staging_copy_and_wait(
        &self,
        staging: &SurfaceDeviceExportStaging,
        record_copy: impl FnOnce(&mut RhiCommandRecorder) -> Result<()>,
    ) -> Result<u64> {
        let signal_value;
        {
            let mut submission = staging.refill_submission.lock();
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
        }
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        Ok(signal_value)
    }

    /// Copy the surface's current pixels into the staging buffer.
    /// Resolves the source fresh — a rotating producer's latest
    /// registration, not a snapshot. Returns the signalled timeline
    /// value a cross-process consumer would wait on instead of relying
    /// on the in-process host wait.
    pub fn refill_device_export_staging(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<u64> {
        let source = self.resolve_device_export_source(&staging.surface_id)?;
        self.submit_staging_copy_and_wait(staging, |recorder| match &source {
            ResolvedBlitSource::RegisteredTexture(registration) => {
                let texture = registration.texture();
                if u64::from(texture.width()) * u64::from(texture.height()) * 4
                    != staging.staging_byte_size
                {
                    return Err(Error::GpuError(format!(
                        "surface {} was re-registered with different geometry ({}x{}); the \
                         cached staging is sized for {}x{} — resolve the surface again",
                        staging.surface_id,
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
                        staging.surface_id,
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
        })
    }

    /// Copy a written staging buffer back into its source surface, so an
    /// in-place device-side edit is visible to every other holder.
    ///
    /// Buffer-backed sources only: the texture write-back (buffer→image
    /// plus the layout dance) has no consumer yet, and a texture-backed
    /// export is read-only by construction.
    pub fn copy_device_export_staging_back_to_surface(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<u64> {
        let source = self.resolve_device_export_source(&staging.surface_id)?;
        let ResolvedBlitSource::PixelBuffer(pixel_buffer) = source else {
            return Err(Error::GpuError(
                "this surface's device export is read-only: it is texture-backed, and the \
                 write-back path exists for buffer-backed surfaces only"
                    .into(),
            ));
        };
        if pixel_buffer.plane_size(0) != staging.staging_byte_size {
            return Err(Error::GpuError(format!(
                "surface {} now resolves to a {}-byte buffer; the staged write is sized for {} \
                 — the edit cannot be published",
                staging.surface_id,
                pixel_buffer.plane_size(0),
                staging.staging_byte_size,
            )));
        }
        self.submit_staging_copy_and_wait(staging, |recorder| {
            recorder.record_copy_buffer_to_buffer(
                staging.staging_buffer.as_ref(),
                &pixel_buffer,
                staging.staging_byte_size,
            )?;
            // The published edit must be visible to whoever reads next —
            // downstream GPU consumers and, via the coherent mapping the
            // host wait covers, CPU readers.
            recorder.record_buffer_barrier(
                &pixel_buffer,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::MEMORY_READ,
            )
        })
    }

    /// Export the staging buffer's OPAQUE_FD plus byte size and the
    /// exporting device's UUID — the CUDA import triple. The fd
    /// transfers to the caller.
    pub fn export_device_staging_opaque_fd(
        &self,
        staging: &SurfaceDeviceExportStaging,
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
    /// See [`GpuContext::share_device_export_staging`].
    #[cfg(target_os = "linux")]
    pub fn share_device_export_staging(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<String> {
        self.host_inner().share_device_export_staging(staging)
    }

    /// See [`GpuContext::surface_device_export_staging`].
    pub fn surface_device_export_staging(
        &self,
        surface_id: &str,
    ) -> Result<Arc<SurfaceDeviceExportStaging>> {
        self.host_inner().surface_device_export_staging(surface_id)
    }

    /// See [`GpuContext::refill_device_export_staging`].
    pub fn refill_device_export_staging(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<u64> {
        self.host_inner().refill_device_export_staging(staging)
    }

    /// See [`GpuContext::copy_device_export_staging_back_to_surface`].
    pub fn copy_device_export_staging_back_to_surface(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<u64> {
        self.host_inner()
            .copy_device_export_staging_back_to_surface(staging)
    }

    /// See [`GpuContext::export_device_staging_opaque_fd`].
    pub fn export_device_staging_opaque_fd(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        self.host_inner().export_device_staging_opaque_fd(staging)
    }
}
