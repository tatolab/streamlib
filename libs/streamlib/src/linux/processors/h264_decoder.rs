// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Decoder Processor
//
// Thin wrapper around vulkan_video::SimpleDecoder (Vulkan Video hardware decoding).
// Decoded NV12 frames are converted to RGBA and written to a pixel buffer for
// output as Videoframe. Future: #270 will couple to the RHI for GPU-resident output.

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContext;
use crate::core::rhi::PixelFormat;
use crate::core::{Result, RuntimeContext, StreamError};

use vulkan_video::{Codec, SimpleDecoder, SimpleDecoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_decoder")]
pub struct H264DecoderProcessor {
    /// Vulkan Video hardware decoder.
    decoder: Option<SimpleDecoder>,

    /// GPU context for creating pixel buffers for decoded frames.
    gpu_context: Option<GpuContext>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl crate::core::ReactiveProcessor for H264DecoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

        let decoder_config = SimpleDecoderConfig {
            codec: Codec::H264,
            ..Default::default()
        };

        let decoder = SimpleDecoder::new(decoder_config).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.264 decoder: {e}"))
        })?;

        tracing::info!("[H264Decoder] Initialized (Vulkan Video hardware)");

        self.decoder = Some(decoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H264Decoder] Shutting down"
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
            .ok_or_else(|| StreamError::Runtime("H.264 decoder not initialized".into()))?;

        let decoded_frames = decoder.feed(&encoded.data).map_err(|e| {
            StreamError::Runtime(format!("H.264 decode failed: {e}"))
        })?;

        for decoded in decoded_frames {
            let width = decoded.width;
            let height = decoded.height;

            // Acquire pixel buffer for output
            let (pool_id, pixel_buffer) =
                gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;

            let rgba_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
            let rgba_size = (width * height * 4) as usize;
            let rgba_data = unsafe { std::slice::from_raw_parts_mut(rgba_ptr, rgba_size) };

            // NV12 → RGBA conversion (BT.601). This CPU path will be replaced by
            // GPU compute in #270 (RHI coupling with shared Vulkan device).
            let y_plane = &decoded.data[..(width * height) as usize];
            let uv_plane = &decoded.data[(width * height) as usize..];
            for row in 0..height {
                for col in 0..width {
                    let y_idx = (row * width + col) as usize;
                    let uv_idx = ((row / 2) * (width / 2) + (col / 2)) as usize * 2;
                    let y = y_plane[y_idx] as f32;
                    let u = uv_plane[uv_idx] as f32 - 128.0;
                    let v = uv_plane[uv_idx + 1] as f32 - 128.0;
                    let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
                    let g = (y - 0.344 * u - 0.714 * v).clamp(0.0, 255.0) as u8;
                    let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
                    let dst = y_idx * 4;
                    rgba_data[dst] = r;
                    rgba_data[dst + 1] = g;
                    rgba_data[dst + 2] = b;
                    rgba_data[dst + 3] = 255;
                }
            }

            let timestamp_ns = encoded.timestamp_ns.clone();

            let video_frame = Videoframe {
                surface_id: pool_id.to_string(),
                width,
                height,
                timestamp_ns,
                frame_index: self.frames_decoded.to_string(),
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
