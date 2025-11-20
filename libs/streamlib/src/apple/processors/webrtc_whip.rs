// WebRTC WHIP Streaming Processor
//
// This file contains the WebRTC WHIP processor that integrates:
// - H.264 encoding via VideoToolbox
// - Opus audio encoding
// - WHIP signaling (RFC 9725)
// - WebRTC session management (webrtc-rs)

use crate::apple::videotoolbox::{EncodedVideoFrame, VideoEncoderConfig, VideoToolboxEncoder};
use crate::apple::webrtc::{WebRtcSession, WhipClient, WhipConfig};
use crate::core::streaming::{
    convert_audio_to_sample, convert_video_to_samples, AudioEncoderConfig, AudioEncoderOpus,
    OpusEncoder, RtpTimestampCalculator,
};
use crate::core::{
    media_clock::MediaClock, AudioFrame, GpuContext, Result, RuntimeContext, StreamError,
    StreamInput, VideoFrame,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use streamlib_macros::StreamProcessor;

// ============================================================================
// VIDEO ENCODER TRAIT (WebRTC-specific interface)
// ============================================================================

#[allow(dead_code)] // Methods reserved for future bitrate/keyframe control
trait VideoEncoderH264: Send {
    fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame>;
    fn force_keyframe(&mut self);
    fn config(&self) -> &VideoEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// VIDEOTOOLBOX H.264 ENCODER WRAPPER (WebRTC-specific)
// ============================================================================

/// Thin wrapper around VideoToolboxEncoder for WebRTC compatibility
struct VideoToolboxH264Encoder {
    encoder: VideoToolboxEncoder,
}

impl VideoToolboxH264Encoder {
    fn new(
        config: VideoEncoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        Ok(Self {
            encoder: VideoToolboxEncoder::new(config, gpu_context, ctx)?,
        })
    }
}

impl VideoEncoderH264 for VideoToolboxH264Encoder {
    fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame> {
        self.encoder.encode(frame)
    }

    fn force_keyframe(&mut self) {
        self.encoder.force_keyframe()
    }

    fn config(&self) -> &VideoEncoderConfig {
        self.encoder.config()
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder.set_bitrate(bitrate_bps)
    }
}

// ============================================================================
// MAIN WEBRTC WHIP PROCESSOR
// ============================================================================

// ============================================================================
// MAIN WEBRTC WHIP PROCESSOR
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WebRtcWhipConfig {
    pub whip: WhipConfig,
    pub video: VideoEncoderConfig,
    pub audio: AudioEncoderConfig,
}

impl Default for WebRtcWhipConfig {
    fn default() -> Self {
        Self {
            whip: WhipConfig {
                endpoint_url: String::new(),
                auth_token: None, // No authentication by default
                timeout_ms: 10000,
            },
            video: VideoEncoderConfig::default(),
            audio: AudioEncoderConfig::default(),
        }
    }
}

#[derive(StreamProcessor)]
#[processor(
    mode = Push,
    description = "Streams video and audio to Cloudflare Stream via WebRTC WHIP"
)]
pub struct WebRtcWhipProcessor {
    #[input(description = "Input video frames to encode and stream")]
    video_in: StreamInput<VideoFrame>,

    #[input(description = "Input audio frames to encode and stream")]
    audio_in: StreamInput<AudioFrame<2>>,

    #[config]
    config: WebRtcWhipConfig,

    // RuntimeContext for main thread dispatch
    ctx: Option<RuntimeContext>,

    // Session state
    session_started: bool,
    start_time_ns: Option<i64>,
    gpu_context: Option<GpuContext>, // Store for lazy encoder init

    // Encoders (will be Box<dyn Trait> when implemented)
    #[cfg(target_os = "macos")]
    video_encoder: Option<VideoToolboxH264Encoder>,
    audio_encoder: Option<OpusEncoder>,

