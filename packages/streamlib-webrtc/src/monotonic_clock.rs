// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one clock a bag's timestamp is on.
//!
//! `CLOCK_MONOTONIC`, read exactly as the wheel reads it, because a stamp this
//! wheel writes onto a bag is compared against stamps the engine wrote.

/// Nanoseconds on `CLOCK_MONOTONIC`.
pub fn monotonic_now_ns() -> i64 {
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

/// Maps an RTP stream's own clock onto the monotonic one.
///
/// The first packet's arrival anchors the stream; every later stamp is the RTP
/// delta since that anchor, so jitter on the wire does not become jitter in the
/// stamps a decoder downstream reads.
pub struct RtpClockAnchoredToMonotonic {
    clock_rate_hz: i64,
    anchor: Option<RtpClockAnchor>,
}

struct RtpClockAnchor {
    monotonic_ns: i64,
    /// Accumulated rather than differenced against the anchor, so the 32-bit
    /// RTP timestamp's wrap — every 13 hours at 48 kHz, 6 at 90 kHz — is just
    /// another delta rather than a jump backwards.
    elapsed_ticks: i64,
    previous_rtp_timestamp: u32,
}

impl RtpClockAnchoredToMonotonic {
    pub fn new(clock_rate_hz: u32) -> Self {
        Self {
            clock_rate_hz: i64::from(clock_rate_hz),
            anchor: None,
        }
    }

    /// The monotonic stamp for a packet carrying `rtp_timestamp`.
    pub fn stamp_for(&mut self, rtp_timestamp: u32) -> i64 {
        let Some(anchor) = self.anchor.as_mut() else {
            let monotonic_ns = monotonic_now_ns();
            self.anchor = Some(RtpClockAnchor {
                monotonic_ns,
                elapsed_ticks: 0,
                previous_rtp_timestamp: rtp_timestamp,
            });
            return monotonic_ns;
        };

        let ticks_since_last =
            i64::from(rtp_timestamp.wrapping_sub(anchor.previous_rtp_timestamp) as i32);
        anchor.elapsed_ticks += ticks_since_last;
        anchor.previous_rtp_timestamp = rtp_timestamp;
        anchor.monotonic_ns + anchor.elapsed_ticks * 1_000_000_000 / self.clock_rate_hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO_CLOCK_RATE_HZ: u32 = 90_000;

    #[test]
    fn the_first_packet_is_stamped_at_its_own_arrival() {
        let mut clock = RtpClockAnchoredToMonotonic::new(VIDEO_CLOCK_RATE_HZ);
        let before = monotonic_now_ns();

        let stamp = clock.stamp_for(1_000);

        assert!(stamp >= before);
        assert!(stamp <= monotonic_now_ns());
    }

    #[test]
    fn later_stamps_advance_by_the_rtp_delta_not_by_arrival() {
        let mut clock = RtpClockAnchoredToMonotonic::new(VIDEO_CLOCK_RATE_HZ);
        let first = clock.stamp_for(1_000);

        // 3000 ticks at 90 kHz is one frame at 30 fps.
        let second = clock.stamp_for(4_000);
        let third = clock.stamp_for(7_000);

        assert_eq!(second - first, 33_333_333);
        assert_eq!(third - first, 66_666_666);
    }

    #[test]
    fn the_rtp_timestamp_wrapping_is_a_delta_and_not_a_jump_backwards() {
        let mut clock = RtpClockAnchoredToMonotonic::new(VIDEO_CLOCK_RATE_HZ);
        let before_the_wrap = clock.stamp_for(u32::MAX - 1_000);

        let after_the_wrap = clock.stamp_for(2_000);

        // 3001 ticks across the boundary, the same third of a frame it would
        // have been anywhere else in the sequence.
        assert_eq!(after_the_wrap - before_the_wrap, 33_344_444);
    }

    #[test]
    fn a_stream_that_pauses_and_resumes_keeps_one_anchor() {
        let mut clock = RtpClockAnchoredToMonotonic::new(48_000);
        let first = clock.stamp_for(0);

        let after_a_second = clock.stamp_for(48_000);

        assert_eq!(after_a_second - first, 1_000_000_000);
    }
}
