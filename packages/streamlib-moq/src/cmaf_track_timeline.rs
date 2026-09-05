// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The clock a CMAF track's boxes are written on.
//!
//! One place, because the timescale in the init segment's `mdhd` and the
//! `tfdt` in every fragment must agree exactly, and they are written by
//! different modules.

/// Nanoseconds, so a monotonic-nanosecond stamp lands in `tfdt` with no
/// rescale at all. A legal `u32`, which is what lets the subtraction stay
/// integral.
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

    /// Account one sample whose media states its own duration.
    ///
    /// RFC 8486 §4.1 makes an Opus sample's duration its decoded sample count,
    /// which the packet carries and the bag repeats — so nothing about when the
    /// bag arrived reaches the `trun`, and capture jitter is not written into
    /// the container as timing.
    pub(crate) fn place_sample_of_stated_duration(
        &mut self,
        stamp_ns: i64,
        duration_in_track_timescale: u32,
    ) -> CmafSamplePlacement {
        CmafSamplePlacement {
            decode_time: self.account_one_samples_stamp(stamp_ns),
            duration: duration_in_track_timescale,
        }
    }

    /// Account one sample whose duration only its successor could state.
    ///
    /// **Wire contract.** The duration written beside such a sample is the gap
    /// to its *predecessor*, so it is one sample late. ISO/IEC 14496-12
    /// §8.8.8.2 makes `sample_duration` the duration of *that* sample; a
    /// publisher that waited for the successor to measure it would add a whole
    /// frame of latency to every frame of a live broadcast, so the field is
    /// deliberately one sample late rather than on time and late by a frame.
    ///
    /// `tfdt` is the truth: every fragment carries its own, placed exactly from
    /// the sample's stamp, so a reader that decodes from `tfdt` — which is what
    /// a one-sample-per-fragment CMAF chunk is for — is never off. A reader
    /// that instead accumulates durations sees a hole after every cadence
    /// slowdown and an equal overlap after it, so a track placed this way is
    /// decode-time contiguous only while the cadence holds steady.
    pub(crate) fn place_sample_of_duration_measured_to_its_predecessor(
        &mut self,
        stamp_ns: i64,
    ) -> CmafSamplePlacement {
        let decode_time = self.account_one_samples_stamp(stamp_ns);
        CmafSamplePlacement {
            decode_time,
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

    /// Fold one sample's stamp into the track's epoch and predecessor gap,
    /// handing back the decode time it lands on.
    fn account_one_samples_stamp(&mut self, stamp_ns: i64) -> u64 {
        let epoch_ns = *self.epoch_ns.get_or_insert(stamp_ns);
        if let Some(newest) = self.newest_stamp_ns {
            let gap = stamp_ns.saturating_sub(newest);
            if gap > 0 {
                self.newest_gap_ns = Some(gap);
            }
        }
        self.newest_stamp_ns = Some(stamp_ns);

        let since_epoch_ns = stamp_ns.saturating_sub(epoch_ns).max(0);
        rescale_nanoseconds(since_epoch_ns, self.timescale_hz)
    }
}

/// What a predecessor-measured track's first sample claims, having no
/// predecessor to measure against. A thirtieth of a second, and a guess: this
/// wheel is never told the frame rate. Wrong for that one sample only, in a
/// field `tfdt` overrides for every fragment after it.
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
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(9_000_000_000)
                .decode_time,
            0
        );
    }

    #[test]
    fn later_samples_are_placed_by_their_distance_from_the_first() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_duration_measured_to_its_predecessor(9_000_000_000);
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(9_033_000_000)
                .decode_time,
            33_000_000,
            "the epoch is the track's own first stamp, so this is the gap"
        );
    }

    #[test]
    fn a_samples_duration_is_the_gap_to_the_one_before_it() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_duration_measured_to_its_predecessor(0);
        timeline.place_sample_of_duration_measured_to_its_predecessor(33_000_000);
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(66_000_000)
                .duration,
            33_000_000,
            "measuring against the next sample would cost a frame of latency"
        );
    }

    #[test]
    fn the_first_sample_claims_a_nominal_duration_rather_than_zero() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(0)
                .duration,
            (NOMINAL_FIRST_SAMPLE_DURATION_NS) as u32,
            "a zero-duration first sample makes a player show nothing at all"
        );
    }

    #[test]
    fn a_stamp_that_goes_backwards_does_not_place_a_sample_before_the_epoch() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_duration_measured_to_its_predecessor(1_000_000_000);
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(500_000_000)
                .decode_time,
            0
        );
    }

    #[test]
    fn a_gap_past_what_the_container_field_can_state_clamps_rather_than_wrapping() {
        // The video timescale is nanoseconds, so a `u32` duration runs out at
        // 4.295 s. A cast would turn a ten-second stall into 1.4 s.
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_duration_measured_to_its_predecessor(0);
        timeline.place_sample_of_duration_measured_to_its_predecessor(10_000_000_000);

        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(20_000_000_000)
                .duration,
            u32::MAX
        );
    }

    #[test]
    fn a_duration_never_rounds_down_to_zero() {
        let mut timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_duration_measured_to_its_predecessor(0);
        // A gap far below one tick of the track's own clock.
        assert_eq!(
            timeline
                .place_sample_of_duration_measured_to_its_predecessor(1)
                .duration,
            1
        );
    }

    /// The wart the predecessor-gap rule leaves, asserted rather than
    /// discovered: a cadence change writes a hole and then an equal overlap.
    /// Changing this test means changing the wire contract, which is an owner
    /// decision and not an implementation detail.
    #[test]
    fn a_cadence_change_leaves_a_hole_and_then_an_equal_overlap() {
        let mut timeline = CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ);
        let placements: Vec<CmafSamplePlacement> = [0, 33_000_000, 66_000_000, 166_000_000]
            .into_iter()
            .map(|stamp_ns| timeline.place_sample_of_duration_measured_to_its_predecessor(stamp_ns))
            .collect();

        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.decode_time)
                .collect::<Vec<_>>(),
            vec![0, 33_000_000, 66_000_000, 166_000_000],
            "`tfdt` is exact for every sample, which is what a reader decodes from"
        );
        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.duration)
                .collect::<Vec<_>>(),
            vec![
                NOMINAL_FIRST_SAMPLE_DURATION_NS as u32,
                33_000_000,
                33_000_000,
                100_000_000
            ],
            "each duration is the gap to the predecessor, so the 100 ms stall is charged to the \
             sample after it"
        );
        // The third sample claims [66 ms, 99 ms) and the fourth starts at
        // 166 ms — 67 ms of hole. The fourth then claims 100 ms, running to
        // 266 ms, over a successor a resumed 33 ms cadence would put at
        // 199 ms.
        assert_eq!(
            placements[3].decode_time - (placements[2].decode_time + placements[2].duration as u64),
            67_000_000,
            "an accumulating reader sees a hole here; a `tfdt`-placing one sees none"
        );
    }

    #[test]
    fn a_stated_duration_is_written_verbatim_however_the_bags_arrived() {
        let mut timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        // Stamps a live capture carries: 20 ms apart in intent, never in fact.
        let durations: Vec<u32> = [9_000_000_000i64, 9_020_500_000, 9_039_500_000]
            .into_iter()
            .map(|stamp_ns| {
                timeline
                    .place_sample_of_stated_duration(stamp_ns, 960)
                    .duration
            })
            .collect();

        assert_eq!(
            durations,
            vec![960, 960, 960],
            "the packet's own sample count, not the jitter between two arrivals"
        );
    }

    #[test]
    fn a_stated_duration_still_places_its_sample_by_its_stamp() {
        let mut timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        timeline.place_sample_of_stated_duration(9_000_000_000, 960);
        assert_eq!(
            timeline
                .place_sample_of_stated_duration(9_020_500_000, 960)
                .decode_time,
            984,
            "`tfdt` follows the stamp even where the duration does not, so a capture gap \
             stays visible in the decode times"
        );
    }
}
