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
use crate::apple::webrtc::{WhepClient, WhepConfig, WebRtcSession};
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
    h264_depacketizer: Arc<Mutex<H264RtpDepacketizer>>,

    // WHEP and WebRTC
    whep_client: Option<Arc<Mutex<WhepClient>>>,
    webrtc_session: Option<WebRtcSession>,

    // Frame counters
    video_frame_count: u64,
    audio_frame_count: u64,

    // Shared buffers for RTP → NAL → Decoded frame pipeline
    // These are written by WebRTC callbacks, read by process()
    pending_video_nals: Arc<Mutex<Vec<Bytes>>>,
    pending_audio_packets: Arc<Mutex<Vec<Bytes>>>,

    // ICE candidate queue (populated by callback, sent via WHEP client)
    pending_ice_candidates: Arc<Mutex<Vec<String>>>,
}

impl WebRtcWhepProcessor {
    /// Called by StreamProcessor macro during setup phase.
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Initialize decoders
        self.audio_decoder = Some(OpusDecoder::new(48000, 2)?);

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

        // 2. Set up callbacks for WebRTC session
        let pending_ice = Arc::clone(&self.pending_ice_candidates);
        let on_ice_candidate = move |candidate: String| {
            tracing::debug!("[WHEP ICE] Gathered candidate: {}", candidate);
            pending_ice.lock().unwrap().push(candidate);
        };

        let pending_nals = Arc::clone(&self.pending_video_nals);
        let pending_audio = Arc::clone(&self.pending_audio_packets);
        let h264_depacketizer = Arc::clone(&self.h264_depacketizer);

        let on_sample = move |media_type: String, rtp_payload: Bytes, timestamp: u32| {
            match media_type.as_str() {
                "video" => {
                    // Depacketize H.264 RTP → NAL units
                    let mut depacketizer = h264_depacketizer.lock().unwrap();

                    // Fake sequence number (monotonic increment) - webrtc-rs doesn't expose seq nums via callback
                    // This works for in-order delivery but won't detect packet loss
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
        };

        // 3. Create WebRTC session in receive mode
        let mut webrtc_session = WebRtcSession::new_receive(on_ice_candidate, on_sample)?;

        // 4. Create SDP offer
        let sdp_offer = webrtc_session.create_offer()?;
        tracing::debug!("[WebRtcWhepProcessor] Generated SDP offer:\n{}", sdp_offer);

        // 5. POST offer to WHEP server, receive answer/counter-offer
        let (sdp_answer, is_counter_offer) = whep_client.post_offer(&sdp_offer)?;

        if is_counter_offer {
            tracing::warn!("[WebRtcWhepProcessor] Received counter-offer (406) - not yet supported");
            return Err(StreamError::Runtime("WHEP counter-offer not yet supported".into()));
        }

        tracing::debug!("[WebRtcWhepProcessor] Received SDP answer:\n{}", sdp_answer);

        // 6. Set remote SDP answer
        webrtc_session.set_remote_answer(&sdp_answer)?;

        // 7. Wait a bit for ICE candidates to be gathered, then send them
        // In a real implementation, we'd send candidates as they're gathered (trickle ICE)
        // For now, we'll just wait and send them in a batch
        std::thread::sleep(std::time::Duration::from_millis(500));

        let candidates = {
            let mut pending = self.pending_ice_candidates.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        tracing::info!("[WebRtcWhepProcessor] Sending {} ICE candidates to WHEP server", candidates.len());

        for candidate in candidates {
            whep_client.queue_ice_candidate(candidate);
        }

        if let Err(e) = whep_client.send_ice_candidates() {
            tracing::warn!("[WebRtcWhepProcessor] Failed to send ICE candidates: {}", e);
        }

        // 8. Store session and client
        self.whep_client = Some(Arc::new(Mutex::new(whep_client)));
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
                    let timestamp_ns = MediaClock::now().as_nanos() as i64;

                    match decoder.decode(&nal, timestamp_ns) {
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
                    tracing::debug!("[WebRtcWhepProcessor] Received video NAL but decoder not ready (waiting for SPS/PPS)");
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

    fn teardown(&mut self) -> Result<()> {
        if let Some(whep_client) = &self.whep_client {
            if let Ok(client) = whep_client.lock() {
                if let Err(e) = client.terminate() {
                    tracing::error!("[WebRtcWhepProcessor] Failed to terminate WHEP session: {}", e);
                }
            }
        }
        tracing::info!("[WebRtcWhepProcessor] Teardown complete");
        Ok(())
    }
}
