// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Encoder Processor
//
// Thin wrapper around vulkan_video::SimpleEncoder (Vulkan Video hardware encoding).
// Uses encode_image() with a BGRA GPU image on the encoder's own Vulkan device.
// Pixel data is uploaded from streamlib's HOST_VISIBLE pixel buffer to the
// encoder's staging buffer, then copied to the GPU image for encoding.
//
// The encoder creates its own Vulkan device and exposes it via device()/allocator()
// so callers can create compatible images. Future #270 will share a single device.

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContext;
use crate::core::{Result, RuntimeContext, StreamError};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use vulkan_video::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

/// GPU upload resources created on the encoder's Vulkan device.
struct EncoderUploadResources {
    bgra_image: vk::Image,
    bgra_view: vk::ImageView,
    bgra_alloc: vma::Allocation,
    staging_buffer: vk::Buffer,
    staging_alloc: vma::Allocation,
    staging_ptr: *mut u8,
    aligned_width: u32,
    aligned_height: u32,
}

unsafe impl Send for EncoderUploadResources {}

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_encoder")]
pub struct H264EncoderProcessor {
    /// Vulkan Video hardware encoder.
    encoder: Option<SimpleEncoder>,

    /// GPU upload resources on the encoder's device.
    upload: Option<EncoderUploadResources>,

    /// GPU context for resolving Videoframe surface_ids to pixel buffers.
    gpu_context: Option<GpuContext>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl crate::core::ReactiveProcessor for H264EncoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

        let width = self.config.width.unwrap_or(1920);
        let height = self.config.height.unwrap_or(1080);

        let encoder_config = SimpleEncoderConfig {
            width,
            height,
            fps: 30,
            codec: Codec::H264,
            preset: Preset::Medium,
            streaming: true,
            idr_interval_secs: self.config.keyframe_interval_seconds.unwrap_or(2.0) as u32,
            bitrate_bps: self.config.bitrate_bps,
            prepend_header_to_idr: Some(true),
            ..Default::default()
        };

        let encoder = SimpleEncoder::new(encoder_config).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.264 encoder: {e}"))
        })?;

        let upload = unsafe { create_bgra_upload_resources(&encoder)? };

        tracing::info!(
            "[H264Encoder] Initialized ({}x{}, aligned {}x{}, Vulkan Video hardware)",
            width, height, upload.aligned_width, upload.aligned_height
        );

        self.upload = Some(upload);
        self.encoder = Some(encoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H264Encoder] Shutting down"
        );

        // Destroy upload resources before dropping encoder (uses encoder's device/allocator).
        if let (Some(upload), Some(ref encoder)) = (self.upload.take(), &self.encoder) {
            unsafe {
                let allocator = encoder.allocator();
                encoder.device().device_wait_idle().ok();
                encoder.device().destroy_image_view(upload.bgra_view, None);
                allocator.destroy_image(upload.bgra_image, upload.bgra_alloc);
                allocator.destroy_buffer(upload.staging_buffer, upload.staging_alloc);
            }
        }

        self.encoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not initialized".into()))?;

        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("H.264 encoder not initialized".into()))?;

        let upload = self
            .upload
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Upload resources not initialized".into()))?;

        // Resolve Videoframe surface_id to HOST_VISIBLE pixel buffer
        let pixel_buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        let src_width = pixel_buffer.width;
        let src_height = pixel_buffer.height;
        let bgra_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
        let bgra_size = (src_width * src_height * 4) as usize;
        let bgra_data = unsafe { std::slice::from_raw_parts(bgra_ptr, bgra_size) };

        let timestamp_ns: Option<i64> = frame.timestamp_ns.parse().ok();

        // Upload BGRA to encoder's GPU image and encode
        let packets = unsafe {
            upload_and_encode(
                encoder,
                bgra_data,
                upload,
                src_width,
                src_height,
                timestamp_ns,
            )?
        };

        for packet in packets {
            let encoded = Encodedvideoframe {
                data: packet.data,
                is_keyframe: packet.is_keyframe,
                timestamp_ns: packet.timestamp_ns.unwrap_or(0).to_string(),
                frame_number: self.frames_encoded.to_string(),
            };
            self.outputs.write("encoded_video_out", &encoded)?;
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("[H264Encoder] First frame encoded");
        } else if self.frames_encoded % 300 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H264Encoder] Encode progress");
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GPU upload helpers (operate on the encoder's own Vulkan device)
// ---------------------------------------------------------------------------

unsafe fn create_bgra_upload_resources(
    encoder: &SimpleEncoder,
) -> Result<EncoderUploadResources> {
    let device = encoder.device();
    let allocator = encoder.allocator();
    let (transfer_qf, _) = encoder.transfer_queue();
    let (compute_qf, _) = encoder.compute_queue();
    let (aligned_w, aligned_h) = encoder.aligned_extent();

    let queue_families = [transfer_qf, compute_qf];
    let mut image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D { width: aligned_w, height: aligned_h, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    if transfer_qf != compute_qf {
        image_info = image_info
            .sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(&queue_families);
    } else {
        image_info = image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
    }

    let alloc_opts = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };
    let (bgra_image, bgra_alloc) = allocator.create_image(image_info, &alloc_opts)
        .map_err(|e| StreamError::GpuError(format!("Failed to create BGRA image: {e}")))?;

    let bgra_view = device.create_image_view(
        &vk::ImageViewCreateInfo::builder()
            .image(bgra_image)
            .view_type(vk::ImageViewType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }),
        None,
    ).map_err(|e| StreamError::GpuError(format!("Failed to create BGRA image view: {e}")))?;

    let staging_size = (aligned_w * aligned_h * 4) as u64;
    let staging_info = vk::BufferCreateInfo::builder()
        .size(staging_size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC);
    let staging_opts = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        flags: vma::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    let (staging_buffer, staging_alloc) = allocator.create_buffer(staging_info, &staging_opts)
        .map_err(|e| StreamError::GpuError(format!("Failed to create staging buffer: {e}")))?;
    let info = allocator.get_allocation_info(staging_alloc);
    let staging_ptr = info.pMappedData as *mut u8;

    Ok(EncoderUploadResources {
        bgra_image,
        bgra_view,
        bgra_alloc,
        staging_buffer,
        staging_alloc,
        staging_ptr,
        aligned_width: aligned_w,
        aligned_height: aligned_h,
    })
}

