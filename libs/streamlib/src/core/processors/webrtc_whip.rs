// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC WHIP Streaming Processor
//
// Transport-only: accepts pre-encoded video (EncodedVideoFrame) and audio
// (EncodedAudioFrame), RTP-packetizes them, and sends via WebRTC WHIP.
// Encoding is handled by upstream H264EncoderProcessor / OpusEncoderProcessor.

use crate::_generated_::{Encodedaudioframe, Encodedvideoframe};
use crate::core::streaming::{convert_audio_to_sample, convert_video_to_samples};
use crate::core::streaming::{WhipClient, WhipConfig};
use crate::core::{media_clock::MediaClock, Result, RuntimeContext, StreamError};
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;

// ============================================================================
// ASYNC CHANNEL MESSAGE
// ============================================================================

/// Message sent from the processor thread to the async WHIP client task.
enum WhipClientMessage {
    VideoSample(webrtc::media::Sample),
    AudioSample(webrtc::media::Sample),
}

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.webrtc_whip")]
pub struct WebRtcWhipProcessor {
    // RuntimeContext for tokio handle
    ctx: Option<RuntimeContext>,

    // Session state
    session_started: bool,

    // WHIP client (owns WebRTC session, moved to async task after connect)
    whip_client: Option<WhipClient>,

    // Async channel sender to the background WHIP client task
    whip_client_message_sender: Option<tokio_mpsc::Sender<WhipClientMessage>>,

    // Peer connection reference for stats (cloned before client moves to async task)
    peer_connection_for_stats: Option<Arc<webrtc::peer_connection::RTCPeerConnection>>,

    // Stats tracking
    last_stats_time_ns: i64,
}

impl crate::core::ReactiveProcessor for WebRtcWhipProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.ctx = Some(ctx);

        // Convert generated config to WhipConfig
        let whip_config = WhipConfig {
            endpoint_url: self.config.whip.endpoint_url.clone(),
            auth_token: self.config.whip.auth_token.clone(),
            timeout_ms: self.config.whip.timeout_ms as u64,
        };
        let whip_client = WhipClient::new(whip_config)?;
        self.whip_client = Some(whip_client);

        tracing::info!("[WebRtcWhip] Initialized (will connect on first frame)");
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[WebRtcWhip] Shutting down");

        // Drop the channel sender to signal the async task to terminate.
        self.whip_client_message_sender.take();
        self.peer_connection_for_stats.take();

        // If the client was never moved to the async task, terminate directly.
        if let Some(mut client) = self.whip_client.take() {
            if let Err(e) = client.terminate().await {
                tracing::warn!("[WebRtcWhip] Error terminating WHIP session: {}", e);
            }
        }

        tracing::info!("[WebRtcWhip] Shutdown complete");
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Read pre-encoded video and audio
        let encoded_video: Option<Encodedvideoframe> = if self.inputs.has_data("encoded_video_in") {
            self.inputs.read("encoded_video_in").ok()
        } else {
            None
        };

        let encoded_audio: Option<Encodedaudioframe> = if self.inputs.has_data("encoded_audio_in") {
            self.inputs.read("encoded_audio_in").ok()
        } else {
            None
        };

        // Start session on first frame
        if !self.session_started && (encoded_video.is_some() || encoded_audio.is_some()) {
            tracing::info!("[WebRtcWhip] Starting session — received first encoded frame");
            self.start_session()?;
            self.session_started = true;
        }

        // Send video if available
        if let Some(encoded) = encoded_video {
            self.send_encoded_video(&encoded)?;
        }

        // Send audio if available
        if let Some(encoded) = encoded_audio {
            self.send_encoded_audio(&encoded)?;
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
        let tokio_handle = self.ctx.as_ref().unwrap().tokio_handle().clone();
        let client = self
            .whip_client
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("WhipClient not initialized".into()))?;

        tokio_handle.block_on(
            client.connect(self.config.video.bitrate_bps, self.config.audio.bitrate_bps),
        )?;

        // Clone the peer connection Arc for stats before moving client to async task
        self.peer_connection_for_stats = client.peer_connection.clone();

        // Move client into async task, communicate via bounded channel
        let mut client = self
            .whip_client
            .take()
            .ok_or_else(|| StreamError::Runtime("WhipClient not initialized".into()))?;

        let (sender, mut receiver) = tokio_mpsc::channel::<WhipClientMessage>(8);

        tokio_handle.spawn(async move {
            while let Some(msg) = receiver.recv().await {
                match msg {
                    WhipClientMessage::VideoSample(sample) => {
                        if let Err(e) = client.write_video_sample(sample).await {
                            tracing::error!("[WebRtcWhip] Async video write failed: {}", e);
                        }
                    }
                    WhipClientMessage::AudioSample(sample) => {
                        if let Err(e) = client.write_audio_sample(sample).await {
                            tracing::error!("[WebRtcWhip] Async audio write failed: {}", e);
                        }
                    }
                }
            }
            tracing::info!("[WebRtcWhip] Channel closed, terminating WHIP client");
            if let Err(e) = client.terminate().await {
                tracing::warn!("[WebRtcWhip] Error terminating WHIP client: {}", e);
            }
        });

        self.whip_client_message_sender = Some(sender);
        self.last_stats_time_ns = MediaClock::now().as_nanos() as i64;

        tracing::info!("[WebRtcWhip] Session started");
        Ok(())
    }

    /// Send pre-encoded video frame via WebRTC.
    fn send_encoded_video(&mut self, encoded: &Encodedvideoframe) -> Result<()> {
        if !self.session_started || encoded.data.is_empty() {
            return Ok(());
        }

        // Convert encoded frame NAL units to RTP samples
        let fps = self.config.video.fps;
        let samples = convert_video_to_samples(encoded, fps)?;

        let sender = self.whip_client_message_sender.as_ref().ok_or_else(|| {
            StreamError::Runtime("WHIP client channel not initialized".into())
        })?;

        for sample in samples {
            match sender.try_send(WhipClientMessage::VideoSample(sample)) {
                Ok(()) => {}
                Err(tokio_mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("[WebRtcWhip] Video channel full, dropping frame");
                }
                Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                    return Err(StreamError::Runtime("WHIP client channel closed".into()));
                }
            }
        }

        Ok(())
    }

    /// Send pre-encoded audio frame via WebRTC.
    fn send_encoded_audio(&mut self, encoded: &Encodedaudioframe) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let sample = convert_audio_to_sample(encoded, self.config.audio.sample_rate)?;

        let sender = self.whip_client_message_sender.as_ref().ok_or_else(|| {
            StreamError::Runtime("WHIP client channel not initialized".into())
        })?;

        match sender.try_send(WhipClientMessage::AudioSample(sample)) {
            Ok(()) => {}
            Err(tokio_mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("[WebRtcWhip] Audio channel full, dropping frame");
            }
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                return Err(StreamError::Runtime("WHIP client channel closed".into()));
            }
        }

        Ok(())
    }

    fn log_stats(&self) {
        if let (Some(pc), Some(ctx)) = (&self.peer_connection_for_stats, &self.ctx) {
            let tokio_handle = ctx.tokio_handle();
            let stats = tokio_handle.block_on(pc.get_stats());
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
                "[WebRtcWhip Stats] Video: {} packets ({:.2} MB), Audio: {} packets ({:.2} KB)",
                video_packets_sent,
                video_bytes_sent as f64 / 1_000_000.0,
                audio_packets_sent,
                audio_bytes_sent as f64 / 1_000.0
            );
        }
    }
}
