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

    #[test]
    fn test_elapsed_secs_conversion() {
        let ctx = TimeContext::new();
        thread::sleep(Duration::from_millis(100));
        let secs = ctx.elapsed_secs();
        assert!((0.09..0.2).contains(&secs), "should be ~0.1 seconds");
    }

    #[test]
    fn test_now_ns_is_monotonic() {
        let ctx = TimeContext::new();
        let t1 = ctx.now_ns();
        let t2 = ctx.now_ns();
        assert!(t2 >= t1, "now_ns should be monotonic");
    }
}
