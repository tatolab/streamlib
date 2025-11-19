// WebRTC WHEP Streaming Processor
//
// This file contains the WebRTC WHEP processor that integrates:
// - H.264 decoding via VideoToolbox
// - Opus audio decoding
// - WHEP signaling (IETF draft)
// - WebRTC session management (webrtc-rs)
// - RTP depacketization (FU-A reassembly for H.264)

use crate::core::{
    VideoFrame, AudioFrame, StreamError, Result,
    media_clock::MediaClock, GpuContext,
    StreamOutput, RuntimeContext,
};
use streamlib_macros::StreamProcessor;
use crate::apple::videotoolbox::VideoToolboxDecoder;
use crate::apple::webrtc::{WhepClient, WhepConfig, WebRtcSession, SampleCallback};
use crate::core::streaming::{OpusDecoder, H264RtpDepacketizer};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use bytes::Bytes;

// ============================================================================
// MAIN WEBRTC WHEP PROCESSOR
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WebRtcWhepConfig {
    pub whep: WhepConfig,
}

impl Default for WebRtcWhepConfig {
    fn default() -> Self {
        Self {
            whep: WhepConfig {
                endpoint_url: String::new(),
                auth_token: None,
                timeout_ms: 10000,
            },
        }
    }
}

/// WebRTC WHEP processor - receives H.264 video and Opus audio from WHEP endpoint
///
/// This is a SOURCE processor (no inputs, only outputs).
/// It connects to a WHEP server, receives RTP packets, depacketizes them,
/// decodes them, and outputs VideoFrame/AudioFrame.
#[derive(StreamProcessor)]
#[processor(
    mode = Pull,
    description = "Receives video and audio from WHEP endpoint (WebRTC egress)"
)]
pub struct WebRtcWhepProcessor {
    #[output(description = "Output video frames (decoded H.264)")]
    video_out: Arc<StreamOutput<VideoFrame>>,

    #[output(description = "Output audio frames (decoded Opus, stereo)")]
    audio_out: Arc<StreamOutput<AudioFrame<2>>>,

    #[config]
    config: WebRtcWhepConfig,

    // RuntimeContext for main thread dispatch
    ctx: Option<RuntimeContext>,

    // Session state
    session_started: bool,
    gpu_context: Option<GpuContext>,

    // Decoders
    video_decoder: Option<VideoToolboxDecoder>,
    audio_decoder: Option<OpusDecoder>,

    // RTP depacketization
    h264_depacketizer: Option<H264RtpDepacketizer>,

    // WHEP and WebRTC
    whip_client: Option<Arc<Mutex<WhepClient>>>,
    webrtc_session: Option<WebRtcSession>,

    // Frame counters
    video_frame_count: u64,
    audio_frame_count: u64,

    // Shared buffers for RTP → NAL → Decoded frame pipeline
    // These are written by WebRTC callbacks, read by process()
    pending_video_nals: Arc<Mutex<Vec<Bytes>>>,
    pending_audio_packets: Arc<Mutex<Vec<Bytes>>>,
}

impl WebRtcWhepProcessor {
    /// Called by StreamProcessor macro during setup phase.
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Initialize decoders
        self.audio_decoder = Some(OpusDecoder::new(48000, 2)?);
        self.h264_depacketizer = Some(H264RtpDepacketizer::new());

        // VideoToolboxDecoder will be created lazily when we receive SPS/PPS

        tracing::info!("[WebRtcWhepProcessor] Initialized (decoders ready, will create session on first process() call)");
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Lazy session initialization on first process() call
        if !self.session_started {
            self.start_session()?;
            self.session_started = true;
        }

        // Process pending video NAL units
        self.process_video_nals()?;

        // Process pending audio packets
        self.process_audio_packets()?;

