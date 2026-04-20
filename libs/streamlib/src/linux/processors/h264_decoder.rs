// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Decoder Processor
//
// Thin wrapper around vulkan_video::SimpleDecoder using the shared RHI
// VulkanDevice. Decoded NV12 frames are written to pixel buffers for output.

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::PixelFormat;
use crate::core::{Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamError};

use vulkan_video::{Codec, SimpleDecoder, SimpleDecoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_decoder")]
pub struct H264DecoderProcessor {
    /// Vulkan Video hardware decoder (shares RHI device).
    decoder: Option<SimpleDecoder>,

    /// GPU context for creating pixel buffers for decoded frames.
    gpu_context: Option<GpuContextLimitedAccess>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl crate::core::ReactiveProcessor for H264DecoderProcessor::Processor {
    async fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        let decoder_config = SimpleDecoderConfig {
            codec: Codec::H264,
            rgba_output: true,
            max_width: 1920,
            max_height: 1080,
            ..Default::default()
        };

        let vulkan_device = &ctx.gpu_full_access().device().inner;

        let decode_queue = vulkan_device.video_decode_queue().ok_or_else(|| {
            StreamError::Runtime("GPU does not support Vulkan Video decode".into())
        })?;
        let decode_queue_family = vulkan_device.video_decode_queue_family_index().ok_or_else(|| {
            StreamError::Runtime("No video decode queue family".into())
        })?;

        let submitter: std::sync::Arc<dyn vulkan_video::RhiQueueSubmitter> =
            ctx.gpu_full_access().device().inner.clone();

        let mut decoder = SimpleDecoder::from_device(
            decoder_config,
            vulkan_device.instance().clone(),
            vulkan_device.device().clone(),
            vulkan_device.physical_device(),
            vulkan_device.allocator().clone(),
            submitter,
            decode_queue,
            decode_queue_family,
            vulkan_device.queue(),
            vulkan_device.queue_family_index(),
        ).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.264 decoder: {e}"))
        })?;

        // Pre-create the video session BEFORE the display swapchain.
        // NVIDIA limits video session creation after swapchain exists.
        decoder.pre_initialize_session().map_err(|e| {
            StreamError::Runtime(format!("Failed to pre-initialize H.264 decoder session: {e}"))
        })?;

        // Eagerly allocate the NV12→RGBA converter before the display swapchain
        // consumes NVIDIA's post-swapchain DEVICE_LOCAL / DMA-BUF allocation budget
        // (docs/learnings/nvidia-dma-buf-after-swapchain.md).
        decoder.prepare_gpu_decode_resources().map_err(|e| {
            StreamError::Runtime(format!("Failed to pre-allocate H.264 decode resources: {e}"))
        })?;

        // Pre-allocate the output pixel buffer pool at the codec-aligned extent
        // derived from the decoder config, also before the swapchain. The pool
        // is DMA-BUF exportable; sizing it to the decoder's max extent ensures
        // the underlying VMA block is large enough for any SPS up to that cap.
        let (aligned_w, aligned_h) = decoder.aligned_extent();
        let (_probe_id, _probe_buffer) = ctx.gpu_full_access().acquire_pixel_buffer(
            aligned_w,
            aligned_h,
            crate::core::rhi::PixelFormat::Rgba32,
        )?;

        tracing::info!("[H264Decoder] Initialized (shared RHI device, Vulkan Video hardware)");

        self.decoder = Some(decoder);
        Ok(())
    }

    async fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H264Decoder] Shutting down"
        );
        self.decoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_video_in") {
            return Ok(());
        }
        let encoded: Encodedvideoframe = self.inputs.read("encoded_video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not initialized".into()))?;

        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("H.264 decoder not initialized".into()))?;

        let decoded_frames = decoder.feed(&encoded.data).map_err(|e| {
            StreamError::Runtime(format!("H.264 decode failed: {e}"))
        })?;

        for decoded in decoded_frames {
            let width = decoded.width;
            let height = decoded.height;

            // Decoded frames come back as RGBA (GPU NV12→RGBA via Nv12ToRgbConverter).
            let rgba_size = (width * height * 4) as usize;
            let src = &decoded.data[..rgba_size.min(decoded.data.len())];

            // Write RGBA to a pixel buffer. The texture cache resolves pixel
            // buffers as textures on demand (buffer→image upload in GpuContext).
            let (pool_id, pixel_buffer) =
                gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
            let dst_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr, src.len());
            }

            // Register as texture by uploading pixel buffer to GPU texture.
            // `upload_pixel_buffer_as_texture` creates a new DEVICE_LOCAL texture
            // per decoded frame, so it's FullAccess-only and must be escalated.
            // TODO(#324-followup): restructure to a pre-allocated texture ring in
            // setup() so steady-state decode doesn't escalate per frame.
            let surface_id = pool_id.to_string();
            gpu_ctx.escalate(|full| {
                full.upload_pixel_buffer_as_texture(&surface_id, &pixel_buffer, width, height)
            })?;

            let timestamp_ns = encoded.timestamp_ns.clone();

            // Carry the encoder's input-frame index (`frame_number`) through to the
            // decoded output so downstream consumers (PSNR rig, display PNG sampler)
            // can pair each decoded frame with the reference input.
            let video_frame = Videoframe {
                surface_id,
                width,
                height,
                timestamp_ns,
                frame_index: encoded.frame_number.clone(),
                fps: encoded.fps,
            };

            self.outputs.write("video_out", &video_frame)?;
            self.frames_decoded += 1;
        }

        if self.frames_decoded == 1 {
            tracing::info!("[H264Decoder] First frame decoded");
        } else if self.frames_decoded % 300 == 0 {
            tracing::info!(frames = self.frames_decoded, "[H264Decoder] Decode progress");
        }

        Ok(())
    }
}
