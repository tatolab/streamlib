
use crate::core::{VideoFrame, AudioFrame};

pub const DEFAULT_SYNC_TOLERANCE_MS: f64 = 16.6;

#[inline]
pub fn timestamp_delta_ms(timestamp_a_ns: i64, timestamp_b_ns: i64) -> f64 {
    let delta_ns = (timestamp_a_ns - timestamp_b_ns).abs();
    delta_ns as f64 / 1_000_000.0
}

#[inline]
pub fn are_synchronized(timestamp_a_ns: i64, timestamp_b_ns: i64, tolerance_ms: f64) -> bool {
    timestamp_delta_ms(timestamp_a_ns, timestamp_b_ns) <= tolerance_ms
}

#[inline]
pub fn video_audio_delta_ms<const CHANNELS: usize>(video: &VideoFrame, audio: &AudioFrame<CHANNELS>) -> f64 {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    timestamp_delta_ms(video_ns, audio.timestamp_ns)
}

#[inline]
pub fn video_audio_synchronized<const CHANNELS: usize>(video: &VideoFrame, audio: &AudioFrame<CHANNELS>) -> bool {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    are_synchronized(video_ns, audio.timestamp_ns, DEFAULT_SYNC_TOLERANCE_MS)
}

#[inline]
pub fn video_audio_synchronized_with_tolerance<const CHANNELS: usize>(
    video: &VideoFrame,
    audio: &AudioFrame<CHANNELS>,
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
        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_000_000_000), 0.0);

        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_001_000_000), 1.0);

        assert_eq!(timestamp_delta_ms(1_001_000_000, 1_000_000_000), 1.0);

        let delta = timestamp_delta_ms(1_000_000_000, 1_016_600_000);
        assert!((delta - 16.6).abs() < 0.01);
    }

    #[test]
    fn test_are_synchronized() {
        assert!(are_synchronized(1_000_000_000, 1_010_000_000, 20.0));

        assert!(are_synchronized(1_000_000_000, 1_020_000_000, 20.0));

        assert!(!are_synchronized(1_000_000_000, 1_030_000_000, 20.0));
    }
}
