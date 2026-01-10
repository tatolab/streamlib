// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command buffer implementation for RHI.

use ash::vk;

use super::VulkanTexture;

/// Vulkan command buffer wrapper.
///
/// Command buffers are single-use: create, record, commit.
/// The buffer is automatically begun when created.
pub struct VulkanCommandBuffer {
    device: ash::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
}

impl VulkanCommandBuffer {
    /// Create a new command buffer wrapper.
    pub fn new(
        device: ash::Device,
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
        let src_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(src_image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ);

        // Transition destination to TRANSFER_DST_OPTIMAL
        let dst_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

        let barriers = [src_barrier, dst_barrier];

        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            );
        }

        // Copy the image
        let copy_width = src.width().min(dst.width());
        let copy_height = src.height().min(dst.height());

        let region = vk::ImageCopy::default()
            .src_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .src_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .dst_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .dst_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .extent(vk::Extent3D {
                width: copy_width,
                height: copy_height,
                depth: 1,
            });

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

    /// Commit the command buffer for execution (async).
    pub fn commit(self) {
        // End command buffer recording
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .expect("Failed to end command buffer");
        }

        // Submit to queue
        let command_buffers = [self.command_buffer];
        let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);

        unsafe {
            self.device
                .queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .expect("Failed to submit command buffer");
        }

        // Free the command buffer after submission
        // Note: This is safe because queue_submit copies the commands
        unsafe {
            self.device
                .free_command_buffers(self.command_pool, &command_buffers);
        }
    }

    /// Commit and wait for completion (blocking).
    pub fn commit_and_wait(self) {
        // End command buffer recording
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .expect("Failed to end command buffer");
        }

        // Submit to queue
        let command_buffers = [self.command_buffer];
        let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);

        unsafe {
            self.device
                .queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .expect("Failed to submit command buffer");

            // Wait for queue to become idle
            self.device
                .queue_wait_idle(self.queue)
                .expect("Failed to wait for queue");

            // Free the command buffer
            self.device
                .free_command_buffers(self.command_pool, &command_buffers);
        }
    }
}
