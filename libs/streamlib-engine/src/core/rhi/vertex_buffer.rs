// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vertex buffer for graphics pipeline vertex input.

use std::sync::Arc;

/// Vertex buffer for graphics pipeline vertex input.
///
/// Linux-only. Graphics kernels bind it via `set_vertex_buffer`,
/// which accepts `&impl VulkanVertexBindable`.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct VertexBuffer {
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
}

#[cfg(target_os = "linux")]
impl VertexBuffer {
    /// Allocate a HOST_VISIBLE vertex buffer of the given byte size.
    /// Underlying `VkBuffer` carries `VERTEX_BUFFER | TRANSFER_SRC |
    /// TRANSFER_DST` usage.
    pub fn new_host_visible(
        device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
        byte_size: u64,
    ) -> crate::core::Result<Self> {
        let inner =
            crate::vulkan::rhi::HostVulkanBuffer::new_vertex_buffer_host_visible(
                device, byte_size,
            )?;
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Wrap a pre-allocated buffer that already has `VERTEX_BUFFER`
    /// usage. Callers are responsible for confirming the usage flag at
    /// allocation time; mismatched usage fails at descriptor write.
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
impl std::fmt::Debug for VertexBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexBuffer")
            .field("byte_size", &self.byte_size())
            .finish()
    }
}