    // RTP timestamp calculators
    video_rtp_calc: Option<RtpTimestampCalculator>,
    audio_rtp_calc: Option<RtpTimestampCalculator>,

    // WHIP and WebRTC
    whip_client: Option<Arc<Mutex<WhipClient>>>,
    webrtc_session: Option<WebRtcSession>,

    // RTCP stats monitoring
    last_stats_time_ns: i64,
    last_video_bytes_sent: u64,
    last_audio_bytes_sent: u64,
    last_video_packets_sent: u64,
    last_audio_packets_sent: u64,
}

impl WebRtcWhipProcessor {
    /// Called by StreamProcessor macro during setup phase.
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Initialize audio encoder (doesn't require main thread)
        self.audio_encoder = Some(OpusEncoder::new(self.config.audio.clone())?);

        tracing::info!(
            "WebRtcWhipProcessor initialized (will create video encoder on first frames)"
        );
        Ok(())
    }

    /// Called by StreamProcessor macro during teardown phase.
    fn teardown(&mut self) -> Result<()> {
        tracing::info!("WebRtcWhipProcessor shutting down");

        // Close WebRTC session
        if let Some(webrtc_session) = &self.webrtc_session {
            if let Err(e) = webrtc_session.close() {
                tracing::warn!("Error closing WebRTC session: {}", e);
            }
        }

        // Terminate WHIP session (DELETE request)
        if let Some(whip_client) = &self.whip_client {
            if let Ok(client) = whip_client.lock() {
                if let Err(e) = client.terminate() {
                    tracing::warn!("Error terminating WHIP session: {}", e);
                }
            }
        }

        tracing::info!("WebRtcWhipProcessor shutdown complete");
        Ok(())
    }

    /// Main processing loop: reads video and audio frames, encodes them, and streams via WebRTC
    fn process(&mut self) -> Result<()> {
        let video_frame = self.video_in.read_latest();
        let audio_frame = self.audio_in.read_latest();

        // Start session on first frame
        if !self.session_started && (video_frame.is_some() || audio_frame.is_some()) {
            tracing::info!("[WebRTC] Starting session - received first frame");
            self.start_session()?;
            self.session_started = true;
        }

        // Process video if available
        if let Some(frame) = video_frame {
            self.process_video_frame(&frame)?;
        }

        // Process audio if available
        if let Some(frame) = audio_frame {
            self.process_audio_frame(&frame)?;
        }

        // Collect and log RTCP stats every 2 seconds
        if self.session_started {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let elapsed_since_last_stats = current_time_ns - self.last_stats_time_ns;

            // Log stats every 2 seconds (2_000_000_000 ns)
            if elapsed_since_last_stats >= 2_000_000_000 {
                self.log_rtcp_stats()?;
                self.last_stats_time_ns = current_time_ns;
            }
        }

        Ok(())
    }

