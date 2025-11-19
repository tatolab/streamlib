// WebRTC WHIP Streaming Implementation
//
// This file contains the complete WebRTC streaming implementation with:
// - H.264 encoding via VideoToolbox with GPU-accelerated RGBA→NV12 conversion
// - Opus audio encoding
// - WHIP signaling (RFC 9725)
// - WebRTC session management (webrtc-rs)

use crate::core::{
    VideoFrame, AudioFrame, StreamError, Result,
    media_clock::MediaClock, GpuContext,
    StreamInput, RuntimeContext,
};
use streamlib_macros::StreamProcessor;
use crate::apple::videotoolbox::{VideoToolboxEncoder, VideoEncoderConfig, H264Profile, EncodedVideoFrame};
use std::sync::{Arc, Mutex};
use objc2::runtime::ProtocolObject;
use webrtc::track::track_local::TrackLocalWriter;
use serde::{Deserialize, Serialize};

// WHIP HTTP client imports
use hyper;
use hyper_rustls;
use hyper_util;
use http_body_util;

// ============================================================================
// INTERNAL TYPES (not exported)
// ============================================================================

/// Internal representation of encoded Opus frame
#[derive(Clone, Debug)]
struct EncodedAudioFrame {
    data: Vec<u8>,
    timestamp_ns: i64,
    sample_count: usize,
}

// ============================================================================
// OPUS ENCODING CONFIGURATION
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioEncoderConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_bps: u32,
    pub frame_duration_ms: u32,
    pub complexity: u32,
    pub vbr: bool,
}

impl Default for AudioEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            bitrate_bps: 128_000,
            frame_duration_ms: 20,
            complexity: 10,
            vbr: true,
        }
    }
}

// ============================================================================
// VIDEO ENCODER TRAIT (WebRTC-specific interface)
// ============================================================================

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
    fn new(config: VideoEncoderConfig, gpu_context: Option<GpuContext>, ctx: &RuntimeContext) -> Result<Self> {
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
// AUDIO ENCODER TRAIT
// ============================================================================

trait AudioEncoderOpus: Send {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame>;
    fn config(&self) -> &AudioEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// OPUS ENCODER IMPLEMENTATION
// ============================================================================

/// Opus audio encoder for real-time WebRTC streaming.
///
/// # Requirements
/// - Input must be stereo (`AudioFrame<2>`)
/// - Sample rate must be 48kHz
/// - Frame size must be exactly 960 samples (20ms @ 48kHz)
///
/// # Pipeline Setup
/// Typical pipeline for Opus encoding requires preprocessing:
/// - AudioSource → AudioResamplerProcessor(48kHz) → BufferRechunkerProcessor(960) → OpusEncoder
///
/// # Configuration
/// - **Bitrate**: 128 kbps default (adjust with `set_bitrate()`)
/// - **VBR**: Enabled by default for better quality
/// - **FEC**: Forward error correction enabled for packet loss resilience
#[derive(Debug)]
struct OpusEncoder {
    config: AudioEncoderConfig,
    encoder: opus::Encoder,
    frame_size: usize,  // 960 samples per channel @ 48kHz (20ms)
}

impl OpusEncoder {
    fn new(config: AudioEncoderConfig) -> Result<Self> {
        // Validate config
        if config.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports 48kHz sample rate, got {}Hz", config.sample_rate)
            ));
        }
        if config.channels != 2 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports stereo (2 channels), got {}", config.channels)
            ));
        }

        // Calculate frame size (20ms @ 48kHz = 960 samples per channel)
        let frame_size = (config.sample_rate as usize * config.frame_duration_ms as usize) / 1000;

        // Create opus encoder
        let mut encoder = opus::Encoder::new(
            config.sample_rate,
            opus::Channels::Stereo,
            opus::Application::Audio,  // Use Audio for best quality (music/broadcast)
        ).map_err(|e| StreamError::Configuration(format!("Failed to create Opus encoder: {:?}", e)))?;

        // Configure encoder
        encoder.set_bitrate(opus::Bitrate::Bits(config.bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        encoder.set_vbr(config.vbr)
            .map_err(|e| StreamError::Configuration(format!("Failed to set VBR: {:?}", e)))?;

        // Enable FEC (Forward Error Correction) for better packet loss resilience
        encoder.set_inband_fec(true)
            .map_err(|e| StreamError::Configuration(format!("Failed to set FEC: {:?}", e)))?;

        tracing::info!(
            "OpusEncoder initialized: {}Hz, {} channels, {} kbps, {}ms frames, VBR={}",
            config.sample_rate,
            config.channels,
            config.bitrate_bps / 1000,
            config.frame_duration_ms,
            config.vbr
        );

        Ok(Self {
            config,
            encoder,
            frame_size,
        })
    }
}

impl AudioEncoderOpus for OpusEncoder {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame> {
        // Validate sample rate
        if frame.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!(
                    "Expected 48kHz, got {}Hz. Use AudioResamplerProcessor upstream to convert to 48kHz.",
                    frame.sample_rate
                )
            ));
        }

        // Validate frame size (should be exactly 960 samples per channel for 20ms @ 48kHz)
        let expected_samples = self.frame_size;  // 960
        let actual_samples = frame.sample_count();

        if actual_samples != expected_samples {
            return Err(StreamError::Configuration(
                format!(
                    "Expected {} samples (20ms @ 48kHz), got {}. Use BufferRechunkerProcessor(960) upstream.",
                    expected_samples, actual_samples
                )
            ));
        }

        // Encode (opus expects interleaved f32, which is what AudioFrame uses)
        // Max packet size ~4KB is enough for worst case Opus output
        let encoded_data = self.encoder
            .encode_vec_float(&frame.samples, 4000)
            .map_err(|e| StreamError::Runtime(format!("Opus encoding failed: {:?}", e)))?;

        tracing::trace!(
            "Encoded audio frame: {} samples → {} bytes (compression: {:.2}x)",
            actual_samples * 2,  // Total samples (stereo)
            encoded_data.len(),
            (actual_samples * 2 * 4) as f32 / encoded_data.len() as f32  // f32 = 4 bytes per sample
        );

        Ok(EncodedAudioFrame {
            data: encoded_data,
            timestamp_ns: frame.timestamp_ns,  // Preserve timestamp exactly
            sample_count: actual_samples,
        })
    }

    fn config(&self) -> &AudioEncoderConfig {
        &self.config
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        self.config.bitrate_bps = bitrate_bps;

        tracing::info!("Opus bitrate changed to {} kbps", bitrate_bps / 1000);
        Ok(())
    }
}

// ============================================================================
// H.264 FORMAT CONVERSION
// ============================================================================

/// Convert AVCC format (length-prefixed NAL units) to Annex-B format (start code prefixed)
///
/// AVCC format: [4-byte length][NAL unit][4-byte length][NAL unit]...
/// Annex-B format: [00 00 00 01][NAL unit][00 00 00 01][NAL unit]...
///
/// WebRTC/RTP requires Annex-B format for H.264 transmission.

// ============================================================================
// H.264 NAL UNIT PARSER
// ============================================================================

