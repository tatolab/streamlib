// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC WHIP Streaming Processor
//
// This processor integrates:
// - H.264 encoding via platform-specific encoder (VideoToolbox on macOS, FFmpeg on Linux)
// - Opus audio encoding
// - WHIP signaling and WebRTC session via WhipClient

use crate::_generated_::{Audioframe, Videoframe};
use crate::core::codec::{H264Profile, VideoCodec, VideoEncoder};
use crate::core::streaming::{
    convert_audio_to_sample, convert_video_to_samples, AudioEncoderConfig, AudioEncoderOpus,
    OpusEncoder, WhipClient, WhipConfig,
};
use crate::core::VideoEncoderConfig;
use crate::core::{media_clock::MediaClock, GpuContext, Result, RuntimeContext, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("src/core/processors/webrtc_whip.yaml")]
pub struct WebRtcWhipProcessor {
    // RuntimeContext for tokio handle
    ctx: Option<RuntimeContext>,

    // GPU context for video encoder and buffer lookup
    gpu_context: Option<GpuContext>,

    // Session state
    session_started: bool,

    // Encoders
    video_encoder: Option<VideoEncoder>,
    audio_encoder: Option<OpusEncoder>,

    // WHIP client (owns WebRTC session)
    whip_client: Option<WhipClient>,

    // Stats tracking
    last_stats_time_ns: i64,
}

impl crate::core::ReactiveProcessor for WebRtcWhipProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Convert generated config to AudioEncoderConfig
        let audio_config = AudioEncoderConfig {
            sample_rate: self.config.audio.sample_rate,
            channels: self.config.audio.channels as u16,
            bitrate_bps: self.config.audio.bitrate_bps,
            frame_duration_ms: 20, // Standard Opus frame duration
            complexity: 10,        // Maximum quality
            vbr: true,             // Variable bitrate for better quality
        };
        self.audio_encoder = Some(OpusEncoder::new(audio_config)?);

        // Convert generated config to WhipConfig
        let whip_config = WhipConfig {
            endpoint_url: self.config.whip.endpoint_url.clone(),
            auth_token: self.config.whip.auth_token.clone(),
            timeout_ms: self.config.whip.timeout_ms as u64,
        };
        let whip_client = WhipClient::new(whip_config)?;
        self.whip_client = Some(whip_client);

        tracing::info!("WebRtcWhipProcessor initialized (will connect on first frame)");
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("WebRtcWhipProcessor shutting down");

        // Terminate WHIP session
        if let Some(mut client) = self.whip_client.take() {
            if let Err(e) = client.terminate().await {
                tracing::warn!("Error terminating WHIP session: {}", e);
            }
        }

        tracing::info!("WebRtcWhipProcessor shutdown complete");
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Read video and audio from IPC inputs
        let video_frame: Option<Videoframe> = if self.inputs.has_data("video") {
            self.inputs.read("video").ok()
        } else {
            None
        };
        let audio_frame: Option<Audioframe> = if self.inputs.has_data("audio") {
            self.inputs.read("audio").ok()
        } else {
            None
        };

        // Start session on first frame
        if !self.session_started && (video_frame.is_some() || audio_frame.is_some()) {
            tracing::info!("[WebRTC] Starting session - received first frame");
            self.start_session()?;
            self.session_started = true;
        }

        // Process video if available
        if let Some(ipc_frame) = video_frame {
            self.process_video_ipc_frame(&ipc_frame)?;
        }

        // Process audio if available
        if let Some(frame) = audio_frame {
            self.process_audio_frame(&frame)?;
        }

        // Log stats periodically
        if self.session_started {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let elapsed = current_time_ns - self.last_stats_time_ns;

            if elapsed >= 2_000_000_000 {
                self.log_stats();
                self.last_stats_time_ns = current_time_ns;
            }
        }

        Ok(())
    }
}

impl WebRtcWhipProcessor::Processor {
    /// Starts the WebRTC WHIP session.
    fn start_session(&mut self) -> Result<()> {
        // Initialize video encoder lazily
        if self.video_encoder.is_none() {
            let gpu_context = self.gpu_context.clone();
            let ctx = self
                .ctx
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?;
            // Convert generated config to VideoEncoderConfig
            let video_config = VideoEncoderConfig {
                width: self.config.video.width,
                height: self.config.video.height,
                fps: self.config.video.fps,
                bitrate_bps: self.config.video.bitrate_bps,
                keyframe_interval_frames: 60, // Keyframe every 2 seconds at 30fps
                codec: VideoCodec::H264(H264Profile::Baseline), // Baseline for WebRTC compatibility
                low_latency: true,            // Real-time streaming mode
            };
            self.video_encoder = Some(VideoEncoder::new(video_config, gpu_context, ctx)?);
            tracing::info!("Video encoder initialized");
        }

        // Connect WHIP client
        let tokio_handle = self.ctx.as_ref().unwrap().tokio_handle().clone();
        let client = self
            .whip_client
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("WhipClient not initialized".into()))?;

