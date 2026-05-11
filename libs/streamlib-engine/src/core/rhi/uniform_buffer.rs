// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Uniform buffer (UBO).

use std::sync::Arc;

/// Uniform buffer for per-draw / per-dispatch shader parameters.
///
/// Linux-only — UBO allocation rides the Vulkan RHI path. Kernels
/// bind it via the kernel's `set_uniform_buffer` method, which
/// accepts `&impl VulkanUniformBindable`.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct UniformBuffer {
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
}

#[cfg(target_os = "linux")]
impl UniformBuffer {
    /// Allocate a HOST_VISIBLE uniform buffer of the given byte size.
    /// Underlying `VkBuffer` carries `UNIFORM_BUFFER | TRANSFER_SRC |
    /// TRANSFER_DST` usage.
    pub fn new_host_visible(
        device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
        byte_size: u64,
    ) -> crate::core::Result<Self> {
        let inner =
            crate::vulkan::rhi::HostVulkanBuffer::new_uniform_buffer_host_visible(
                device, byte_size,
            )?;
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Wrap a pre-allocated buffer that already has `UNIFORM_BUFFER`
    /// usage. Callers are responsible for confirming the usage flag at
    /// allocation time; mismatched usage will fail at descriptor write.
    pub fn from_host_vulkan_buffer(
        inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    ) -> Self {
        Self { inner }
    }

    /// Total buffer size in bytes.
    pub fn byte_size(&self) -> u64 {
        self.inner.size() as u64
    }

    /// Persistently mapped CPU pointer.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.inner.mapped_ptr()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for UniformBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UniformBuffer")
            .field("byte_size", &self.byte_size())
            .finish()
    }
}
