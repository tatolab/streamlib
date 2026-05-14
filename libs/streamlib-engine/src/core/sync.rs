// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
pub fn sync_action(
    video_timestamp_ns: i64,
    audio_timestamp_ns: i64,
    tolerance_ms: f64,
) -> SyncAction {
    let drift_ns = video_timestamp_ns - audio_timestamp_ns;
    let drift_ms = drift_ns as f64 / 1_000_000.0;

    if drift_ms.abs() <= tolerance_ms {
        SyncAction::NoAction
    } else if drift_ms > 0.0 {
        SyncAction::DropVideoFrame
    } else {
        SyncAction::DuplicateVideoFrame
    }
}

#[inline]
pub fn sync_statistics(
    video_timestamp_ns: i64,
    audio_timestamp_ns: i64,
    tolerance_ms: f64,
) -> (f64, bool) {
    let drift_ns = video_timestamp_ns - audio_timestamp_ns;
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
    fn test_sync_action() {
        // NoAction within tolerance.
        assert_eq!(
            sync_action(1_000_000_000, 1_000_000_000, DEFAULT_SYNC_TOLERANCE_MS),
            SyncAction::NoAction
        );
        // Video ahead → drop.
        assert_eq!(
            sync_action(1_050_000_000, 1_000_000_000, DEFAULT_SYNC_TOLERANCE_MS),
            SyncAction::DropVideoFrame
        );
        // Video behind → duplicate.
        assert_eq!(
            sync_action(950_000_000, 1_000_000_000, DEFAULT_SYNC_TOLERANCE_MS),
            SyncAction::DuplicateVideoFrame
        );
    }

    #[test]
    fn test_sync_statistics() {
        let (drift_ms, is_synced) = sync_statistics(1_020_000_000, 1_000_000_000, DEFAULT_SYNC_TOLERANCE_MS);
        assert!((drift_ms - 20.0).abs() < 0.1);
        assert!(!is_synced);

        let (_drift_ms, is_synced) = sync_statistics(1_020_000_000, 1_000_000_000, 25.0);
        assert!(is_synced);
    }
}
