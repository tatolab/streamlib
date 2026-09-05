// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The clock a CMAF track's boxes are written on.
//!
//! One place, because the timescale in the init segment's `mdhd` and the
//! `tfdt` in every fragment must agree exactly, and they are written by
//! different modules.

/// Nanoseconds, so a monotonic-nanosecond stamp lands in `tfdt` with no
/// rescale at all. A legal `u32`, which is what lets the subtraction stay
/// integral — the engine's own writer made the same call for the same reason.
pub(crate) const VIDEO_TRACK_TIMESCALE_HZ: u32 = 1_000_000_000;

/// Opus's own clock, and the only rate an `OpusEncoder` bag carries. Writing
/// an Opus track on any other timescale makes `sample_count` a division.
pub(crate) const OPUS_TRACK_TIMESCALE_HZ: u32 = 48_000;

/// What one track's fragments are placed against.
///
/// The epoch is that track's own first stamp, so its first fragment sits at
/// `tfdt = 0`. Tracks are not aligned to a shared epoch the way a recording's
/// are: a MoQ subscriber may join at any moment and receives whatever each
/// track's live group holds, so a cross-track epoch it never saw would place
/// every fragment it does see at a meaningless offset. Alignment between the
/// audio and video a subscriber receives comes from the bags' own stamps,
/// which the `streamlib_bag` container carries and which CMAF's decode times
/// preserve as differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CmafTrackTimeline {
    timescale_hz: u32,
    epoch_ns: Option<i64>,
    newest_stamp_ns: Option<i64>,
    /// The gap between the two most recent stamps, which is the only honest
    /// duration a publisher that cannot see the next sample has.
    newest_gap_ns: Option<i64>,
}

impl CmafTrackTimeline {
    pub(crate) fn on(timescale_hz: u32) -> Self {
        Self {
            timescale_hz,
            epoch_ns: None,
            newest_stamp_ns: None,
            newest_gap_ns: None,
        }
    }

    pub(crate) fn timescale_hz(self) -> u32 {
        self.timescale_hz
    }

    /// Account one sample, handing back its decode time and the duration to
    /// write beside it.
    ///
    /// The duration is the gap to the *previous* sample, not the next: a
    /// publisher that waited for the next sample to measure the current one's
    /// duration would add a frame of latency to a live stream. The decode time
    /// is exact regardless, so a player that reads `tfdt` — which is what a
    /// one-sample-per-fragment CMAF chunk is for — is never off.
    pub(crate) fn place(&mut self, stamp_ns: i64) -> CmafSamplePlacement {
        let epoch_ns = *self.epoch_ns.get_or_insert(stamp_ns);
        if let Some(newest) = self.newest_stamp_ns {
            let gap = stamp_ns.saturating_sub(newest);
            if gap > 0 {
                self.newest_gap_ns = Some(gap);
            }
        }
        self.newest_stamp_ns = Some(stamp_ns);

        let since_epoch_ns = stamp_ns.saturating_sub(epoch_ns).max(0);
        CmafSamplePlacement {
            decode_time: rescale_nanoseconds(since_epoch_ns, self.timescale_hz),
            // Clamped, not truncated: on the nanosecond video timescale a
            // `u32` runs out at 4.295 s, so an upstream stall longer than that
            // would wrap a five-second gap into a seven-hundred-millisecond
            // one. `tfdt` carries the exact decode time regardless, so
            // stopping at the field's ceiling is the honest failure.
            duration: u32::try_from(
                rescale_nanoseconds(
                    self.newest_gap_ns
                        .unwrap_or(NOMINAL_FIRST_SAMPLE_DURATION_NS),
                    self.timescale_hz,
                )
                .max(1),
            )
            .unwrap_or(u32::MAX),
        }
    }
}

/// What the first sample of a track claims, having no predecessor to measure
/// against. A thirtieth of a second: wrong only for that one sample, and only
/// in a field `tfdt` overrides for every fragment after it.
const NOMINAL_FIRST_SAMPLE_DURATION_NS: i64 = 1_000_000_000 / 30;

/// Where one sample sits and how long it claims to last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CmafSamplePlacement {
    pub(crate) decode_time: u64,
    pub(crate) duration: u32,
}

/// Nanoseconds into a track's own timescale, rounding to nearest.
pub(crate) fn rescale_nanoseconds(nanoseconds: i64, timescale_hz: u32) -> u64 {
    if nanoseconds <= 0 {
        return 0;
    }
    let nanoseconds = nanoseconds as u128;
    let timescale = u128::from(timescale_hz);
    let half = 500_000_000u128;
    ((nanoseconds * timescale + half) / 1_000_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nanosecond_timescale_carries_a_stamp_across_unchanged() {
        assert_eq!(
            rescale_nanoseconds(1_234_567_891, VIDEO_TRACK_TIMESCALE_HZ),
            1_234_567_891
        );
    }

    #[test]
    fn opus_rescaling_lands_on_whole_samples() {
        // 20 ms at 48 kHz is exactly 960 samples, the Opus frame every
        // encoder in this tree mints.
        assert_eq!(
            rescale_nanoseconds(20_000_000, OPUS_TRACK_TIMESCALE_HZ),
            960
        );
    }

    #[test]
    fn the_first_sample_of_a_track_sits_at_zero() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        assert_eq!(timeline.place(9_000_000_000).decode_time, 0);
    }

    #[test]
    fn later_samples_are_placed_by_their_distance_from_the_first() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place(9_000_000_000);
        assert_eq!(
            timeline.place(9_033_000_000).decode_time,
            33_000_000,
            "the epoch is the track's own first stamp, so this is the gap"
        );
    }

    #[test]
    fn a_samples_duration_is_the_gap_to_the_one_before_it() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place(0);
        timeline.place(33_000_000);
        assert_eq!(
            timeline.place(66_000_000).duration,
            33_000_000,
            "measuring against the next sample would cost a frame of latency"
        );
    }

    #[test]
    fn the_first_sample_claims_a_nominal_duration_rather_than_zero() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        assert_eq!(
            timeline.place(0).duration,
            (NOMINAL_FIRST_SAMPLE_DURATION_NS) as u32,
            "a zero-duration first sample makes a player show nothing at all"
        );
    }

    #[test]
    fn a_stamp_that_goes_backwards_does_not_place_a_sample_before_the_epoch() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place(1_000_000_000);
        assert_eq!(timeline.place(500_000_000).decode_time, 0);
    }

    #[test]
    fn a_gap_past_what_the_container_field_can_state_clamps_rather_than_wrapping() {
        // The video timescale is nanoseconds, so a `u32` duration runs out at
        // 4.295 s. A cast would turn a ten-second stall into 1.4 s.
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place(0);
        timeline.place(10_000_000_000);

        assert_eq!(timeline.place(20_000_000_000).duration, u32::MAX);
    }

    #[test]
    fn a_duration_never_rounds_down_to_zero() {
        let mut timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        timeline.place(0);
        // A gap far below one tick of the track's own clock.
        assert_eq!(timeline.place(1).duration, 1);
    }
}
