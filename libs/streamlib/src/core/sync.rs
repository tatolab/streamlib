//! Timestamp Utilities for Multimodal Synchronization
//!
//! This module provides lightweight timestamp comparison utilities for
//! cross-modal synchronization (video ↔ audio, etc.).
//!
//! ## Architecture Philosophy
//!
//! For audio signal processing, **use dasp primitives**:
//! - Sample-and-hold: `dasp::ring_buffer::Bounded` + `Signal::buffered()`
//! - Mixing: `dasp::Signal::add_amp()` (lock-step, production quality)
//! - Frame operations: `dasp::Frame::add_amp()`, `scale_amp()`, etc.
//! - Sample rate conversion: `dasp::signal::Converter` or `rubato`
//!
//! For element synchronization, **use GStreamer model**:
//! - `StreamInput::read_latest()` provides sample-and-hold behavior
//! - Transformer elements with multiple input ports
//! - Runtime scheduling (Loop, Reactive, Callback modes)
//!
//! This module only provides **timestamp comparison** - the missing piece
//! that dasp and our element model don't cover.

use crate::core::{VideoFrame, AudioFrame};

/// Default tolerance for considering frames synchronized (in milliseconds)
///
/// 16.6ms ≈ one frame at 60 FPS - a reasonable default for real-time systems
pub const DEFAULT_SYNC_TOLERANCE_MS: f64 = 16.6;

/// Calculate timestamp delta between two timestamps in milliseconds
///
/// Returns absolute difference - always positive regardless of order.
///
/// # Example
/// ```
/// use streamlib::sync::timestamp_delta_ms;
///
/// let delta = timestamp_delta_ms(1_000_000_000, 1_000_000_500);
/// assert_eq!(delta, 500.0); // 500ms difference
/// ```
#[inline]
pub fn timestamp_delta_ms(timestamp_a_ns: i64, timestamp_b_ns: i64) -> f64 {
    let delta_ns = (timestamp_a_ns - timestamp_b_ns).abs();
    delta_ns as f64 / 1_000_000.0
}

/// Check if two timestamps are synchronized within tolerance
///
/// # Arguments
/// * `timestamp_a_ns` - First timestamp in nanoseconds
/// * `timestamp_b_ns` - Second timestamp in nanoseconds
/// * `tolerance_ms` - Maximum allowed difference in milliseconds
///
/// # Example
/// ```
/// use streamlib::sync::are_synchronized;
///
/// // Frames within 10ms are synchronized
/// assert!(are_synchronized(1_000_000_000, 1_000_005_000, 10.0));
///
/// // Frames 50ms apart are not synchronized with 10ms tolerance
/// assert!(!are_synchronized(1_000_000_000, 1_000_050_000, 10.0));
/// ```
#[inline]
pub fn are_synchronized(timestamp_a_ns: i64, timestamp_b_ns: i64, tolerance_ms: f64) -> bool {
    timestamp_delta_ms(timestamp_a_ns, timestamp_b_ns) <= tolerance_ms
}

/// Calculate timestamp delta between video frame and audio frame
///
/// Note: VideoFrame uses f64 seconds, AudioFrame uses i64 nanoseconds.
/// This function converts VideoFrame timestamp to nanoseconds for comparison.
#[inline]
pub fn video_audio_delta_ms(video: &VideoFrame, audio: &AudioFrame) -> f64 {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    timestamp_delta_ms(video_ns, audio.timestamp_ns)
}

/// Check if video and audio frames are synchronized
///
/// Uses default tolerance of 16.6ms (one 60 Hz frame).
#[inline]
pub fn video_audio_synchronized(video: &VideoFrame, audio: &AudioFrame) -> bool {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    are_synchronized(video_ns, audio.timestamp_ns, DEFAULT_SYNC_TOLERANCE_MS)
}

/// Check if video and audio frames are synchronized with custom tolerance
#[inline]
pub fn video_audio_synchronized_with_tolerance(
    video: &VideoFrame,
    audio: &AudioFrame,
    tolerance_ms: f64,
) -> bool {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    are_synchronized(video_ns, audio.timestamp_ns, tolerance_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_delta() {
        // Same timestamp = 0 delta
        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_000_000_000), 0.0);

        // 1ms difference
        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_001_000_000), 1.0);

        // Order doesn't matter (absolute value)
        assert_eq!(timestamp_delta_ms(1_001_000_000, 1_000_000_000), 1.0);

        // 16.6ms (one 60 Hz frame)
        let delta = timestamp_delta_ms(1_000_000_000, 1_016_600_000);
        assert!((delta - 16.6).abs() < 0.01);
    }

    #[test]
    fn test_are_synchronized() {
        // Within tolerance
        assert!(are_synchronized(1_000_000_000, 1_010_000_000, 20.0));

        // Exactly at tolerance boundary
        assert!(are_synchronized(1_000_000_000, 1_020_000_000, 20.0));

        // Exceeds tolerance
        assert!(!are_synchronized(1_000_000_000, 1_030_000_000, 20.0));
    }
}
