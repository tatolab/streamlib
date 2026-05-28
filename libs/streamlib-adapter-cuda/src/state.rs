// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state.
//!
//! `CudaSurfaceAdapter<D>` is generic over the device flavor and
//! registers two resource flavors at the same scope:
//!
//! - **Buffer** ([`HostSurfaceRegistration`]) — OPAQUE_FD-exportable
//!   `VkBuffer` for the DLPack flat-tensor path
//!   (`cudaExternalMemoryGetMappedBuffer` → flat `void*` consumed by
//!   `from_dlpack`-compatible AI frameworks).
//! - **Image** ([`HostImageSurfaceRegistration`]) — OPAQUE_FD-exportable
//!   `VkImage` for the texture / surface-object path
//!   (`cudaExternalMemoryGetMappedMipmappedArray` → tiled mipmapped
//!   array consumed by `cudaCreateTextureObject` /
//!   `cudaCreateSurfaceObject` for hardware-bilinear sampling and
//!   surface-write writes from CUDA kernels).
//!
//! Both flavors share the same per-surface bookkeeping (timeline,
//! holder counters, release value, layout); the [`SurfaceResource`]
//! enum is the discriminator. The OpenGL adapter uses the same shape
//! at a different scope — one `SurfaceState` with a `target: u32`
//! discriminator covers both `GL_TEXTURE_2D` and
//! `GL_TEXTURE_EXTERNAL_OES`.

use std::sync::Arc;

use streamlib_adapter_abi::{SurfaceId, SurfaceRegistration};
use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout};

/// Buffer-flavored registration — handed to
/// [`crate::CudaSurfaceAdapter::register_host_surface`].
///
/// On the host side `pixel_buffer` is a fresh `Arc<HostVulkanBuffer>`
/// allocated via either `HostVulkanBuffer::new_opaque_fd_export`
/// (HOST_VISIBLE — Python writes via CPU mmap; the carve-out test path)
/// or `HostVulkanBuffer::new_opaque_fd_export_device_local`
/// (DEVICE_LOCAL — host pipeline writes via `vkCmdCopyImageToBuffer`;
/// hot-path camera→inference flows). The adapter holds the `Arc` for the
/// surface's lifetime so the underlying GPU memory stays alive while
/// CUDA references the imported handle.
pub struct HostSurfaceRegistration<P: DevicePrivilege> {
    /// OPAQUE_FD-exportable staging buffer — the resource CUDA imports
    /// via `cudaImportExternalMemory`. Either HOST_VISIBLE (CPU-write
    /// flow) or DEVICE_LOCAL (host-pipeline-write flow); CUDA classifies
    /// the imported pointer automatically via
    /// `cudaPointerGetAttributes` so the adapter doesn't need to know.
    /// Host- or consumer-flavored per `P`.
    pub pixel_buffer: Arc<P::Buffer>,
    /// `produce_done` timeline — signaled exclusively by the producer
    /// process when a write completes (host: GPU submit from
    /// `submit_host_copy_image_to_buffer` or CPU signal from
    /// `end_write_access`). The consumer waits on this timeline before
    /// reading. Single-writer-per-edge per
    /// `docs/architecture/adapter-timeline-single-writer.md`.
    pub produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — signaled exclusively by the consumer
    /// process when a read completes (`end_read_access`). The producer
    /// waits on this timeline before re-writing.
    pub consume_done: Arc<P::TimelineSemaphore>,
    /// Unused on the buffer path (buffers have no `VkImageLayout`); kept
    /// for shape parity with [`HostImageSurfaceRegistration`]. Pass
    /// [`VulkanLayout::UNDEFINED`].
    pub initial_layout: VulkanLayout,
}

