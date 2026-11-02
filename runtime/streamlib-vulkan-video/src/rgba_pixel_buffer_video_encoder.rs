// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Encode RGBA pixel buffers: each frame's buffer is copied into an
//! `R8G8B8A8_UNORM` image the encoder samples, the same canvas the engine
//! resolves a buffer-backed surface into.

use std::sync::Arc;

use streamlib_consumer_rhi::{ConsumerVulkanBuffer, SampledImageReadLayout};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc as _};

use crate::{EncodePacket, SimpleEncoder, VideoError, VideoResult, VulkanVideoCodecDevice};

/// One RGBA canvas sized to the frames it receives, and the command buffer
/// that fills it from a pixel buffer.
struct RgbaCanvasFilledFromPixelBuffers {
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    width: u32,
    height: u32,
    image: vk::Image,
    allocation: vma::Allocation,
    image_view: vk::ImageView,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl RgbaCanvasFilledFromPixelBuffers {
    fn new(
        codec_device: &Arc<dyn VulkanVideoCodecDevice>,
        width: u32,
        height: u32,
    ) -> VideoResult<Self> {
        let device = codec_device.device();
        let queue_family_index = codec_device.compute_queue().queue_family_index;
        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (image, allocation) = {
            let _device_lock = codec_device.lock_device();
            unsafe {
                codec_device
                    .allocator()
                    .create_image(image_info, &alloc_options)
            }
            .map_err(|e| VideoError::Rhi(format!("RGBA canvas: vmaCreateImage failed: {e}")))?
        };
        let mut canvas = Self {
            codec_device: Arc::clone(codec_device),
            width,
            height,
            image,
            allocation,
            image_view: vk::ImageView::null(),
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
        };
        unsafe {
            canvas.image_view = device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .subresource_range(SINGLE_COLOR_SUBRESOURCE_RANGE),
                None,
            )?;
            canvas.command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            canvas.command_buffer = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(canvas.command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            canvas.fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
        }
        Ok(canvas)
    }

    /// Copy `pixel_buffer` into the canvas and leave the canvas in
    /// `SHADER_READ_ONLY_OPTIMAL`, waiting for the copy to land.
    fn fill_from(&self, pixel_buffer: vk::Buffer, bytes_per_row: u64) -> VideoResult<()> {
        let device = self.codec_device.device();
        let command_buffer = self.command_buffer;
        let into_transfer_destination = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_READ)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(SINGLE_COLOR_SUBRESOURCE_RANGE)
            .build();
        let into_sampled_read = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(SINGLE_COLOR_SUBRESOURCE_RANGE)
            .build();
        let copy_region = vk::BufferImageCopy::builder()
            .buffer_offset(0)
            .buffer_row_length((bytes_per_row / RGBA8_BYTES_PER_PIXEL) as u32)
            .buffer_image_height(0)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width: self.width,
                height: self.height,
                depth: 1,
            })
            .build();
        unsafe {
            device.reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                command_buffer,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_pipeline_barrier2(
                command_buffer,
                &vk::DependencyInfo::builder().image_memory_barriers(&[into_transfer_destination]),
            );
            device.cmd_copy_buffer_to_image(
                command_buffer,
                pixel_buffer,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy_region],
            );
            device.cmd_pipeline_barrier2(
                command_buffer,
                &vk::DependencyInfo::builder().image_memory_barriers(&[into_sampled_read]),
            );
            device.end_command_buffer(command_buffer)?;
            let command_buffer_infos = [vk::CommandBufferSubmitInfo::builder()
                .command_buffer(command_buffer)
                .build()];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&command_buffer_infos)
                .build();
            device.reset_fences(&[self.fence])?;
            self.codec_device.submit_to_queue(
                self.codec_device.compute_queue().queue,
                &[submit],
                self.fence,
            )?;
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        }
        Ok(())
    }
}

