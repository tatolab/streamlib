// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's `MediaClock` domain on Linux, read without linking the engine.

/// Nanoseconds on `CLOCK_MONOTONIC`, the clock every block timestamp on this machine is on.
pub(crate) fn monotonic_now_ns() -> i64 {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timespec` is a valid stack slot and CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
    timespec
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(timespec.tv_nsec)
}