/// Parses H.264 Annex B format into individual NAL units.
///
/// VideoToolbox outputs Annex B format with start codes:
/// - 4-byte: `[0, 0, 0, 1]`
/// - 3-byte: `[0, 0, 1]`
///
/// webrtc-rs expects NAL units WITHOUT start codes for RTP packetization.
///
/// # Arguments
/// * `data` - Annex B formatted data (with start codes)
///
/// # Returns
/// Vec of NAL units (without start codes), preserving order.
///
/// # Performance
/// - Zero-copy slicing where possible
/// - ~1-2µs for typical frame (3-5 NAL units)
/// Parse NAL units from AVCC format (length-prefixed, used by VideoToolbox)
///
/// AVCC format: [4-byte length][NAL unit][4-byte length][NAL unit]...
/// Each NAL unit length is a 4-byte big-endian integer.
fn parse_nal_units_avcc(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i + 4 <= data.len() {
        // Read 4-byte big-endian length
        let nal_length = u32::from_be_bytes([
            data[i],
            data[i + 1],
            data[i + 2],
            data[i + 3],
        ]) as usize;

        i += 4; // Skip length prefix

        // Check bounds
        if i + nal_length > data.len() {
            tracing::warn!(
                "AVCC NAL unit length {} exceeds remaining data {} at offset {}",
                nal_length,
                data.len() - i,
                i - 4
            );
            break;
        }

        // Extract NAL unit
        let nal_unit = data[i..i + nal_length].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i += nal_length;
    }

    nal_units
}

/// Parse NAL units from Annex B format (start-code-prefixed)
///
/// Annex B format: [00 00 00 01 or 00 00 01][NAL unit][start code][NAL unit]...
fn parse_nal_units_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i < data.len() {
        // Look for start code (4-byte or 3-byte)
        let start_code_len = if i + 3 < data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1
        {
            4
        } else if i + 2 < data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1
        {
            3
        } else {
            i += 1;
            continue;
        };

        // Find next start code (or end of data)
        let mut nal_end = i + start_code_len;
        while nal_end < data.len() {
            if (nal_end + 3 < data.len()
                && data[nal_end] == 0
                && data[nal_end + 1] == 0
                && data[nal_end + 2] == 0
                && data[nal_end + 3] == 1)
                || (nal_end + 2 < data.len()
                    && data[nal_end] == 0
                    && data[nal_end + 1] == 0
                    && data[nal_end + 2] == 1)
            {
                break;
            }
            nal_end += 1;
        }

        // Extract NAL unit (without start code)
        let nal_unit = data[i + start_code_len..nal_end].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i = nal_end;
    }

    nal_units
}

/// Auto-detect format and parse NAL units
///
/// Checks first 4 bytes to determine if data is AVCC or Annex B format:
/// - AVCC: First 4 bytes = big-endian length (typically < 100KB for a frame)
/// - Annex B: Starts with 00 00 00 01 or 00 00 01
fn parse_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < 4 {
        return Vec::new();
    }

    // Check for Annex B start codes
    let is_annex_b = (data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1)
        || (data[0] == 0 && data[1] == 0 && data[2] == 1);

    if is_annex_b {
        tracing::info!("🔍 [NAL Parser] Detected Annex B format H.264 (start code present)");
        parse_nal_units_annex_b(data)
    } else {
        // Assume AVCC format (VideoToolbox default on macOS)
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

        // Sanity check: length should be reasonable (< 1MB for a single NAL unit)
        if length > 0 && length < 1_000_000 && length + 4 <= data.len() {
            tracing::info!("🔍 [NAL Parser] Detected AVCC format H.264 (first NAL length: {}, total data: {} bytes)", length, data.len());
            parse_nal_units_avcc(data)
        } else {
            tracing::error!(
                "❌ [NAL Parser] UNKNOWN H.264 FORMAT! First 4 bytes = {:02x} {:02x} {:02x} {:02x} (interpreted length={}, total data={})",
                data[0], data[1], data[2], data[3], length, data.len()
            );
            tracing::error!("❌ [NAL Parser] This will result in NO NAL units parsed - stream will fail!");
            Vec::new()
        }
    }
}

// ============================================================================
// RTP SAMPLE CONVERSION
// ============================================================================

use bytes::Bytes;
use std::time::Duration;

/// Converts encoded H.264 video frame to webrtc Sample(s).
///
/// # Process
/// 1. Parse NAL units from Annex B format
/// 2. Create one Sample per NAL unit (webrtc-rs handles packetization)
/// 3. Calculate frame duration from fps
///
/// # Arguments
/// * `frame` - Encoded H.264 frame (Annex B format with start codes)
/// * `fps` - Frames per second (for duration calculation)
///
/// # Returns
/// Vec of Samples (one per NAL unit), all with same duration
///
/// # Performance
/// - ~2-3µs for typical frame (parsing + allocation)
fn convert_video_to_samples(
    frame: &EncodedVideoFrame,
    fps: u32,
) -> Result<Vec<webrtc::media::Sample>> {
    // Parse NAL units from Annex B format
    let nal_units = parse_nal_units(&frame.data);

    if nal_units.is_empty() {
        return Err(StreamError::Runtime(
            "No NAL units found in H.264 frame".into()
        ));
    }

    // Calculate frame duration
    let duration = Duration::from_secs_f64(1.0 / fps as f64);

    // Convert each NAL unit to a Sample
    let samples = nal_units
        .into_iter()
        .map(|nal| webrtc::media::Sample {
            data: Bytes::from(nal),
            duration,
            ..Default::default()
        })
        .collect();

    Ok(samples)
}

/// Converts encoded Opus audio frame to webrtc Sample.
///
/// # Process
/// 1. Wrap encoded data in Bytes (zero-copy if possible)
/// 2. Calculate duration from sample count and rate
///
/// # Arguments
/// * `frame` - Encoded Opus frame
/// * `sample_rate` - Audio sample rate (typically 48000 Hz)
///
/// # Returns
/// Single Sample (Opus frames fit in one RTP packet)
///
/// # Performance
/// - ~0.5µs (duration calc + Bytes conversion)
fn convert_audio_to_sample(
    frame: &EncodedAudioFrame,
    sample_rate: u32,
) -> Result<webrtc::media::Sample> {
    // Calculate duration from sample count
    let duration = Duration::from_secs_f64(frame.sample_count as f64 / sample_rate as f64);

    Ok(webrtc::media::Sample {
        data: Bytes::from(frame.data.clone()),
        duration,
        ..Default::default()
    })
}

// ============================================================================
// RTP TIMESTAMP CALCULATOR
// ============================================================================

/// Calculates RTP timestamps from monotonic MediaClock timestamps.
///
/// # RTP Timestamp Format
/// - Video (H.264): 90kHz clock rate
/// - Audio (Opus): 48kHz clock rate (same as sample rate)
///
/// # Security
/// Uses random RTP base timestamp (RFC 3550 Section 5.1) to prevent:
/// - Timing prediction attacks
/// - Known-plaintext attacks on encryption
///
/// # Performance
/// - `new()`: ~0.5ns (fastrand)
/// - `calculate()`: ~2-3ns (i128 multiplication + division)
struct RtpTimestampCalculator {
    start_time_ns: i64,
    rtp_base: u32,
    clock_rate: u32,
}

