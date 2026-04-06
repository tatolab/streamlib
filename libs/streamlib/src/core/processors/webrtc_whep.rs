// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC WHEP Streaming Processor
//
// Transport-only: receives video and audio via WebRTC WHEP, depacketizes
// RTP, and outputs EncodedVideoFrame / EncodedAudioFrame.
// Decoding is handled by downstream H264BaselineDecoderProcessor / OpusDecoderProcessor.

use crate::_generated_::{Encodedaudioframe, Encodedvideoframe};
use crate::core::media_clock::MediaClock;
use crate::core::streaming::{H264RtpDepacketizer, RtpSample, WhepClient, WhepConfig};
use crate::core::{Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.webrtc_whep")]
pub struct WebRtcWhepProcessor {
    // RuntimeContext for tokio handle
    ctx: Option<RuntimeContext>,

    // WHEP client (owns WebRTC session)
    whep_client: Option<WhepClient>,

    // Audio sample rate from SDP negotiation (needed for EncodedAudioFrame)
    audio_sample_rate: Option<u32>,

    // Shutdown signaling
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for WebRtcWhepProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.ctx = Some(ctx);

        async move {
            // Convert generated config to WhepConfig
            let whep_config = WhepConfig {
                endpoint_url: self.config.whep.endpoint_url.clone(),
                auth_token: self.config.whep.auth_token.clone(),
                timeout_ms: self.config.whep.timeout_ms as u64,
            };

            // Create and connect WHEP client
            let mut whep_client = WhepClient::new(whep_config)?;
            whep_client.connect().await?;

            // Get audio configuration from SDP negotiation
            let (sample_rate, _channels) = whep_client.audio_config();
            self.audio_sample_rate = sample_rate;

            self.whep_client = Some(whep_client);

            tracing::info!(
                "[WebRtcWhep] Connected to {}",
                self.config.whep.endpoint_url
            );
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[WebRtcWhep] Shutting down");

        if let Some(mut client) = self.whep_client.take() {
            if let Err(e) = client.terminate().await {
                tracing::warn!("[WebRtcWhep] Error terminating WHEP session: {}", e);
            }
        }

        tracing::info!("[WebRtcWhep] Shutdown complete");
        Ok(())
    }

    fn on_pause(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_resume(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?
            .clone();

        // Take ownership of receivers from WHEP client
        let client = self
            .whep_client
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("WhepClient not initialized".into()))?;

        let video_rx = client
            .take_video_rx()
            .ok_or_else(|| StreamError::Runtime("Video receiver not available".into()))?;
        let audio_rx = client
            .take_audio_rx()
            .ok_or_else(|| StreamError::Runtime("Audio receiver not available".into()))?;

        let outputs = self.outputs.clone();
        let audio_sample_rate = self.audio_sample_rate.unwrap_or(48000);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        ctx.tokio_handle().spawn(async move {
            run_whep_receive_loop(video_rx, audio_rx, outputs, audio_sample_rate, shutdown_rx).await;
        });

        tracing::info!("[WebRtcWhep] Started async receive loop");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        tracing::info!("[WebRtcWhep] Stopped");
        Ok(())
    }
}

// ============================================================================
// ASYNC RECEIVE LOOP
// ============================================================================

/// Async loop that receives RTP samples and outputs encoded frames.
async fn run_whep_receive_loop(
    mut video_rx: mpsc::Receiver<RtpSample>,
    mut audio_rx: mpsc::Receiver<RtpSample>,
    outputs: Arc<OutputWriter>,
    audio_sample_rate: u32,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let mut h264_depacketizer = H264RtpDepacketizer::new();
    let mut video_seq: u16 = 0;
    let mut video_frame_count: u64 = 0;
    let mut audio_frame_count: u64 = 0;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("[WebRtcWhep] Shutdown signal received");
                break;
            }
            Some(sample) = video_rx.recv() => {
                // Depacketize RTP to H.264 NAL units
                let seq = video_seq;
                video_seq = video_seq.wrapping_add(1);

                let nals = match h264_depacketizer.process_packet(
                    sample.payload, sample.timestamp, seq
                ) {
                    Ok(nals) => nals,
                    Err(e) => {
                        tracing::warn!("[WebRtcWhep] H.264 depacketization failed: {}", e);
                        continue;
                    }
                };

                // Assemble NAL units into Annex B format for EncodedVideoFrame
                if !nals.is_empty() {
                    let mut annex_b_data = Vec::new();
                    let mut is_keyframe = false;

                    for nal in &nals {
                        if !nal.is_empty() {
                            let nal_type = nal[0] & 0x1F;
                            if nal_type == 5 { is_keyframe = true; } // IDR
                            annex_b_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                            annex_b_data.extend_from_slice(nal);
                        }
                    }

                    if !annex_b_data.is_empty() {
                        let timestamp_ns = MediaClock::now().as_nanos() as i64;
                        video_frame_count += 1;

                        let encoded = Encodedvideoframe {
                            data: annex_b_data,
                            timestamp_ns: timestamp_ns.to_string(),
                            is_keyframe,
                            frame_number: video_frame_count.to_string(),
                        };

                        if let Err(e) = outputs.write("encoded_video_out", &encoded) {
                            tracing::warn!("[WebRtcWhep] Failed to write encoded video: {}", e);
                        }

                        if video_frame_count == 1 {
                            tracing::info!("[WebRtcWhep] First encoded video frame output");
                        } else if video_frame_count % 100 == 0 {
                            tracing::info!(frames = video_frame_count, "[WebRtcWhep] Video progress");
                        }
                    }
                }
            }
            Some(sample) = audio_rx.recv() => {
                // Output raw Opus packet as EncodedAudioFrame
                let timestamp_ns = MediaClock::now().as_nanos() as i64;
                audio_frame_count += 1;

                // Opus frame at 48kHz/20ms = 960 samples
                let sample_count = (audio_sample_rate as f64 * 0.02) as u32;

                let encoded = Encodedaudioframe {
                    data: sample.payload.to_vec(),
                    timestamp_ns: timestamp_ns.to_string(),
                    sample_count,
                };

                if let Err(e) = outputs.write("encoded_audio_out", &encoded) {
                    tracing::warn!("[WebRtcWhep] Failed to write encoded audio: {}", e);
                }

                if audio_frame_count == 1 {
                    tracing::info!("[WebRtcWhep] First encoded audio frame output");
                } else if audio_frame_count % 500 == 0 {
                    tracing::info!(frames = audio_frame_count, "[WebRtcWhep] Audio progress");
                }
            }
            else => {
                tracing::info!("[WebRtcWhep] All channels closed");
                break;
            }
        }
    }
}
