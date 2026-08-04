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
//! Shape constraint: this is the surface both placements share. In-process
//! callers use it directly; the helper-process arrangement (#1714)
//! registers the same staging + timeline with surface-share once and
//! triggers refills over the existing timeline protocol — so nothing
//! here may assume the consumer lives in this process. The bounded host
//! wait after each submit is the in-process convenience only; the
//! exportable timeline is the contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::core::context::GpuContext;
use crate::core::error::{Error, Result};
use crate::core::rhi::PixelBuffer;
use crate::host_rhi::{HostGpuDeviceExt as _, VulkanAccess, VulkanStage};
use crate::vulkan::rhi::{
    HostVulkanBuffer, HostVulkanTimelineSemaphore, ImageCopyRegion, RhiCommandRecorder,
};
use streamlib_consumer_rhi::{TextureFormat, VulkanLayout};

/// Bound on the host wait for a staging refill. The copy is VRAM→VRAM
/// (tens of microseconds); a wait that reaches this bound is a wedged
/// queue, not a slow copy.
const STAGING_REFILL_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

/// The texture formats a device-export blit accepts: 4-byte single-plane
/// color. The staging buffer is sized `width * height * 4` and DLPack
/// consumers see `(H, W, 4)` u8 — a format that breaks that arithmetic
/// must be refused at creation, not exported with wrong strides.
fn texture_format_bytes_per_pixel(format: TextureFormat) -> Result<u64> {
    match format {
        TextureFormat::Rgba8Unorm
        | TextureFormat::Rgba8UnormSrgb
        | TextureFormat::Bgra8Unorm
        | TextureFormat::Bgra8UnormSrgb => Ok(4),
        other => Err(Error::GpuError(format!(
            "device export supports 4-byte single-plane color textures; {other:?} has no \
             single-buffer DLPack shape at 4 bytes per pixel"
        ))),
    }
}

/// The source a staging buffer refills from, resolved once at creation.
///
/// Texture-first, matching the engine's own resolve order — the
/// registered ring texture is the frame's device-resident truth, and
/// blitting from it is VRAM→VRAM; the pooled pixel buffer is already a
/// derived host-visible copy, so sourcing from it crosses PCIe and is
/// the fallback for textureless producers.
enum DeviceExportSource {
    RegisteredTexture {
        registration: crate::core::context::TextureRegistration,
        width: u32,
        height: u32,
    },
    PixelBuffer {
        pixel_buffer: PixelBuffer,
    },
}

/// One surface's device-export staging: the OPAQUE_FD buffer an external
/// device consumer imports, the exportable timeline that orders refills,
/// and the recorder that reuses one command pool across them.
pub struct SurfaceDeviceExportStaging {
    staging_buffer: Arc<HostVulkanBuffer>,
    refill_done_timeline: Arc<HostVulkanTimelineSemaphore>,
    next_refill_signal_value: AtomicU64,
    /// One recorder, reused: per-refill pool creation is the exact churn
    /// `docs/learnings/nvidia-opaque-fd-after-swapchain.md` warns about.
    refill_recorder: Mutex<RhiCommandRecorder>,
    source: DeviceExportSource,
    staging_byte_size: u64,
    /// Whether [`GpuContext::copy_device_export_staging_back_to_surface`]
    /// can honour a write — true only for buffer-backed sources today.
    writable: bool,
}