impl RtpTimestampCalculator {
    /// Creates a new RTP timestamp calculator with random base.
    ///
    /// # Arguments
    /// * `start_time_ns` - Session start time from MediaClock::now()
    /// * `clock_rate` - RTP clock rate (90000 for video, 48000 for audio)
    fn new(start_time_ns: i64, clock_rate: u32) -> Self {
        // Random RTP base (RFC 3550 compliance)
        // Uses fastrand for speed (~0.5ns) - doesn't need crypto-grade PRNG
        let rtp_base = fastrand::u32(..);

        Self {
            start_time_ns,
            rtp_base,
            clock_rate,
        }
    }

    /// Converts monotonic nanosecond timestamp to RTP timestamp.
    ///
    /// # Arguments
    /// * `timestamp_ns` - Current MediaClock timestamp (nanoseconds)
    ///
    /// # Returns
    /// RTP timestamp (u32) - automatically wraps at 2^32
    ///
    /// # Example
    /// ```ignore
    /// let calc = RtpTimestampCalculator::new(0, 90000);
    /// let rtp_ts = calc.calculate(1_000_000_000); // 1 second
    /// // For 90kHz: 1s × 90000 ticks/s = 90000 ticks (+ random base)
    /// ```
    fn calculate(&self, timestamp_ns: i64) -> u32 {
        let elapsed_ns = timestamp_ns - self.start_time_ns;
        let elapsed_ticks = (elapsed_ns as i128 * self.clock_rate as i128) / 1_000_000_000;
        self.rtp_base.wrapping_add(elapsed_ticks as u32)
    }
}

// ============================================================================
// WHIP SIGNALING
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WhipConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication.
    /// Set to None for endpoints that don't require authentication (e.g., Cloudflare Stream).
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

/// WHIP (WebRTC-HTTP Ingestion Protocol) client per RFC 9725
///
/// Handles HTTP signaling for WebRTC streaming:
/// - POST /whip: Create session (SDP offer → answer)
/// - PATCH /session: Send ICE candidates (trickle ICE)
/// - DELETE /session: Terminate session
///
/// Uses pollster::block_on() for async HTTP calls (same pattern as WebRtcSession).
struct WhipClient {
    config: WhipConfig,

    /// HTTP client with HTTPS support
    /// Body type: http_body_util::combinators::BoxBody for flexibility
    http_client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::combinators::BoxBody<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>,
    >,

    /// Session URL (from Location header after POST success)
    session_url: Option<String>,

    /// Session ETag (for ICE restart - future use)
    session_etag: Option<String>,

    /// Pending ICE candidates (buffered for batch sending)
    pending_candidates: Arc<Mutex<Vec<String>>>,

    /// Tokio runtime for HTTP operations (required for tokio::time::timeout)
    _runtime: tokio::runtime::Runtime,
}

impl WhipClient {
    fn new(config: WhipConfig) -> Result<Self> {
        tracing::info!("[WhipClient] Creating WHIP client for endpoint: {}", config.endpoint_url);

        // Build HTTPS connector using rustls with ring crypto provider and native CA roots
        tracing::debug!("[WhipClient] Building HTTPS connector with native roots...");
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()  // Use system CA store (includes ring provider via feature flag)
            .map_err(|e| {
                tracing::error!("[WhipClient] Failed to load CA roots: {}", e);
                StreamError::Configuration(format!("Failed to load CA roots: {}", e))
            })?
            .https_or_http()      // Allow http:// for local testing
            .enable_http1()
            .enable_http2()
            .build();
        tracing::debug!("[WhipClient] HTTPS connector built successfully");

        // Create HTTP client
        tracing::debug!("[WhipClient] Creating HTTP client...");
        let http_client = hyper_util::client::legacy::Client::builder(
            hyper_util::rt::TokioExecutor::new()
        )
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .build(https);
        tracing::info!("[WhipClient] HTTP client created successfully");

        // Create Tokio runtime for HTTP operations (tokio::time::timeout needs it)
        tracing::debug!("[WhipClient] Creating Tokio runtime for HTTP operations...");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StreamError::Runtime(format!("Failed to create Tokio runtime for WHIP client: {}", e)))?;
        tracing::debug!("[WhipClient] Tokio runtime created successfully");

