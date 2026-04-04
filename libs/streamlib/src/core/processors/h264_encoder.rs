// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Encoder Processor
//
// Encodes VideoFrame to EncodedVideoFrame using platform-specific hardware
// acceleration (Vulkan Video on Linux, VideoToolbox on macOS).

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::codec::{VideoEncoder, VideoEncoderConfig};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_encoder")]
pub struct H264EncoderProcessor {
    /// GPU context for encoder and buffer lookup.
    gpu_context: Option<GpuContext>,

    /// Video encoder (platform-specific).
    video_encoder: Option<VideoEncoder>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl crate::core::ReactiveProcessor for H264EncoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        let mut encoder_config = VideoEncoderConfig::default();
        if let Some(w) = self.config.width {
            encoder_config.width = w;
        }
        if let Some(h) = self.config.height {
            encoder_config.height = h;
        }
        if let Some(bps) = self.config.bitrate_bps {
            encoder_config.bitrate_bps = bps;
        }
        if let Some(ki) = self.config.keyframe_interval {
            encoder_config.keyframe_interval_frames = ki;
        }

        let gpu_context = ctx.gpu.clone();
        let encoder = VideoEncoder::new(encoder_config, Some(gpu_context.clone()), &ctx)?;

        tracing::info!("[H264Encoder] Initialized");

        self.gpu_context = Some(gpu_context);
        self.video_encoder = Some(encoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H264Encoder] Shutting down"
        );
        self.video_encoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Encoder not initialized".into()))?;

        let gpu = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not available".into()))?;

        let encoded: Encodedvideoframe = encoder.encode(&frame, gpu)?;
        self.outputs.write("encoded_video_out", &encoded)?;

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("[H264Encoder] First frame encoded");
        } else if self.frames_encoded % 100 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H264Encoder] Encode progress");
        }

        Ok(())
    }
}
