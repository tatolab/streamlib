// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal trait that lets command-buffer recording methods on
//! [`RhiCommandRecorder`](super::RhiCommandRecorder) accept any of
//! streamlib's typed buffer wrappers ([`PixelBuffer`],
//! [`StorageBuffer`], [`UniformBuffer`], [`VertexBuffer`],
//! [`IndexBuffer`]) or a raw [`HostVulkanBuffer`] uniformly. The raw
//! variant is needed because some allocation flavors — notably
//! OPAQUE_FD-exportable buffers used in CUDA / OpenCL interop — have
//! no typed wrapper above [`HostVulkanBuffer`] but still participate
//! in transfer + barrier recording.
//!
//! Distinct from the binding-site traits in
//! [`vulkan_storage_binding`](super::vulkan_storage_binding) (which gate
//! kernel-side `set_*_buffer` slot type-safety). [`VulkanBufferLike`]
//! only exposes the underlying `(vk::Buffer, vk::DeviceSize)` for
//! transfer + barrier recording — slot semantics don't apply.
//!
//! [`PixelBuffer`]: crate::core::rhi::PixelBuffer
//! [`StorageBuffer`]: crate::core::rhi::StorageBuffer
//! [`UniformBuffer`]: crate::core::rhi::UniformBuffer
//! [`VertexBuffer`]: crate::core::rhi::VertexBuffer
//! [`IndexBuffer`]: crate::core::rhi::IndexBuffer
//! [`HostVulkanBuffer`]: super::HostVulkanBuffer

use vulkanalia::vk;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::{IndexBuffer, StorageBuffer, UniformBuffer, VertexBuffer};

/// Any of streamlib's typed buffer wrappers, projected onto the raw
/// `(vk::Buffer, vk::DeviceSize)` pair the recorder needs.
pub trait VulkanBufferLike {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

impl VulkanBufferLike for PixelBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        #[cfg(target_os = "linux")]
        {
            self.buffer_ref().inner.buffer()
        }
        #[cfg(not(target_os = "linux"))]
        {
            vk::Buffer::null()
        }
    }

    fn vk_buffer_size(&self) -> vk::DeviceSize {
        #[cfg(target_os = "linux")]
        {
            self.buffer_ref().inner.size()
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for StorageBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.inner.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.inner.size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for UniformBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.inner.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.inner.size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for VertexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.inner.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.inner.size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for IndexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.inner.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.inner.size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for crate::vulkan::rhi::HostVulkanBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.size()
    }
}
