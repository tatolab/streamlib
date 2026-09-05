// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one clock a bag's timestamp is on.
//!
//! `CLOCK_MONOTONIC`, read exactly as the wheel reads it, because a stamp this
//! wheel writes onto a bag is compared against stamps the engine wrote.

/// Nanoseconds on `CLOCK_MONOTONIC`.
pub(crate) fn monotonic_now_ns() -> i64 {
    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timespec` is a valid stack slot, and CLOCK_MONOTONIC exists on
    // every platform this wheel targets, so the call cannot fail here.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
    // Both fields are already `i64` on the 64-bit Linux targets this wheel
    // builds for. A 32-bit port would fail to compile right here, which is
    // the honest outcome for a port nobody has made.
    timespec
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(timespec.tv_nsec)
}