        tokio_handle.block_on(
            client.connect(self.config.video.bitrate_bps, self.config.audio.bitrate_bps),
        )?;

        self.last_stats_time_ns = MediaClock::now().as_nanos() as i64;

        tracing::info!("WebRTC WHIP session started");
        Ok(())
    }

    /// Process video frame received as IPC type (Videoframe).
    fn process_video_ipc_frame(&mut self, ipc_frame: &Videoframe) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        // Get GPU context for buffer resolution
        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GpuContext not available".into()))?;

        // Encode directly from IPC frame (encoder resolves buffer via GpuContext)
        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Video encoder not initialized".into()))?;

        let encoded = encoder.encode(ipc_frame, gpu_context)?;
        let samples = convert_video_to_samples(&encoded, self.config.video.fps)?;

        let tokio_handle = self.ctx.as_ref().unwrap().tokio_handle().clone();
        let client = self.whip_client.as_mut().unwrap();

        tokio_handle.block_on(client.write_video_samples(samples))?;

        Ok(())
    }

    fn process_audio_frame(&mut self, frame: &Audioframe) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let encoder = self
            .audio_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio encoder not initialized".into()))?;

        let encoded = encoder.encode(frame)?;
        let sample = convert_audio_to_sample(&encoded, self.config.audio.sample_rate)?;

        let tokio_handle = self.ctx.as_ref().unwrap().tokio_handle().clone();
        let client = self.whip_client.as_mut().unwrap();

        tokio_handle.block_on(client.write_audio_sample(sample))?;

        Ok(())
    }

    fn log_stats(&self) {
        if let (Some(client), Some(ctx)) = (&self.whip_client, &self.ctx) {
            let tokio_handle = ctx.tokio_handle();
            if let Some(stats) = tokio_handle.block_on(client.get_stats()) {
                let mut video_bytes_sent = 0u64;
                let mut audio_bytes_sent = 0u64;
                let mut video_packets_sent = 0u64;
                let mut audio_packets_sent = 0u64;

                for (_id, stat_type) in stats.reports.iter() {
                    if let webrtc::stats::StatsReportType::OutboundRTP(outbound) = stat_type {
                        if outbound.kind == "video" {
                            video_bytes_sent = outbound.bytes_sent;
                            video_packets_sent = outbound.packets_sent;
                        } else if outbound.kind == "audio" {
                            audio_bytes_sent = outbound.bytes_sent;
                            audio_packets_sent = outbound.packets_sent;
                        }
                    }
                }

                tracing::info!(
                    "[WebRTC Stats] Video: {} packets ({:.2} MB), Audio: {} packets ({:.2} KB)",
                    video_packets_sent,
                    video_bytes_sent as f64 / 1_000_000.0,
                    audio_packets_sent,
                    audio_bytes_sent as f64 / 1_000.0
                );
            }
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::videotoolbox::parse_nal_units;
    use crate::core::streaming::EncodedAudioFrame;
    use crate::core::EncodedVideoFrame;
    use std::time::Duration;

    #[test]
    fn test_convert_video_to_samples() {
        let encoded = EncodedVideoFrame {
            data: vec![
                0, 0, 0, 1, 0x67, 0x42, // SPS
                0, 0, 0, 1, 0x68, 0x43, // PPS
                0, 0, 0, 1, 0x65, 0xAA, // IDR
            ],
            timestamp_ns: 1_000_000_000,
            is_keyframe: true,
            frame_number: 0,
        };

        let samples = convert_video_to_samples(&encoded, 30).unwrap();
        assert_eq!(samples.len(), 3);

        let expected_duration = Duration::from_secs_f64(1.0 / 30.0);
        assert_eq!(samples[0].duration, expected_duration);
    }

    #[test]
    fn test_convert_audio_to_sample() {
        let encoded = EncodedAudioFrame {
            data: vec![0xAA, 0xBB, 0xCC, 0xDD],
            timestamp_ns: 1_000_000_000,
            sample_count: 960,
        };

        let sample = convert_audio_to_sample(&encoded, 48000).unwrap();
        let expected_duration = Duration::from_secs_f64(960.0 / 48000.0);
        assert_eq!(sample.duration, expected_duration);
    }

    #[test]
    fn test_parse_nal_units_multiple() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0x42, // SPS
            0, 0, 0, 1, 0x68, 0x43, // PPS
            0, 0, 1, 0x65, 0xAA, // IDR
        ];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);
    }

    #[test]
    fn test_config_defaults() {
        let video_config = VideoEncoderConfig::default();
        assert_eq!(video_config.width, 1280);
        assert_eq!(video_config.height, 720);
        assert_eq!(video_config.fps, 30);

        let audio_config = AudioEncoderConfig::default();
        assert_eq!(audio_config.sample_rate, 48000);
        assert_eq!(audio_config.channels, 2);
    }

    #[test]
    fn test_opus_encoder_creation() {
        let config = AudioEncoderConfig::default();
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_ok());
    }
}
