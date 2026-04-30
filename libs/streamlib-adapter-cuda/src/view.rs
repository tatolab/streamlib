// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire
//! scope.
//!
//! For #587's host-flavor scaffold, the views expose the underlying
//! `vk::Buffer` handle and the buffer size — enough metadata for the
//! carve-out test to validate that registration + acquire flow through
//! the trait correctly. Public CUDA-typed accessors (raw `CUdeviceptr`,
//! DLPack capsule construction) ship in #589/#590 once the cdylib +
//! `cudarc` integration land. The view types themselves stay compatible
//! across that change — only their inherent methods grow.

use std::marker::PhantomData;

use vulkanalia::vk;

/// Read view of an acquired surface — exposes the host's `vk::Buffer`
/// handle and the buffer's size in bytes. CUDA-typed accessors arrive
/// in #589/#590.
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
}
