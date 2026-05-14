// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire
//! scope.
//!
//! Two flavors, one per resource kind:
//!
//! - **Buffer flavor** ([`CudaReadView`] / [`CudaWriteView`]) — exposes
//!   `vk::Buffer` + size + a DLPack-spec
//!   [`crate::dlpack::ManagedTensor`] constructor over a caller-supplied
//!   CUDA device pointer. The cdylib subprocess runtimes obtain the
//!   device pointer via `cudaExternalMemoryGetMappedBuffer` against the
//!   imported OPAQUE_FD `VkBuffer` and feed it into the constructor.
//! - **Image flavor** ([`CudaTextureView`] / [`CudaSurfaceView`]) —
//!   exposes `vk::Image` + dimensions + format. The cdylib imports the
//!   image via `cudaImportExternalMemory` +
//!   `cudaExternalMemoryGetMappedMipmappedArray`, then constructs a
//!   `cudaTextureObject_t` (read-only sampling, [`CudaTextureView`]) or
//!   a `cudaSurfaceObject_t` (read-write surface ops, [`CudaSurfaceView`])
//!   at the cdylib's own FFI surface. The Rust view stays
//!   `cudarc`-free so non-CUDA customers don't pay for unused
//!   dependencies.
//!
//! Image-flavored guards ([`CudaTextureGuard`], [`CudaSurfaceGuard`])
//! live alongside the views because the `SurfaceAdapter` trait fixes a
//! single `ReadView` / `WriteView` associated type per adapter (the
//! buffer flavor); the image flavor's acquire methods are
//! adapter-specific and use their own guard types. Drop on both guard
//! types calls into the same `end_read_access` / `end_write_access`
//! machinery the trait guards use — the underlying registry
//! bookkeeping is resource-agnostic.

use std::marker::PhantomData;

use streamlib_adapter_abi::{SurfaceAdapter, SurfaceId};
use streamlib_consumer_rhi::{TextureFormat, VulkanRhiDevice};
use vulkanalia::vk;

use crate::adapter::CudaSurfaceAdapter;
use crate::dlpack::{
    self, CapsuleOwner, Device as DlpackDevice, ManagedTensor as DlpackManagedTensor,
};

/// Read view of an acquired buffer-flavored surface — exposes the
/// host's `vk::Buffer` handle, its size in bytes, and a DLPack capsule
/// constructor over a caller-supplied CUDA device pointer.
pub struct CudaReadView<'g> {
    pub(crate) buffer: vk::Buffer,
    pub(crate) size: vk::DeviceSize,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CudaReadView<'g> {
    /// Underlying Vulkan buffer handle.
    pub fn vk_buffer(&self) -> vk::Buffer {
        self.buffer
    }
    /// Buffer size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }
    /// Wrap `device_ptr` as a DLPack 1-D `u8` [`DlpackManagedTensor`]
    /// of length [`Self::size`]. The cdylib obtains `device_ptr` via
    /// `cudaExternalMemoryGetMappedBuffer` against the OPAQUE_FD
    /// imported memory; `device` selects between `kDLCUDA` and
    /// `kDLCUDAHost` based on
    /// `cudaPointerGetAttributes` (Stage 8 calibration).
    /// `owner` is heap-allocated state the deleter drops once the
    /// consumer releases the capsule — typically an `Arc` clone of
    /// the surface registration the cdylib is keeping alive.
    pub fn dlpack_managed_tensor(
        &self,
        device_ptr: u64,
        device: DlpackDevice,
        owner: CapsuleOwner,
    ) -> *mut DlpackManagedTensor {
        dlpack::build_byte_buffer_managed_tensor(device_ptr, self.size, device, owner)
    }
}

/// Write view of an acquired buffer-flavored surface — symmetric
/// counterpart to [`CudaReadView`].
pub struct CudaWriteView<'g> {
    pub(crate) buffer: vk::Buffer,
    pub(crate) size: vk::DeviceSize,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CudaWriteView<'g> {
    /// Underlying Vulkan buffer handle.
    pub fn vk_buffer(&self) -> vk::Buffer {
        self.buffer
    }
    /// Buffer size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }
    /// See [`CudaReadView::dlpack_managed_tensor`].
    pub fn dlpack_managed_tensor(
        &self,
        device_ptr: u64,
        device: DlpackDevice,
        owner: CapsuleOwner,
    ) -> *mut DlpackManagedTensor {
        dlpack::build_byte_buffer_managed_tensor(device_ptr, self.size, device, owner)
    }
}

