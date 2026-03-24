// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

use crate::core::rhi::blitter::RhiBlitter;
use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

/// Vulkan implementation of [`RhiBlitter`] for GPU copy operations on Linux.
pub struct VulkanBlitter {
    device: ash::Device,
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
    fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        let src_buffer = src.buffer_ref().inner.buffer();
        let dest_buffer = dest.buffer_ref().inner.buffer();
        let src_size = src.buffer_ref().inner.size();
        let dest_size = dest.buffer_ref().inner.size();

        if src_size != dest_size {
            return Err(StreamError::GpuError(format!(
                "blit_copy requires same-size buffers: src={} bytes, dest={} bytes",
                src_size, dest_size
            )));
        }

        let copy_size = src_size;

        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        let command_buffer = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to allocate blit command buffer: {e}")))?[0];

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe {
            self.device
                .begin_command_buffer(command_buffer, &begin_info)
                .map_err(|e| StreamError::GpuError(format!("Failed to begin blit command buffer: {e}")))?;

            let region = vk::BufferCopy::default()
                .src_offset(0)
                .dst_offset(0)
                .size(copy_size);

            self.device
                .cmd_copy_buffer(command_buffer, src_buffer, dest_buffer, &[region]);

            self.device
                .end_command_buffer(command_buffer)
                .map_err(|e| StreamError::GpuError(format!("Failed to end blit command buffer: {e}")))?;

            let submit_info =
                vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&command_buffer));

            let fence_info = vk::FenceCreateInfo::default();
            let fence = self
                .device
                .create_fence(&fence_info, None)
                .map_err(|e| StreamError::GpuError(format!("Failed to create blit fence: {e}")))?;

            self.device
                .queue_submit(self.queue, &[submit_info], fence)
                .map_err(|e| {
                    self.device.destroy_fence(fence, None);
                    StreamError::GpuError(format!("Failed to submit blit command: {e}"))
                })?;

            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    self.device.destroy_fence(fence, None);
                    StreamError::GpuError(format!("Failed to wait for blit fence: {e}"))
                })?;

            self.device.destroy_fence(fence, None);
            self.device
                .free_command_buffers(self.command_pool, &[command_buffer]);
        }

        Ok(())
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
