// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command queue wrapper for RHI.

use ash::vk;

use crate::core::{Result, StreamError};

use super::VulkanCommandBuffer;

/// Vulkan command queue wrapper.
///
/// Manages the Vulkan queue and command pool for allocating command buffers.
pub struct VulkanCommandQueue {
    device: ash::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
}

impl VulkanCommandQueue {
    /// Create a new command queue wrapper.
    pub fn new(device: ash::Device, queue: vk::Queue, queue_family_index: u32) -> Self {
        // Create command pool for this queue family
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .expect("Failed to create command pool");

        Self {
            device,
            queue,
            command_pool,
        }
    }

    /// Create a new command buffer from this queue.
    pub fn create_command_buffer(&self) -> Result<VulkanCommandBuffer> {
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        let command_buffers = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to allocate command buffer: {e}"))
            })?;

        let command_buffer = command_buffers[0];

        // Begin the command buffer immediately (single-use pattern)
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe {
            self.device
                .begin_command_buffer(command_buffer, &begin_info)
        }
        .map_err(|e| StreamError::GpuError(format!("Failed to begin command buffer: {e}")))?;

        Ok(VulkanCommandBuffer::new(
            self.device.clone(),
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
