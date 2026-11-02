// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! When a captured frame was taken, on the clock the runtime stamps every message with.

/// Nanoseconds on `CLOCK_MONOTONIC`, the clock `tatolab.stream.clock.monotonic_now_ns` reads on Linux.
pub fn monotonic_now_ns() -> i64 {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timespec` is a valid stack slot and `CLOCK_MONOTONIC` always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
    timespec
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(timespec.tv_nsec)
}

/// What a dequeued buffer's stamp says, and the stamp the frame is published with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedCaptureTimestamp {
    /// The driver stamped the buffer on the monotonic clock, at or before dequeue.
    FromTheDevice(i64),
    /// The driver's monotonic stamp lay ahead of dequeue (vivid stamps about
    /// 0.9 of a period early), so the dequeue instant stands in for it.
    ClampedToDequeue(i64),
    /// The stamp is on a clock the runtime cannot join, so the dequeue instant stands in for it.
    DequeueInstant(i64),
}

impl ResolvedCaptureTimestamp {
    /// The nanoseconds the frame is published with.
    pub fn timestamp_ns(self) -> i64 {
        match self {
            Self::FromTheDevice(ns) | Self::ClampedToDequeue(ns) | Self::DequeueInstant(ns) => ns,
        }
    }
}

/// Resolve a buffer's `flags` and `timestamp` against the dequeue instant.
///
/// Only `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` names the machine's monotonic clock.
pub fn resolve_capture_timestamp(
    buffer_flags: u32,
    timestamp_seconds: i64,
    timestamp_microseconds: i64,
    dequeue_instant_ns: i64,
) -> ResolvedCaptureTimestamp {
    const V4L2_BUF_FLAG_TIMESTAMP_MASK: u32 = 0x0000_e000;
    const V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC: u32 = 0x0000_2000;
    if buffer_flags & V4L2_BUF_FLAG_TIMESTAMP_MASK != V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC {
        return ResolvedCaptureTimestamp::DequeueInstant(dequeue_instant_ns);
    }
    let device_stamp_ns = timestamp_seconds
        .saturating_mul(1_000_000_000)
        .saturating_add(timestamp_microseconds.saturating_mul(1_000));
    if device_stamp_ns > dequeue_instant_ns {
        return ResolvedCaptureTimestamp::ClampedToDequeue(dequeue_instant_ns);
    }
    ResolvedCaptureTimestamp::FromTheDevice(device_stamp_ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONOTONIC: u32 = 0x2000;
    const UNKNOWN_CLOCK: u32 = 0x0000;

    #[test]
    fn a_monotonic_stamp_before_dequeue_is_the_capture_instant() {
        assert_eq!(
            resolve_capture_timestamp(MONOTONIC, 10, 500, 10_000_600_000),
            ResolvedCaptureTimestamp::FromTheDevice(10_000_500_000)
        );
    }

    #[test]
    fn a_monotonic_stamp_after_dequeue_is_clamped_to_it() {
        assert_eq!(
            resolve_capture_timestamp(MONOTONIC, 10, 900, 10_000_600_000),
            ResolvedCaptureTimestamp::ClampedToDequeue(10_000_600_000)
        );
    }

    #[test]
    fn a_stamp_on_an_unknown_clock_is_replaced_by_the_dequeue_instant() {
        assert_eq!(
            resolve_capture_timestamp(UNKNOWN_CLOCK, 10, 500, 42),
            ResolvedCaptureTimestamp::DequeueInstant(42)
        );
    }

    #[test]
    fn the_clock_reads_forward() {
        let earlier = monotonic_now_ns();
        assert!(monotonic_now_ns() >= earlier);
    }
}
