// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a publisher does with an object it cannot deliver on time.
//!
//! The decision is taken *before* the object reaches the transport, and that
//! is not a preference. `moq-transport`'s `SubgroupReader::next` hands the
//! forwarder every object already written to a subgroup before it ever looks
//! at whether the writer was closed, so an object handed over is an object
//! delivered — `SubgroupWriter::close` cannot pre-empt one, and the stream
//! reset it eventually produces arrives after the whole backlog has gone out.
//! The only reachable moment to shed work is therefore the one before
//! `write`, which is where this policy sits.
//!
//! The deadline reads two things. A sample's own stamp against the monotonic
//! clock says how late it arrived *at* this publisher — capture, encode, the
//! link into the helper. The uplink backlog says how far the transport is
//! *behind* it: the vendored `moq-transport` keeps the forwarder's cursor in
//! the subgroup's shared state, so the writer can read how many of its
//! objects have not left, and the oldest of them is stamped like any other.
//! `write` itself never blocks, so without that cursor a congested uplink
//! leaves every stamp untouched and the deadline blind to it. The same
//! reading decides what a group cut does with the group it supersedes: one
//! whose backlog is past the deadline is abandoned with a stream reset, so
//! the uplink stops carrying it, rather than finished.

use crate::moq_track_sample::MoqTrackSample;

/// How late an object may be before this publisher stops writing it.
///
/// Unconfigured is the shipped behaviour — every object is written however
/// late it is, and no group is ever abandoned — and is what the measured
/// baseline arm runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MoqPublisherDeliveryDeadline {
    longest_object_age_ns: Option<i64>,
}

/// What a track's open group says about the uplink at one instant: how much
/// of it no forwarder has written to the transport yet, and how old that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UplinkBacklogReading {
    /// `None` while nothing is forwarding the group — no subscription has
    /// reached it — which is not a backlog and never sheds.
    pub(crate) unforwarded_objects: Option<usize>,
    pub(crate) unforwarded_bytes: usize,
    /// The stamp of the oldest unforwarded object: the one the forwarder is
    /// on, however long it has been on it.
    pub(crate) oldest_unforwarded_stamp_ns: Option<i64>,
}

/// Why the deadline shed one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhyTheDeadlineSheds {
    /// An earlier object of its group was shed, so it is unusable whatever
    /// its own age.
    ItsGroupIsAlreadyBeingShed,
    /// It reached this publisher too late by its own stamp.
    ItsStampIsPastTheDeadline,
    /// The transport is still behind on an object of its group older than
    /// the deadline, so writing it would only lengthen that queue.
    TheUplinkBacklogIsPastTheDeadline,
}

/// What the deadline says about one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryDeadlineVerdict {
    /// Write it, and stop shedding this track's group if it was being shed.
    PublishIt,
    /// Do not write it, and shed the rest of its group too: a decoder cannot
    /// use a frame whose reference was shed, so the bytes after a shed object
    /// would spend uplink no decoder can turn into a picture. The shed ends at
    /// the next sync point, which is also what opens the next group.
    ShedItAndTheRestOfItsGroup(WhyTheDeadlineSheds),
}

impl MoqPublisherDeliveryDeadline {
    /// The deadline a caller configured in milliseconds, or none at all.
    pub(crate) fn of_optional_milliseconds(longest_object_age_ms: Option<u64>) -> Self {
        Self {
            longest_object_age_ns: longest_object_age_ms.map(|milliseconds| {
                i64::try_from(milliseconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000_000)
            }),
        }
    }

