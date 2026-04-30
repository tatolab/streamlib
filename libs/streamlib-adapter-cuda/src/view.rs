// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire
//! scope.
//!
//! Each view exposes the underlying `vk::Buffer` handle, the buffer
//! size, and a [`dlpack_managed_tensor`](CudaReadView::dlpack_managed_tensor)
//! constructor that wraps a caller-supplied CUDA device pointer in a
//! DLPack-spec [`crate::dlpack::ManagedTensor`]. The cdylib subprocess
//! runtimes (`streamlib-python-native` / `streamlib-deno-native` in
//! #589/#590) obtain the device pointer via
//! `cudaExternalMemoryGetMappedBuffer` against the
//! [`vk_buffer`](CudaReadView::vk_buffer) FD and feed it into the
//! constructor; this crate stays free of `cudarc` so non-CUDA customers
//! don't pay for unused dependencies.

use std::marker::PhantomData;

use vulkanalia::vk;

use crate::dlpack::{
    self, CapsuleOwner, Device as DlpackDevice, ManagedTensor as DlpackManagedTensor,
};

/// Read view of an acquired surface — exposes the host's `vk::Buffer`
/// handle, its size in bytes, and a DLPack capsule constructor over a
/// caller-supplied CUDA device pointer.
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

/// Write view of an acquired surface — symmetric counterpart to
/// [`CudaReadView`].
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