    /// Starts the WebRTC WHIP session.
    fn start_session(&mut self) -> Result<()> {
        // Initialize VideoToolbox encoder lazily
        if self.video_encoder.is_none() {
            let gpu_context = self.gpu_context.clone();
            let ctx = self
                .ctx
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?;
            self.video_encoder = Some(VideoToolboxH264Encoder::new(
                self.config.video.clone(),
                gpu_context,
                ctx,
            )?);
            tracing::info!("VideoToolbox H.264 encoder initialized");
        }

        // 2. Set start time
        self.start_time_ns = Some(MediaClock::now().as_nanos() as i64);

        // 3. Initialize RTP timestamp calculators
        self.video_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            90000, // 90kHz for video
        ));
        self.audio_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            48000, // 48kHz for Opus
        ));

        // Install rustls crypto provider if needed
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to install rustls crypto provider: {:?}",
                        e
                    ))
                })?;
        }

        // Create WHIP client
        let whip_client = Arc::new(Mutex::new(WhipClient::new(self.config.whip.clone())?));
        self.whip_client = Some(whip_client.clone());

        // Create WebRTC session with ICE callback
        let whip_clone = whip_client.clone();
        let mut webrtc_session = WebRtcSession::new(move |candidate_sdp| {
            if let Ok(whip) = whip_clone.lock() {
                whip.queue_ice_candidate(candidate_sdp);
            }
        })?;

        // Create SDP offer and add bandwidth attributes
        let offer = webrtc_session.create_offer()?;
        let offer_with_bandwidth = WebRtcSession::add_bandwidth_to_sdp(
            &offer,
            self.config.video.bitrate_bps,
            self.config.audio.bitrate_bps,
        );

        tracing::info!("[WebRTC] ========== SDP OFFER (with bandwidth) ==========");
        for (i, line) in offer_with_bandwidth.lines().enumerate() {
            tracing::info!("[WebRTC] SDP OFFER [{}]: {}", i, line);
        }
        tracing::info!("[WebRTC] ================================");

        // POST offer to WHIP endpoint
        let answer = whip_client
            .lock()
            .unwrap()
            .post_offer(&offer_with_bandwidth)?;
        tracing::info!("[WebRTC] ========== SDP ANSWER ==========");
        for (i, line) in answer.lines().enumerate() {
            tracing::info!("[WebRTC] SDP ANSWER [{}]: {}", i, line);
        }
        tracing::info!("[WebRTC] =================================");

        // Set remote answer
        webrtc_session.set_remote_answer(&answer)?;

        // Send any buffered ICE candidates (optional - trickle ICE may not be supported)
        match whip_client.lock().unwrap().send_ice_candidates() {
            Ok(_) => {
                tracing::info!("[WebRTC] ICE candidates sent successfully (trickle ICE supported)");
            }
            Err(e) => {
                tracing::debug!("[WebRTC] Trickle ICE not supported: {}", e);
            }
        }

        self.webrtc_session = Some(webrtc_session);
        self.session_started = true;

        tracing::info!("WebRTC WHIP session started");
        Ok(())
    }

    fn process_video_frame(&mut self, frame: &VideoFrame) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Video encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;
        let samples = convert_video_to_samples(&encoded, self.config.video.fps)?;
        self.webrtc_session
            .as_mut()
            .unwrap()
            .write_video_samples(samples)?;

        Ok(())
    }

    fn process_audio_frame(&mut self, frame: &AudioFrame<2>) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let encoder = self
            .audio_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;
        let sample = convert_audio_to_sample(&encoded, self.config.audio.sample_rate)?;
        self.webrtc_session
            .as_mut()
            .unwrap()
            .write_audio_sample(sample)?;

        Ok(())
    }

    /// Collects and logs RTCP statistics from the WebRTC peer connection.
    /// Calculates bitrates from delta of bytes sent since last measurement.
    fn log_rtcp_stats(&mut self) -> Result<()> {
        let webrtc_session = self
            .webrtc_session
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("WebRTC session not initialized".into()))?;

        // Get stats from peer connection (async operation, run in Tokio runtime)
        let stats = webrtc_session.get_stats()?;

        let mut video_bytes_sent = 0u64;
        let mut audio_bytes_sent = 0u64;
        let mut video_packets_sent = 0u64;
        let mut audio_packets_sent = 0u64;

        // Iterate over stats to find OutboundRTP for video and audio
        for (_id, stat_type) in stats.reports.iter() {
            match stat_type {
                webrtc::stats::StatsReportType::OutboundRTP(outbound) => {
                    if outbound.kind == "video" {
                        video_bytes_sent = outbound.bytes_sent;
                        video_packets_sent = outbound.packets_sent;
                        tracing::debug!(
                            "[WebRTC Stats] Video OutboundRTP - bytes_sent: {}, packets_sent: {}, header_bytes_sent: {}",
                            outbound.bytes_sent,
                            outbound.packets_sent,
                            outbound.header_bytes_sent
                        );
                    } else if outbound.kind == "audio" {
                        audio_bytes_sent = outbound.bytes_sent;
                        audio_packets_sent = outbound.packets_sent;
                        tracing::debug!(
                            "[WebRTC Stats] Audio OutboundRTP - bytes_sent: {}, packets_sent: {}, header_bytes_sent: {}",
                            outbound.bytes_sent,
                            outbound.packets_sent,
                            outbound.header_bytes_sent
                        );
                    }
                }
                webrtc::stats::StatsReportType::RemoteInboundRTP(remote_inbound) => {
                    // These stats come from Cloudflare's RTCP receiver reports
                    // They tell us what Cloudflare is actually receiving
                    tracing::debug!(
                        "[WebRTC Stats] RemoteInboundRTP ({}) - packets_received: {}, packets_lost: {}",
                        remote_inbound.kind,
                        remote_inbound.packets_received,
                        remote_inbound.packets_lost
                    );
                }
                _ => {
                    // Ignore other stat types for now
                }
            }
        }

        // Calculate bitrates from deltas (bytes sent since last measurement)
        if self.last_stats_time_ns > 0 {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let delta_time_s = (current_time_ns - self.last_stats_time_ns) as f64 / 1_000_000_000.0;

            let video_bytes_delta = video_bytes_sent.saturating_sub(self.last_video_bytes_sent);
            let audio_bytes_delta = audio_bytes_sent.saturating_sub(self.last_audio_bytes_sent);

            let video_packets_delta =
                video_packets_sent.saturating_sub(self.last_video_packets_sent);
            let audio_packets_delta =
                audio_packets_sent.saturating_sub(self.last_audio_packets_sent);

            // Calculate bitrates (bits per second)
            let video_bitrate_bps = (video_bytes_delta as f64 * 8.0) / delta_time_s;
            let audio_bitrate_bps = (audio_bytes_delta as f64 * 8.0) / delta_time_s;

            // Calculate packet rates (packets per second)
            let video_packet_rate = video_packets_delta as f64 / delta_time_s;
            let audio_packet_rate = audio_packets_delta as f64 / delta_time_s;

            tracing::info!(
                "[WebRTC Stats] ========== OUTBOUND STATS ==========\n\
                 Video: {:.2} Mbps ({:.0} pps, {} packets, {:.2} MB total)\n\
                 Audio: {:.2} kbps ({:.0} pps, {} packets, {:.2} KB total)\n\
                 Total: {:.2} Mbps\n\
                 ===========================================",
                video_bitrate_bps / 1_000_000.0,
                video_packet_rate,
                video_packets_sent,
                video_bytes_sent as f64 / 1_000_000.0,
                audio_bitrate_bps / 1_000.0,
                audio_packet_rate,
                audio_packets_sent,
                audio_bytes_sent as f64 / 1_000.0,
                (video_bitrate_bps + audio_bitrate_bps) / 1_000_000.0
            );
        }

        // Update last stats for next delta calculation
        self.last_video_bytes_sent = video_bytes_sent;
        self.last_audio_bytes_sent = audio_bytes_sent;
        self.last_video_packets_sent = video_packets_sent;
        self.last_audio_packets_sent = audio_packets_sent;

        Ok(())
    }
}

