// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ A/V Publish Processor
//
// Encodes video (H.264) and audio (Opus) from input ports and publishes
// encoded frames to a MoQ relay via MoqPublishSession.

use crate::_generated_::{Audioframe, Videoframe};
use crate::core::codec::{H264Profile, VideoCodec, VideoEncoder};
use crate::core::streaming::{AudioEncoderConfig, AudioEncoderOpus, MoqPublishSession, MoqRelayConfig, OpusEncoder};
use crate::core::VideoEncoderConfig;
use crate::core::{media_clock::MediaClock, GpuContext, Result, RuntimeContext, StreamError};

const VIDEO_TRACK: &str = "video";
const AUDIO_TRACK: &str = "audio";

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_publish")]
pub struct MoqPublishProcessor {
    /// Runtime context for tokio handle.
    ctx: Option<RuntimeContext>,

    /// GPU context for video encoder and buffer lookup.
    gpu_context: Option<GpuContext>,

    /// Session state.
    session_started: bool,

    /// Video encoder (H.264).
    video_encoder: Option<VideoEncoder>,

    /// Audio encoder (Opus).
    audio_encoder: Option<OpusEncoder>,

    /// MoQ publish session (connected to relay).
    moq_publish_session: Option<MoqPublishSession>,

    /// Stats tracking.
    last_stats_time_ns: i64,
    video_frames_published: u64,
    audio_frames_published: u64,
}

impl crate::core::ReactiveProcessor for MoqPublishProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Initialize audio encoder
        let audio_config = AudioEncoderConfig {
            sample_rate: self.config.audio.sample_rate,
            channels: self.config.audio.channels as u16,
            bitrate_bps: self.config.audio.bitrate_bps,
            frame_duration_ms: 20,
            complexity: 5,
            vbr: true,
        };
        self.audio_encoder = Some(OpusEncoder::new(audio_config)?);

        // Connect to MoQ relay eagerly during setup so tracks exist before
        // any subscriber tries to subscribe. Video encoder is still lazy
        // (needs GPU context from first frame), but the MoQ session and
        // track announcement happen now.
        let relay_config = MoqRelayConfig {
            relay_endpoint_url: self.config.relay.endpoint_url.clone(),
            broadcast_path: self.config.relay.broadcast_path.clone(),
            tls_disable_verify: self.config.relay.tls_disable_verify.unwrap_or(false),
            timeout_ms: 10000,
        };

        let session = MoqPublishSession::connect(relay_config).await?;
        self.moq_publish_session = Some(session);
        self.session_started = true;

        tracing::info!(
            broadcast = %self.config.relay.broadcast_path,
            "MoqPublishProcessor connected to relay (video encoder deferred to first frame)"
        );
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("MoqPublishProcessor shutting down");

        // Drop session to close QUIC connection
        self.moq_publish_session.take();

        tracing::info!(
            video_published = self.video_frames_published,
            audio_published = self.audio_frames_published,
            "MoqPublishProcessor shutdown complete"
        );
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Read video and audio from IPC inputs
        let video_frame: Option<Videoframe> = if self.inputs.has_data("video_in") {
            match self.inputs.read("video_in") {
                Ok(frame) => Some(frame),
                Err(e) => {
                    tracing::warn!("[MoqPublish] Failed to read video frame: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let audio_frame: Option<Audioframe> = if self.inputs.has_data("audio_in") {
            self.inputs.read("audio_in").ok()
        } else {
            None
        };

        // Initialize video encoder on first frame (MoQ session already connected in setup)
        if self.video_encoder.is_none() && (video_frame.is_some() || audio_frame.is_some()) {
            tracing::info!("[MoqPublish] First frame received, initializing video encoder");
            self.start_session()?;
        }

        // Encode and publish video
        if let Some(ipc_frame) = video_frame {
            self.encode_and_publish_video(&ipc_frame)?;
        }

        // Encode and publish audio
        if let Some(frame) = audio_frame {
            self.encode_and_publish_audio(&frame)?;
        }

        // Log stats periodically
        if self.session_started {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let elapsed = current_time_ns - self.last_stats_time_ns;

            if elapsed >= 2_000_000_000 {
                tracing::info!(
                    "[MoqPublish Stats] Video: {} frames, Audio: {} frames",
                    self.video_frames_published,
                    self.audio_frames_published,
                );
                self.last_stats_time_ns = current_time_ns;
            }
        }

        Ok(())
    }
}

impl MoqPublishProcessor::Processor {
    /// Starts the MoQ publish session: initializes video encoder and connects to relay.
    fn start_session(&mut self) -> Result<()> {
        // Initialize video encoder lazily (needs actual frame dimensions from GPU)
        if self.video_encoder.is_none() {
            let gpu_context = self.gpu_context.clone();
            let ctx = self
                .ctx
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?;

            let video_config = VideoEncoderConfig {
                width: self.config.video.width,
                height: self.config.video.height,
                fps: self.config.video.fps,
                bitrate_bps: self.config.video.bitrate_bps,
                keyframe_interval_frames: 60, // Keyframe every 2 seconds at 30fps
                codec: VideoCodec::H264(H264Profile::Baseline),
                low_latency: true,
            };
            self.video_encoder = Some(VideoEncoder::new(video_config, gpu_context, ctx)?);
            tracing::info!("[MoqPublish] Video encoder initialized");
        }

        self.last_stats_time_ns = MediaClock::now().as_nanos() as i64;
        tracing::info!("[MoqPublish] Session ready, encoding started");
        Ok(())
    }

    /// Encode a raw video frame to H.264 and publish to the MoQ "video" track.
    fn encode_and_publish_video(&mut self, ipc_frame: &Videoframe) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GpuContext not available".into()))?;

        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Video encoder not initialized".into()))?;

        let encoded = match encoder.encode(ipc_frame, gpu_context) {
            Ok(enc) => enc,
            Err(e) => {
                tracing::error!("[MoqPublish] Video encode failed: {}", e);
                return Err(e);
            }
        };

        // Skip frames where encoder hasn't produced output yet (normal buffering)
        if encoded.data.is_empty() {
            tracing::debug!("[MoqPublish] Encoder buffering (no output yet), skipping frame");
            return Ok(());
        }

        let is_keyframe = encoded.is_keyframe;

        let session = self
            .moq_publish_session
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("MoQ session not connected".into()))?;

        // Publish the full Annex B H.264 frame to the "video" track
        session.publish_frame(VIDEO_TRACK, &encoded.data, is_keyframe)?;

        self.video_frames_published += 1;

        if is_keyframe {
            tracing::debug!(
                "[MoqPublish] Published video keyframe: {} bytes",
                encoded.data.len()
            );
        }

        Ok(())
    }

    /// Encode a raw audio frame to Opus and publish to the MoQ "audio" track.
    fn encode_and_publish_audio(&mut self, frame: &Audioframe) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let encoder = self
            .audio_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio encoder not initialized".into()))?;

        let encoded = encoder.encode(frame)?;

        let session = self
            .moq_publish_session
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("MoQ session not connected".into()))?;

        // Publish raw Opus bytes to the "audio" track
        session.publish_frame(AUDIO_TRACK, &encoded.data, false)?;

        self.audio_frames_published += 1;

        Ok(())
    }
}
