// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified timing context for processors.
//!
//! Provides a single monotonic clock that starts when the runtime starts.
//! All processors share this clock for coordinated animations and timing.

use crate::core::media_clock::MediaClock;

/// Shared timing context for all processors.
///
/// The clock starts when [`TimeContext::new`] is called (typically at runtime start).
/// Values are computed lazily on access from the monotonic [`MediaClock`].
#[derive(Debug, Clone)]
pub struct TimeContext {
    start_ns: i64,
}

impl TimeContext {
    /// Create a new TimeContext, capturing the current time as the start.
    pub fn new() -> Self {
        Self {
            start_ns: MediaClock::now().as_nanos() as i64,
        }
    }

    /// Nanoseconds since the runtime started.
    #[inline]
    pub fn elapsed_ns(&self) -> i64 {
        MediaClock::now().as_nanos() as i64 - self.start_ns
    }

    /// Seconds since the runtime started.
    #[inline]
    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed_ns() as f64 / 1_000_000_000.0
    }

    /// Raw monotonic clock value in nanoseconds.
    #[inline]
    pub fn now_ns(&self) -> i64 {
        MediaClock::now().as_nanos() as i64
    }
}

impl Default for TimeContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_elapsed_increases() {
        let ctx = TimeContext::new();
        let t1 = ctx.elapsed_ns();
        thread::sleep(Duration::from_millis(10));
        let t2 = ctx.elapsed_ns();
        assert!(t2 > t1, "elapsed should increase over time");
    }

    /// Bracketed by two `elapsed_ns` reads rather than compared against the
    /// sleep: the subject is the divide by a billion, and how long a sleep
    /// actually takes is the scheduler's business. Asserting an upper bound on
    /// it fails on a loaded machine while the conversion is perfectly correct.
    #[test]
    fn elapsed_secs_is_elapsed_ns_in_seconds() {
        let ctx = TimeContext::new();
        thread::sleep(Duration::from_millis(10));

        let before_ns = ctx.elapsed_ns();
        let secs = ctx.elapsed_secs();
        let after_ns = ctx.elapsed_ns();

        assert!(before_ns > 0, "the clock advanced across a sleep");
        assert!(
            secs >= before_ns as f64 / 1_000_000_000.0,
            "{secs} is before the read that preceded it ({before_ns} ns)"
        );
        assert!(
            secs <= after_ns as f64 / 1_000_000_000.0,
            "{secs} is after the read that followed it ({after_ns} ns)"
        );
    }

    #[test]
    fn test_now_ns_is_monotonic() {
        let ctx = TimeContext::new();
        let t1 = ctx.now_ns();
        let t2 = ctx.now_ns();
        assert!(t2 >= t1, "now_ns should be monotonic");
    }
}