    /// What to do with one sample on a track that may already be shedding.
    ///
    /// A sync point is never shed, whatever its age or its track's backlog:
    /// the group a decoder re-enters at is the one that turns a late frame
    /// into a stall rather than a skip, and every Opus packet is one — which
    /// is what makes audio outrank video here as well as in the priority the
    /// group is opened at. That rule is per sample: it decides what is
    /// written, never what becomes of a group already written. A superseded
    /// group the uplink is behind on past the deadline is abandoned at the
    /// cut whether it carried video or audio — every Opus packet is a sync
    /// point, so an audio group is superseded the moment the next one opens,
    /// and its stale packets delivered late are a gap either way. A data
    /// object is never shed, and a data group never abandoned: each object
    /// stands alone, so a late one leaves no group undecodable, and whether
    /// one may be dropped at all is undecided — answered here by dropping
    /// none.
    pub(crate) fn verdict_for_one_sample(
        &self,
        sample: &MoqTrackSample,
        now_ns: i64,
        the_tracks_open_group_is_already_being_shed: bool,
        the_tracks_uplink_backlog: UplinkBacklogReading,
    ) -> DeliveryDeadlineVerdict {
        let sample = match sample {
            MoqTrackSample::DataObject(_) => return DeliveryDeadlineVerdict::PublishIt,
            MoqTrackSample::EncodedMedia(sample) => sample,
        };
        if sample.is_sync_point() {
            return DeliveryDeadlineVerdict::PublishIt;
        }
        let Some(longest_object_age_ns) = self.longest_object_age_ns else {
            return DeliveryDeadlineVerdict::PublishIt;
        };
        if the_tracks_open_group_is_already_being_shed {
            return DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::ItsGroupIsAlreadyBeingShed,
            );
        }
        if age_of(sample.timestamp_ns(), now_ns) > longest_object_age_ns {
            return DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::ItsStampIsPastTheDeadline,
            );
        }
        if self.the_uplink_backlog_is_past_the_deadline(the_tracks_uplink_backlog, now_ns) {
            return DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::TheUplinkBacklogIsPastTheDeadline,
            );
        }
        DeliveryDeadlineVerdict::PublishIt
    }

    /// Whether the transport is behind on an object older than the deadline.
    ///
    /// Read by its oldest unforwarded object's stamp — the one the forwarder
    /// is on — so a forwarder that has merely not woken yet for the newest
    /// object reads as on time, and one parked for longer than the deadline
    /// does not. Nothing forwarding is nothing behind.
    pub(crate) fn the_uplink_backlog_is_past_the_deadline(
        &self,
        reading: UplinkBacklogReading,
        now_ns: i64,
    ) -> bool {
        let Some(longest_object_age_ns) = self.longest_object_age_ns else {
            return false;
        };
        reading
            .oldest_unforwarded_stamp_ns
            .is_some_and(|stamp_ns| age_of(stamp_ns, now_ns) > longest_object_age_ns)
    }
}

/// Saturating, because a stamp ahead of the reading is an age of zero rather
/// than a wrap into lateness.
fn age_of(stamp_ns: i64, now_ns: i64) -> i64 {
    now_ns.saturating_sub(stamp_ns)
}

/// What one track's delivery deadline threw away, named by the link an
/// operator wired rather than by the MoQ track the container happened to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectsTheDeliveryDeadlineShedOnOneTrack {
    pub(crate) inbound_link_name: String,
    pub(crate) objects_shed: u64,
    pub(crate) bytes_shed: u64,
}

