// RTP Utilities
//
// Provides RTP timestamp calculation and sample conversion for WebRTC streaming.

use crate::core::{StreamError, Result};
use crate::apple::videotoolbox::{EncodedVideoFrame, parse_nal_units};
use crate::core::streaming::opus::EncodedAudioFrame;
use bytes::Bytes;
use std::time::Duration;

// ============================================================================
// RTP SAMPLE CONVERSION
// ============================================================================

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
pub fn convert_video_to_samples(
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
pub fn convert_audio_to_sample(
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
pub struct RtpTimestampCalculator {
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
    pub fn new(start_time_ns: i64, clock_rate: u32) -> Self {
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
    pub fn calculate(&self, timestamp_ns: i64) -> u32 {
        let elapsed_ns = timestamp_ns - self.start_time_ns;
        let elapsed_ticks = (elapsed_ns as i128 * self.clock_rate as i128) / 1_000_000_000;
        self.rtp_base.wrapping_add(elapsed_ticks as u32)
    }
}
