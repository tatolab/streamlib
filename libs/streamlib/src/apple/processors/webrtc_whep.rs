// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC WHEP Streaming Processor
//
// This processor receives video and audio from a WHEP endpoint.
// It integrates:
// - H.264 decoding via VideoToolbox
// - Opus audio decoding
// - WHEP signaling and WebRTC session via WhepClient

use crate::apple::videotoolbox::VideoToolboxDecoder;
use crate::apple::webrtc::{WhepClient, WhepConfig};
use crate::core::streaming::{H264RtpDepacketizer, OpusDecoder};
use crate::core::{
    media_clock::MediaClock, AudioFrame, GpuContext, LinkOutput, Result, RuntimeContext,
    StreamError, VideoFrame,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

// ============================================================================
// CONFIGURATION
// ============================================================================

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebRtcWhepConfig {
    pub whep: WhepConfig,
}

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor(
    execution = Continuous,
    description = "Receives video and audio from WHEP endpoint (WebRTC egress)"
)]
pub struct WebRtcWhepProcessor {
    #[crate::output(description = "Output video frames (decoded H.264)")]
    video_out: LinkOutput<VideoFrame>,

    #[crate::output(description = "Output audio frames (decoded Opus, stereo)")]
    audio_out: LinkOutput<AudioFrame>,

    #[crate::config]
    config: WebRtcWhepConfig,

    // RuntimeContext for tokio handle
    ctx: Option<RuntimeContext>,

    // GPU context for video decoder
    gpu_context: Option<GpuContext>,

    // Session state
    session_started: bool,

    // WHEP client (owns WebRTC session)
    whep_client: Option<WhepClient>,

    // Audio configuration from SDP negotiation
    audio_sample_rate: Option<u32>,
    audio_channels: Option<usize>,

    // Decoders
    video_decoder: Option<VideoToolboxDecoder>,
    audio_decoder: Option<OpusDecoder>,

    // RTP depacketization
    h264_depacketizer: H264RtpDepacketizer,
    video_seq_counter: u16,

    // SPS/PPS tracking for decoder initialization
    sps_nal: Option<Bytes>,
    pps_nal: Option<Bytes>,

    // Frame counters
    video_frame_count: u64,
    audio_frame_count: u64,
}

impl crate::core::ContinuousProcessor for WebRtcWhepProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Create WHEP client
        let whep_client = WhepClient::new(self.config.whep.clone())?;
        self.whep_client = Some(whep_client);

        tracing::info!("[WebRtcWhepProcessor] Initialized (will connect on first process)");
        Ok(())
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

    fn process(&mut self) -> Result<()> {
        // Lazy session initialization on first process() call
        if !self.session_started {
            self.start_session()?;
            self.session_started = true;
        }

        // Process any pending video samples
        self.process_video_samples()?;

        // Process any pending audio samples
        self.process_audio_samples()?;

        // Small sleep to avoid busy-waiting
        std::thread::sleep(std::time::Duration::from_micros(10));

        Ok(())
    }
}

impl WebRtcWhepProcessor::Processor {
    /// Starts the WebRTC WHEP session.
    fn start_session(&mut self) -> Result<()> {
        tracing::info!(
            "[WebRtcWhepProcessor] Starting WHEP session to {}",
            self.config.whep.endpoint_url
        );

        let tokio_handle = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?
            .tokio_handle()
            .clone();

        let client = self
            .whep_client
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("WhepClient not initialized".into()))?;

        // Connect to WHEP endpoint
        tokio_handle.block_on(client.connect())?;

        // Get audio configuration from SDP negotiation
        let (sample_rate, channels) = client.audio_config();
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

