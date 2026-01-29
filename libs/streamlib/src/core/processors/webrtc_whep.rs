// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC WHEP Streaming Processor
//
// This processor receives video and audio from a WHEP endpoint.
// It integrates:
// - H.264 decoding via platform-specific decoder (VideoToolbox on macOS, FFmpeg on Linux)
// - Opus audio decoding
// - WHEP signaling and WebRTC session via WhepClient

use crate::_generated_::{Audioframe, Videoframe};
use crate::core::codec::VideoDecoder;
use crate::core::streaming::{H264RtpDepacketizer, OpusDecoder, RtpSample, WhepClient, WhepConfig};
use crate::core::{media_clock::MediaClock, GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use bytes::Bytes;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("src/core/processors/webrtc_whep.yaml")]
pub struct WebRtcWhepProcessor {
    // RuntimeContext for tokio handle
    ctx: Option<RuntimeContext>,

    // WHEP client (owns WebRTC session)
    whep_client: Option<WhepClient>,

    // Audio configuration from SDP negotiation
    audio_sample_rate: Option<u32>,
    audio_channels: Option<usize>,

    // Decoders (moved to async task on start)
    video_decoder: Option<VideoDecoder>,
    audio_decoder: Option<OpusDecoder>,

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
            let (sample_rate, channels) = whep_client.audio_config();
            self.audio_sample_rate = sample_rate;
            self.audio_channels = channels;

            // Initialize audio decoder with negotiated parameters
            if let (Some(rate), Some(ch)) = (sample_rate, channels) {
                tracing::info!(
                    "[WebRtcWhepProcessor] Initializing Opus decoder: {}Hz, {} channels",
                    rate,
                    ch
                );
                self.audio_decoder = Some(OpusDecoder::new(rate, ch)?);
            }

            self.whep_client = Some(whep_client);

            tracing::info!(
                "[WebRtcWhepProcessor] Connected to {}",
                self.config.whep.endpoint_url
            );
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[WebRtcWhepProcessor] Shutting down");

        // Terminate WHEP session
        if let Some(mut client) = self.whep_client.take() {
            if let Err(e) = client.terminate().await {
                tracing::warn!(
                    "[WebRtcWhepProcessor] Error terminating WHEP session: {}",
                    e
                );
            }
        }

        tracing::info!("[WebRtcWhepProcessor] Shutdown complete");
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

        // Clone output writer for the async task
        let outputs = self.outputs.clone();

        // Take audio decoder (video decoder initialized lazily on SPS/PPS)
        let audio_decoder = self.audio_decoder.take();

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        // Clone ctx for the async task (we need the handle to spawn, and ctx inside the task)
        let ctx_for_task = ctx.clone();

        // Get GpuContext for buffer pooling in video decode
        let gpu_context = ctx.gpu.clone();

        // Spawn the async receive loop
        ctx.tokio_handle().spawn(async move {
            run_receive_loop(
                video_rx,
                audio_rx,
                outputs,
                audio_decoder,
                ctx_for_task,
                gpu_context,
                shutdown_rx,
            )
            .await;
        });

        tracing::info!("[WebRtcWhepProcessor] Started async receive loop");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Signal shutdown to the async task
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()); // Ignore error if receiver already dropped
        }
        tracing::info!("[WebRtcWhepProcessor] Stopped");
        Ok(())
    }
}

// ============================================================================
// ASYNC RECEIVE LOOP
// ============================================================================

/// Async loop that receives RTP samples and decodes them.
async fn run_receive_loop(
    mut video_rx: mpsc::Receiver<RtpSample>,
    mut audio_rx: mpsc::Receiver<RtpSample>,
    outputs: Arc<OutputWriter>,
    audio_decoder: Option<OpusDecoder>,
    ctx: RuntimeContext,
    gpu_context: GpuContext,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let mut video_state = VideoDecodeState::new(ctx, gpu_context);
    let mut audio_state = AudioDecodeState::new(audio_decoder);

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("[WebRtcWhepProcessor] Shutdown signal received");
                break;
            }
            Some(sample) = video_rx.recv() => {
                if let Some(frame) = video_state.process_sample(sample) {
                    if let Err(e) = outputs.write("video", &frame) {
                        tracing::warn!("[WebRtcWhepProcessor] Failed to write video frame: {}", e);
                    }
                }
            }
            Some(sample) = audio_rx.recv() => {
                if let Some(frame) = audio_state.process_sample(sample) {
                    if let Err(e) = outputs.write("audio", &frame) {
                        tracing::warn!("[WebRtcWhepProcessor] Failed to write audio frame: {}", e);
                    }
                }
            }
            else => {
                // Both channels closed
                tracing::info!("[WebRtcWhepProcessor] All channels closed");
                break;
            }
        }
    }
}

// ============================================================================
// VIDEO DECODE STATE
// ============================================================================

/// State for video decoding in the async task.
struct VideoDecodeState {
    ctx: RuntimeContext,
    gpu_context: GpuContext,
    decoder: Option<VideoDecoder>,
    h264_depacketizer: H264RtpDepacketizer,
    video_seq_counter: u16,
    sps_nal: Option<Bytes>,
    pps_nal: Option<Bytes>,
    frame_count: u64,
}

impl VideoDecodeState {
    fn new(ctx: RuntimeContext, gpu_context: GpuContext) -> Self {
        Self {
            ctx,
            gpu_context,
            decoder: None,
            h264_depacketizer: H264RtpDepacketizer::new(),
            video_seq_counter: 0,
            sps_nal: None,
            pps_nal: None,
            frame_count: 0,
        }
    }

