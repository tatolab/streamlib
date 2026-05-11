// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Common binding shape for storage / uniform-buffer slots.

use vulkanalia::vk;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::StorageBuffer;

/// Buffer types that can be bound to a Vulkan compute / graphics /
/// ray-tracing kernel's `set_storage_buffer` or `set_uniform_buffer`
/// slot.
///
/// Both [`PixelBuffer`] and [`StorageBuffer`] implement this — the
/// kernel reads `vk::Buffer` + `vk::DeviceSize` directly without
/// caring about the wrapper's pixel-shaped metadata.
pub trait VulkanStorageBufferBinding {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

impl VulkanStorageBufferBinding for PixelBuffer {
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
impl VulkanStorageBufferBinding for StorageBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.inner.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.inner.size()
    }
}
