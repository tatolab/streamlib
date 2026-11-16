// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::GpuContext;
use crate::core::context::surfaces_and_their_stores::surface_backing_resolution::{
    ResolvedSurfaceBacking, export_bytes_per_pixel_for_pixel_format, export_pixel_shape_for_texture,
};
use crate::core::context::texture_pools_and_rings::texture_registration::TextureLayoutSettledByThisCopy;
use crate::core::error::{Error, Result};
use crate::core::rhi::Texture;
use crate::host_rhi::HostGpuDeviceExt as _;
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt as _;
use crate::vulkan::rhi::{
    HostVulkanBuffer, HostVulkanTimelineSemaphore, ImageCopyRegion, RhiCommandRecorder,
};
use streamlib_consumer_rhi::{PixelFormat, TextureFormat, VulkanLayout};

mod limited_access_surface_export_staging_passthroughs;
mod staging_copy_submission_and_refusals;
mod surface_export_staging_copy_back;
mod surface_export_staging_open_share_and_evict;
mod surface_export_staging_refill;
#[cfg(all(test, target_os = "linux"))]
mod tests;

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
    /// A DMA-BUF image with an explicit tiled DRM format modifier — what a
    /// display (Chromium's shared-image import in Tato Studio's viewer)
    /// imports and samples. An OPAQUE_FD image or a LINEAR DMA-BUF does not
    /// show on NVIDIA, so a viewer of such a frame reads this copy of it.
    /// Read-only: nothing writes a viewer's copy back.
    DisplayImportable,
}

impl SurfaceExportStagingResidency {
    /// The suffix distinguishing this residency's surface-share
    /// registration from the other's for the same surface. Two
    /// residencies are two allocations and must never collide on one id.
    fn surface_share_id_suffix(self) -> &'static str {
        match self {
            Self::DeviceLocal => "device-export-staging",
            Self::HostVisible => "cpu-readback-staging",
            Self::DisplayImportable => "display-importable-staging",
        }
    }
}

impl std::fmt::Display for SurfaceExportStagingResidency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DeviceLocal => "device-local",
            Self::HostVisible => "host-visible",
            Self::DisplayImportable => "display-importable",
        })
    }
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

/// How much of the frame a staged write-back replaces, which decides whether
/// the frame must have been read into the staging first.
#[derive(Clone, Copy)]
enum StagedWriteBackCoverage {
    /// An edit of the pixels read in: the rest of the staging is that frame.
    AnEditOfTheFrameReadIn,
    /// Every byte of the staging was written, so nothing of the old frame
    /// survives to be read in.
    EveryPixelOfTheFrame,
}

impl StagedWriteBackCoverage {
    fn refuse_a_write_this_staging_does_not_cover(
        self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<()> {
        match self {
            Self::AnEditOfTheFrameReadIn => {
                GpuContext::refuse_a_write_of_a_frame_this_staging_does_not_hold(
                    staging, surface_id,
                )
            }
            Self::EveryPixelOfTheFrame => Ok(()),
        }
    }
}

/// The recorder plus the next timeline value, under one lock: the
/// correctness of the strictly-increasing signal values depends on the
/// value being drawn and submitted while this lock is held, so the
/// invariant is structural rather than a convention beside an atomic.
struct RefillSubmission {
    recorder: RhiCommandRecorder,
    next_signal_value: u64,
}

/// The memory a staging is, decided by its residency.
enum SurfaceExportStagingMemory {
    /// An OPAQUE_FD `VkBuffer`, device-local or host-visible.
    OpaqueFdBuffer(Arc<HostVulkanBuffer>),
    /// A DMA-BUF image with an explicit tiled modifier, for a display.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "only the Linux build uses it")
    )]
    DisplayImportableImage(Texture),
}

/// One surface's export staging: the memory a consumer one hop away
/// imports, the timeline that orders refills, and the recorder that
/// reuses one command pool across them.
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
    staging_memory: SurfaceExportStagingMemory,
    /// UUID of the Vulkan device that owns the staging allocation.
    exporting_device_uuid: [u8; 16],
    refill_done_timeline: Arc<HostVulkanTimelineSemaphore>,
    /// Whether `refill_done_timeline` was minted exportable — true exactly
    /// where the device exports a timeline semaphore as an OPAQUE_FD.
    refill_done_timeline_crosses_processes: bool,
    /// One recorder, reused: per-refill pool creation is the exact churn
    /// NVIDIA's OPAQUE_FD export cap after a swapchain causes.
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
    /// rule lives on its own variant. A display's copy is never written.
    pub fn writable(&self) -> bool {
        if self.residency == SurfaceExportStagingResidency::DisplayImportable {
            return false;
        }
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

    /// Borrow the staging buffer (for export or import bookkeeping),
    /// refusing a display's staging, which is an image.
    pub fn staging_buffer(&self) -> Result<&Arc<HostVulkanBuffer>> {
        match &self.staging_memory {
            SurfaceExportStagingMemory::OpaqueFdBuffer(staging_buffer) => Ok(staging_buffer),
            SurfaceExportStagingMemory::DisplayImportableImage(_) => Err(Error::GpuError(format!(
                "the {} export staging for surface {} is an image a display imports, not a \
                     buffer",
                self.residency, self.source_surface_key
            ))),
        }
    }

    /// The exported timeline a consumer in another process waits on, or
    /// `None` where the device exports no timeline semaphore and every copy
    /// is retired on the host before it returns.
    pub fn refill_done_timeline_another_process_waits_on(
        &self,
    ) -> Option<&Arc<HostVulkanTimelineSemaphore>> {
        self.refill_done_timeline_crosses_processes
            .then_some(&self.refill_done_timeline)
    }

    /// UUID of the Vulkan device that owns the staging allocation. An
    /// external device API must import onto this GPU; picking any other
    /// reads the wrong memory rather than failing.
    pub fn exporting_device_uuid(&self) -> [u8; 16] {
        self.exporting_device_uuid
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
    /// The image format a display's staging of this source is allocated
    /// in, `None` where no single-plane colour format carries it.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "only the Linux build uses it")
    )]
    display_importable_image_format: Option<TextureFormat>,
}

/// The single-plane colour format of a pixel buffer's byte order, which
/// a display's staging image of it is allocated in.
fn display_importable_image_format_of_pixel_format(
    pixel_format: PixelFormat,
) -> Option<TextureFormat> {
    match pixel_format {
        PixelFormat::Rgba32 => Some(TextureFormat::Rgba8Unorm),
        PixelFormat::Bgra32 => Some(TextureFormat::Bgra8Unorm),
        PixelFormat::Rgba16Float => Some(TextureFormat::Rgba16Float),
        PixelFormat::Rgba32Float => Some(TextureFormat::Rgba32Float),
        _ => None,
    }
}
