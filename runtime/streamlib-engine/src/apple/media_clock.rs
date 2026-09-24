// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::OnceLock;
use std::time::Duration;

/// The machine's monotonic clock, the epoch every media timestamp carries.
pub struct MediaClock;

impl MediaClock {
    /// Current machine monotonic time, in the `mach_absolute_time` domain.
    #[inline]
    pub fn now() -> Duration {
        // SAFETY: `mach_absolute_time` takes no arguments and cannot fail.
        let host_time = unsafe { mach_absolute_time() };
        Duration::from_nanos(Self::host_time_to_nanos(host_time))
    }

    /// Current machine monotonic time in raw, unconverted mach ticks.
    #[inline]
    pub fn raw_timestamp() -> u64 {
        // SAFETY: `mach_absolute_time` takes no arguments and cannot fail.
        unsafe { mach_absolute_time() }
    }

    /// A reading in raw mach ticks, converted to the nanoseconds [`Self::now`]
    /// reports.
    #[inline]
    pub fn nanos_from_raw_timestamp(raw_host_ticks: u64) -> Duration {
        Duration::from_nanos(Self::host_time_to_nanos(raw_host_ticks))
    }

    /// The earliest raw mach tick whose [`Self::nanos_from_raw_timestamp`] is at
    /// or after `nanos` — the tick a deadline must be armed at to never fire early.
    #[inline]
    pub fn raw_timestamp_at_or_after_nanos(nanos: Duration) -> u64 {
        let mach_timebase_ratio = Self::mach_timebase();
        let raw_host_ticks = (nanos.as_nanos() * mach_timebase_ratio.denom as u128)
            .div_ceil(mach_timebase_ratio.numer as u128);
        raw_host_ticks.min(u64::MAX as u128) as u64
    }

    #[inline]
    fn host_time_to_nanos(host_time: u64) -> u64 {
        let mach_timebase_ratio = Self::mach_timebase();
        host_time * mach_timebase_ratio.numer as u64 / mach_timebase_ratio.denom as u64
    }

    #[inline]
    fn mach_timebase() -> &'static MachTimebaseInfo {
        // The timebase ratio is fixed for the life of the machine, so one
        // query serves every conversion.
        static MACH_TIMEBASE: OnceLock<MachTimebaseInfo> = OnceLock::new();
        MACH_TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: `info` is a valid stack slot, which is the call's only
            // requirement.
            unsafe { mach_timebase_info(&mut info) };
            info
        })
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[cfg(test)]
mod tests {
    use super::MediaClock;
    use std::time::Duration;

    #[test]
    fn a_tick_rounded_up_from_nanos_never_converts_back_to_an_earlier_nano() {
        let now_nanos = MediaClock::now().as_nanos() as u64;
        for offset_nanos in [0, 1, 7, 41, 42, 125, 999, 16_666_667, 1_000_000_001] {
            let target = Duration::from_nanos(now_nanos + offset_nanos);
            let raw_host_ticks = MediaClock::raw_timestamp_at_or_after_nanos(target);
            assert!(
                MediaClock::nanos_from_raw_timestamp(raw_host_ticks) >= target,
                "tick {raw_host_ticks} converts to before {target:?}"
            );
            assert!(
                MediaClock::nanos_from_raw_timestamp(raw_host_ticks - 1) < target,
                "tick {raw_host_ticks} is not the earliest at or after {target:?}"
            );
        }
    }
}