        Ok(Self {
            config,
            http_client,
            session_url: None,
            session_etag: None,
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            _runtime: runtime,
        })
    }

    /// POST SDP offer to WHIP endpoint, receive SDP answer
    ///
    /// RFC 9725 Section 4.1: Client creates session by POSTing SDP offer.
    /// Server responds with 201 Created, Location header (session URL), and SDP answer.
    ///
    /// # Arguments
    /// * `sdp_offer` - SDP offer string (application/sdp)
    ///
    /// # Returns
    /// SDP answer string from server
    ///
    /// # Errors
    /// - 400 Bad Request: Malformed SDP
    /// - 401 Unauthorized: Invalid auth token
    /// - 422 Unprocessable Content: Unsupported SDP configuration
    /// - 503 Service Unavailable: Server overloaded (should retry with backoff)
    /// - 307 Temporary Redirect: Load balancing (automatically followed)
    fn post_offer(&mut self, sdp_offer: &str) -> Result<String> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::Full;

        // Clone what we need to avoid borrow issues
        let endpoint_url = self.config.endpoint_url.clone();
        let auth_token = self.config.auth_token.clone();
        let timeout_ms = self.config.timeout_ms;

        // Extract http_client reference before async block
        let http_client = &self.http_client;

        // MUST use Tokio runtime (tokio::time::timeout requires it)
        let result = self._runtime.block_on(async {
            // Build POST request per RFC 9725 Section 4.1
            use http_body_util::BodyExt;
            let body = Full::new(bytes::Bytes::from(sdp_offer.to_owned()));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("POST")
                .uri(&endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp");

            // Add Authorization header only if token is provided
            if let Some(token) = &auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP POST request: {}", e)))?;

            tracing::debug!("WHIP POST to {}", endpoint_url);

            // Send request with timeout
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP POST timed out after {}ms", timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP POST request failed: {}", e)))?;

            let status = response.status();
            let headers = response.headers().clone();

            // Read response body
            let body_bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to read WHIP response body: {}", e)))?
                .to_bytes();

            // Return status, headers, and body for processing outside async block
            Ok::<_, StreamError>((status, headers, body_bytes))
        })?;

        // Process response outside async block to avoid borrow conflicts
        let (status, headers, body_bytes) = result;

        match status {
            StatusCode::CREATED => {
                // Extract Location header (REQUIRED per RFC 9725)
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| StreamError::Runtime(
                        "WHIP server returned 201 Created without Location header".into()
                    ))?;

                // Convert relative URLs to absolute URLs
                // Cloudflare returns relative paths like "/stream-id/webRTC/publish/session-id"
                self.session_url = if location.starts_with('/') {
                    // Parse endpoint URL to get base
                    let base_url = self.config.endpoint_url
                        .split('/')
                        .take(3)  // Take "https:", "", "hostname"
                        .collect::<Vec<_>>()
                        .join("/");
                    Some(format!("{}{}", base_url, location))
                } else {
                    Some(location.to_owned())
                };

                tracing::debug!(
                    "WHIP Location header: '{}' → session URL: '{}'",
                    location,
                    self.session_url.as_ref().unwrap()
                );

                    // Extract ETag header (optional, used for ICE restart)
                    self.session_etag = headers
                        .get(header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_owned());

                    // Parse SDP answer from body
                    let sdp_answer = String::from_utf8(body_bytes.to_vec())
                        .map_err(|e| StreamError::Runtime(format!("Invalid UTF-8 in SDP answer: {}", e)))?;

                    tracing::info!(
                        "WHIP session created: {} (ETag: {})",
                        self.session_url.as_ref().unwrap(),
                        self.session_etag.as_deref().unwrap_or("none")
                    );

                    Ok(sdp_answer)
                }

                StatusCode::TEMPORARY_REDIRECT => {
                    // Handle redirect per RFC 9725 Section 4.5
                    let location = headers
                        .get(header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| StreamError::Runtime(
                            "WHIP 307 redirect without Location header".into()
                        ))?;

                    tracing::info!("WHIP redirecting to: {}", location);

                    // Update endpoint and retry (recursive, but 307 should be rare)
                    self.config.endpoint_url = location.to_owned();
                    self.post_offer(sdp_offer)
                }

                StatusCode::SERVICE_UNAVAILABLE => {
                    // Server overloaded - caller should retry with backoff
                    let retry_after = headers
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown");

                    Err(StreamError::Runtime(format!(
                        "WHIP server overloaded (503), retry after: {}",
                        retry_after
                    )))
                }

                _ => {
                    // Other error (400, 401, 422, etc.)
                    let error_body = String::from_utf8(body_bytes.to_vec())
                        .unwrap_or_else(|_| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHIP POST failed ({}): {}",
                        status,
                        error_body
                    )))
                }
        }
    }

    /// Queue an ICE candidate for batched transmission
    ///
    /// Candidates are buffered and sent in batches via PATCH to reduce HTTP overhead.
    ///
    /// # Arguments
    /// * `candidate_sdp` - ICE candidate in SDP fragment format (e.g., "a=candidate:...")
    fn queue_ice_candidate(&self, candidate_sdp: String) {
        self.pending_candidates.lock().unwrap().push(candidate_sdp);
    }

    /// Send pending ICE candidates to WHIP server via PATCH
    ///
    /// RFC 9725 Section 4.2: Trickle ICE candidates sent via PATCH with
    /// Content-Type: application/trickle-ice-sdpfrag
    ///
    /// Sends all buffered candidates in a single PATCH request, then clears the queue.
    fn send_ice_candidates(&self) -> Result<()> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::{BodyExt, Full};

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                return Err(StreamError::Configuration(
                    "Cannot send ICE candidates: no WHIP session URL".into()
                ));
            }
        };

        // Drain pending candidates (atomic swap)
        let candidates = {
            let mut queue = self.pending_candidates.lock().unwrap();
            if queue.is_empty() {
                return Ok(()); // Nothing to send
            }
            queue.drain(..).collect::<Vec<_>>()
        };

        // Build SDP fragment per RFC 8840 (trickle-ice-sdpfrag)
        // Format: Multiple "a=candidate:..." lines joined by CRLF
        let sdp_fragment = candidates.join("\r\n");

        // MUST use Tokio runtime for HTTP operations
        self._runtime.block_on(async {
            let body = Full::new(bytes::Bytes::from(sdp_fragment));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("PATCH")
                .uri(session_url)
                .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag");

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP PATCH request: {}", e)))?;

            tracing::debug!("WHIP PATCH to {} ({} candidates)", session_url, candidates.len());

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP PATCH timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP PATCH request failed: {}", e)))?;

            let status = response.status();

            match status {
                StatusCode::NO_CONTENT => {
                    tracing::debug!("Sent {} ICE candidates to WHIP server", candidates.len());
                    Ok(())
                }
                StatusCode::OK => {
                    // 200 OK with body indicates ICE restart (server sends new candidates)
                    tracing::debug!("ICE restart response (200 OK)");
                    Ok(())
                }
                _ => {
                    // Read error body for debugging
                    let body_bytes = BodyExt::collect(response.into_body())
                        .await
                        .ok()
                        .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                        .unwrap_or_else(|| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHIP PATCH failed: {}",
                        body_bytes
                    )))
                }
            }
        })
    }

    /// DELETE session to WHIP server (graceful shutdown)
    ///
    /// RFC 9725 Section 4.4: Client terminates session by DELETEing session URL.
    /// Server responds with 200 OK.
    fn terminate(&self) -> Result<()> {
        use hyper::{Request, header};
        use http_body_util::Empty;

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                tracing::debug!("No WHIP session to terminate");
                return Ok(()); // No session was created
            }
        };

        // MUST use Tokio runtime for HTTP operations
        self._runtime.block_on(async {
            use http_body_util::BodyExt;
            let body = Empty::<bytes::Bytes>::new();
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("DELETE")
                .uri(session_url);

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP DELETE request: {}", e)))?;

            tracing::debug!("WHIP DELETE to {}", session_url);

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP DELETE timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP DELETE request failed: {}", e)))?;

            if response.status().is_success() {
                tracing::info!("WHIP session terminated: {}", session_url);
                Ok(())
            } else {
                // Non-fatal - session will timeout server-side
                tracing::warn!(
                    "WHIP DELETE failed (status {}), session may still exist server-side",
                    response.status()
                );
                Ok(())
            }
        })
    }
}

// ============================================================================
// WEBRTC SESSION
// ============================================================================

/// WebRTC session wrapper using webrtc-rs (0.14.0)
///
/// Manages PeerConnection, tracks, and RTP packetization.
/// Uses Tokio runtime for WebRTC operations (required for ICE gathering, DTLS, and stats).
struct WebRtcSession {
    /// RTCPeerConnection (handles ICE, DTLS, RTP/RTCP)
    peer_connection: Arc<webrtc::peer_connection::RTCPeerConnection>,

    /// Video track (H.264 @ 90kHz) - using TrackLocalStaticRTP for manual PT control
    video_track: Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// Audio track (Opus @ 48kHz) - using TrackLocalStaticRTP for manual PT control
    audio_track: Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// Flag indicating ICE connection is ready (tracks are bound with correct PT)
    /// CRITICAL: Must be true before calling write_sample() to ensure PT is set correctly
    /// Set to true when ICE connection state becomes Connected
    ice_connected: Arc<std::sync::atomic::AtomicBool>,

    /// Tokio runtime for WebRTC background tasks
    /// CRITICAL: Must stay alive for session lifetime (ICE gathering, DTLS, stats)
    _runtime: tokio::runtime::Runtime,
}

