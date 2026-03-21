// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

use crate::core::rhi::blitter::RhiBlitter;
use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

/// Vulkan implementation of [`RhiBlitter`] for GPU copy operations on Linux.
pub struct VulkanBlitter {
    device: ash::Device,
    #[allow(dead_code)]
    queue: vk::Queue,
    #[allow(dead_code)]
    queue_family_index: u32,
    command_pool: vk::CommandPool,
}

impl VulkanBlitter {
    /// Create a new Vulkan blitter with a dedicated command pool.
    pub fn new(device: &ash::Device, queue: vk::Queue, queue_family_index: u32) -> Result<Self> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index);

        let command_pool =
            unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create blitter command pool: {e}"))
            })?;

        Ok(Self {
            device: device.clone(),
            queue,
            queue_family_index,
            command_pool,
        })
    }
}

impl RhiBlitter for VulkanBlitter {
    fn blit_copy(&self, _src: &RhiPixelBuffer, _dest: &RhiPixelBuffer) -> Result<()> {
        Err(StreamError::NotSupported(
            "Vulkan blitter blit_copy not yet implemented".into(),
        ))
    }

    unsafe fn blit_copy_iosurface_raw(
        &self,
        _src: *const std::ffi::c_void,
        _dest: &RhiPixelBuffer,
        _width: u32,
        _height: u32,
    ) -> Result<()> {
        Err(StreamError::NotSupported(
            "IOSurface not available on Linux".into(),
        ))
    }

    fn clear_cache(&self) {}
}

impl Drop for VulkanBlitter {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

unsafe impl Send for VulkanBlitter {}
unsafe impl Sync for VulkanBlitter {}