/// What the uplink backlog stands at and has cost on one track, named by its
/// link: the objects behind now, the sheds it began, and the superseded groups
/// abandoned for it with the objects and bytes those never delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UplinkBacklogOnOneTrack {
    pub(crate) inbound_link_name: String,
    /// `None` while nothing is forwarding the track's open group.
    pub(crate) unforwarded_objects: Option<u64>,
    pub(crate) sheds_the_backlog_began: u64,
    pub(crate) groups_abandoned: u64,
    pub(crate) objects_abandoned: u64,
    pub(crate) bytes_abandoned: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::encoded_media_sample::{EncodedMediaSample, EncodedVideoAccessUnit};
    use crate::moq_track_sample::DataTrackObject;

    const A_STAMP_NS: i64 = 5_000_000_000;

    fn a_deadline_of_100_ms() -> MoqPublisherDeliveryDeadline {
        MoqPublisherDeliveryDeadline::of_optional_milliseconds(Some(100))
    }

    fn a_delta_frame_stamped_at(timestamp_ns: i64) -> MoqTrackSample {
        a_frame_stamped_at(timestamp_ns, false)
    }

    fn a_sync_point_stamped_at(timestamp_ns: i64) -> MoqTrackSample {
        a_frame_stamped_at(timestamp_ns, true)
    }

    fn a_frame_stamped_at(timestamp_ns: i64, is_sync_point: bool) -> MoqTrackSample {
        MoqTrackSample::EncodedMedia(EncodedMediaSample::VideoAccessUnit(
            EncodedVideoAccessUnit {
                codec: "h264".to_owned(),
                annex_b_access_unit: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x41]),
                is_sync_point,
                group_index: 0,
                sequence_index: 0,
                width: 320,
                height: 180,
                color: None,
                timestamp_ns,
            },
        ))
    }

    fn a_data_object() -> MoqTrackSample {
        MoqTrackSample::DataObject(DataTrackObject {
            envelope_bytes: bytes::Bytes::from_static(b"\x81\xa3bag\x80"),
        })
    }

    /// Nothing is forwarding the track's group, or nothing is behind on it.
    fn no_backlog() -> UplinkBacklogReading {
        UplinkBacklogReading::default()
    }

    /// A forwarder still on an object stamped at `oldest_stamp_ns`, with
    /// `objects` of the group behind it.
    fn a_backlog_from(oldest_stamp_ns: i64, objects: usize) -> UplinkBacklogReading {
        UplinkBacklogReading {
            unforwarded_objects: Some(objects),
            unforwarded_bytes: objects * 1000,
            oldest_unforwarded_stamp_ns: Some(oldest_stamp_ns),
        }
    }

    #[test]
    fn a_sample_inside_the_deadline_is_published() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 99_000_000,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_sample_exactly_at_the_deadline_is_published() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 100_000_000,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_sample_past_the_deadline_is_shed_along_with_the_rest_of_its_group() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 100_000_001,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::ItsStampIsPastTheDeadline
            )
        );
    }

    #[test]
    fn a_sample_inside_the_deadline_is_still_shed_while_its_group_is_being_shed() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS,
                true,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::ItsGroupIsAlreadyBeingShed
            )
        );
    }

    #[test]
    fn a_sync_point_is_published_however_late_it_is_and_ends_the_shed() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_sync_point_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 60_000_000_000,
                true,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_data_object_is_published_however_old_the_reading_says_it_is_and_even_mid_shed() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_data_object(),
                i64::MAX,
                true,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn an_unconfigured_deadline_sheds_nothing_however_late_the_sample_is() {
        let unconfigured = MoqPublisherDeliveryDeadline::of_optional_milliseconds(None);

        assert_eq!(
            unconfigured.verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 60_000_000_000,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_stamp_ahead_of_the_reading_is_not_late() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                0,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_zero_millisecond_deadline_sheds_every_frame_that_is_not_a_sync_point() {
        let shed_everything = MoqPublisherDeliveryDeadline::of_optional_milliseconds(Some(0));

        assert_eq!(
            shed_everything.verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 1,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::ItsStampIsPastTheDeadline
            )
        );
        assert_eq!(
            shed_everything.verdict_for_one_sample(
                &a_sync_point_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 1,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_deadline_far_past_what_nanoseconds_hold_saturates_rather_than_wrapping() {
        let saturated = MoqPublisherDeliveryDeadline::of_optional_milliseconds(Some(u64::MAX));

        assert_eq!(
            saturated.verdict_for_one_sample(
                &a_delta_frame_stamped_at(0),
                i64::MAX,
                false,
                no_backlog()
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_stale_uplink_backlog_sheds_a_delta_frame_that_is_itself_on_time() {
        // The frame arrived on time; the forwarder is still on an object of
        // its group older than the deadline, so writing it would only queue.
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 10_000_000,
                false,
                a_backlog_from(A_STAMP_NS - 100_000_001, 3)
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup(
                WhyTheDeadlineSheds::TheUplinkBacklogIsPastTheDeadline
            )
        );
    }

    #[test]
    fn a_fresh_uplink_backlog_sheds_nothing_however_many_objects_are_behind() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 10_000_000,
                false,
                a_backlog_from(A_STAMP_NS - 50_000_000, 40)
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_group_nobody_is_forwarding_is_not_a_backlog() {
        let nobody_forwarding = UplinkBacklogReading {
            unforwarded_objects: None,
            unforwarded_bytes: 0,
            oldest_unforwarded_stamp_ns: None,
        };

        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 10_000_000,
                false,
                nobody_forwarding
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
        assert!(
            !a_deadline_of_100_ms()
                .the_uplink_backlog_is_past_the_deadline(nobody_forwarding, i64::MAX)
        );
    }

    #[test]
    fn a_sync_point_is_published_over_a_stale_uplink_backlog() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_sync_point_stamped_at(A_STAMP_NS),
                A_STAMP_NS,
                false,
                a_backlog_from(0, 60)
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn an_unconfigured_deadline_reads_no_backlog_as_stale() {
        let unconfigured = MoqPublisherDeliveryDeadline::of_optional_milliseconds(None);
        let stale = a_backlog_from(0, 60);

        assert_eq!(
            unconfigured.verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS,
                false,
                stale
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
        assert!(!unconfigured.the_uplink_backlog_is_past_the_deadline(stale, i64::MAX));
    }

    #[test]
    fn the_backlog_is_stale_by_its_oldest_objects_stamp_and_at_the_deadline_it_is_not() {
        let deadline = a_deadline_of_100_ms();

        assert!(!deadline.the_uplink_backlog_is_past_the_deadline(
            a_backlog_from(A_STAMP_NS, 1),
            A_STAMP_NS + 100_000_000
        ));
        assert!(deadline.the_uplink_backlog_is_past_the_deadline(
            a_backlog_from(A_STAMP_NS, 1),
            A_STAMP_NS + 100_000_001
        ));
    }
}
