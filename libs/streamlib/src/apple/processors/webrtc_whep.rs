// WebRTC WHEP Streaming Processor
//
// This file contains the WebRTC WHEP processor that integrates:
// - H.264 decoding via VideoToolbox
// - Opus audio decoding
// - WHEP signaling (IETF draft)
// - WebRTC session management (webrtc-rs)
// - RTP depacketization (FU-A reassembly for H.264)

use crate::apple::videotoolbox::VideoToolboxDecoder;
use crate::apple::webrtc::{WebRtcSession, WhepClient, WhepConfig};
use crate::core::streaming::{H264RtpDepacketizer, OpusDecoder};
use crate::core::{
    media_clock::MediaClock, AudioFrame, GpuContext, LinkOutput, Result, RuntimeContext,
    StreamError, VideoFrame,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ============================================================================
// H.264 NAL FORMAT DETECTION
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H264NalFormat {
    /// Raw NAL units (just NAL header + data, no start codes or length prefixes)
    /// This is what RTP depacketizers output
    RawNal,
    /// Annex B format (start codes: 0x00 0x00 0x00 0x01 or 0x00 0x00 0x01)
    AnnexB,
    /// AVCC format (4-byte length prefix)
    Avcc,
}

impl H264NalFormat {
    /// Detect format from NAL unit data
    fn detect(data: &[u8]) -> Self {
        if data.len() < 4 {
            // Too small to have start codes or length prefix, assume raw NAL
            return Self::RawNal;
        }

        // Check for Annex B start codes
        if data.len() >= 4 && data[0] == 0x00 && data[1] == 0x00 {
            if data[2] == 0x00 && data[3] == 0x01 {
                return Self::AnnexB; // 4-byte start code
            }
            if data[2] == 0x01 {
                return Self::AnnexB; // 3-byte start code
            }
        }

        // Check for AVCC (first byte should be high 3 bits = 0, NAL unit type in low 5 bits)
        // If first 4 bytes form a reasonable length, it's probably AVCC
        let potential_length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if potential_length > 0 && potential_length < data.len() && potential_length < 1_000_000 {
            // Check if byte after length looks like NAL header
            if data.len() > 4 {
                let nal_header = data[4];
                let forbidden_zero = (nal_header & 0x80) == 0;
                let nal_type = nal_header & 0x1F;
                if forbidden_zero && nal_type > 0 && nal_type <= 24 {
                    return Self::Avcc;
                }
            }
        }

        // Default: assume raw NAL (most common for RTP)
        Self::RawNal
    }
}

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

    // RuntimeContext for main thread dispatch
    ctx: Option<RuntimeContext>,

    // Session state
    session_started: bool,
    gpu_context: Option<GpuContext>,

    // Audio configuration from SDP negotiation
    audio_sample_rate: Option<u32>,
    audio_channels: Option<usize>,

    // Decoders
    video_decoder: Option<VideoToolboxDecoder>,
    audio_decoder: Option<OpusDecoder>,

    // RTP depacketization
    h264_depacketizer: Arc<Mutex<H264RtpDepacketizer>>,

    // SPS/PPS tracking for decoder initialization
    sps_nal: Option<Bytes>,
    pps_nal: Option<Bytes>,

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

impl WebRtcWhepProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Audio decoder will be initialized after SDP negotiation
        // (need to know actual sample rate and channel count from server)

        // VideoToolboxDecoder will be created lazily when we receive SPS/PPS

        tracing::info!(
            "[WebRtcWhepProcessor] Initialized (will create decoders after SDP negotiation)"
        );
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

        // Small sleep to avoid busy-waiting (10μs like Loop mode)
        // The runtime will call process() again immediately in Loop mode
        std::thread::sleep(std::time::Duration::from_micros(10));

        Ok(())
    }

    fn start_session(&mut self) -> Result<()> {
        let _ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not available".into()))?;

        tracing::info!(
            "[WebRtcWhepProcessor] Starting WHEP session to {}",
            self.config.whep.endpoint_url
        );

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
            // Log ALL RTP packets received
            static RTP_PACKET_COUNT: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let count = RTP_PACKET_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            if count.is_multiple_of(30) {
                // Log every 30th packet to avoid spam
                tracing::info!(
                    "[WHEP RTP CALLBACK] Packet #{}: media_type={}, size={}, timestamp={}",
                    count,
                    media_type,
                    rtp_payload.len(),
                    timestamp
                );
            }

            // Media types arrive as MIME types: "video/H264", "audio/opus", etc.
            // Use starts_with() to match the media category
            if media_type.starts_with("video") {
                // Depacketize H.264 RTP → NAL units
                let mut depacketizer = h264_depacketizer.lock().unwrap();

                // Fake sequence number (monotonic increment) - webrtc-rs doesn't expose seq nums via callback
                // This works for in-order delivery but won't detect packet loss
                static FAKE_SEQ: std::sync::atomic::AtomicU16 =
                    std::sync::atomic::AtomicU16::new(0);
                let seq_num = FAKE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                match depacketizer.process_packet(rtp_payload, timestamp, seq_num) {
                    Ok(nals) => {
                        if !nals.is_empty() {
                            // Log NAL types received (skip empty NALs)
                            let nal_types: Vec<u8> = nals
                                .iter()
                                .filter(|nal| !nal.is_empty())
                                .map(|nal| nal[0] & 0x1F)
                                .collect();
                            tracing::info!(
                                "[WHEP RTP] Depacketized {} NAL units from {}: types={:?}, timestamp={}",
                                nals.len(),
                                media_type,
                                nal_types,
                                timestamp
                            );

                            let mut pending = pending_nals.lock().unwrap();
                            pending.extend(nals);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[WHEP RTP] H.264 depacketization failed: {}", e);
                    }
                }
            } else if media_type.starts_with("audio") {
                // Opus packets are already complete in RTP payload
                static AUDIO_PACKET_COUNT: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let audio_count =
                    AUDIO_PACKET_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Enhanced audio logging - first packet is critical
                if audio_count == 0 {
                    tracing::info!(
                        "[WHEP RTP] 🎵 FIRST AUDIO PACKET received! media_type='{}', size={} bytes, timestamp={}",
                        media_type,
                        rtp_payload.len(),
                        timestamp
                    );
                    tracing::info!("[WHEP RTP] Audio codec from media_type: {}", media_type);
                } else if audio_count.is_multiple_of(50) {
                    // Log every 50th audio packet
                    tracing::info!(
                        "[WHEP RTP] Audio packet #{}: media_type={}, size={}, timestamp={}",
                        audio_count,
                        media_type,
                        rtp_payload.len(),
                        timestamp
                    );
                }

                let mut pending = pending_audio.lock().unwrap();
                let queue_size_before = pending.len();
                pending.push(rtp_payload);

                if audio_count == 0 {
                    tracing::info!(
                        "[WHEP RTP] Audio packet queued (queue was {} packets, now {})",
                        queue_size_before,
                        pending.len()
                    );
                }
            } else {
                tracing::warn!("[WHEP RTP] Unknown media type: {}", media_type);
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
            tracing::warn!(
                "[WebRtcWhepProcessor] Received counter-offer (406) - not yet supported"
            );
            return Err(StreamError::Runtime(
                "WHEP counter-offer not yet supported".into(),
            ));
        }

        tracing::info!("[WebRtcWhepProcessor] 📋 ========================================");
        tracing::info!("[WebRtcWhepProcessor] 📋 PARSING SDP ANSWER FOR CODEC CONFIG");
        tracing::info!("[WebRtcWhepProcessor] 📋 ========================================");

        // Parse SDP answer to extract detailed codec configuration
        let has_audio = sdp_answer.contains("m=audio");
        let has_video = sdp_answer.contains("m=video");
        tracing::info!(
            "[WebRtcWhepProcessor] SDP tracks: video={}, audio={}",
            has_video,
            has_audio
        );

        // Print full SDP for debugging
        tracing::info!("[WebRtcWhepProcessor] Full SDP answer:\n{}", sdp_answer);

        // Parse audio codec details from SDP and extract configuration
        let mut negotiated_sample_rate: Option<u32> = None;
        let mut negotiated_channels: Option<usize> = None;

        if has_audio {
            // Check codec type
            if sdp_answer.contains("opus") || sdp_answer.contains("OPUS") {
                tracing::info!("[WebRtcWhepProcessor] ✅ Audio codec: Opus");
            } else {
                tracing::error!("[WebRtcWhepProcessor] ❌ Audio codec: NOT Opus! Check SDP above");
            }

            // Extract sample rate and channels from rtpmap line
            // Format: a=rtpmap:111 opus/48000/2
            if let Some(rtpmap_line) = sdp_answer
                .lines()
                .find(|line| line.contains("rtpmap") && line.contains("opus"))
            {
                tracing::info!(
                    "[WebRtcWhepProcessor] 🎵 Audio rtpmap line: '{}'",
                    rtpmap_line
                );

                // Parse rtpmap format: a=rtpmap:<pt> <codec>/<sample_rate>/<channels>
                if let Some(codec_info) = rtpmap_line.split_whitespace().nth(1) {
                    let parts: Vec<&str> = codec_info.split('/').collect();
                    if parts.len() >= 2 {
                        // Parse sample rate
                        if let Ok(sample_rate) = parts[1].parse::<u32>() {
                            negotiated_sample_rate = Some(sample_rate);
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 Opus sample rate from SDP rtpmap: {} Hz",
                                sample_rate
                            );
                        }

                        // Parse channels (RFC 7587: if omitted, defaults to 1=mono)
                        if parts.len() >= 3 {
                            if let Ok(channels) = parts[2].parse::<usize>() {
                                negotiated_channels = Some(channels);
                                tracing::info!("[WebRtcWhepProcessor] 🎵 Opus channels from SDP rtpmap: {} (1=mono, 2=stereo)", channels);
                            }
                        } else {
                            // RFC 7587 Section 4: If omitted, defaults to 1 (mono)
                            negotiated_channels = Some(1);
                            tracing::warn!("[WebRtcWhepProcessor] ⚠️  Opus channels NOT specified in rtpmap, using RFC default: 1 (mono)");
                        }
                    }
                }
            } else {
                tracing::warn!("[WebRtcWhepProcessor] ⚠️  No rtpmap line found for Opus");
            }

            // Check fmtp parameters (format-specific parameters)
            // Format: a=fmtp:111 minptime=10;useinbandfec=1;stereo=1
            if let Some(fmtp_line) = sdp_answer
                .lines()
                .find(|line| line.contains("fmtp") && line.contains("111"))
            {
                tracing::info!("[WebRtcWhepProcessor] 🎵 Audio fmtp line: '{}'", fmtp_line);

                // Check for stereo indicator (decoder behavior, not stream content)
                if fmtp_line.contains("stereo=1") {
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 Decoder should output: STEREO (stereo=1 in fmtp)"
                    );
                } else if fmtp_line.contains("stereo=0") {
                    tracing::warn!(
                        "[WebRtcWhepProcessor] ⚠️  Decoder should output: MONO (stereo=0 in fmtp)"
                    );
                } else {
                    tracing::warn!("[WebRtcWhepProcessor] ⚠️  No 'stereo' parameter in fmtp (default is mono output)");
                }

                // Check for sprop-stereo (indicates stream content)
                if fmtp_line.contains("sprop-stereo=1") {
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 Stream content is: STEREO (sprop-stereo=1)"
                    );
                } else if fmtp_line.contains("sprop-stereo=0") {
                    tracing::warn!(
                        "[WebRtcWhepProcessor] ⚠️  Stream content is: MONO (sprop-stereo=0)"
                    );
                } else {
                    tracing::warn!("[WebRtcWhepProcessor] ⚠️  No 'sprop-stereo' in fmtp (stream might be mono)");
                }

                // Check for maxaveragebitrate
                if let Some(bitrate_param) = fmtp_line
                    .split(';')
                    .find(|p| p.contains("maxaveragebitrate"))
                {
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 Max average bitrate: {}",
                        bitrate_param
                    );
                }
            } else {
                tracing::warn!(
                    "[WebRtcWhepProcessor] ⚠️  No fmtp line found for Opus payload type 111"
                );
            }

            tracing::info!("[WebRtcWhepProcessor] 📋 ========================================");
        } else {
            tracing::error!(
                "[WebRtcWhepProcessor] ❌ NO AUDIO TRACK in SDP answer! Server not sending audio."
            );
        }

        // Store negotiated audio configuration
        self.audio_sample_rate = negotiated_sample_rate;
        self.audio_channels = negotiated_channels;

        // Initialize audio decoder with negotiated parameters
        if let (Some(sample_rate), Some(channels)) = (negotiated_sample_rate, negotiated_channels) {
            tracing::info!(
                "[WebRtcWhepProcessor] 🎵 Initializing Opus decoder with negotiated settings: {}Hz, {} channels",
                sample_rate,
                channels
            );

            match OpusDecoder::new(sample_rate, channels) {
                Ok(decoder) => {
                    self.audio_decoder = Some(decoder);
                    tracing::info!(
                        "[WebRtcWhepProcessor] ✅ Audio decoder initialized successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "[WebRtcWhepProcessor] ❌ Failed to initialize audio decoder: {}",
                        e
                    );
                    return Err(e);
                }
            }
        } else {
            tracing::warn!(
                "[WebRtcWhepProcessor] ⚠️  Could not extract audio config from SDP (sample_rate={:?}, channels={:?})",
                negotiated_sample_rate,
                negotiated_channels
            );
        }

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

        tracing::info!(
            "[WebRtcWhepProcessor] Sending {} ICE candidates to WHEP server",
            candidates.len()
        );

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
            // Validate NAL unit has at least one byte
            if nal.is_empty() {
                tracing::warn!("[WebRtcWhepProcessor] Skipping empty NAL unit");
                continue;
            }

            // Check NAL type
            let nal_type = nal[0] & 0x1F;

            // SPS (7) - Sequence Parameter Set
            if nal_type == 7 {
                tracing::info!(
                    "[WebRtcWhepProcessor] Received SPS NAL ({} bytes)",
                    nal.len()
                );
                self.sps_nal = Some(nal.clone());

                // Try to parse dimensions from SPS (early resolution detection)
                use crate::apple::videotoolbox::format::parse_sps_dimensions;
                if let Some((width, height)) = parse_sps_dimensions(&nal) {
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎥 SPS indicates resolution: {}x{}",
                        width,
                        height
                    );
                }

                // If we have both SPS and PPS, initialize decoder
                if let (Some(sps), Some(pps)) = (self.sps_nal.as_ref(), self.pps_nal.as_ref()) {
                    let sps = sps.clone();
                    let pps = pps.clone();
                    self.initialize_decoder(&sps, &pps)?;
                }
                continue;
            }

            // PPS (8) - Picture Parameter Set
            if nal_type == 8 {
                tracing::info!(
                    "[WebRtcWhepProcessor] Received PPS NAL ({} bytes)",
                    nal.len()
                );
                self.pps_nal = Some(nal.clone());

                // If we have both SPS and PPS, initialize decoder
                if let (Some(sps), Some(pps)) = (self.sps_nal.as_ref(), self.pps_nal.as_ref()) {
                    let sps = sps.clone();
                    let pps = pps.clone();
                    self.initialize_decoder(&sps, &pps)?;
                }
                continue;
            }

            // IDR (5) or Non-IDR (1) - decode frame
            if nal_type == 1 || nal_type == 5 {
                if let Some(decoder) = &mut self.video_decoder {
                    let timestamp_ns = MediaClock::now().as_nanos() as i64;

                    tracing::debug!(
                        "[WebRtcWhepProcessor] Decoding {} NAL (type={}, size={} bytes)",
                        if nal_type == 5 { "IDR" } else { "Non-IDR" },
                        nal_type,
                        nal.len()
                    );

                    // Detect NAL format and convert to Annex B if needed
                    let nal_for_decoder = match H264NalFormat::detect(&nal) {
                        H264NalFormat::RawNal => {
                            // Raw NAL from RTP depacketizer - add Annex B start code
                            tracing::trace!(
                                "[WebRtcWhepProcessor] Converting raw NAL to Annex B format"
                            );
                            let mut annex_b = Vec::with_capacity(4 + nal.len());
                            annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // 4-byte start code
                            annex_b.extend_from_slice(&nal);
                            Bytes::from(annex_b)
                        }
                        H264NalFormat::AnnexB => {
                            // Already Annex B
                            tracing::trace!("[WebRtcWhepProcessor] NAL already in Annex B format");
                            nal.clone()
                        }
                        H264NalFormat::Avcc => {
                            // AVCC format - would need conversion but decoder handles it
                            tracing::trace!(
                                "[WebRtcWhepProcessor] NAL in AVCC format (decoder will handle)"
                            );
                            nal.clone()
                        }
                    };

                    // VideoToolboxDecoder expects Annex B format
                    match decoder.decode(&nal_for_decoder, timestamp_ns) {
                        Ok(Some(video_frame)) => {
                            self.video_out.write(video_frame);
                            self.video_frame_count += 1;

                            if self.video_frame_count.is_multiple_of(30) {
                                // Log every 30th frame
                                tracing::info!(
                                    "[WebRtcWhepProcessor] ✅ Decoded and output video frame #{}",
                                    self.video_frame_count
                                );
                            } else {
                                tracing::debug!(
                                    "[WebRtcWhepProcessor] Decoded video frame #{}",
                                    self.video_frame_count
                                );
                            }
                        }
                        Ok(None) => {
                            tracing::trace!(
                                "[WebRtcWhepProcessor] Decoder buffering (needs more data)"
                            );
                        }
                        Err(e) => {
                            tracing::warn!("[WebRtcWhepProcessor] Video decode error: {}", e);
                        }
                    }
                } else {
                    tracing::warn!("[WebRtcWhepProcessor] Received NAL type {} but decoder not ready (waiting for SPS/PPS)", nal_type);
                }
            }
        }

        Ok(())
    }

    fn initialize_decoder(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        if self.video_decoder.is_some() {
            // Already initialized
            return Ok(());
        }

        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not available".into()))?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GpuContext not available".into()))?;

        tracing::info!("[WebRtcWhepProcessor] Initializing VideoToolbox decoder with SPS ({} bytes) and PPS ({} bytes)",
            sps.len(), pps.len());

        // VideoToolbox decoder expects raw NAL units (no start codes) for SPS/PPS
        // If we received Annex B format, strip the start codes
        let sps_raw = match H264NalFormat::detect(sps) {
            H264NalFormat::AnnexB => {
                // Strip 4-byte or 3-byte start code
                if sps.len() >= 4 && sps[0..4] == [0x00, 0x00, 0x00, 0x01] {
                    tracing::trace!("[WebRtcWhepProcessor] Stripping 4-byte start code from SPS");
                    &sps[4..]
                } else if sps.len() >= 3 && sps[0..3] == [0x00, 0x00, 0x01] {
                    tracing::trace!("[WebRtcWhepProcessor] Stripping 3-byte start code from SPS");
                    &sps[3..]
                } else {
                    sps // Shouldn't happen, but use as-is
                }
            }
            _ => sps, // Raw NAL or AVCC - use as-is
        };

        let pps_raw = match H264NalFormat::detect(pps) {
            H264NalFormat::AnnexB => {
                // Strip 4-byte or 3-byte start code
                if pps.len() >= 4 && pps[0..4] == [0x00, 0x00, 0x00, 0x01] {
                    tracing::trace!("[WebRtcWhepProcessor] Stripping 4-byte start code from PPS");
                    &pps[4..]
                } else if pps.len() >= 3 && pps[0..3] == [0x00, 0x00, 0x01] {
                    tracing::trace!("[WebRtcWhepProcessor] Stripping 3-byte start code from PPS");
                    &pps[3..]
                } else {
                    pps // Shouldn't happen, but use as-is
                }
            }
            _ => pps, // Raw NAL or AVCC - use as-is
        };

        // Create decoder with default config (we'll update format with SPS/PPS)
        let mut decoder = VideoToolboxDecoder::new(Default::default(), Some(gpu_ctx.clone()), ctx)?;

        // Configure decoder with SPS/PPS (raw NAL units)
        decoder.update_format(sps_raw, pps_raw)?;

        self.video_decoder = Some(decoder);

        tracing::info!("[WebRtcWhepProcessor] ✅ VideoToolbox decoder initialized successfully");

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

        tracing::debug!(
            "[WebRtcWhepProcessor] Processing {} audio packets",
            packets.len()
        );

        if let Some(decoder) = &mut self.audio_decoder {
            for (idx, packet) in packets.iter().enumerate() {
                // Log first packet details
                if self.audio_frame_count == 0 && idx == 0 {
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 ========================================"
                    );
                    tracing::info!("[WebRtcWhepProcessor] 🎵 DECODING FIRST AUDIO PACKET");
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 ========================================"
                    );
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 Packet size: {} bytes",
                        packet.len()
                    );
                    tracing::info!("[WebRtcWhepProcessor] 🎵 Negotiated from SDP:");
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵   - Sample rate: {:?} Hz",
                        self.audio_sample_rate
                    );
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵   - Input channels: {:?} (from server)",
                        self.audio_channels
                    );
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵   - Decoder input: {:?} channels",
                        decoder.input_channels()
                    );
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵   - Decoder output: 2 channels (stereo, always)"
                    );
                    tracing::info!(
                        "[WebRtcWhepProcessor] 🎵 ========================================"
                    );
                }

                // Generate timestamp for audio frame
                let timestamp_ns = MediaClock::now().as_nanos() as i64;

                match decoder.decode_to_audio_frame(packet, timestamp_ns) {
                    Ok(audio_frame) => {
                        // Enhanced logging for first frame
                        if self.audio_frame_count == 0 {
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 ========================================"
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 FIRST AUDIO FRAME DECODED SUCCESSFULLY"
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 ========================================"
                            );
                            tracing::info!("[WebRtcWhepProcessor] 🎵 Decoded frame details:");
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵   - Total samples: {} (interleaved L,R,L,R...)",
                                audio_frame.samples.len()
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵   - Samples per channel: {}",
                                audio_frame.samples.len() / 2
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵   - Output channels: 2 (stereo)"
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵   - Sample rate (output): {} Hz",
                                audio_frame.sample_rate
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵   - Duration: {:.2} ms",
                                (audio_frame.samples.len() / 2) as f64 * 1000.0
                                    / audio_frame.sample_rate as f64
                            );
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 ========================================"
                            );
                            tracing::info!("[WebRtcWhepProcessor] 🎵 Writing first audio frame to audio_out port...");
                        }

                        self.audio_out.write(audio_frame);
                        self.audio_frame_count += 1;

                        if self.audio_frame_count == 1 {
                            tracing::info!("[WebRtcWhepProcessor] ✅ First audio frame written to output (count now {})", self.audio_frame_count);
                            tracing::info!(
                                "[WebRtcWhepProcessor] 🎵 ========================================"
                            );
                        } else if self.audio_frame_count.is_multiple_of(50) {
                            // Log every 50th frame
                            tracing::info!(
                                "[WebRtcWhepProcessor] ✅ Decoded and output audio frame #{}",
                                self.audio_frame_count
                            );
                        } else {
                            tracing::debug!(
                                "[WebRtcWhepProcessor] Decoded audio frame #{}",
                                self.audio_frame_count
                            );
                        }
                    }
                    Err(e) => {
                        if self.audio_frame_count == 0 {
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ ========================================"
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ FAILED TO DECODE FIRST AUDIO PACKET"
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ ========================================"
                            );
                            tracing::error!("[WebRtcWhepProcessor] ❌ Error: {}", e);
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ Packet size: {} bytes",
                                packet.len()
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ Negotiated sample rate: {:?} Hz",
                                self.audio_sample_rate
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ Negotiated channels: {:?}",
                                self.audio_channels
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ Decoder input channels: {:?}",
                                decoder.input_channels()
                            );
                            tracing::error!(
                                "[WebRtcWhepProcessor] ❌ ========================================"
                            );
                        } else {
                            tracing::warn!(
                                "[WebRtcWhepProcessor] Audio decode error (frame #{}): {}",
                                self.audio_frame_count,
                                e
                            );
                        }
                    }
                }
            }
        } else {
            tracing::error!(
                "[WebRtcWhepProcessor] ❌ Received {} audio packets but decoder not initialized!",
                packets.len()
            );
            tracing::error!(
                "[WebRtcWhepProcessor] ❌ Negotiated sample rate: {:?} Hz",
                self.audio_sample_rate
            );
            tracing::error!(
                "[WebRtcWhepProcessor] ❌ Negotiated channels: {:?}",
                self.audio_channels
            );
        }

        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(whep_client) = &self.whep_client {
            if let Ok(client) = whep_client.lock() {
                if let Err(e) = client.terminate() {
                    tracing::error!(
                        "[WebRtcWhepProcessor] Failed to terminate WHEP session: {}",
                        e
                    );
                }
            }
        }
        tracing::info!("[WebRtcWhepProcessor] Teardown complete");
        Ok(())
    }
}