        Ok(())
    }

    fn start_session(&mut self) -> Result<()> {
        let ctx = self.ctx.as_ref().ok_or_else(|| {
            StreamError::Configuration("RuntimeContext not available".into())
        })?;

        tracing::info!("[WebRtcWhepProcessor] Starting WHEP session to {}", self.config.whep.endpoint_url);

        // 1. Create WHEP client
        let mut whep_client = WhepClient::new(self.config.whep.clone())?;

        // 2. Create WebRTC session in receive mode
        //    This will generate SDP offer with recvonly transceivers for H.264 and Opus
        let pending_nals = Arc::clone(&self.pending_video_nals);
        let pending_audio = Arc::clone(&self.pending_audio_packets);
        let h264_depacketizer = Arc::new(Mutex::new(H264RtpDepacketizer::new()));

        // Note: SampleCallback signature is: Fn(String, Bytes, u32) -> (media_type, payload, timestamp)
        // We don't get sequence number from the callback, only timestamp
        let sample_callback: SampleCallback = Arc::new(move |media_type, rtp_payload, timestamp| {
            match media_type.as_str() {
                "video" => {
                    // For H.264 RTP depacketization, we need sequence numbers
                    // Since SampleCallback doesn't provide them, we'll track them internally
                    // or use timestamp-based reassembly

                    // For now, store raw RTP payload and process later
                    // TODO: Extract seq num from RTP header if available
                    let mut depacketizer = h264_depacketizer.lock().unwrap();

                    // Fake sequence number (just incrementing) - not ideal but workable for ordered delivery
                    static FAKE_SEQ: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
                    let seq_num = FAKE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    match depacketizer.process_packet(rtp_payload, timestamp, seq_num) {
                        Ok(nals) => {
                            if !nals.is_empty() {
                                let mut pending = pending_nals.lock().unwrap();
                                pending.extend(nals);
                                tracing::trace!("[WHEP RTP] Depacketized {} NAL units (timestamp={})", pending.len(), timestamp);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[WHEP RTP] H.264 depacketization failed: {}", e);
                        }
                    }
                }
                "audio" => {
                    // Opus packets are already complete in RTP payload
                    let mut pending = pending_audio.lock().unwrap();
                    pending.push(rtp_payload);
                    tracing::trace!("[WHEP RTP] Received Opus packet (timestamp={})", timestamp);
                }
                _ => {
                    tracing::debug!("[WHEP RTP] Unknown media type: {}", media_type);
                }
            }
        });

        let webrtc_session = WebRtcSession::new_receive(sample_callback, ctx)?;

        // 3. Get SDP offer from session
        let sdp_offer = webrtc_session.get_local_sdp()?;
        tracing::debug!("[WebRtcWhepProcessor] Generated SDP offer:\n{}", sdp_offer);

        // 4. POST offer to WHEP server, receive answer/counter-offer
        let (sdp_answer, is_counter_offer) = whep_client.post_offer(&sdp_offer)?;

        if is_counter_offer {
            tracing::warn!("[WebRtcWhepProcessor] Received counter-offer (406) - client should accept server's proposal");
            // TODO: Handle counter-offer by creating new offer matching server's requirements
            return Err(StreamError::Runtime("WHEP counter-offer not yet supported".into()));
        }

        tracing::debug!("[WebRtcWhepProcessor] Received SDP answer:\n{}", sdp_answer);

        // 5. Set remote SDP answer
        webrtc_session.set_remote_sdp(&sdp_answer)?;

        // 6. Collect local ICE candidates and send to server
        let ice_candidates = webrtc_session.gather_ice_candidates()?;
        tracing::info!("[WebRtcWhepProcessor] Gathered {} ICE candidates", ice_candidates.len());

        for candidate in ice_candidates {
            whep_client.queue_ice_candidate(candidate);
        }

        whep_client.send_ice_candidates()?;

        // 7. Store session and client
        self.whip_client = Some(Arc::new(Mutex::new(whep_client)));
        self.webrtc_session = Some(webrtc_session);

        tracing::info!("[WebRtcWhepProcessor] ✅ WHEP session started successfully");
        Ok(())
    }

    fn process_video_nals(&mut self) -> Result<()> {
        // Drain pending NAL units
        let nals = {
            let mut pending = self.pending_video_nals.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        if nals.is_empty() {
            return Ok(());
        }

        tracing::debug!("[WebRtcWhepProcessor] Processing {} NAL units", nals.len());

        // Decode NAL units
        for nal in nals {
            // Check NAL type
            let nal_type = nal[0] & 0x1F;

            // SPS (7) or PPS (8) - configure decoder
            if nal_type == 7 || nal_type == 8 {
                tracing::info!("[WebRtcWhepProcessor] Received {} NAL (config)",
                    if nal_type == 7 { "SPS" } else { "PPS" });
                // TODO: Extract width/height from SPS, create VideoToolboxDecoder
                // For now, skip configuration NALs
                continue;
            }

            // IDR (5) or Non-IDR (1) - decode frame
            if nal_type == 1 || nal_type == 5 {
                if let Some(decoder) = &mut self.video_decoder {
                    match decoder.decode(&nal) {
                        Ok(Some(video_frame)) => {
                            self.video_out.write(video_frame);
                            self.video_frame_count += 1;
                            tracing::trace!("[WebRtcWhepProcessor] Decoded video frame #{}", self.video_frame_count);
                        }
                        Ok(None) => {
                            // Decoder buffering frame (needs more data)
                        }
                        Err(e) => {
                            tracing::warn!("[WebRtcWhepProcessor] Video decode error: {}", e);
                        }
                    }
                } else {
                    tracing::warn!("[WebRtcWhepProcessor] Received video NAL but decoder not ready");
                }
            }
        }

        Ok(())
    }

    fn process_audio_packets(&mut self) -> Result<()> {
        // Drain pending audio packets
        let packets = {
            let mut pending = self.pending_audio_packets.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        if packets.is_empty() {
            return Ok(());
        }

        tracing::debug!("[WebRtcWhepProcessor] Processing {} audio packets", packets.len());

        if let Some(decoder) = &mut self.audio_decoder {
            for packet in packets {
                // Generate timestamp for audio frame
                let timestamp_ns = MediaClock::now().as_nanos() as i64;

                match decoder.decode_to_audio_frame(&packet, timestamp_ns) {
                    Ok(audio_frame) => {
                        self.audio_out.write(audio_frame);
                        self.audio_frame_count += 1;
                        tracing::trace!("[WebRtcWhepProcessor] Decoded audio frame #{}", self.audio_frame_count);
                    }
                    Err(e) => {
                        tracing::warn!("[WebRtcWhepProcessor] Audio decode error: {}", e);
                    }
                }
            }
        }

        Ok(())
    }
}

impl Drop for WebRtcWhepProcessor {
    fn drop(&mut self) {
        if let Some(whep_client) = &self.whip_client {
            if let Ok(client) = whep_client.lock() {
                if let Err(e) = client.terminate() {
                    tracing::error!("[WebRtcWhepProcessor] Failed to terminate WHEP session: {}", e);
                }
            }
        }
        tracing::info!("[WebRtcWhepProcessor] Shut down gracefully");
    }
}
