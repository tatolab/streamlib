// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan command buffer implementation for RHI.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::{HostVulkanDevice, HostVulkanTexture};

/// Vulkan command buffer wrapper.
///
/// Command buffers are single-use: create, record, commit.
/// The buffer is automatically begun when created.
pub struct VulkanCommandBuffer {
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
}

impl VulkanCommandBuffer {
    /// Create a new command buffer wrapper.
    pub fn new(
        vulkan_device: Arc<HostVulkanDevice>,
        queue: vk::Queue,
        command_pool: vk::CommandPool,
        command_buffer: vk::CommandBuffer,
    ) -> Self {
        let device = vulkan_device.device().clone();
        Self {
            vulkan_device,
            device,
            queue,
            command_pool,
            command_buffer,
        }
    }

    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &HostVulkanTexture, dst: &HostVulkanTexture) {
        // Skip if either texture doesn't have a valid image
        let (Some(src_image), Some(dst_image)) = (src.image(), dst.image()) else {
            return;
        };

        // Transition source to TRANSFER_SRC_OPTIMAL
        let src_barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
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
            .build();

        // Transition destination to TRANSFER_DST_OPTIMAL
        let dst_barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
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
            .build();

        let barriers = [src_barrier, dst_barrier];

        unsafe {
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&barriers)
                .build();
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }

        // Copy the image
        let copy_width = src.width().min(dst.width());
        let copy_height = src.height().min(dst.height());

        let region = vk::ImageCopy2::builder()
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
            let copy_info = vk::CopyImageInfo2::builder()
                .src_image(src_image)
                .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .dst_image(dst_image)
                .dst_image_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .regions(&[region])
                .build();
            self.device.cmd_copy_image2(self.command_buffer, &copy_info);
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

            let signal_semaphore = vk::SemaphoreSubmitInfo::builder()
                .semaphore(timeline_semaphore)
                .value(1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build();
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let signal_semaphore_infos = [signal_semaphore];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .signal_semaphore_infos(&signal_semaphore_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], vk::Fence::null())
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
                .free_command_buffers(self.command_pool, &[self.command_buffer]);
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

            let signal_semaphore = vk::SemaphoreSubmitInfo::builder()
                .semaphore(timeline_semaphore)
                .value(1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build();
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let signal_semaphore_infos = [signal_semaphore];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .signal_semaphore_infos(&signal_semaphore_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], vk::Fence::null())
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
                .free_command_buffers(self.command_pool, &[self.command_buffer]);
        }
    }
}
