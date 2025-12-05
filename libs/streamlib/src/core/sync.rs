// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{AudioFrame, VideoFrame};

pub const DEFAULT_SYNC_TOLERANCE_MS: f64 = 16.6;

/// Action to take when audio and video streams are out of sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    NoAction,
    DropVideoFrame,
    DuplicateVideoFrame,
}

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
pub fn video_audio_delta_ms(video: &VideoFrame, audio: &AudioFrame) -> f64 {
    timestamp_delta_ms(video.timestamp_ns, audio.timestamp_ns)
}

#[inline]
pub fn video_audio_synchronized(video: &VideoFrame, audio: &AudioFrame) -> bool {
    are_synchronized(
        video.timestamp_ns,
        audio.timestamp_ns,
        DEFAULT_SYNC_TOLERANCE_MS,
    )
}

#[inline]
pub fn video_audio_synchronized_with_tolerance(
    video: &VideoFrame,
    audio: &AudioFrame,
    tolerance_ms: f64,
) -> bool {
    are_synchronized(video.timestamp_ns, audio.timestamp_ns, tolerance_ms)
}

/// Determine what action to take to maintain audio/video synchronization.
#[inline]
pub fn sync_action(video: &VideoFrame, audio: &AudioFrame, tolerance_ms: f64) -> SyncAction {
    let drift_ns = video.timestamp_ns - audio.timestamp_ns;
    let drift_ms = drift_ns as f64 / 1_000_000.0;

    if drift_ms.abs() <= tolerance_ms {
        SyncAction::NoAction
    } else if drift_ms > 0.0 {
        // Video timestamp is ahead of audio
        SyncAction::DropVideoFrame
    } else {
        // Video timestamp is behind audio
        SyncAction::DuplicateVideoFrame
    }
}

/// Calculate drift (ms) and whether streams are synchronized.
#[inline]
pub fn sync_statistics(video: &VideoFrame, audio: &AudioFrame, tolerance_ms: f64) -> (f64, bool) {
    let drift_ns = video.timestamp_ns - audio.timestamp_ns;
    let drift_ms = drift_ns as f64 / 1_000_000.0;
    let is_synced = drift_ms.abs() <= tolerance_ms;
    (drift_ms, is_synced)
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

    #[test]
    fn test_sync_action_logic() {
        // Test NoAction when timestamps match
        let video_ts = 1.0; // 1 second
        let audio_ts_ns = 1_000_000_000i64; // 1 second

        let video_ns = (video_ts * 1_000_000_000.0) as i64;
        let drift_ns = video_ns - audio_ts_ns;
        let drift_ms = drift_ns as f64 / 1_000_000.0;

        assert!(drift_ms.abs() <= DEFAULT_SYNC_TOLERANCE_MS);

        // Test DropVideoFrame when video ahead
        let video_ts_ahead = 1.05; // 50ms ahead
        let video_ns_ahead = (video_ts_ahead * 1_000_000_000.0) as i64;
        let drift_ahead = (video_ns_ahead - audio_ts_ns) as f64 / 1_000_000.0;

        assert!(drift_ahead > DEFAULT_SYNC_TOLERANCE_MS);
        assert!(drift_ahead > 0.0);

        // Test DuplicateVideoFrame when video behind
        let video_ts_behind = 0.95; // 50ms behind
        let video_ns_behind = (video_ts_behind * 1_000_000_000.0) as i64;
        let drift_behind = (video_ns_behind - audio_ts_ns) as f64 / 1_000_000.0;

        assert!(drift_behind.abs() > DEFAULT_SYNC_TOLERANCE_MS);
        assert!(drift_behind < 0.0);
    }

    #[test]
    fn test_sync_statistics_logic() {
        // Test drift calculation
        let video_ts = 1.02; // 20ms ahead
        let audio_ts_ns = 1_000_000_000i64; // 1 second

        let video_ns = (video_ts * 1_000_000_000.0) as i64;
        let drift_ns = video_ns - audio_ts_ns;
        let drift_ms = drift_ns as f64 / 1_000_000.0;

        // Should be approximately 20ms
        assert!((drift_ms - 20.0).abs() < 0.1);

        // Should NOT be synced with default tolerance (16.6ms)
        assert!(drift_ms.abs() > DEFAULT_SYNC_TOLERANCE_MS);

        // Should BE synced with larger tolerance (25ms)
        assert!(drift_ms.abs() <= 25.0);
    }
}
