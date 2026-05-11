// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Raw byte-shaped GPU storage buffer (SSBO).
//!
//! Sibling of [`PixelBuffer`](super::PixelBuffer) for callers that
//! have raw bytes rather than formatted pixel data — V4L2-shape capture
//! frames pre-conversion, audio→GPU compute inputs, ML tensor uploads.
//! Exposes byte size and a mapped pointer only; no pixel-shaped
//! getters that would be meaningless on an SSBO.

use std::sync::Arc;

/// Raw byte-shaped GPU storage buffer (SSBO).
///
/// Linux-only — SSBO allocation rides the Vulkan RHI path. Compute
/// kernels bind it via
/// [`crate::vulkan::rhi::VulkanComputeKernel::set_storage_buffer`].
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct StorageBuffer {
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
}

#[cfg(target_os = "linux")]
impl StorageBuffer {
    /// Wrap an externally-allocated `Arc<HostVulkanBuffer>` as a
    /// `StorageBuffer`. The inner buffer must have been allocated via
    /// one of the SSBO constructors
    /// ([`crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible`]
    /// or
    /// [`crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer`]).
    pub fn from_host_vulkan_buffer(
        inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    ) -> Self {
        Self { inner }
    }

    /// Total buffer size in bytes.
    pub fn byte_size(&self) -> u64 {
        self.inner.size() as u64
    }

    /// Persistently mapped CPU pointer for HOST_VISIBLE allocations.
    /// Returns null for DEVICE_LOCAL imports (DMA-BUF without
    /// HOST_VISIBLE).
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.inner.mapped_ptr()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for StorageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageBuffer")
            .field("byte_size", &self.byte_size())
            .finish()
    }
}
