// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

/// Vulkan format converter for pixel buffer format conversion.
pub struct VulkanFormatConverter {
    device: ash::Device,
    #[allow(dead_code)]
    queue: vk::Queue,
    #[allow(dead_code)]
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    source_bytes_per_pixel: u32,
    dest_bytes_per_pixel: u32,
}

impl VulkanFormatConverter {
    /// Create a new format converter with a dedicated command pool.
    pub fn new(
        device: &ash::Device,
        queue: vk::Queue,
        queue_family_index: u32,
        source_bytes_per_pixel: u32,
        dest_bytes_per_pixel: u32,
    ) -> Result<Self> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))?;

        Ok(Self {
            device: device.clone(),
            queue,
            queue_family_index,
            command_pool,
            source_bytes_per_pixel,
            dest_bytes_per_pixel,
        })
    }

    /// Convert pixel data from source buffer to destination buffer.
    pub fn convert(&self, _source: &RhiPixelBuffer, _dest: &RhiPixelBuffer) -> Result<()> {
        Err(StreamError::NotSupported(
            "Vulkan format conversion not yet implemented".into(),
        ))
    }

    /// Source format bytes per pixel.
    pub fn source_bytes_per_pixel(&self) -> u32 {
        self.source_bytes_per_pixel
    }

    /// Destination format bytes per pixel.
    pub fn dest_bytes_per_pixel(&self) -> u32 {
        self.dest_bytes_per_pixel
    }
}

impl Drop for VulkanFormatConverter {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// Safety: Vulkan handles are thread-safe
unsafe impl Send for VulkanFormatConverter {}
unsafe impl Sync for VulkanFormatConverter {}