impl WebRtcSession {
    /// Creates a new WebRTC session with H.264 video and Opus audio tracks.
    ///
    /// # Arguments
    /// * `on_ice_candidate` - Callback invoked when ICE candidates are discovered
    fn new<F>(on_ice_candidate: F) -> Result<Self>
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        // Create Tokio runtime for WebRTC background tasks
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)  // Minimal threads: 1 for blocking ops, 1 for background tasks
            .thread_name("webrtc-tokio")
            .enable_all()
            .build()
            .map_err(|e| StreamError::Runtime(format!("Failed to create Tokio runtime for WebRTC: {}", e)))?;

        tracing::info!("[WebRTC] Created Tokio runtime with 2 worker threads");

        // Block on async initialization within Tokio context
        let init_result = runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating MediaEngine and registering codecs...");

            // Create MediaEngine and register only the codecs we use
            let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

            // Register H.264 video codec (Baseline profile)
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                            clock_rate: 90000,
                            channels: 0,
                            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 102,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register H.264 codec: {}", e)))?;

            // Register Opus audio codec
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                            clock_rate: 48000,
                            channels: 2,
                            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 111,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register Opus codec: {}", e)))?;

            tracing::info!("[WebRTC] Registered ONLY H.264 (PT=102) and Opus (PT=111) codecs");

            tracing::debug!("[WebRTC] Creating interceptor registry...");

            // Create InterceptorRegistry for RTCP feedback (NACK, reports, stats)
            let mut registry = webrtc::interceptor::registry::Registry::new();
            registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut media_engine)
                .map_err(|e| StreamError::Configuration(format!("Failed to register interceptors: {}", e)))?;

            tracing::debug!("[WebRTC] Creating WebRTC API...");

            // Create API with MediaEngine and InterceptorRegistry
            let api = webrtc::api::APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build();

            tracing::debug!("[WebRTC] Creating RTCPeerConnection...");

            // Create RTCPeerConnection
            let config = webrtc::peer_connection::configuration::RTCConfiguration::default();
            let peer_connection = Arc::new(
                api
                    .new_peer_connection(config)
                    .await
                    .map_err(|e| StreamError::Configuration(format!("Failed to create PeerConnection: {}", e)))?
            );

            tracing::debug!("[WebRTC] RTCPeerConnection created successfully");

            // Subscribe to ICE candidate events
            let on_candidate = Arc::new(on_ice_candidate);
            let pc_for_ice_candidate = Arc::clone(&peer_connection);
            pc_for_ice_candidate.on_ice_candidate(Box::new(move |candidate_opt| {
                let callback = Arc::clone(&on_candidate);
                Box::pin(async move {
                    if let Some(candidate) = candidate_opt {
                        if let Ok(json) = candidate.to_json() {
                            let sdp_fragment = format!("a={}", json.candidate);
                            tracing::debug!("ICE candidate discovered: {}", sdp_fragment);
                            callback(sdp_fragment);
                        }
                    } else {
                        tracing::debug!("ICE candidate gathering complete");
                    }
                })
            }));

            // Monitor signaling state changes
            peer_connection.on_signaling_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🔄 Signaling state: {:?}", state);
                })
            }));

            // Monitor peer connection state (includes DTLS handshake)
            peer_connection.on_peer_connection_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] 🔗 Peer connection state: {:?}", state);
                    tracing::info!("[WebRTC] ========================================");
                    match state {
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::New => {
                            tracing::debug!("[WebRTC] Peer connection: New");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connecting => {
                            tracing::info!("[WebRTC] Peer connection: Connecting... (DTLS handshake in progress)");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                            tracing::info!("[WebRTC] ✅✅✅ Peer connection: CONNECTED! ✅✅✅");
                            tracing::info!("[WebRTC] DTLS handshake completed successfully!");
                            tracing::info!("[WebRTC] RTP packets can now be sent/received!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected => {
                            tracing::warn!("[WebRTC] ⚠️  Peer connection: DISCONNECTED!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                            tracing::error!("[WebRTC] ❌❌❌ Peer connection: FAILED! ❌❌❌");
                            tracing::error!("[WebRTC] This means DTLS handshake or ICE failed!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                            tracing::info!("[WebRTC] Peer connection: Closed");
                        }
                        _ => {}
                    }
                })
            }));

            // Monitor ICE gathering state
            peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🧊 ICE gathering state: {:?}", state);
                })
            }));

            // Monitor ICE connection state
            let ice_connected_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ice_connected_clone = Arc::clone(&ice_connected_flag);
            let pc_for_ice_handler = Arc::clone(&peer_connection);

            peer_connection.on_ice_connection_state_change(Box::new(move |connection_state| {
                let flag = Arc::clone(&ice_connected_clone);
                let pc = Arc::clone(&pc_for_ice_handler);
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] ICE connection state changed: {:?}", connection_state);
                    tracing::info!("[WebRTC] ========================================");

                    if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected {
                        tracing::info!("[WebRTC] ✅ ICE Connected!");

                        // Verify transceiver payload types
                        let transceivers = pc.get_transceivers().await;
                        tracing::info!("[WebRTC] Verifying PT values for {} transceivers after ICE connection:", transceivers.len());

                        for (i, transceiver) in transceivers.iter().enumerate() {
                            let sender = transceiver.sender().await;
                            let params = sender.get_parameters().await;

                            tracing::info!("[WebRTC] Transceiver #{}: {} codec(s), PT={:?}",
                                i,
                                params.rtp_parameters.codecs.len(),
                                params.encodings.first().map(|e| e.payload_type));
                        }

                        flag.store(true, std::sync::atomic::Ordering::Release);
                        tracing::info!("[WebRTC] 🚀 Ready to send samples!");

                    } else if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected
                           || connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed {
                        tracing::warn!("[WebRTC] ❌ ICE connection lost: {:?}", connection_state);
                        flag.store(false, std::sync::atomic::Ordering::Release);
                    }

                    ()
                })
            }));

            // Create video track (H.264)
            let video_track = Arc::new(
                webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                        clock_rate: 90000,
                        channels: 0,
                        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                        ..Default::default()
                    },
                    "video".to_owned(),
                    "streamlib-video".to_owned(),
                ),
            );

            // Add video track to PeerConnection
            let video_rtp_sender = peer_connection
                .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add video track: {}", e)))?;

            let video_params = video_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in video_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:VIDEO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = video_params.encodings.first() {
                tracing::info!("[TELEMETRY:VIDEO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:VIDEO_ENCODING] NO_ENCODING_FOUND");
            }

            // Create audio track (Opus)
            let audio_track = Arc::new(
                webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                        clock_rate: 48000,
                        channels: 2,
                        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                        ..Default::default()
                    },
                    "audio".to_owned(),
                    "streamlib-audio".to_owned(),
                ),
            );

            // Add audio track to PeerConnection
            let audio_rtp_sender = peer_connection
                .add_track(Arc::clone(&audio_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add audio track: {}", e)))?;

            let audio_params = audio_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in audio_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:AUDIO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = audio_params.encodings.first() {
                tracing::info!("[TELEMETRY:AUDIO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:AUDIO_ENCODING] NO_ENCODING_FOUND");
            }

            // Set transceivers to send-only (WHIP unidirectional publishing)
            let transceivers = peer_connection.get_transceivers().await;
            for transceiver in transceivers {
                if transceiver.sender().await.track().await.is_some() {
                    transceiver.set_direction(webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Sendonly).await;
                }
            }

            // Return tuple of (peer_connection, video_track, audio_track, ice_connected_flag)
            Ok::<_, StreamError>((peer_connection, video_track, audio_track, ice_connected_flag))
        })?;

        let (peer_connection, video_track, audio_track, ice_connected) = init_result;

        Ok(Self {
            peer_connection,
            video_track: Some(video_track),
            audio_track: Some(audio_track),
            ice_connected,
            _runtime: runtime,
        })
    }

    /// Adds bandwidth attributes to SDP for WHIP compatibility.
    ///
    /// Cloudflare Stream requires explicit bandwidth signaling in the SDP.
    /// Adds b=AS (Application-Specific) and b=TIAS (Transport-Independent) lines
    /// after each m= line based on configured bitrates.
    ///
    /// # Arguments
    /// * `sdp` - Original SDP string
    /// * `video_bitrate_bps` - Video bitrate in bits per second
    /// * `audio_bitrate_bps` - Audio bitrate in bits per second
    ///
    /// # Returns
    /// Modified SDP with bandwidth attributes
    fn add_bandwidth_to_sdp(sdp: &str, video_bitrate_bps: u32, audio_bitrate_bps: u32) -> String {
        let mut result = String::new();
        let lines: Vec<&str> = sdp.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            result.push_str(line);
            result.push('\n');

            // Add bandwidth attributes after m=video line
            if line.starts_with("m=video") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = video_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", video_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added video bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    video_bitrate_bps
                );
            }
            // Add bandwidth attributes after m=audio line
            else if line.starts_with("m=audio") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = audio_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", audio_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added audio bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    audio_bitrate_bps
                );
            }

            i += 1;
        }

        result
    }

    /// Creates SDP offer for WHIP signaling.
    fn create_offer(&self) -> Result<String> {
        self._runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating SDP offer...");

            let offer = self
                .peer_connection
                .create_offer(None)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to create offer: {}", e)))?;

            tracing::debug!("[WebRTC] Setting local description (starts ICE gathering)...");

            // Set local description (triggers ICE candidate gathering via mDNS)
            self.peer_connection
                .set_local_description(offer)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to set local description: {}", e)))?;

            // Wait for ICE gathering to complete
            tracing::debug!("[WebRTC] Waiting for ICE gathering to complete...");

            let mut done_rx = self.peer_connection.gathering_complete_promise().await;
            let _ = done_rx.recv().await;

            tracing::debug!("[WebRTC] ICE gathering completed");

            // Get updated SDP with ICE candidates included
            let local_desc = self
                .peer_connection
                .local_description()
                .await
                .ok_or_else(|| StreamError::Runtime("No local description".into()))?;

            let candidate_count = local_desc.sdp.matches("a=candidate:").count();
            tracing::debug!("[WebRTC] SDP offer created successfully with {} ICE candidates", candidate_count);
            Ok(local_desc.sdp)
        })
    }

    /// Sets remote SDP answer from WHIP server.
    fn set_remote_answer(&mut self, sdp: &str) -> Result<()> {
        self._runtime.block_on(async {
            tracing::debug!("[WebRTC] Setting remote SDP answer...");

            let answer = webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp.to_owned())
                .map_err(|e| StreamError::Runtime(format!("Failed to parse SDP answer: {}", e)))?;

            self.peer_connection
                .set_remote_description(answer)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to set remote description: {}", e)))?;

            tracing::debug!("[WebRTC] Remote SDP answer set successfully");

            let transceivers = self.peer_connection.get_transceivers().await;
            tracing::debug!("[WebRTC] Configured {} transceivers after SDP negotiation", transceivers.len());

            Ok(())
        })
    }

    /// Validate and log H.264 NAL unit format
    fn validate_and_log_h264_nal(sample_data: &[u8], sample_idx: usize) {
        if sample_data.len() < 5 {
            tracing::error!("[H264 Validation] ❌ Sample {}: Too short ({} bytes, need ≥5)",
                sample_idx, sample_data.len());
            return;
        }

        // Log first 8 bytes to identify format
        tracing::info!("[H264 Validation] Sample {}: First 8 bytes: {:02X?}",
            sample_idx,
            &sample_data[..sample_data.len().min(8)]);

        // Check for Annex-B start codes (0x00 0x00 0x00 0x01 or 0x00 0x00 0x01)
        let is_annex_b = (sample_data.len() >= 4
            && sample_data[0] == 0x00
            && sample_data[1] == 0x00
            && sample_data[2] == 0x00
            && sample_data[3] == 0x01)
            || (sample_data.len() >= 3
                && sample_data[0] == 0x00
                && sample_data[1] == 0x00
                && sample_data[2] == 0x01);

        if is_annex_b {
            tracing::error!("[H264 Validation] ❌❌❌ Sample {}: ANNEX-B FORMAT DETECTED!", sample_idx);
            tracing::error!("[H264 Validation] WebRTC requires AVCC format (length-prefixed), not Annex-B!");
            tracing::error!("[H264 Validation] This explains why Cloudflare receives no packets!");

            // Extract NAL unit type from after start code
            let nal_offset = if sample_data.len() >= 4 && sample_data[3] == 0x01 { 4 } else { 3 };
            if sample_data.len() > nal_offset {
                let nal_unit_type = sample_data[nal_offset] & 0x1F;
                tracing::error!("[H264 Validation] NAL type: {} (after Annex-B start code)", nal_unit_type);
            }
            return;
        }

        // AVCC format: [4-byte length][NAL unit data]
        let nal_length = u32::from_be_bytes([
            sample_data[0],
            sample_data[1],
            sample_data[2],
            sample_data[3],
        ]) as usize;

        if nal_length + 4 != sample_data.len() {
            tracing::warn!("[H264 Validation] ⚠️  Sample {}: NAL length mismatch (prefix says {}, actual {})",
                sample_idx, nal_length, sample_data.len() - 4);
        }

        // Extract NAL unit type from first byte of NAL data (after 4-byte length)
        let nal_unit_type = sample_data[4] & 0x1F;

        // Log NAL unit type
        match nal_unit_type {
            1 => tracing::trace!("[H264] Sample {}: Coded slice (non-IDR)", sample_idx),
            5 => tracing::info!("[H264] Sample {}: IDR (keyframe) ✅", sample_idx),
            6 => tracing::trace!("[H264] Sample {}: SEI", sample_idx),
            7 => tracing::info!("[H264] Sample {}: SPS (Sequence Parameter Set) ✅", sample_idx),
            8 => tracing::info!("[H264] Sample {}: PPS (Picture Parameter Set) ✅", sample_idx),
            9 => tracing::trace!("[H264] Sample {}: AUD (Access Unit Delimiter)", sample_idx),
            _ => tracing::debug!("[H264] Sample {}: NAL type {}", sample_idx, nal_unit_type),
        }
    }

    /// Writes video samples to the video track.
    fn write_video_samples(&mut self, samples: Vec<webrtc::media::Sample>) -> Result<()> {
        let track = self
            .video_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Video track not initialized".into()))?;

        // Track first write and periodic telemetry
        static VIDEO_SAMPLE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static VIDEO_SEQ_NUM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static VIDEO_TIMESTAMP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let counter = VIDEO_SAMPLE_COUNTER.fetch_add(samples.len() as u64, std::sync::atomic::Ordering::Relaxed);

        if counter == 0 {
            tracing::info!("[WebRTC] 🎬 FIRST VIDEO WRITE after ICE Connected!");
            tracing::info!("[WebRTC]    NAL units: {}, First NAL bytes: {}",
                samples.len(),
                samples.first().map(|s| s.data.len()).unwrap_or(0));
        } else if counter % 30 == 0 {
            tracing::debug!("[TELEMETRY:VIDEO_SAMPLE_WRITE] sample_num={}, nal_count={}, total_bytes={}",
                counter,
                samples.len(),
                samples.iter().map(|s| s.data.len()).sum::<usize>());
        }

        // RFC 6184 H.264 RTP Packetization
        // - Single NAL Unit mode: NAL < MTU (send as-is)
        // - FU-A mode: NAL >= MTU (fragment into multiple packets)
        use webrtc::rtp::packet::Packet as RtpPacket;
        use webrtc::rtp::header::Header as RtpHeader;

        const MAX_PAYLOAD_SIZE: usize = 1200; // Conservative MTU minus headers
        let timestamp_increment = 90000 / 30; // H.264 @ 90kHz, 30fps
        let current_timestamp = VIDEO_TIMESTAMP.load(std::sync::atomic::Ordering::Relaxed);

        for (i, sample) in samples.iter().enumerate() {
            let is_last_nal_in_frame = i == samples.len() - 1;

            // Decode NAL unit type (bits 0-4 of first byte)
            let nal_type = sample.data[0] & 0x1F;
            let nal_type_name = match nal_type {
                1 => "P-frame (non-IDR)",
                5 => "IDR (keyframe)",
                6 => "SEI",
                7 => "SPS",
                8 => "PPS",
                _ => "Other",
            };

            // Log NAL unit types for debugging decoder issues
            if counter <= 10 || nal_type == 5 || nal_type == 7 || nal_type == 8 {
                tracing::info!("[WebRTC] 🎬 NAL unit #{}: type={} ({}), size={} bytes",
                    counter, nal_type, nal_type_name, sample.data.len());
            }

            if sample.data.len() <= MAX_PAYLOAD_SIZE {
                // Single NAL Unit mode - send entire NAL as one RTP packet
                let seq_num = VIDEO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                let rtp_packet = RtpPacket {
                    header: RtpHeader {
                        version: 2,
                        padding: false,
                        extension: false,
                        marker: is_last_nal_in_frame, // Mark last NAL of frame
                        payload_type: 102,  // H.264 (registered as PT=102)
                        sequence_number: seq_num as u16,
                        timestamp: current_timestamp,
                        ssrc: 0,  // Will be set by track
                        ..Default::default()
                    },
                    payload: sample.data.clone(),
                };

                self._runtime.block_on(async {
                    track
                        .write_rtp(&rtp_packet)
                        .await
                        .map_err(|e| StreamError::Runtime(format!("Failed to write video RTP: {}", e)))
                })?;

                if counter == 0 && i == 0 {
                    tracing::info!("[WebRTC] ✅ Successfully wrote first video RTP packet (Single NAL, {} bytes)", sample.data.len());
                } else if counter % 30 == 0 && i == 0 {
                    tracing::info!("[WebRTC] 📊 Video RTP packet #{} sent (Single NAL, {} bytes)", counter, sample.data.len());
                }
            } else {
                // FU-A (Fragmentation Unit) mode - split NAL into multiple RTP packets
                // RFC 6184 Section 5.8: FU-A format

                let nal_header = sample.data[0]; // First byte is NAL header
                let nal_payload = &sample.data[1..]; // Rest is NAL payload

                // FU Indicator: F=0, NRI from NAL header, Type=28 (FU-A)
                let fu_indicator = (nal_header & 0xE0) | 28;

                // FU Header: S (start), E (end), R=0, Type from NAL header
                let nal_type = nal_header & 0x1F;

                let mut offset = 0;
                let mut frag_count = 0;

                while offset < nal_payload.len() {
                    let remaining = nal_payload.len() - offset;
                    let payload_size = remaining.min(MAX_PAYLOAD_SIZE - 2); // -2 for FU indicator + header

                    let is_start = offset == 0;
                    let is_end = offset + payload_size >= nal_payload.len();

                    // FU Header: S | E | R | Type
                    let fu_header =
                        (if is_start { 0x80 } else { 0x00 }) |  // S bit
                        (if is_end { 0x40 } else { 0x00 }) |    // E bit
                        nal_type;                                // Type

                    // Build FU-A payload: FU indicator + FU header + NAL fragment
                    let mut fu_payload = Vec::with_capacity(2 + payload_size);
                    fu_payload.push(fu_indicator);
                    fu_payload.push(fu_header);
                    fu_payload.extend_from_slice(&nal_payload[offset..offset + payload_size]);

                    let seq_num = VIDEO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    let rtp_packet = RtpPacket {
                        header: RtpHeader {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker: is_end && is_last_nal_in_frame, // Mark last fragment of last NAL
                            payload_type: 102,
                            sequence_number: seq_num as u16,
                            timestamp: current_timestamp,
                            ssrc: 0,
                            ..Default::default()
                        },
                        payload: fu_payload.into(),
                    };

                    self._runtime.block_on(async {
                        track
                            .write_rtp(&rtp_packet)
                            .await
                            .map_err(|e| StreamError::Runtime(format!("Failed to write video RTP: {}", e)))
                    })?;

                    if counter == 0 && i == 0 && frag_count == 0 {
                        tracing::info!("[WebRTC] ✅ Successfully wrote first video RTP packet (FU-A mode, NAL size {} bytes, fragments ~{})",
                            sample.data.len(),
                            (sample.data.len() + MAX_PAYLOAD_SIZE - 1) / MAX_PAYLOAD_SIZE);
                    } else if counter % 30 == 0 && i == 0 && frag_count == 0 {
                        tracing::info!("[WebRTC] 📊 Video RTP packet #{} sent (FU-A mode, NAL size {} bytes)", counter, sample.data.len());
                    }

                    offset += payload_size;
                    frag_count += 1;
                }
            }
        }

        // Increment timestamp for next frame
        VIDEO_TIMESTAMP.fetch_add(timestamp_increment, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    /// Writes audio sample to the audio track.
    fn write_audio_sample(&mut self, sample: webrtc::media::Sample) -> Result<()> {
        let track = self
            .audio_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Audio track not initialized".into()))?;

        // Track first write and periodic telemetry
        static AUDIO_SAMPLE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static AUDIO_SEQ_NUM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static AUDIO_TIMESTAMP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let counter = AUDIO_SAMPLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if counter == 0 {
            tracing::info!("[WebRTC] 🎵 FIRST AUDIO WRITE after ICE Connected!");
            tracing::info!("[WebRTC]    Bytes: {}, Duration: {:?}",
                sample.data.len(),
                sample.duration);
        } else if counter % 50 == 0 {
            tracing::debug!("[TELEMETRY:AUDIO_SAMPLE_WRITE] sample_num={}, bytes={}, duration_ms={:?}",
                counter,
                sample.data.len(),
                sample.duration.as_millis());
        }

        use webrtc::rtp::packet::Packet as RtpPacket;
        use webrtc::rtp::header::Header as RtpHeader;

        let timestamp_increment = 960;

        let rtp_packet = RtpPacket {
            header: RtpHeader {
                version: 2,
                padding: false,
                extension: false,
                marker: false,
                payload_type: 111,  // Opus (registered as PT=111)
                sequence_number: AUDIO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u16,
                timestamp: AUDIO_TIMESTAMP.fetch_add(timestamp_increment, std::sync::atomic::Ordering::Relaxed),
                ssrc: 0,  // Will be set by track
                ..Default::default()
            },
            payload: sample.data,
        };

        let result = self._runtime.block_on(async {
            track
                .write_rtp(&rtp_packet)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to write audio RTP: {}", e)))
        });

        if let Err(ref e) = result {
            tracing::error!("[WebRTC] ❌ Failed to write audio RTP {}: {}", counter, e);
        } else if counter == 0 {
            tracing::info!("[WebRTC] ✅ Successfully wrote first audio RTP packet with PT=111");
        } else if counter % 50 == 0 {
            tracing::info!("[WebRTC] 📊 Audio RTP packet #{} sent (PT=111, {} bytes)", counter, rtp_packet.payload.len());
        }

        result.map(|_| ())
    }

    /// Gets RTCP statistics from the peer connection.
    fn get_stats(&self) -> Result<webrtc::stats::StatsReport> {
        self._runtime.block_on(async {
            Ok(self.peer_connection.get_stats().await)
        })
    }

    /// Closes the WebRTC session.
    fn close(&self) -> Result<()> {
        self._runtime.block_on(async {
            self.peer_connection
                .close()
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to close peer connection: {}", e)))
        })
    }
}

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
    gpu_context: Option<GpuContext>,  // Store for lazy encoder init

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

        tracing::info!("WebRtcWhipProcessor initialized (will create video encoder on first frames)");
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
            let ctx = self.ctx.as_ref().ok_or_else(|| {
                StreamError::Runtime("RuntimeContext not available".into())
            })?;
            self.video_encoder = Some(VideoToolboxH264Encoder::new(
                self.config.video.clone(),
                gpu_context,
                ctx
            )?);
            tracing::info!("VideoToolbox H.264 encoder initialized");
        }

        // 2. Set start time
        self.start_time_ns = Some(MediaClock::now().as_nanos() as i64);

        // 3. Initialize RTP timestamp calculators
        self.video_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            90000 // 90kHz for video
        ));
        self.audio_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            48000 // 48kHz for Opus
        ));

        // Install rustls crypto provider if needed
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|e| StreamError::Runtime(format!("Failed to install rustls crypto provider: {:?}", e)))?;
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
        let answer = whip_client.lock().unwrap().post_offer(&offer_with_bandwidth)?;
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

        let encoder = self.video_encoder.as_mut()
            .ok_or_else(|| StreamError::Configuration("Video encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;
        let samples = convert_video_to_samples(&encoded, self.config.video.fps)?;
        self.webrtc_session.as_mut().unwrap().write_video_samples(samples)?;

        Ok(())
    }

    fn process_audio_frame(&mut self, frame: &AudioFrame<2>) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        let encoder = self.audio_encoder.as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;
        let sample = convert_audio_to_sample(&encoded, self.config.audio.sample_rate)?;
        self.webrtc_session.as_mut().unwrap().write_audio_sample(sample)?;

        Ok(())
    }

    /// Collects and logs RTCP statistics from the WebRTC peer connection.
    /// Calculates bitrates from delta of bytes sent since last measurement.
    fn log_rtcp_stats(&mut self) -> Result<()> {
        let webrtc_session = self.webrtc_session.as_ref()
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

            let video_packets_delta = video_packets_sent.saturating_sub(self.last_video_packets_sent);
            let audio_packets_delta = audio_packets_sent.saturating_sub(self.last_audio_packets_sent);

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
                0, 0, 0, 1, 0x67, 0x42,  // SPS
                0, 0, 0, 1, 0x68, 0x43,  // PPS
                0, 0, 0, 1, 0x65, 0xAA,  // IDR
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
            data: vec![0xAA, 0xBB, 0xCC],  // No start codes
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
            sample_count: 960,  // 20ms @ 48kHz
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
            (480, 48000, 10.0),    // 10ms
            (960, 48000, 20.0),    // 20ms
            (1920, 48000, 40.0),   // 40ms
        ];

        for (sample_count, sample_rate, expected_ms) in test_cases {
            let encoded = EncodedAudioFrame {
                data: vec![0x00],
                timestamp_ns: 0,
                sample_count,
            };

            let sample = convert_audio_to_sample(&encoded, sample_rate).unwrap();
            let actual_ms = sample.duration.as_secs_f64() * 1000.0;

            assert!((actual_ms - expected_ms).abs() < 0.01,
                "Expected ~{}ms, got {}ms for {} samples @ {}Hz",
                expected_ms, actual_ms, sample_count, sample_rate);
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
            0, 0, 0, 1, 0x67, 0x42,  // SPS (4-byte start code)
            0, 0, 0, 1, 0x68, 0x43,  // PPS (4-byte start code)
            0, 0, 1, 0x65, 0xAA,     // IDR (3-byte start code)
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
            0, 0, 0, 1, 0x67, 0x11,  // 4-byte start code
            0, 0, 1, 0x68, 0x22,     // 3-byte start code
            0, 0, 0, 1, 0x65, 0x33,  // 4-byte start code
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
        data.extend_from_slice(&[0x67, 0x42, 0xC0, 0x1E]);  // Fake SPS

        // PPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x68, 0xCE, 0x3C, 0x80]);  // Fake PPS

        // IDR slice
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x10]);  // Fake IDR

        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);

        // Verify NAL unit types (first byte & 0x1F)
        assert_eq!(nals[0][0] & 0x1F, 0x07);  // SPS
        assert_eq!(nals[1][0] & 0x1F, 0x08);  // PPS
        assert_eq!(nals[2][0] & 0x1F, 0x05);  // IDR
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
        assert_ne!(calc1.rtp_base, calc2.rtp_base,
            "RTP base should be random, not deterministic");
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
        assert!((diff as i32 - 3000).abs() < 2, "Expected ~3000 ticks, got {}", diff);
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
        config.sample_rate = 44100;  // Not supported
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_err());
        let err = encoder.unwrap_err().to_string();
        assert!(err.contains("48kHz"));
    }

    #[test]
    fn test_opus_encoder_invalid_channels() {
        let mut config = AudioEncoderConfig::default();
        config.channels = 1;  // Mono not supported
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
            samples,
            0,      // timestamp_ns
            0,      // frame_number
            48000   // sample_rate
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
        let frame = AudioFrame::<2>::new(
            samples,
            0,
            0,
            48000
        );

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
            samples,
            0,
            0,
            44100  // Wrong sample rate
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
        let frame = AudioFrame::<2>::new(
            samples,
            timestamp_ns,
            42,
            48000
        );

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
            samples.push(sample);  // Left
            samples.push(sample);  // Right
        }

        let frame = AudioFrame::<2>::new(
            samples,
            0,
            0,
            48000
        );
        let encoded = encoder.encode(&frame).unwrap();

        // Encoded size should be reasonable (< 4KB for 20ms)
        assert!(encoded.data.len() > 10);  // At least some bytes
        assert!(encoded.data.len() < 4000);  // Not too large
    }

    #[test]
    fn test_opus_encode_multiple_frames() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Simulate encoding 10 frames (200ms of audio)
        for frame_num in 0..10 {
            let timestamp_ns = frame_num * 20_000_000;  // 20ms increments
            let samples = vec![0.1f32; 1920];
            let frame = AudioFrame::<2>::new(
                samples,
                timestamp_ns,
                frame_num as u64,
                48000
            );

            let encoded = encoder.encode(&frame).unwrap();
            assert!(encoded.data.len() > 0);
            assert_eq!(encoded.timestamp_ns, timestamp_ns);
            assert_eq!(encoded.sample_count, 960);
        }
    }
}
