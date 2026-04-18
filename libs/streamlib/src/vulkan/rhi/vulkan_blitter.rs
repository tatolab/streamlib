// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::blitter::RhiBlitter;
use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

use super::VulkanDevice;

/// Vulkan implementation of [`RhiBlitter`] for GPU copy operations on Linux.
pub struct VulkanBlitter {
    vulkan_device: Arc<VulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    #[allow(dead_code)]
    queue_family_index: u32,
    command_pool: vk::CommandPool,
}

impl VulkanBlitter {
    /// Create a new Vulkan blitter with a dedicated command pool.
    pub fn new(vulkan_device: &Arc<VulkanDevice>, queue: vk::Queue, queue_family_index: u32) -> Result<Self> {
        let device = vulkan_device.device();
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index)
            .build();

        let command_pool =
            unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create blitter command pool: {e}"))
            })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
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

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();

        let command_buffer = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to allocate blit command buffer: {e}")))?[0];

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();

        unsafe {
            self.device
                .begin_command_buffer(command_buffer, &begin_info)
                .map(|_| ())
                .map_err(|e| StreamError::GpuError(format!("Failed to begin blit command buffer: {e}")))?;

            let region = vk::BufferCopy2::builder()
                .src_offset(0)
                .dst_offset(0)
                .size(copy_size)
                .build();

            let copy_info = vk::CopyBufferInfo2::builder()
                .src_buffer(src_buffer)
                .dst_buffer(dest_buffer)
                .regions(&[region])
                .build();
            self.device.cmd_copy_buffer2(command_buffer, &copy_info);

            self.device
                .end_command_buffer(command_buffer)
                .map(|_| ())
                .map_err(|e| StreamError::GpuError(format!("Failed to end blit command buffer: {e}")))?;

            // Timeline semaphore (Vulkan 1.2 core) for targeted blit synchronization.
            // More efficient than fences for GPU-GPU ordering.
            let mut timeline_type_info = vk::SemaphoreTypeCreateInfo::builder()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0)
                .build();
            let timeline_semaphore_info = vk::SemaphoreCreateInfo::builder()
                .push_next(&mut timeline_type_info)
                .build();
            let timeline_semaphore = self
                .device
                .create_semaphore(&timeline_semaphore_info, None)
                .map_err(|e| StreamError::GpuError(format!("Failed to create blit timeline semaphore: {e}")))?;

            let signal_semaphore = vk::SemaphoreSubmitInfo::builder()
                .semaphore(timeline_semaphore)
                .value(1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build();
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(command_buffer)
                .build();
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&[cmd_info])
                .signal_semaphore_infos(&[signal_semaphore])
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], vk::Fence::null())
                .map_err(|e| {
                    self.device.destroy_semaphore(timeline_semaphore, None);
                    e
                })?;

            let wait_semaphores = [timeline_semaphore];
            let wait_values = [1u64];
            let wait_info = vk::SemaphoreWaitInfo::builder()
                .semaphores(&wait_semaphores)
                .values(&wait_values)
                .build();
            self.device
                .wait_semaphores(&wait_info, u64::MAX)
                .map_err(|e| {
                    self.device.destroy_semaphore(timeline_semaphore, None);
                    StreamError::GpuError(format!("Failed to wait for blit timeline semaphore: {e}"))
                })
                .map(|_| ())?;

            self.device.destroy_semaphore(timeline_semaphore, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
    use crate::vulkan::rhi::{VulkanDevice, VulkanPixelBuffer};
    use std::sync::Arc;

    fn make_rhi_buffer(
        device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
    ) -> RhiPixelBuffer {
        let buf = VulkanPixelBuffer::new(device, width, height, 4, PixelFormat::Bgra32)
            .expect("pixel buffer allocation failed");
        RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::new(buf),
        })
    }

    #[test]
    fn test_blit_copy_between_equal_size_buffers() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let blitter = VulkanBlitter::new(
            &device,
            device.queue(),
            device.queue_family_index(),
        )
        .expect("blitter creation failed");

        let src = make_rhi_buffer(&device, 64, 64);
        let dst = make_rhi_buffer(&device, 64, 64);

        // Write a known pattern into src
        let pattern: u8 = 0xAB;
        let size = src.buffer_ref().inner.size() as usize;
        unsafe {
            std::ptr::write_bytes(src.buffer_ref().inner.mapped_ptr(), pattern, size);
        }

        blitter.blit_copy(&src, &dst).expect("blit_copy failed");

        // Verify dst received the pattern
        let dst_slice =
            unsafe { std::slice::from_raw_parts(dst.buffer_ref().inner.mapped_ptr(), size) };
        assert!(
            dst_slice.iter().all(|&b| b == pattern),
            "blit_copy must transfer all bytes from src to dst"
        );
    }

    #[test]
    fn test_blit_copy_rejects_mismatched_buffer_sizes() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let blitter = VulkanBlitter::new(
            &device,
            device.queue(),
            device.queue_family_index(),
        )
        .expect("blitter creation failed");

        let src = make_rhi_buffer(&device, 64, 64);
        let dst = make_rhi_buffer(&device, 128, 128); // different size

        let result = blitter.blit_copy(&src, &dst);
        assert!(
            result.is_err(),
            "blit_copy must return Err for mismatched buffer sizes"
        );
    }
}