        tracing::info!("[WebRtcWhepProcessor] WHEP session started successfully");
        Ok(())
    }

    /// Processes video samples from the WHEP client.
    fn process_video_samples(&mut self) -> Result<()> {
        // Collect all pending NAL units first (release borrow on client)
        let nals_to_process: Vec<Bytes> = {
            let client = match &mut self.whep_client {
                Some(c) => c,
                None => return Ok(()),
            };

            let mut nals = Vec::new();

            // Drain all available video samples
            while let Some(sample) = client.try_recv_video() {
                // Depacketize RTP payload to NAL units
                let seq_num = self.video_seq_counter;
                self.video_seq_counter = self.video_seq_counter.wrapping_add(1);

                match self.h264_depacketizer.process_packet(
                    sample.payload,
                    sample.timestamp,
                    seq_num,
                ) {
                    Ok(depacketized_nals) => {
                        nals.extend(depacketized_nals);
                    }
                    Err(e) => {
                        tracing::warn!("[WebRtcWhepProcessor] H.264 depacketization failed: {}", e);
                    }
                }
            }

            nals
        };

        // Now process collected NALs (borrow on client released)
        for nal in nals_to_process {
            self.process_nal_unit(nal)?;
        }

        Ok(())
    }

    /// Processes a single NAL unit.
    fn process_nal_unit(&mut self, nal: Bytes) -> Result<()> {
        if nal.is_empty() {
            return Ok(());
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
                self.initialize_video_decoder(sps.clone(), pps.clone())?;
            }
            return Ok(());
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
                self.initialize_video_decoder(sps.clone(), pps.clone())?;
            }
            return Ok(());
        }

        // IDR (5) or Non-IDR (1) - decode frame
        if nal_type == 1 || nal_type == 5 {
            if let Some(decoder) = &mut self.video_decoder {
                let timestamp_ns = MediaClock::now().as_nanos() as i64;

                // Convert raw NAL to Annex B format (add start code)
                let mut annex_b = Vec::with_capacity(4 + nal.len());
                annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                annex_b.extend_from_slice(&nal);
                let nal_data = Bytes::from(annex_b);

                match decoder.decode(&nal_data, timestamp_ns) {
                    Ok(Some(video_frame)) => {
                        self.video_out.write(video_frame);
                        self.video_frame_count += 1;

                        if self.video_frame_count.is_multiple_of(30) {
                            tracing::info!(
                                "[WebRtcWhepProcessor] Decoded video frame #{}",
                                self.video_frame_count
                            );
                        }
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

        Ok(())
    }

    /// Initializes the video decoder with SPS and PPS.
    fn initialize_video_decoder(&mut self, sps: Bytes, pps: Bytes) -> Result<()> {
        if self.video_decoder.is_some() {
            return Ok(()); // Already initialized
        }

        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not available".into()))?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GpuContext not available".into()))?;

        tracing::info!(
            "[WebRtcWhepProcessor] Initializing VideoToolbox decoder with SPS ({} bytes) and PPS ({} bytes)",
            sps.len(),
            pps.len()
        );

        let mut decoder = VideoToolboxDecoder::new(Default::default(), Some(gpu_ctx.clone()), ctx)?;
        decoder.update_format(&sps, &pps)?;

        self.video_decoder = Some(decoder);
        tracing::info!("[WebRtcWhepProcessor] VideoToolbox decoder initialized");

        Ok(())
    }

    /// Processes audio samples from the WHEP client.
    fn process_audio_samples(&mut self) -> Result<()> {
        let client = match &mut self.whep_client {
            Some(c) => c,
            None => return Ok(()),
        };

        let decoder = match &mut self.audio_decoder {
            Some(d) => d,
            None => return Ok(()),
        };

        // Drain all available audio samples
        while let Some(sample) = client.try_recv_audio() {
            let timestamp_ns = MediaClock::now().as_nanos() as i64;

            match decoder.decode_to_audio_frame(&sample.payload, timestamp_ns) {
                Ok(audio_frame) => {
                    self.audio_out.write(audio_frame);
                    self.audio_frame_count += 1;

                    if self.audio_frame_count == 1 {
                        tracing::info!("[WebRtcWhepProcessor] First audio frame decoded");
                    } else if self.audio_frame_count.is_multiple_of(50) {
                        tracing::info!(
                            "[WebRtcWhepProcessor] Decoded audio frame #{}",
                            self.audio_frame_count
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("[WebRtcWhepProcessor] Audio decode error: {}", e);
                }
            }
        }

        Ok(())
    }
}