    fn process_sample(&mut self, sample: RtpSample) -> Option<Videoframe> {
        // Depacketize RTP payload to NAL units
        let seq_num = self.video_seq_counter;
        self.video_seq_counter = self.video_seq_counter.wrapping_add(1);

        let nals =
            match self
                .h264_depacketizer
                .process_packet(sample.payload, sample.timestamp, seq_num)
            {
                Ok(nals) => nals,
                Err(e) => {
                    tracing::warn!("[WebRtcWhepProcessor] H.264 depacketization failed: {}", e);
                    return None;
                }
            };

        // Process each NAL unit, return the last decoded frame (if any)
        let mut result_frame = None;
        for nal in nals {
            if let Some(frame) = self.process_nal_unit(nal) {
                result_frame = Some(frame);
            }
        }
        result_frame
    }

    fn process_nal_unit(&mut self, nal: Bytes) -> Option<Videoframe> {
        if nal.is_empty() {
            return None;
        }

        let nal_type = nal[0] & 0x1F;

        // SPS (7) - Sequence Parameter Set
        if nal_type == 7 {
            tracing::info!(
                "[WebRtcWhepProcessor] Received SPS NAL ({} bytes)",
                nal.len()
            );
            self.sps_nal = Some(nal.clone());

            // Try to initialize decoder if we have both SPS and PPS
            if let (Some(sps), Some(pps)) = (&self.sps_nal, &self.pps_nal) {
                self.initialize_decoder(sps.clone(), pps.clone());
            }
            return None;
        }

        // PPS (8) - Picture Parameter Set
        if nal_type == 8 {
            tracing::info!(
                "[WebRtcWhepProcessor] Received PPS NAL ({} bytes)",
                nal.len()
            );
            self.pps_nal = Some(nal.clone());

            // Try to initialize decoder if we have both SPS and PPS
            if let (Some(sps), Some(pps)) = (&self.sps_nal, &self.pps_nal) {
                self.initialize_decoder(sps.clone(), pps.clone());
            }
            return None;
        }

        // IDR (5) or Non-IDR (1) - decode frame
        if nal_type == 1 || nal_type == 5 {
            if let Some(decoder) = &mut self.decoder {
                let timestamp_ns = MediaClock::now().as_nanos() as i64;

                // Convert raw NAL to Annex B format (add start code)
                let mut annex_b = Vec::with_capacity(4 + nal.len());
                annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                annex_b.extend_from_slice(&nal);
                let nal_data = Bytes::from(annex_b);

                // Decoder returns Videoframe directly (handles buffer pooling internally)
                match decoder.decode(&nal_data, timestamp_ns, &self.gpu_context) {
                    Ok(Some(ipc_frame)) => {
                        self.frame_count += 1;

                        if self.frame_count.is_multiple_of(30) {
                            tracing::info!(
                                "[WebRtcWhepProcessor] Decoded video frame #{}",
                                self.frame_count
                            );
                        }
                        return Some(ipc_frame);
                    }
                    Ok(None) => {
                        // Decoder needs more data
                    }
                    Err(e) => {
                        tracing::warn!("[WebRtcWhepProcessor] Video decode error: {}", e);
                    }
                }
            } else {
                tracing::debug!(
                    "[WebRtcWhepProcessor] Received NAL type {} but decoder not ready",
                    nal_type
                );
            }
        }

        None
    }

    fn initialize_decoder(&mut self, sps: Bytes, pps: Bytes) {
        if self.decoder.is_some() {
            return; // Already initialized
        }

        tracing::info!(
            "[WebRtcWhepProcessor] Initializing video decoder with SPS ({} bytes) and PPS ({} bytes)",
            sps.len(),
            pps.len()
        );

        match VideoDecoder::new(Default::default(), &self.ctx) {
            Ok(mut decoder) => {
                if let Err(e) = decoder.update_format(&sps, &pps) {
                    tracing::error!(
                        "[WebRtcWhepProcessor] Failed to update decoder format: {}",
                        e
                    );
                    return;
                }
                self.decoder = Some(decoder);
                tracing::info!("[WebRtcWhepProcessor] Video decoder initialized");
            }
            Err(e) => {
                tracing::error!(
                    "[WebRtcWhepProcessor] Failed to create video decoder: {}",
                    e
                );
            }
        }
    }
}

// ============================================================================
// AUDIO DECODE STATE
// ============================================================================

/// State for audio decoding in the async task.
struct AudioDecodeState {
    decoder: Option<OpusDecoder>,
    frame_count: u64,
}

impl AudioDecodeState {
    fn new(decoder: Option<OpusDecoder>) -> Self {
        Self {
            decoder,
            frame_count: 0,
        }
    }

    fn process_sample(&mut self, sample: RtpSample) -> Option<Audioframe> {
        let decoder = self.decoder.as_mut()?;
        let timestamp_ns = MediaClock::now().as_nanos() as i64;

        match decoder.decode_to_audio_frame(&sample.payload, timestamp_ns) {
            Ok(audio_frame) => {
                self.frame_count += 1;

                if self.frame_count == 1 {
                    tracing::info!("[WebRtcWhepProcessor] First audio frame decoded");
                } else if self.frame_count.is_multiple_of(50) {
                    tracing::info!(
                        "[WebRtcWhepProcessor] Decoded audio frame #{}",
                        self.frame_count
                    );
                }
                Some(audio_frame)
            }
            Err(e) => {
                tracing::warn!("[WebRtcWhepProcessor] Audio decode error: {}", e);
                None
            }
        }
    }
}
