// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one clock a bag's timestamp is on.
//!
//! The engine's `MediaClock` domain, read and converted exactly as the engine
//! reads it, because a stamp this wheel writes onto a bag is compared against
//! stamps the engine wrote. This wheel links no engine crate, so it reads the
//! platform clock itself: `mach_absolute_time` on macOS, which stops while the
//! machine sleeps, and `CLOCK_MONOTONIC` everywhere else. macOS's own
//! `CLOCK_MONOTONIC` keeps counting through sleep, so it is off from the
//! engine's stamps by every second the machine has slept.

#[cfg(target_os = "macos")]
pub(crate) use mach_absolute_time_clock::monotonic_now_ns;

#[cfg(target_os = "macos")]
mod mach_absolute_time_clock {
    /// Nanoseconds on the engine's `MediaClock`.
    pub(crate) fn monotonic_now_ns() -> i64 {
        // SAFETY: `mach_absolute_time` takes no arguments and cannot fail.
        let host_time = unsafe { mach_absolute_time() };
        i64::try_from(mach_host_time_to_nanos(host_time, mach_timebase())).unwrap_or(i64::MAX)
    }

    /// The engine's own conversion, truncating in `u64`, so a reading taken here
    /// and one the engine takes from the same tick are the same nanosecond.
    pub(super) fn mach_host_time_to_nanos(
        host_time: u64,
        mach_timebase_ratio: &MachTimebaseInfo,
    ) -> u64 {
        host_time * mach_timebase_ratio.numer as u64 / mach_timebase_ratio.denom as u64
    }

    fn mach_timebase() -> &'static MachTimebaseInfo {
        // The timebase ratio is fixed for the life of the machine, so one query
        // serves every conversion.
        static MACH_TIMEBASE: std::sync::OnceLock<MachTimebaseInfo> = std::sync::OnceLock::new();
        MACH_TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: `info` is a valid stack slot, which is the call's only
            // requirement.
            unsafe { mach_timebase_info(&mut info) };
            info
        })
    }

    #[repr(C)]
    pub(super) struct MachTimebaseInfo {
        pub(super) numer: u32,
        pub(super) denom: u32,
    }

    #[link(name = "System", kind = "dylib")]
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }
}

/// Nanoseconds on the engine's `MediaClock`.
#[cfg(not(target_os = "macos"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_mach_tick_converts_to_nanoseconds_exactly_as_the_engine_converts_it() {
        // Apple Silicon's timebase: 24 MHz ticks, 125/3 ns each.
        use super::mach_absolute_time_clock::{MachTimebaseInfo, mach_host_time_to_nanos};

        let apple_silicon_timebase = MachTimebaseInfo {
            numer: 125,
            denom: 3,
        };

        assert_eq!(
            mach_host_time_to_nanos(24_000_000, &apple_silicon_timebase),
            1_000_000_000
        );
        // Truncated, not rounded: 41.67 ns reads as 41, as the engine reads it.
        assert_eq!(mach_host_time_to_nanos(1, &apple_silicon_timebase), 41);
    }

    #[test]
    fn successive_readings_never_go_backwards() {
        let mut previous = monotonic_now_ns();
        for _ in 0..1_000 {
            let next = monotonic_now_ns();
            assert!(next >= previous, "{next} ns read after {previous} ns");
            previous = next;
        }
    }
}
