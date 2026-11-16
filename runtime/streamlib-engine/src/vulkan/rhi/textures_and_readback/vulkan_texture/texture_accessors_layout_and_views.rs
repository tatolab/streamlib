// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A texture's accessors, its lazily made image view, and its transition to the general layout.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::TextureFormat;
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;

use super::{HostVulkanTexture, texture_format_to_vk};

impl HostVulkanTexture {
    /// Get the underlying Vulkan image handle.
    pub fn image(&self) -> Option<vk::Image> {
        self.image
    }

    /// Texture width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Tiling this image was created with — what `vkGetImageSubresourceLayout`
    /// binds its aspect mask to, and the signal that says whether a
    /// driver-chosen DRM format modifier applies to this image at all.
    pub fn vk_image_tiling(&self) -> vk::ImageTiling {
        self.vk_image_meta.vk_image_tiling
    }

    /// Whether the image was created with TRANSFER_SRC usage — the
    /// capability a copy that reads it (`vkCmdCopyImageToBuffer`)
    /// requires; recording one without it is a Vulkan spec violation
    /// (VUID-vkCmdCopyImageToBuffer-srcImage-01998), not an error the
    /// driver reports.
    pub fn supports_transfer_read(&self) -> bool {
        self.vk_image_meta
            .vk_image_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC)
    }

    /// Whether the image was created with STORAGE usage — the capability a
    /// compute kernel writing it as a storage image requires.
    pub fn supports_storage_write(&self) -> bool {
        self.vk_image_meta
            .vk_image_usage_flags
            .contains(vk::ImageUsageFlags::STORAGE)
    }

    /// Whether the image was created with TRANSFER_DST usage — the
    /// capability a copy that writes it (`vkCmdCopyBufferToImage`)
    /// requires (VUID-vkCmdCopyBufferToImage-dstImage-01997).
    pub fn supports_transfer_write(&self) -> bool {
        self.vk_image_meta
            .vk_image_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_DST)
    }

    /// Lazy-cached image view for this texture.
    ///
    /// Creates the image view on first call, returns the cached handle on
    /// subsequent calls. The view uses the texture's own format and full
    /// subresource range.
    pub fn image_view(&self) -> Result<vk::ImageView> {
        if let Some(&view) = self.cached_image_view.get() {
            return Ok(view);
        }

        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            Error::GpuError("Cannot create image view: no HostVulkanDevice stored".into())
        })?;
        let image = self
            .image
            .ok_or_else(|| Error::GpuError("Cannot create image view: no image".into()))?;

        let vk_format = texture_format_to_vk(self.format);
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(vk_format)
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

        let view = unsafe { vk_dev.device().create_image_view(&view_info, None) }
            .map_err(|e| Error::GpuError(format!("Failed to create image view: {e}")))?;

        Ok(*self.cached_image_view.get_or_init(|| view))
    }

    /// One-shot UNDEFINED → GENERAL barrier on a freshly-allocated
    /// `vk::Image`. Used by [`crate::core::context::GpuContext::transition_storage_image_to_general`]
    /// to give example / processor code a way to bring a storage-image
    /// texture into the layout compute / RT kernels expect — without
    /// pulling vulkanalia into the consumer.
    ///
    /// Synchronous: submits to the graphics queue and waits on a fence
    /// before returning. The image must not have content the caller
    /// cares about; UNDEFINED-source transitions allow the driver to
    /// discard contents.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn transition_to_general(
        vulkan_device: &Arc<HostVulkanDevice>,
        image: vk::Image,
    ) -> Result<()> {
        let device = vulkan_device.device();
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(vulkan_device.queue_family_index())
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: create_command_pool: {e}"))
        })?;
        let cb_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = match unsafe { device.allocate_command_buffers(&cb_info) } {
            Ok(c) => c[0],
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(Error::GpuError(format!(
                    "transition_to_general: allocate_command_buffers: {e}"
                )));
            }
        };
        let begin = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { device.begin_command_buffer(cmd, &begin) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: begin_command_buffer: {e}"))
        })?;
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::SHADER_READ | vk::AccessFlags2::SHADER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build();
        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &dep) };
        unsafe { device.end_command_buffer(cmd) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: end_command_buffer: {e}"))
        })?;

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build();
        let cmd_infos = [cmd_info];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        let fence_info = vk::FenceCreateInfo::default();
        let fence = unsafe { device.create_fence(&fence_info, None) }
            .map_err(|e| Error::GpuError(format!("transition_to_general: create_fence: {e}")))?;
        let submits = [submit];
        let submit_result = unsafe {
            HostVulkanDevice::submit_to_queue(vulkan_device, vulkan_device.queue(), &submits, fence)
        };
        if let Err(e) = submit_result {
            unsafe {
                device.destroy_fence(fence, None);
                device.destroy_command_pool(pool, None);
            }
            return Err(e);
        }
        let wait_result = unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("transition_to_general: wait_for_fences: {e}")));
        unsafe {
            device.destroy_fence(fence, None);
            device.destroy_command_pool(pool, None);
        }
        wait_result
    }
}
