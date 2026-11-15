// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Copies between images and buffers, and the refusal of an image-to-image copy that cannot be recorded.

use super::*;

impl RhiCommandRecorderInner {
    /// Record `vkCmdCopyImageToBuffer`. The source texture must already
    /// be in `src_layout` (typically `TRANSFER_SRC_OPTIMAL`); transition
    /// it there via [`Self::record_image_barrier`] first.
    #[tracing::instrument(level = "trace", skip(self, src, dst, region), fields(label = %self.label))]
    pub fn record_copy_image_to_buffer(
        &mut self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.expect_recording("record_copy_image_to_buffer")?;

        use crate::host_rhi::HostTextureExt;
        let image = src.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_image_to_buffer: source texture has no VkImage",
                self.label
            ))
        })?;

        let region_vk = region.to_vk();
        unsafe {
            self.device.cmd_copy_image_to_buffer(
                self.command_buffer,
                image,
                src_layout.as_vk(),
                dst.vk_buffer(),
                &[region_vk],
            );
        }
        Ok(())
    }

    /// Record `vkCmdCopyBufferToImage`. The destination texture must
    /// already be in `dst_layout` (typically `TRANSFER_DST_OPTIMAL`);
    /// transition it there via [`Self::record_image_barrier`] first.
    #[tracing::instrument(level = "trace", skip(self, src, dst, region), fields(label = %self.label))]
    pub fn record_copy_buffer_to_image(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &Texture,
        dst_layout: VulkanLayout,
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.expect_recording("record_copy_buffer_to_image")?;

        use crate::host_rhi::HostTextureExt;
        let image = dst.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_buffer_to_image: dest texture has no VkImage",
                self.label
            ))
        })?;

        let region_vk = region.to_vk();
        unsafe {
            self.device.cmd_copy_buffer_to_image(
                self.command_buffer,
                src.vk_buffer(),
                image,
                dst_layout.as_vk(),
                &[region_vk],
            );
        }
        Ok(())
    }

    /// Record `vkCmdCopyBuffer` over the first `byte_size` bytes of both
    /// buffers.
    pub fn record_copy_buffer_to_buffer(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &(impl VulkanBufferLike + ?Sized),
        byte_size: u64,
    ) -> Result<()> {
        self.expect_recording("record_copy_buffer_to_buffer")?;
        if src.vk_buffer() == dst.vk_buffer() {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_buffer_to_buffer: source and destination \
                 are one buffer, and a whole-range copy onto itself overlaps",
                self.label
            )));
        }
        if byte_size > src.vk_buffer_size() || byte_size > dst.vk_buffer_size() {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_buffer_to_buffer: copy of {} bytes exceeds \
                 src ({}) or dst ({})",
                self.label,
                byte_size,
                src.vk_buffer_size(),
                dst.vk_buffer_size(),
            )));
        }
        let region = vk::BufferCopy::builder()
            .src_offset(0)
            .dst_offset(0)
            .size(byte_size as vk::DeviceSize)
            .build();
        unsafe {
            self.device.cmd_copy_buffer(
                self.command_buffer,
                src.vk_buffer(),
                dst.vk_buffer(),
                &[region],
            );
        }
        Ok(())
    }

    /// Refuse an image→image copy that cannot be recorded legally, naming
    /// why: a missing image, one image on both sides, a format or extent
    /// mismatch, a source holding nothing, or a usage forbidding the copy.
    pub(crate) fn refuse_an_image_to_image_copy_it_cannot_record(
        &self,
        source: &Texture,
        source_known_layout: VulkanLayout,
        destination: &Texture,
    ) -> Result<()> {
        use crate::host_rhi::HostTextureExt;
        let refusal = |reason: String| {
            Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_image_to_image: {reason}",
                self.label
            ))
        };
        let source_image = source
            .vulkan_inner()
            .image()
            .ok_or_else(|| refusal("source texture has no VkImage".into()))?;
        let destination_image = destination
            .vulkan_inner()
            .image()
            .ok_or_else(|| refusal("destination texture has no VkImage".into()))?;
        if source_image == destination_image {
            return Err(refusal(
                "source and destination are one image, which cannot be in TRANSFER_SRC and \
                 TRANSFER_DST at once"
                    .into(),
            ));
        }
        if source.format() != destination.format() {
            return Err(refusal(format!(
                "source is {:?} and destination is {:?}; an image copy converts nothing",
                source.format(),
                destination.format()
            )));
        }
        if (source.width(), source.height()) != (destination.width(), destination.height()) {
            return Err(refusal(format!(
                "source is {}x{} and destination is {}x{}; a whole-image copy needs one extent",
                source.width(),
                source.height(),
                destination.width(),
                destination.height()
            )));
        }
        if source_known_layout == VulkanLayout::UNDEFINED {
            return Err(refusal(
                "the source's known layout is UNDEFINED, so it holds no contents to copy".into(),
            ));
        }
        if !source.supports_transfer_read() {
            return Err(refusal(
                "the source texture was allocated without \"copy_src\" usage".into(),
            ));
        }
        if !destination.supports_transfer_write() {
            return Err(refusal(
                "the destination texture was allocated without \"copy_dst\" usage".into(),
            ));
        }
        Ok(())
    }

    /// Record a whole-image `vkCmdCopyImage` from `source`, already in
    /// `TRANSFER_SRC_OPTIMAL`, into `destination`, already in
    /// `TRANSFER_DST_OPTIMAL`; the caller has run
    /// [`Self::refuse_an_image_to_image_copy_it_cannot_record`].
    #[tracing::instrument(level = "trace", skip(self, source, destination), fields(label = %self.label))]
    pub fn record_copy_image_to_image_in_transfer_layouts(
        &mut self,
        source: &Texture,
        destination: &Texture,
    ) -> Result<()> {
        self.expect_recording("record_copy_image_to_image_in_transfer_layouts")?;

        use crate::host_rhi::HostTextureExt;
        let (Some(source_image), Some(destination_image)) = (
            source.vulkan_inner().image(),
            destination.vulkan_inner().image(),
        ) else {
            return Err(Error::GpuError(format!(
                "RhiCommandRecorder '{}': record_copy_image_to_image_in_transfer_layouts: a \
                 texture has no VkImage",
                self.label
            )));
        };
        let whole_color_layer = vk::ImageSubresourceLayers::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .mip_level(0)
            .base_array_layer(0)
            .layer_count(1)
            .build();
        let region = vk::ImageCopy::builder()
            .src_subresource(whole_color_layer)
            .src_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .dst_subresource(whole_color_layer)
            .dst_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .extent(vk::Extent3D {
                width: source.width(),
                height: source.height(),
                depth: 1,
            })
            .build();
        unsafe {
            self.device.cmd_copy_image(
                self.command_buffer,
                source_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                destination_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        }
        Ok(())
    }
}
