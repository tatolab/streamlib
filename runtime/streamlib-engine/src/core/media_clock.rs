// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[cfg(target_os = "macos")]
pub use crate::apple::media_clock::MediaClock;

#[cfg(not(target_os = "macos"))]
pub struct MediaClock;

#[cfg(not(target_os = "macos"))]
impl MediaClock {
    /// Current machine monotonic time, in the kernel's `CLOCK_MONOTONIC` domain.
    #[inline]
    pub fn now() -> std::time::Duration {
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `timespec` is a valid stack slot; CLOCK_MONOTONIC exists on
        // every platform the engine targets, so the call cannot fail with
        // these arguments.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
        std::time::Duration::new(timespec.tv_sec as u64, timespec.tv_nsec as u32)
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::MediaClock;

    fn clock_gettime_monotonic_ns() -> u128 {
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: same contract as `MediaClock::now`.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timespec) };
        timespec.tv_sec as u128 * 1_000_000_000 + timespec.tv_nsec as u128
    }

    #[test]
    fn now_lands_in_the_kernel_monotonic_domain() {
        let before = clock_gettime_monotonic_ns();
        let sampled = MediaClock::now().as_nanos();
        let after = clock_gettime_monotonic_ns();

        assert!(
            before <= sampled && sampled <= after,
            "MediaClock::now() ({sampled} ns) fell outside the CLOCK_MONOTONIC \
             bracket [{before}, {after}] — it is not reading the machine's clock"
        );
    }

    #[test]
    fn successive_reads_never_go_backwards() {
        let mut previous = MediaClock::now();
        for _ in 0..1_000 {
            let current = MediaClock::now();
            assert!(
                current >= previous,
                "MediaClock::now() went backwards: {previous:?} then {current:?}"
            );
            previous = current;
        }
    }
}