unsafe fn upload_and_encode(
    encoder: &mut SimpleEncoder,
    bgra_data: &[u8],
    upload: &EncoderUploadResources,
    src_width: u32,
    src_height: u32,
    timestamp_ns: Option<i64>,
) -> Result<Vec<vulkan_video::EncodePacket>> {
    let device = encoder.device().clone();
    let (transfer_qf, transfer_queue) = encoder.transfer_queue();

    // Copy BGRA pixels into staging buffer (row-by-row if alignment differs).
    let src_row_bytes = (src_width * 4) as usize;
    let dst_row_bytes = (upload.aligned_width * 4) as usize;
    if src_row_bytes == dst_row_bytes && src_width == upload.aligned_width && src_height == upload.aligned_height {
        std::ptr::copy_nonoverlapping(bgra_data.as_ptr(), upload.staging_ptr, bgra_data.len());
    } else {
        std::ptr::write_bytes(upload.staging_ptr, 0, (upload.aligned_width * upload.aligned_height * 4) as usize);
        for row in 0..src_height as usize {
            let src_off = row * src_row_bytes;
            let dst_off = row * dst_row_bytes;
            std::ptr::copy_nonoverlapping(
                bgra_data.as_ptr().add(src_off),
                upload.staging_ptr.add(dst_off),
                src_row_bytes,
            );
        }
    }

    // One-shot command buffer for staging → GPU image upload.
    let pool = device.create_command_pool(
        &vk::CommandPoolCreateInfo::builder()
            .queue_family_index(transfer_qf)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT),
        None,
    ).map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))?;

    let cb = device.allocate_command_buffers(
        &vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1),
    ).map_err(|e| StreamError::GpuError(format!("Failed to allocate command buffer: {e}")))?[0];

    device.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
        .map_err(|e| StreamError::GpuError(format!("Failed to begin command buffer: {e}")))?;

    // Transition: UNDEFINED → TRANSFER_DST_OPTIMAL
    device.cmd_pipeline_barrier(
        cb,
        vk::PipelineStageFlags::TOP_OF_PIPE,
        vk::PipelineStageFlags::TRANSFER,
        vk::DependencyFlags::empty(),
        &[] as &[vk::MemoryBarrier],
        &[] as &[vk::BufferMemoryBarrier],
        &[vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(upload.bgra_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0, level_count: 1,
                base_array_layer: 0, layer_count: 1,
            })
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)],
    );

    // Copy staging buffer → GPU image
    device.cmd_copy_buffer_to_image(
        cb,
        upload.staging_buffer,
        upload.bgra_image,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        &[vk::BufferImageCopy::builder()
            .buffer_offset(0)
            .buffer_row_length(upload.aligned_width)
            .buffer_image_height(upload.aligned_height)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0, base_array_layer: 0, layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: upload.aligned_width, height: upload.aligned_height, depth: 1,
            })],
    );

    // Transition: TRANSFER_DST → SHADER_READ_ONLY_OPTIMAL
    device.cmd_pipeline_barrier(
        cb,
        vk::PipelineStageFlags::TRANSFER,
        vk::PipelineStageFlags::COMPUTE_SHADER,
        vk::DependencyFlags::empty(),
        &[] as &[vk::MemoryBarrier],
        &[] as &[vk::BufferMemoryBarrier],
        &[vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(upload.bgra_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0, level_count: 1,
                base_array_layer: 0, layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)],
    );

    device.end_command_buffer(cb)
        .map_err(|e| StreamError::GpuError(format!("Failed to end command buffer: {e}")))?;

    let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)
        .map_err(|e| StreamError::GpuError(format!("Failed to create fence: {e}")))?;
    device.queue_submit(
        transfer_queue,
        &[vk::SubmitInfo::builder().command_buffers(&[cb])],
        fence,
    ).map_err(|e| StreamError::GpuError(format!("Failed to submit transfer: {e}")))?;
    device.wait_for_fences(&[fence], true, u64::MAX)
        .map_err(|e| StreamError::GpuError(format!("Fence wait failed: {e}")))?;
    device.destroy_fence(fence, None);
    device.destroy_command_pool(pool, None);

    // Encode the GPU-resident BGRA image via compute shader (RGB→NV12) + video encode.
    encoder.encode_image(upload.bgra_view, timestamp_ns)
        .map_err(|e| StreamError::Runtime(format!("H.264 encode failed: {e}")))
}