impl Drop for WebRtcWhipProcessor {
    fn drop(&mut self) {
        if let Some(whip_client) = &self.whip_client {
            if let Ok(client) = whip_client.lock() {
                let _ = client.terminate();
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

    // ========================================================================
    // SAMPLE CONVERSION TESTS
    // ========================================================================

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

        // Should create 3 samples (one per NAL unit)
        assert_eq!(samples.len(), 3);

        // Check duration (1/30 fps = ~33.33ms)
        let expected_duration = Duration::from_secs_f64(1.0 / 30.0);
        assert_eq!(samples[0].duration, expected_duration);
        assert_eq!(samples[1].duration, expected_duration);
        assert_eq!(samples[2].duration, expected_duration);

        // Check data (should be NAL units without start codes)
        assert_eq!(samples[0].data.as_ref(), &[0x67, 0x42]);
        assert_eq!(samples[1].data.as_ref(), &[0x68, 0x43]);
        assert_eq!(samples[2].data.as_ref(), &[0x65, 0xAA]);
    }

    #[test]
    fn test_convert_video_no_nal_units() {
        let encoded = EncodedVideoFrame {
            data: vec![0xAA, 0xBB, 0xCC], // No start codes
            timestamp_ns: 0,
            is_keyframe: false,
            frame_number: 0,
        };

        let result = convert_video_to_samples(&encoded, 30);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No NAL units"));
    }

    #[test]
    fn test_convert_audio_to_sample() {
        let encoded = EncodedAudioFrame {
            data: vec![0xAA, 0xBB, 0xCC, 0xDD],
            timestamp_ns: 1_000_000_000,
            sample_count: 960, // 20ms @ 48kHz
        };

        let sample = convert_audio_to_sample(&encoded, 48000).unwrap();

        // Check duration (960 samples @ 48kHz = 20ms)
        let expected_duration = Duration::from_secs_f64(960.0 / 48000.0);
        assert_eq!(sample.duration, expected_duration);

        // Check data
        assert_eq!(sample.data.as_ref(), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn test_convert_audio_duration_calculation() {
        // Test various sample counts
        let test_cases = vec![
            (480, 48000, 10.0),  // 10ms
            (960, 48000, 20.0),  // 20ms
            (1920, 48000, 40.0), // 40ms
        ];

        for (sample_count, sample_rate, expected_ms) in test_cases {
            let encoded = EncodedAudioFrame {
                data: vec![0x00],
                timestamp_ns: 0,
                sample_count,
            };

            let sample = convert_audio_to_sample(&encoded, sample_rate).unwrap();
            let actual_ms = sample.duration.as_secs_f64() * 1000.0;

            assert!(
                (actual_ms - expected_ms).abs() < 0.01,
                "Expected ~{}ms, got {}ms for {} samples @ {}Hz",
                expected_ms,
                actual_ms,
                sample_count,
                sample_rate
            );
        }
    }

    // ========================================================================
    // NAL UNIT PARSER TESTS
    // ========================================================================

    #[test]
    fn test_parse_nal_units_single_4byte() {
        let data = vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB, 0xCC];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], vec![0x65, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn test_parse_nal_units_single_3byte() {
        let data = vec![0, 0, 1, 0x65, 0xDD, 0xEE];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], vec![0x65, 0xDD, 0xEE]);
    }

    #[test]
    fn test_parse_nal_units_multiple() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0x42, // SPS (4-byte start code)
            0, 0, 0, 1, 0x68, 0x43, // PPS (4-byte start code)
            0, 0, 1, 0x65, 0xAA, // IDR (3-byte start code)
        ];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], vec![0x67, 0x42]);
        assert_eq!(nals[1], vec![0x68, 0x43]);
        assert_eq!(nals[2], vec![0x65, 0xAA]);
    }

    #[test]
    fn test_parse_nal_units_mixed_start_codes() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0x11, // 4-byte start code
            0, 0, 1, 0x68, 0x22, // 3-byte start code
            0, 0, 0, 1, 0x65, 0x33, // 4-byte start code
        ];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], vec![0x67, 0x11]);
        assert_eq!(nals[1], vec![0x68, 0x22]);
        assert_eq!(nals[2], vec![0x65, 0x33]);
    }

    #[test]
    fn test_parse_nal_units_empty() {
        let data = vec![];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 0);
    }

    #[test]
    fn test_parse_nal_units_no_start_code() {
        let data = vec![0x65, 0xAA, 0xBB, 0xCC];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 0, "Should not parse data without start codes");
    }

    #[test]
    fn test_parse_nal_units_realistic_frame() {
        // Simulate real VideoToolbox output (SPS + PPS + IDR)
        let mut data = Vec::new();

        // SPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x67, 0x42, 0xC0, 0x1E]); // Fake SPS

        // PPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x68, 0xCE, 0x3C, 0x80]); // Fake PPS

        // IDR slice
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x10]); // Fake IDR

        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);

        // Verify NAL unit types (first byte & 0x1F)
        assert_eq!(nals[0][0] & 0x1F, 0x07); // SPS
        assert_eq!(nals[1][0] & 0x1F, 0x08); // PPS
        assert_eq!(nals[2][0] & 0x1F, 0x05); // IDR
    }

    // ========================================================================
    // RTP TIMESTAMP CALCULATOR TESTS
    // ========================================================================

    #[test]
    fn test_rtp_timestamp_calculator() {
        let start_ns = 1_000_000_000; // 1 second
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // At start time, should return base timestamp
        let ts1 = calc.calculate(start_ns);

        // After 1 second (90000 ticks for 90kHz)
        let ts2 = calc.calculate(start_ns + 1_000_000_000);

        // Difference should be ~90000
        let diff = ts2.wrapping_sub(ts1);
        assert_eq!(diff, 90000);
    }

    #[test]
    fn test_rtp_timestamp_random_base() {
        // RTP base should be random (not predictable)
        let calc1 = RtpTimestampCalculator::new(0, 90000);
        let calc2 = RtpTimestampCalculator::new(0, 90000);

        // Different calculators should have different bases
        assert_ne!(
            calc1.rtp_base, calc2.rtp_base,
            "RTP base should be random, not deterministic"
        );
    }

    #[test]
    fn test_rtp_timestamp_wraparound() {
        let start_ns = 0;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // Simulate enough time for wraparound at 90kHz
        // u32::MAX / 90000 ~= 47721 seconds ~= 13.25 hours
        // Test 50 seconds which should wrap if base is near max
        let ts_50s = 50_000_000_000i64;
        let rtp_ts = calc.calculate(ts_50s);

        // Check that calculation doesn't panic
        // Verify wrapping math is correct
        let expected_ticks = (50_000_000_000i128 * 90000) / 1_000_000_000;
        let expected = calc.rtp_base.wrapping_add(expected_ticks as u32);
        assert_eq!(rtp_ts, expected);
    }

    #[test]
    fn test_rtp_timestamp_audio_48khz() {
        let start_ns = 1_000_000_000;
        let calc = RtpTimestampCalculator::new(start_ns, 48000);

        // 20ms audio frame @ 48kHz = 960 samples
        let frame_ns = start_ns + 20_000_000;
        let rtp_ts1 = calc.calculate(frame_ns);

        // Next frame (another 20ms)
        let rtp_ts2 = calc.calculate(frame_ns + 20_000_000);

        // Should increment by 960 samples
        assert_eq!(rtp_ts2.wrapping_sub(rtp_ts1), 960);
    }

    #[test]
    fn test_rtp_timestamp_video_90khz() {
        let start_ns = 1_000_000_000;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // 33.33ms video frame @ 30fps
        let frame_duration_ns = 33_333_333i64;
        let frame_ns = start_ns + frame_duration_ns;
        let rtp_ts1 = calc.calculate(frame_ns);

        // Next frame
        let rtp_ts2 = calc.calculate(frame_ns + frame_duration_ns);

        // Should increment by ~3000 ticks (33.33ms × 90kHz)
        let diff = rtp_ts2.wrapping_sub(rtp_ts1);
        assert!(
            (diff as i32 - 3000).abs() < 2,
            "Expected ~3000 ticks, got {}",
            diff
        );
    }

    #[test]
    fn test_rtp_timestamp_long_session() {
        let start_ns = 0;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // Simulate 1 hour of video @ 30fps
        let one_hour_ns = 3600_000_000_000i64;
        let rtp_ts = calc.calculate(one_hour_ns);

        // Should handle large elapsed times without overflow
        let expected_ticks = (one_hour_ns as i128 * 90000) / 1_000_000_000;
        let expected = calc.rtp_base.wrapping_add(expected_ticks as u32);
        assert_eq!(rtp_ts, expected);
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

    // ========================================================================
    // OPUS ENCODER TESTS
    // ========================================================================

    #[test]
    fn test_opus_encoder_creation() {
        let config = AudioEncoderConfig::default();
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_ok());
    }

    #[test]
    fn test_opus_encoder_invalid_sample_rate() {
        let mut config = AudioEncoderConfig::default();
        config.sample_rate = 44100; // Not supported
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_err());
        let err = encoder.unwrap_err().to_string();
        assert!(err.contains("48kHz"));
    }

    #[test]
    fn test_opus_encoder_invalid_channels() {
        let mut config = AudioEncoderConfig::default();
        config.channels = 1; // Mono not supported
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_err());
        let err = encoder.unwrap_err().to_string();
        assert!(err.contains("stereo"));
    }

    #[test]
    fn test_opus_encode_correct_frame_size() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Create 20ms frame @ 48kHz stereo = 960 samples * 2 channels = 1920 f32
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(
            samples, 0,     // timestamp_ns
            0,     // frame_number
            48000, // sample_rate
        );

        let result = encoder.encode(&frame);
        assert!(result.is_ok());

        let encoded = result.unwrap();
        assert!(encoded.data.len() > 0);
        assert_eq!(encoded.timestamp_ns, 0);
        assert_eq!(encoded.sample_count, 960);
    }

    #[test]
    fn test_opus_encode_wrong_frame_size() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Wrong size: 512 samples instead of 960
        let samples = vec![0.0f32; 512 * 2];
        let frame = AudioFrame::<2>::new(samples, 0, 0, 48000);

        let result = encoder.encode(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("BufferRechunkerProcessor"));
    }

    #[test]
    fn test_opus_encode_wrong_sample_rate() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Wrong sample rate
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(
            samples, 0, 0, 44100, // Wrong sample rate
        );

        let result = encoder.encode(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("AudioResamplerProcessor"));
    }

    #[test]
    fn test_opus_timestamp_preservation() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        let timestamp_ns = 123456789i64;
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(samples, timestamp_ns, 42, 48000);

        let encoded = encoder.encode(&frame).unwrap();
        assert_eq!(encoded.timestamp_ns, timestamp_ns);
    }

    #[test]
    fn test_opus_bitrate_change() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        let result = encoder.set_bitrate(96_000);
        assert!(result.is_ok());
        assert_eq!(encoder.config().bitrate_bps, 96_000);
    }

    #[test]
    fn test_opus_encode_sine_wave() {
        use std::f32::consts::PI;

        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Generate 20ms of 440Hz sine wave @ 48kHz stereo
        let mut samples = Vec::with_capacity(1920);
        for i in 0..960 {
            let t = i as f32 / 48000.0;
            let sample = (2.0 * PI * 440.0 * t).sin() * 0.5;
            samples.push(sample); // Left
            samples.push(sample); // Right
        }

        let frame = AudioFrame::<2>::new(samples, 0, 0, 48000);
        let encoded = encoder.encode(&frame).unwrap();

        // Encoded size should be reasonable (< 4KB for 20ms)
        assert!(encoded.data.len() > 10); // At least some bytes
        assert!(encoded.data.len() < 4000); // Not too large
    }

    #[test]
    fn test_opus_encode_multiple_frames() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Simulate encoding 10 frames (200ms of audio)
        for frame_num in 0..10 {
            let timestamp_ns = frame_num * 20_000_000; // 20ms increments
            let samples = vec![0.1f32; 1920];
            let frame = AudioFrame::<2>::new(samples, timestamp_ns, frame_num as u64, 48000);

            let encoded = encoder.encode(&frame).unwrap();
            assert!(encoded.data.len() > 0);
            assert_eq!(encoded.timestamp_ns, timestamp_ns);
            assert_eq!(encoded.sample_count, 960);
        }
    }
}
