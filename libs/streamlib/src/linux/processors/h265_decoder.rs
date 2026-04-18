// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.265 Decoder Processor
//
// Thin wrapper around vulkan_video::SimpleDecoder using the shared RHI
// VulkanDevice. Decoded NV12 frames are written to pixel buffers for output.

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContext;
use crate::core::rhi::PixelFormat;
use crate::core::{Result, RuntimeContext, StreamError};

use vulkan_video::{Codec, SimpleDecoder, SimpleDecoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h265_decoder")]
pub struct H265DecoderProcessor {
    /// Vulkan Video hardware decoder (shares RHI device).
    decoder: Option<SimpleDecoder>,

    /// GPU context for creating pixel buffers for decoded frames.
    gpu_context: Option<GpuContext>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl crate::core::ReactiveProcessor for H265DecoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

        let decoder_config = SimpleDecoderConfig {
            codec: Codec::H265,
            rgba_output: false,
            ..Default::default()
        };

        let vulkan_device = &ctx.gpu.device().inner;

        let decode_queue = vulkan_device.video_decode_queue().ok_or_else(|| {
            StreamError::Runtime("GPU does not support Vulkan Video decode".into())
        })?;
        let decode_queue_family = vulkan_device.video_decode_queue_family_index().ok_or_else(|| {
            StreamError::Runtime("No video decode queue family".into())
        })?;

        let decoder = SimpleDecoder::from_device(
            decoder_config,
            vulkan_device.instance().clone(),
            vulkan_device.device().clone(),
            vulkan_device.physical_device(),
            vulkan_device.allocator().clone(),
            decode_queue,
            decode_queue_family,
            vulkan_device.queue(),
            vulkan_device.queue_family_index(),
        ).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.265 decoder: {e}"))
        })?;

        tracing::info!("[H265Decoder] Initialized (shared RHI device, Vulkan Video hardware)");

        self.decoder = Some(decoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H265Decoder] Shutting down"
        );
        self.decoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
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
            .ok_or_else(|| StreamError::Runtime("H.265 decoder not initialized".into()))?;

        let decoded_frames = decoder.feed(&encoded.data).map_err(|e| {
            StreamError::Runtime(format!("H.265 decode failed: {e}"))
        })?;

        for decoded in decoded_frames {
            let width = decoded.width;
            let height = decoded.height;

            // Write decoded NV12 data directly to pixel buffer.
            // NV12 = Y plane (W*H) + interleaved UV plane (W*H/2).
            // The consumer (MP4 writer / display) handles NV12→RGB conversion.
            let nv12_size = (width * height * 3 / 2) as usize;
            let (pool_id, pixel_buffer) =
                gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;

            let dst_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
            let src = &decoded.data[..nv12_size.min(decoded.data.len())];
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr, src.len());
            }

            // Dump raw decoded NV12 + encoded bitstream for PSNR verification.
            {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/streamlib_decoded_nv12.raw") {
                    let _ = f.write_all(src);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/streamlib_encoded.h265") {
                    let _ = f.write_all(&encoded.data);
                }
            }

            let timestamp_ns = encoded.timestamp_ns.clone();

            let video_frame = Videoframe {
                surface_id: pool_id.to_string(),
                width,
                height,
                timestamp_ns,
                frame_index: self.frames_decoded.to_string(),
                fps: encoded.fps,
            };

            self.outputs.write("video_out", &video_frame)?;
            self.frames_decoded += 1;
        }

        if self.frames_decoded == 1 {
            tracing::info!("[H265Decoder] First frame decoded");
        } else if self.frames_decoded % 300 == 0 {
            tracing::info!(frames = self.frames_decoded, "[H265Decoder] Decode progress");
        }

        Ok(())
    }
}
