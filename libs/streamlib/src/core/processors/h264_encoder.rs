// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Encoder Processor
//
// Encodes VideoFrame to EncodedVideoFrame using platform-specific hardware
// acceleration (Vulkan Video on Linux, VideoToolbox on macOS).
// Defers encoder creation to the first frame so GPU resources are fully
// initialized and frame dimensions are known.

use crate::_generated_::Videoframe;
use crate::core::codec::{H264Profile, VideoCodec, VideoEncoder, VideoEncoderConfig};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_encoder")]
pub struct H264EncoderProcessor {
    /// Runtime context (kept for deferred encoder creation).
    runtime_context: Option<RuntimeContext>,

    /// GPU context for encoder and buffer lookup.
    gpu_context: Option<GpuContext>,

    /// Video encoder (created on first frame, not in setup).
    video_encoder: Option<VideoEncoder>,

    /// Frames encoded counter.
    frames_encoded: u64,

    /// Set to true when GPU device is lost — stops further encode attempts.
    device_lost: bool,
}

impl crate::core::ReactiveProcessor for H264EncoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.runtime_context = Some(ctx);
        tracing::info!("[H264Encoder] Initialized (encoder deferred to first frame)");
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H264Encoder] Shutting down"
        );
        self.video_encoder.take();
        self.gpu_context.take();
        self.runtime_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if self.device_lost {
            return Ok(());
        }

        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        // Create encoder on first frame — dimensions are now known from the actual frame
        if self.video_encoder.is_none() {
            let ctx = self.runtime_context.as_ref()
                .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?;
            let gpu_context = self.gpu_context.clone();

            let width = self.config.width.unwrap_or(frame.width);
            let height = self.config.height.unwrap_or(frame.height);

            // Baseline only — Main/High (CABAC) decode is broken on NVIDIA
            // Vulkan Video. See issue #233 for details. AV1 is the path forward
            // for better compression.
            let profile = H264Profile::Baseline;
            if let Some(requested) = self.config.profile.as_deref() {
                if requested != "baseline" {
                    tracing::warn!(
                        "[H264Encoder] Requested profile '{}' not supported — using Baseline (CAVLC). \
                         Main/High CABAC decode has a known issue on NVIDIA Vulkan Video.",
                        requested
                    );
                }
            }

            let encoder_config = VideoEncoderConfig {
                width,
                height,
                fps: 30,
                bitrate_bps: self.config.bitrate_bps.unwrap_or(2_500_000),
                keyframe_interval_frames: self.config.keyframe_interval.unwrap_or(15),
                codec: VideoCodec::H264(profile),
                low_latency: true,
            };

            tracing::info!(
                width, height,
                "[H264Encoder] Creating encoder for {}x{} (from first frame)",
                width, height
            );

            match VideoEncoder::new(encoder_config, gpu_context, ctx) {
                Ok(encoder) => {
                    self.video_encoder = Some(encoder);
                }
                Err(e) => {
                    tracing::error!("[H264Encoder] Failed to create encoder: {}", e);
                    self.device_lost = true;
                    return Ok(());
                }
            }
        }

        let encoder = self.video_encoder.as_mut().unwrap();
        let gpu = self.gpu_context.as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not available".into()))?;

        match encoder.encode(&frame, gpu) {
            Ok(encoded) => {
                if encoded.data.is_empty() {
                    return Ok(());
                }
                // Log NAL types for keyframes to verify IDR production
                if encoded.is_keyframe {
                    let mut nal_types = Vec::new();
                    let d = &encoded.data;
                    let mut j = 0;
                    while j + 3 < d.len() {
                        if (j + 3 < d.len() && d[j] == 0 && d[j+1] == 0 && d[j+2] == 1)
                            || (j + 4 <= d.len() && d[j] == 0 && d[j+1] == 0 && d[j+2] == 0 && d[j+3] == 1)
                        {
                            let sc_len = if d[j+2] == 1 { 3 } else { 4 };
                            let nh = d[j + sc_len];
                            nal_types.push(nh & 0x1f);
                            j += sc_len + 1;
                        } else { j += 1; }
                    }
                    tracing::info!(
                        "[H264Encoder] Keyframe output: {} bytes, NAL types={:?}",
                        encoded.data.len(), nal_types
                    );
                }
                self.outputs.write("encoded_video_out", &encoded)?;
                self.frames_encoded += 1;
                if self.frames_encoded == 1 {
                    tracing::info!("[H264Encoder] First frame encoded");
                } else if self.frames_encoded % 100 == 0 {
                    tracing::info!(frames = self.frames_encoded, "[H264Encoder] Encode progress");
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("device has been lost") || err_str.contains("device memory allocation") {
                    tracing::error!(
                        frames_encoded = self.frames_encoded,
                        "[H264Encoder] GPU device lost — stopping encode. Encoded {} frames before failure.",
                        self.frames_encoded
                    );
                    self.device_lost = true;
                    self.video_encoder.take();
                } else {
                    tracing::warn!("[H264Encoder] Encode error: {}", e);
                }
            }
        }

        Ok(())
    }
}