/// Read-only view of an acquired image-flavored surface — exposes the
/// underlying `vk::Image` handle and the image's dimensions + format.
///
/// The CUDA cdylib uses these fields to construct a
/// `cudaTextureObject_t` for the surface's pre-imported mipmapped
/// array (one-time `cudaImportExternalMemory` +
/// `cudaExternalMemoryGetMappedMipmappedArray` at registration) and
/// returns the raw `uint64_t` handle to the customer at the cdylib's
/// FFI surface. The construction step itself lives in the cdylib
/// because `cudarc` isn't a dep of this crate.
pub struct CudaTextureView<'g> {
    pub(crate) image: vk::Image,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CudaTextureView<'g> {
    /// Underlying Vulkan image handle.
    pub fn vk_image(&self) -> vk::Image {
        self.image
    }
    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Image format — guaranteed to be in the CUDA-mappable subset
    /// (`Rgba8Unorm` / `Rgba16Float` / `Rgba32Float`) by the adapter's
    /// registration-time check.
    pub fn format(&self) -> TextureFormat {
        self.format
    }
}

/// Read-write view of an acquired image-flavored surface — same
/// Vulkan-side shape as [`CudaTextureView`], but the cdylib constructs
/// a `cudaSurfaceObject_t` (the writeable side of CUDA texture
/// interop) for kernels that need surface-write semantics.
pub struct CudaSurfaceView<'g> {
    pub(crate) image: vk::Image,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CudaSurfaceView<'g> {
    /// Underlying Vulkan image handle.
    pub fn vk_image(&self) -> vk::Image {
        self.image
    }
    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Image format — see [`CudaTextureView::format`].
    pub fn format(&self) -> TextureFormat {
        self.format
    }
}

/// RAII guard for [`CudaSurfaceAdapter::acquire_texture`] /
/// [`CudaSurfaceAdapter::try_acquire_texture`]. Scoped to the
/// image-flavored read path; drop releases the read holder and signals
/// the timeline.
pub struct CudaTextureGuard<'g, D: VulkanRhiDevice + 'static> {
    pub(crate) adapter: &'g CudaSurfaceAdapter<D>,
    pub(crate) surface_id: SurfaceId,
    pub(crate) view: CudaTextureView<'g>,
}

impl<'g, D: VulkanRhiDevice + 'static> CudaTextureGuard<'g, D> {
    pub fn view(&self) -> &CudaTextureView<'g> {
        &self.view
    }
    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }
}

impl<D: VulkanRhiDevice + 'static> Drop for CudaTextureGuard<'_, D> {
    fn drop(&mut self) {
        self.adapter.end_read_access(self.surface_id);
    }
}

impl<D: VulkanRhiDevice + 'static> std::fmt::Debug for CudaTextureGuard<'_, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CudaTextureGuard")
            .field("surface_id", &self.surface_id)
            .finish_non_exhaustive()
    }
}

/// RAII guard for [`CudaSurfaceAdapter::acquire_surface`] /
/// [`CudaSurfaceAdapter::try_acquire_surface`]. Symmetric to
/// [`CudaTextureGuard`] for the writeable image path.
pub struct CudaSurfaceGuard<'g, D: VulkanRhiDevice + 'static> {
    pub(crate) adapter: &'g CudaSurfaceAdapter<D>,
    pub(crate) surface_id: SurfaceId,
    pub(crate) view: CudaSurfaceView<'g>,
}

impl<'g, D: VulkanRhiDevice + 'static> CudaSurfaceGuard<'g, D> {
    pub fn view(&self) -> &CudaSurfaceView<'g> {
        &self.view
    }
    pub fn view_mut(&mut self) -> &mut CudaSurfaceView<'g> {
        &mut self.view
    }
    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }
}

impl<D: VulkanRhiDevice + 'static> Drop for CudaSurfaceGuard<'_, D> {
    fn drop(&mut self) {
        self.adapter.end_write_access(self.surface_id);
    }
}

impl<D: VulkanRhiDevice + 'static> std::fmt::Debug for CudaSurfaceGuard<'_, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CudaSurfaceGuard")
            .field("surface_id", &self.surface_id)
            .finish_non_exhaustive()
    }
}
