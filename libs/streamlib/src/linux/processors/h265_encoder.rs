// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.265 Encoder Processor
//
// Thin wrapper around vulkan_video::SimpleEncoder (Vulkan Video hardware encoding).
// Uses submit_frame() with CPU-side NV12 data as the initial integration path.
// Future: #270 will couple to the RHI, enabling encode_image() with GPU-resident
// textures on a shared Vulkan device (zero-copy).

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContext;
use crate::core::{Result, RuntimeContext, StreamError};

use vulkan_video::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h265_encoder")]
pub struct H265EncoderProcessor {
    /// Vulkan Video hardware encoder.
    encoder: Option<SimpleEncoder>,

    /// GPU context for resolving Videoframe surface_ids to pixel buffers.
    gpu_context: Option<GpuContext>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl crate::core::ReactiveProcessor for H265EncoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

        let width = self.config.width.unwrap_or(1920);
        let height = self.config.height.unwrap_or(1080);

        let encoder_config = SimpleEncoderConfig {
            width,
            height,
            fps: 30,
            codec: Codec::H265,
            preset: Preset::Medium,
            streaming: true,
            idr_interval_secs: self.config.keyframe_interval_seconds.unwrap_or(2.0) as u32,
            bitrate_bps: self.config.bitrate_bps,
            prepend_header_to_idr: Some(true),
            ..Default::default()
        };

        let encoder = SimpleEncoder::new(encoder_config).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.265 encoder: {e}"))
        })?;

        tracing::info!(
            "[H265Encoder] Initialized ({}x{}, Vulkan Video hardware)",
            width,
            height
        );

        self.encoder = Some(encoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H265Encoder] Shutting down"
        );
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
            .ok_or_else(|| StreamError::Runtime("H.265 encoder not initialized".into()))?;

        // Resolve Videoframe surface_id to HOST_VISIBLE pixel buffer
        let pixel_buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        let width = pixel_buffer.width;
        let height = pixel_buffer.height;
        let rgba_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
        let rgba_size = (width * height * 4) as usize;
        let rgba_data =
            unsafe { std::slice::from_raw_parts(rgba_ptr, rgba_size) };

        // RGBA → NV12 conversion (BT.601). This CPU path will be replaced by
        // GPU compute in #270 (RHI coupling with shared Vulkan device).
        let nv12_size = (width * height * 3 / 2) as usize;
        let mut nv12_data = vec![0u8; nv12_size];
        let y_plane = &mut nv12_data[..(width * height) as usize];
        for i in 0..(width * height) as usize {
            let r = rgba_data[i * 4] as f32;
            let g = rgba_data[i * 4 + 1] as f32;
            let b = rgba_data[i * 4 + 2] as f32;
            y_plane[i] = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
        }
        let uv_plane = &mut nv12_data[(width * height) as usize..];
        let half_w = width / 2;
        let half_h = height / 2;
        for row in 0..half_h {
            for col in 0..half_w {
                let src_row = (row * 2) as usize;
                let src_col = (col * 2) as usize;
                let i = src_row * width as usize + src_col;
                let r = rgba_data[i * 4] as f32;
                let g = rgba_data[i * 4 + 1] as f32;
                let b = rgba_data[i * 4 + 2] as f32;
                let u = (-0.169 * r - 0.331 * g + 0.500 * b + 128.0).clamp(0.0, 255.0) as u8;
                let v = (0.500 * r - 0.419 * g - 0.081 * b + 128.0).clamp(0.0, 255.0) as u8;
                let uv_idx = (row * half_w + col) as usize * 2;
                uv_plane[uv_idx] = u;
                uv_plane[uv_idx + 1] = v;
            }
        }

        let timestamp_ns: Option<i64> = frame.timestamp_ns.parse().ok();

        let packets = encoder.submit_frame(&nv12_data, timestamp_ns).map_err(|e| {
            StreamError::Runtime(format!("H.265 encode failed: {e}"))
        })?;

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
            tracing::info!("[H265Encoder] First frame encoded");
        } else if self.frames_encoded % 300 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H265Encoder] Encode progress");
        }

        Ok(())
    }
}
