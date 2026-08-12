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

    #[inline]
    fn host_time_to_nanos(host_time: u64) -> u64 {
        // The timebase ratio is fixed for the life of the machine, so one
        // query serves every conversion.
        static MACH_TIMEBASE: OnceLock<MachTimebaseInfo> = OnceLock::new();
        let info = MACH_TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: `info` is a valid stack slot, which is the call's only
            // requirement.
            unsafe { mach_timebase_info(&mut info) };
            info
        });
        host_time * info.numer as u64 / info.denom as u64
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}
