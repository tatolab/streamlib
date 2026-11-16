// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Image and buffer barriers, image layout transitions, and the release of an image to the external queue family.

use super::*;

impl RhiCommandRecorderInner {
    /// Image layout transition. Caller supplies `from_layout` (typically
    /// `registration.current_layout()` from `TextureRegistration`), the
    /// target `to_layout`, and the surrounding stage/access masks.
    /// Records a single `cmd_pipeline_barrier2` with one image memory
    /// barrier; updating any `TextureRegistration` after the barrier is
    /// the caller's responsibility.
    #[tracing::instrument(level = "trace", skip(self, texture), fields(label = %self.label, from = ?from_layout, to = ?to_layout))]
    pub(crate) fn record_image_barrier(
        &mut self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.expect_recording("record_image_barrier")?;

        use crate::host_rhi::HostTextureExt;
        let image = texture.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_image_barrier: texture has no VkImage",
                self.label
            ))
        })?;

        let subresource = vk::ImageSubresourceRange::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1)
            .build();

        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(from_stage.as_vk())
            .src_access_mask(from_access.as_vk())
            .dst_stage_mask(to_stage.as_vk())
            .dst_access_mask(to_access.as_vk())
            .old_layout(from_layout.as_vk())
            .new_layout(to_layout.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(subresource)
            .build();

        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }

    /// Release `texture` to `VK_QUEUE_FAMILY_EXTERNAL` in `layout`, after
    /// every earlier write, so an importer in another process or API
    /// acquires what was written.
    pub(crate) fn record_image_release_to_the_external_queue_family(
        &mut self,
        texture: &Texture,
        layout: VulkanLayout,
    ) -> Result<()> {
        self.expect_recording("record_image_release_to_the_external_queue_family")?;

        use crate::host_rhi::HostTextureExt;
        let image = texture.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_image_release_to_the_external_queue_family: \
                 texture has no VkImage",
                self.label
            ))
        })?;
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(layout.as_vk())
            .new_layout(layout.as_vk())
            .src_queue_family_index(self.vulkan_device.queue_family_index())
            .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .image(image)
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
        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }

    /// Buffer memory barrier covering the whole buffer.
    #[tracing::instrument(level = "trace", skip(self, buffer), fields(label = %self.label))]
    pub(crate) fn record_buffer_barrier(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.expect_recording("record_buffer_barrier")?;

        let barrier = vk::BufferMemoryBarrier2::builder()
            .src_stage_mask(from_stage.as_vk())
            .src_access_mask(from_access.as_vk())
            .dst_stage_mask(to_stage.as_vk())
            .dst_access_mask(to_access.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(buffer.vk_buffer())
            .offset(0)
            .size(buffer.vk_buffer_size())
            .build();

        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .buffer_memory_barriers(&barriers)
            .build();
        unsafe {
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);
        }
        Ok(())
    }
}
