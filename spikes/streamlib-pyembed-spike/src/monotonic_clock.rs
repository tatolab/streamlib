// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Raw `CLOCK_MONOTONIC` stamps, the one clock both spike arms share.
//!
//! The engine's `MediaClock` is `Instant::elapsed()` from a process-local
//! `OnceLock` (`runtime/streamlib-engine/src/core/media_clock.rs:12-17`), so two
//! processes report unrelated epochs. The subprocess baseline arm stamps in
//! Python with `time.clock_gettime_ns(time.CLOCK_MONOTONIC)`
//! (`sdk/streamlib-python/python/streamlib/clock.py:29-34`); stamping raw
//! `CLOCK_MONOTONIC` on the Rust side too is what makes the two arms comparable
//! by construction rather than by luck.
//!
//! `CLOCK_MONOTONIC` (not `_RAW`) is deliberate: it is NTP-slewed but never
//! stepped, it is what Python exposes, and slew over a 10-minute cell is far
//! below the microsecond resolution these measurements care about.

/// A `CLOCK_MONOTONIC` reading in nanoseconds since an unspecified epoch that is
/// fixed for the lifetime of the machine's boot.
pub type MonotonicNanoseconds = i64;

/// Read `CLOCK_MONOTONIC` as nanoseconds.
pub fn read_monotonic_clock_nanoseconds() -> MonotonicNanoseconds {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timespec` is a valid, fully-initialized out-parameter and
    // CLOCK_MONOTONIC is always available on Linux.
    let return_code = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
    debug_assert_eq!(return_code, 0, "clock_gettime(CLOCK_MONOTONIC) cannot fail");
    timespec.tv_sec * 1_000_000_000 + timespec.tv_nsec
}

/// Read the resolution `CLOCK_MONOTONIC` reports for itself, in nanoseconds.
/// A measurement whose spread approaches this number is reporting clock
/// granularity rather than the thing being measured.
pub fn read_monotonic_clock_resolution_nanoseconds() -> i64 {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: same contract as `clock_gettime` above.
    let return_code = unsafe { libc::clock_getres(libc::CLOCK_MONOTONIC, &mut timespec) };
    debug_assert_eq!(return_code, 0, "clock_getres(CLOCK_MONOTONIC) cannot fail");
    timespec.tv_sec * 1_000_000_000 + timespec.tv_nsec
}

/// Busy-wait until `deadline_nanoseconds`, yielding the CPU between polls.
///
/// The synthetic source paces itself with this rather than `thread::sleep`
/// because sleep overshoot at 60fps (16.6ms period) is the same order as the
/// latency being measured — an imprecise pacer would show up as source jitter
/// and be misread as pipeline latency.
pub fn spin_until_monotonic_deadline(deadline_nanoseconds: MonotonicNanoseconds) {
    // Sleep away the bulk, spin the last slice. 1ms is comfortably above the
    // worst observed `nanosleep` overshoot under SCHED_OTHER on this platform.
    const SPIN_THRESHOLD_NANOSECONDS: i64 = 1_000_000;
    loop {
        let remaining_nanoseconds = deadline_nanoseconds - read_monotonic_clock_nanoseconds();
        if remaining_nanoseconds <= 0 {
            return;
        }
        if remaining_nanoseconds > SPIN_THRESHOLD_NANOSECONDS {
            std::thread::sleep(std::time::Duration::from_nanos(
                (remaining_nanoseconds - SPIN_THRESHOLD_NANOSECONDS) as u64,
            ));
        } else {
            std::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock must be strictly non-decreasing across back-to-back reads —
    /// the property every latency number in the artifact depends on.
    #[test]
    fn monotonic_clock_never_goes_backwards() {
        let mut previous_reading = read_monotonic_clock_nanoseconds();
        for _ in 0..100_000 {
            let current_reading = read_monotonic_clock_nanoseconds();
            assert!(
                current_reading >= previous_reading,
                "CLOCK_MONOTONIC went backwards: {previous_reading} then {current_reading}"
            );
            previous_reading = current_reading;
        }
    }

    /// The pacer must not return early — a source that fires ahead of its
    /// deadline would inflate the measured inter-frame rate.
    #[test]
    fn spin_until_deadline_never_returns_early() {
        for _ in 0..200 {
            let deadline = read_monotonic_clock_nanoseconds() + 200_000;
            spin_until_monotonic_deadline(deadline);
            assert!(
                read_monotonic_clock_nanoseconds() >= deadline,
                "pacer returned before its deadline"
            );
        }
    }

    /// Resolution must be fine enough that microsecond-scale stage overhead is
    /// not quantization noise. Linux reports 1ns here; a platform reporting
    /// coarser than 1µs would invalidate the gate-5 sanity check.
    #[test]
    fn monotonic_clock_resolution_is_finer_than_one_microsecond() {
        let resolution_nanoseconds = read_monotonic_clock_resolution_nanoseconds();
        assert!(
            resolution_nanoseconds < 1_000,
            "CLOCK_MONOTONIC resolution {resolution_nanoseconds}ns is too coarse for this spike"
        );
    }
}