impl SurfaceDeviceExportStaging {
    /// Byte size of the staging buffer — the size a DLPack consumer's
    /// tensor spans.
    pub fn staging_byte_size(&self) -> u64 {
        self.staging_byte_size
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
}

impl GpuContext {
    /// Create the device-export staging for `surface_id`, resolving the
    /// blit source once: the registered texture when one exists, the
    /// pooled pixel buffer otherwise.
    ///
    /// One per surface, held by the caller — creation per frame would
    /// churn exportable `vkAllocateMemory`, which NVIDIA's kernel
    /// accounting punishes even when freed.
    pub fn create_surface_device_export_staging(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
    ) -> Result<Arc<SurfaceDeviceExportStaging>> {
        let (source, staging_byte_size, writable) = match self
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, 0, 0)
        {
            Ok(registration) => {
                let texture = registration.texture();
                let (width, height, format) = (texture.width(), texture.height(), texture.format());
                let bytes_per_pixel = texture_format_bytes_per_pixel(format)?;
                let byte_size = u64::from(width) * u64::from(height) * bytes_per_pixel;
                (
                    DeviceExportSource::RegisteredTexture {
                        registration,
                        width,
                        height,
                    },
                    byte_size,
                    false,
                )
            }
            Err(_) => {
                let pixel_buffer = self.resolve_pixel_buffer_by_surface_id(surface_id)?;
                let byte_size = pixel_buffer.plane_size(0);
                if byte_size == 0 {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} resolves to a zero-byte plane; nothing to export"
                    )));
                }
                (
                    DeviceExportSource::PixelBuffer { pixel_buffer },
                    byte_size,
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
        let refill_recorder = Mutex::new(self.create_command_recorder("device_export_refill")?);

        Ok(Arc::new(SurfaceDeviceExportStaging {
            staging_buffer,
            refill_done_timeline,
            next_refill_signal_value: AtomicU64::new(1),
            refill_recorder,
            source,
            staging_byte_size,
            writable,
        }))
    }

    /// Copy the surface's current pixels into the staging buffer, signal
    /// the refill timeline, and wait (bounded) for the copy to land.
    /// Returns the signalled timeline value — what a cross-process
    /// consumer would wait on instead of relying on this host wait.
    pub fn refill_device_export_staging(
        &self,
        staging: &SurfaceDeviceExportStaging,
    ) -> Result<u64> {
        let mut recorder = staging.refill_recorder.lock();
        recorder.begin()?;
        match &staging.source {
            DeviceExportSource::RegisteredTexture {
                registration,
                width,
                height,
            } => {
                // The texture's last-known layout is the barrier's source;
                // restored afterwards so the producer's next frame and any
                // sibling consumer see the layout the registration claims.
                let resting_layout = registration.current_layout();
                recorder.record_image_barrier(
                    registration.texture(),
                    resting_layout,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::ALL_TRANSFER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::TRANSFER_READ,
                )?;
                recorder.record_copy_image_to_buffer(
                    registration.texture(),
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    staging.staging_buffer.as_ref(),
                    ImageCopyRegion::tightly_packed(*width, *height),
                )?;
                recorder.record_image_barrier(
                    registration.texture(),
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    resting_layout,
                    VulkanStage::ALL_TRANSFER,
                    VulkanStage::ALL_COMMANDS,
                    VulkanAccess::TRANSFER_READ,
                    VulkanAccess::MEMORY_READ,
                )?;
            }
            DeviceExportSource::PixelBuffer { pixel_buffer } => {
                recorder.record_copy_buffer_to_buffer(
                    pixel_buffer,
                    staging.staging_buffer.as_ref(),
                    staging.staging_byte_size,
                )?;
            }
        }
        let signal_value = staging
            .next_refill_signal_value
            .fetch_add(1, Ordering::SeqCst);
        recorder.submit_signaling_timeline(&staging.refill_done_timeline, signal_value)?;
        drop(recorder);
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        Ok(signal_value)
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
        let DeviceExportSource::PixelBuffer { pixel_buffer } = &staging.source else {
            return Err(Error::GpuError(
                "this surface's device export is read-only: it is texture-backed, and the \
                 write-back path exists for buffer-backed surfaces only"
                    .into(),
            ));
        };
        let mut recorder = staging.refill_recorder.lock();
        recorder.begin()?;
        recorder.record_copy_buffer_to_buffer(
            staging.staging_buffer.as_ref(),
            pixel_buffer,
            staging.staging_byte_size,
        )?;
        let signal_value = staging
            .next_refill_signal_value
            .fetch_add(1, Ordering::SeqCst);
        recorder.submit_signaling_timeline(&staging.refill_done_timeline, signal_value)?;
        drop(recorder);
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        Ok(signal_value)
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
    /// See [`GpuContext::create_surface_device_export_staging`].
    pub fn create_surface_device_export_staging(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
    ) -> Result<Arc<SurfaceDeviceExportStaging>> {
        self.host_inner()
            .create_surface_device_export_staging(surface_id, texture_layout)
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
