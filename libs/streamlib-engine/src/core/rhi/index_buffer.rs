// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Index buffer for graphics pipeline indexed draws.

use std::sync::Arc;

/// Index buffer for graphics pipeline indexed draws.
///
/// Linux-only. Graphics kernels bind it via `set_index_buffer`,
/// which accepts `&impl VulkanIndexBindable`. The caller separately
/// specifies the index element type (u16 / u32) at the binding
/// callsite via `IndexType`.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct IndexBuffer {
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
}

#[cfg(target_os = "linux")]
impl IndexBuffer {
    /// Allocate a HOST_VISIBLE index buffer of the given byte size.
    /// Underlying `VkBuffer` carries `INDEX_BUFFER | TRANSFER_SRC |
    /// TRANSFER_DST` usage.
    pub fn new_host_visible(
        device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
        byte_size: u64,
    ) -> crate::core::Result<Self> {
        let inner =
            crate::vulkan::rhi::HostVulkanBuffer::new_index_buffer_host_visible(
                device, byte_size,
            )?;
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Wrap a pre-allocated buffer that already has `INDEX_BUFFER` usage.
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
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.inner.mapped_ptr()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for IndexBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBuffer")
            .field("byte_size", &self.byte_size())
            .finish()
    }
}