impl Drop for RgbaCanvasFilledFromPixelBuffers {
    fn drop(&mut self) {
        let device = self.codec_device.device();
        unsafe {
            if self.fence != vk::Fence::null() {
                let _ = device.wait_for_fences(&[self.fence], true, u64::MAX);
                device.destroy_fence(self.fence, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                device.destroy_command_pool(self.command_pool, None);
            }
            if self.image_view != vk::ImageView::null() {
                device.destroy_image_view(self.image_view, None);
            }
            self.codec_device
                .allocator()
                .destroy_image(self.image, self.allocation);
        }
    }
}

// SAFETY: the canvas's handles are used only through `&self` / `&mut self` of
// the encoder that owns it, one frame at a time.
unsafe impl Send for RgbaCanvasFilledFromPixelBuffers {}

const RGBA8_BYTES_PER_PIXEL: u64 = 4;

const SINGLE_COLOR_SUBRESOURCE_RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

/// A [`SimpleEncoder`] fed imported RGBA pixel buffers.
pub struct RgbaPixelBufferVideoEncoder {
    encoder: SimpleEncoder,
    rgba_canvas: Option<RgbaCanvasFilledFromPixelBuffers>,
}

impl RgbaPixelBufferVideoEncoder {
    /// Wrap `encoder`; the RGBA canvas mints on the first frame.
    pub fn new(encoder: SimpleEncoder) -> Self {
        Self {
            encoder,
            rgba_canvas: None,
        }
    }

    /// The encode session.
    pub fn encoder(&self) -> &SimpleEncoder {
        &self.encoder
    }

    /// The encode session, to force a sync point.
    pub fn encoder_mut(&mut self) -> &mut SimpleEncoder {
        &mut self.encoder
    }

    /// Encode one RGBA8 frame held in `pixel_buffer`, rows `bytes_per_row`
    /// apart.
    pub fn encode_rgba_pixel_buffer(
        &mut self,
        pixel_buffer: &ConsumerVulkanBuffer,
        bytes_per_row: u64,
        width: u32,
        height: u32,
        timestamp_ns: i64,
    ) -> VideoResult<Vec<EncodePacket>> {
        let tightest_row = u64::from(width) * RGBA8_BYTES_PER_PIXEL;
        if bytes_per_row < tightest_row || bytes_per_row % RGBA8_BYTES_PER_PIXEL != 0 {
            return Err(VideoError::Configuration(format!(
                "an RGBA8 pixel buffer {width} pixels wide cannot have rows {bytes_per_row} \
                 bytes apart"
            )));
        }
        if u64::from(height) * bytes_per_row > pixel_buffer.size() {
            return Err(VideoError::Configuration(format!(
                "a {width}x{height} RGBA8 frame with rows {bytes_per_row} bytes apart does not \
                 fit the {}-byte pixel buffer that carries it",
                pixel_buffer.size()
            )));
        }
        let canvas_fits = self
            .rgba_canvas
            .as_ref()
            .is_some_and(|canvas| canvas.width == width && canvas.height == height);
        if !canvas_fits {
            self.rgba_canvas = None;
            self.rgba_canvas = Some(RgbaCanvasFilledFromPixelBuffers::new(
                &self.encoder.codec_device,
                width,
                height,
            )?);
        }
        let Some(canvas) = self.rgba_canvas.as_ref() else {
            return Err(VideoError::Rhi("the RGBA canvas did not mint".to_string()));
        };
        canvas.fill_from(pixel_buffer.buffer(), bytes_per_row)?;
        let canvas_view = canvas.image_view;
        self.encoder.encode_image(
            canvas_view,
            SampledImageReadLayout::ShaderReadOnlyOptimal,
            width,
            height,
            Some(timestamp_ns),
        )
    }
}

impl Drop for RgbaPixelBufferVideoEncoder {
    fn drop(&mut self) {
        let _ = self.encoder.codec_device.wait_idle();
        self.rgba_canvas = None;
    }
}