/// Image-flavored registration — handed to
/// [`crate::CudaSurfaceAdapter::register_host_image_surface`].
///
/// `texture` is a fresh `Arc<HostVulkanTexture>` allocated via
/// `HostVulkanTexture::new_opaque_fd_export` — DEVICE_LOCAL,
/// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier, format restricted to the
/// CUDA-mappable subset (`Rgba8Unorm`, `Rgba16Float`, `Rgba32Float`).
/// The CUDA cdylib imports this image via `cudaImportExternalMemory`
/// + `cudaExternalMemoryGetMappedMipmappedArray` and constructs
/// `cudaTextureObject_t` / `cudaSurfaceObject_t` handles per acquire.
pub struct HostImageSurfaceRegistration<P: DevicePrivilege> {
    /// OPAQUE_FD-exportable image — the resource CUDA imports as a
    /// tiled mipmapped array. Host- or consumer-flavored per `P`.
    pub texture: Arc<P::Texture>,
    /// `produce_done` timeline — same single-writer contract as
    /// [`HostSurfaceRegistration::produce_done`].
    pub produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — same single-writer contract as
    /// [`HostSurfaceRegistration::consume_done`].
    pub consume_done: Arc<P::TimelineSemaphore>,
    /// Vulkan image layout the image is in at registration time. The
    /// cross-process release path (a `CudaContext.release_for_cross_process`
    /// SDK shim that delegates to `VulkanSurfaceAdapter::release_to_foreign`,
    /// per `docs/architecture/adapter-authoring.md` → "Cross-process
    /// producer composition") reads this when running the producer-side
    /// QFOT release barrier. The cuda adapter itself does NOT issue any
    /// Vulkan-side barriers on the imported image — CUDA's sync runs
    /// pairwise via `cudaWaitExternalSemaphoresAsync` /
    /// `cudaSignalExternalSemaphoresAsync` on the timeline.
    pub initial_layout: VulkanLayout,
}

/// Resource discriminator stored on every [`SurfaceState`].
///
/// The cuda adapter holds both `Buffer` and `Image` registrations in
/// the same [`streamlib_adapter_abi::Registry`]; the variant identifies
/// which acquire path (`acquire_read`/`acquire_write` for buffers,
/// `acquire_texture`/`acquire_surface` for images) is valid for a
/// given surface_id. Mixing paths (e.g. calling `acquire_read` on an
/// image surface) returns
/// [`streamlib_adapter_abi::AdapterError::BackendRejected`] with a
/// usage-correction hint, the same shape the OpenGL adapter uses for
/// its EXTERNAL_OES read-only restriction.
pub(crate) enum SurfaceResource<P: DevicePrivilege> {
    Buffer {
        pixel_buffer: Arc<P::Buffer>,
    },
    Image {
        texture: Arc<P::Texture>,
    },
}

/// Per-surface state held inside the adapter's `Registry<...>`.
///
/// All mutation goes through the registry's locking so `acquire_*` /
/// `end_*_access` stay sequenced — the trait's `&self` shape is
/// satisfied by interior mutability.
pub(crate) struct SurfaceState<P: DevicePrivilege> {
    #[allow(dead_code)] // kept for tracing / debug output, not read in hot paths
    pub(crate) surface_id: SurfaceId,
    pub(crate) resource: SurfaceResource<P>,
    /// `produce_done` timeline — see
    /// [`HostSurfaceRegistration::produce_done`].
    pub(crate) produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — see
    /// [`HostSurfaceRegistration::consume_done`].
    pub(crate) consume_done: Arc<P::TimelineSemaphore>,
    /// Vulkan image layout the resource is in. Load-bearing for image
    /// surfaces (consumed by the cross-process release shim that
    /// composes `VulkanSurfaceAdapter::release_to_foreign`); ignored on
    /// the buffer path.
    #[allow(dead_code)] // consumed by the cross-process release shim (see HostImageSurfaceRegistration::initial_layout)
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    /// Per-process monotonic signal counter. This adapter instance only
    /// writes ONE side's timeline per the single-writer-per-edge rule
    /// (host instantiation = producer signaling `produce_done`; cdylib
    /// instantiation = consumer signaling `consume_done`). Same-process
    /// test code that exercises both sides increments this counter for
    /// every signal regardless of which timeline — produce_done and
    /// consume_done each see strictly monotonic values from their
    /// respective writer code paths, which is all VUID-03258 requires.
    pub(crate) current_signal_value: u64,
}

impl<P: DevicePrivilege> SurfaceState<P> {
    pub(crate) fn next_signal_value(&self) -> u64 {
        self.current_signal_value + 1
    }
}

impl<P: DevicePrivilege> SurfaceRegistration for SurfaceState<P> {
    fn write_held(&self) -> bool {
        self.write_held
    }
    fn read_holders(&self) -> u64 {
        self.read_holders
    }
    fn set_write_held(&mut self, held: bool) {
        self.write_held = held;
    }
    fn inc_read_holders(&mut self) {
        self.read_holders += 1;
    }
    fn dec_read_holders(&mut self) {
        self.read_holders = self.read_holders.saturating_sub(1);
    }
}
