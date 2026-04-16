// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command buffer implementation for RHI.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::VulkanTexture;

/// Vulkan command buffer wrapper.
///
/// Command buffers are single-use: create, record, commit.
/// The buffer is automatically begun when created.
pub struct VulkanCommandBuffer {
    device: vulkanalia::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
}

impl VulkanCommandBuffer {
    /// Create a new command buffer wrapper.
    pub fn new(
        device: vulkanalia::Device,
        queue: vk::Queue,
        command_pool: vk::CommandPool,
        command_buffer: vk::CommandBuffer,
    ) -> Self {
        Self {
            device,
            queue,
            command_pool,
            command_buffer,
        }
    }

    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &VulkanTexture, dst: &VulkanTexture) {
        // Skip if either texture doesn't have a valid image
        let (Some(src_image), Some(dst_image)) = (src.image(), dst.image()) else {
            return;
        };

        // Transition source to TRANSFER_SRC_OPTIMAL
        let src_barrier = vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(src_image)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .build();

        // Transition destination to TRANSFER_DST_OPTIMAL
        let dst_barrier = vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .build();

        let barriers = [src_barrier, dst_barrier];

        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[] as &[vk::MemoryBarrier],
                &[] as &[vk::BufferMemoryBarrier],
                &barriers,
            );
        }

        // Copy the image
        let copy_width = src.width().min(dst.width());
        let copy_height = src.height().min(dst.height());

        let region = vk::ImageCopy::builder()
            .src_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .src_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .dst_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .dst_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .extent(vk::Extent3D {
                width: copy_width,
                height: copy_height,
                depth: 1,
            })
            .build();

        unsafe {
            self.device.cmd_copy_image(
                self.command_buffer,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        }
    }

    /// Commit the command buffer for execution.
    ///
    /// Uses a timeline semaphore (Vulkan 1.2 core) to ensure GPU completion
    /// before freeing the command buffer. More efficient than fences for
    /// GPU-GPU ordering — avoids kernel roundtrip.
    pub fn commit(self) {
        // End command buffer recording
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .expect("Failed to end command buffer");
        }

        // Submit to queue with a timeline semaphore to track GPU completion
        let command_buffers = [self.command_buffer];

        unsafe {
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
                .expect("Failed to create command buffer timeline semaphore");

            let signal_semaphores = [timeline_semaphore];
            let signal_values = [1u64];
            let mut timeline_submit_info = vk::TimelineSemaphoreSubmitInfo::builder()
                .signal_semaphore_values(&signal_values)
                .build();

            let submit_info = vk::SubmitInfo::builder()
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal_semaphores)
                .push_next(&mut timeline_submit_info)
                .build();

            self.device
                .queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .expect("Failed to submit command buffer");

            // Wait for GPU completion via timeline semaphore before freeing
            let wait_semaphores = [timeline_semaphore];
            let wait_values = [1u64];
            let wait_info = vk::SemaphoreWaitInfo::builder()
                .semaphores(&wait_semaphores)
                .values(&wait_values)
                .build();
            self.device
                .wait_semaphores(&wait_info, u64::MAX)
                .expect("Failed to wait for command buffer timeline semaphore");

            self.device.destroy_semaphore(timeline_semaphore, None);

            // Now safe to free — GPU has finished with the command buffer
            self.device
                .free_command_buffers(self.command_pool, &command_buffers);
        }
    }

    /// Commit and wait for completion (blocking).
    ///
    /// Uses a timeline semaphore (Vulkan 1.2 core) for targeted synchronization
    /// on this specific command buffer, not the entire queue.
    pub fn commit_and_wait(self) {
        // End command buffer recording
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .expect("Failed to end command buffer");
        }

        // Submit to queue with a timeline semaphore for targeted synchronization
        let command_buffers = [self.command_buffer];

        unsafe {
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
                .expect("Failed to create command buffer timeline semaphore");

            let signal_semaphores = [timeline_semaphore];
            let signal_values = [1u64];
            let mut timeline_submit_info = vk::TimelineSemaphoreSubmitInfo::builder()
                .signal_semaphore_values(&signal_values)
                .build();

            let submit_info = vk::SubmitInfo::builder()
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal_semaphores)
                .push_next(&mut timeline_submit_info)
                .build();

            self.device
                .queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .expect("Failed to submit command buffer");

            // Wait for this specific command buffer via timeline semaphore
            let wait_semaphores = [timeline_semaphore];
            let wait_values = [1u64];
            let wait_info = vk::SemaphoreWaitInfo::builder()
                .semaphores(&wait_semaphores)
                .values(&wait_values)
                .build();
            self.device
                .wait_semaphores(&wait_info, u64::MAX)
                .expect("Failed to wait for command buffer timeline semaphore");

            self.device.destroy_semaphore(timeline_semaphore, None);

            // Free the command buffer
            self.device
                .free_command_buffers(self.command_pool, &command_buffers);
        }
    }
}
