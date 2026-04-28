// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command queue wrapper for RHI.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Result, StreamError};

use super::{VulkanCommandBuffer, HostVulkanDevice};

/// Vulkan command queue wrapper.
///
/// Manages the Vulkan queue and command pool for allocating command buffers.
pub struct VulkanCommandQueue {
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
}

impl VulkanCommandQueue {
    /// Create a new command queue wrapper.
    pub fn new(vulkan_device: Arc<HostVulkanDevice>, queue: vk::Queue, queue_family_index: u32) -> Self {
        let device = vulkan_device.device().clone();

        // Create command pool for this queue family
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .build();

        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .expect("Failed to create command pool");

        Self {
            vulkan_device,
            device,
            queue,
            command_pool,
        }
    }

    /// Create a new command buffer from this queue.
    pub fn create_command_buffer(&self) -> Result<VulkanCommandBuffer> {
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();

        let command_buffers = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to allocate command buffer: {e}"))
            })?;

        let command_buffer = command_buffers[0];

        // Begin the command buffer immediately (single-use pattern)
        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();

        unsafe {
            self.device
                .begin_command_buffer(command_buffer, &begin_info)
        }
        .map_err(|e| StreamError::GpuError(format!("Failed to begin command buffer: {e}")))?;

        Ok(VulkanCommandBuffer::new(
            Arc::clone(&self.vulkan_device),
            self.queue,
            self.command_pool,
            command_buffer,
        ))
    }

    /// Get the underlying Vulkan queue.
    #[allow(dead_code)]
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }
}

impl Drop for VulkanCommandQueue {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// VulkanCommandQueue is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanCommandQueue {}
unsafe impl Sync for VulkanCommandQueue {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[test]
    fn test_creates_command_buffer() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let queue = device.create_command_queue_wrapper();
        let result = queue.create_command_buffer();
        assert!(result.is_ok(), "command buffer allocation must succeed: {:?}", result.err());
    }

    #[test]
    fn test_empty_command_buffer_commit_and_wait_completes() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let queue = device.create_command_queue_wrapper();
        let cmd = queue
            .create_command_buffer()
            .expect("command buffer allocation failed");

        // commit_and_wait on an empty command buffer must complete without panic.
        // This validates the vulkanalia timeline semaphore submit/wait path
        // introduced during the ash → vulkanalia migration.
        cmd.commit_and_wait();
    }
}
