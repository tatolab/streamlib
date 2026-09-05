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
//! A sample is late by its own stamp against the monotonic clock, never by a
//! queue depth: a queue depth measures the uplink, and what a viewer
//! experiences is how old the picture is.

use crate::encoded_media_sample::EncodedMediaSample;

/// How late an object may be before this publisher stops writing it.
///
/// Unconfigured is the shipped behaviour — every object is written however
/// late it is — and is what the measured baseline arm runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MoqPublisherDeliveryDeadline {
    longest_object_age_ns: Option<i64>,
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
    ShedItAndTheRestOfItsGroup,
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
    /// A sync point is never shed, whatever its age: the group a decoder
    /// re-enters at is the one that turns a late frame into a stall rather
    /// than a skip, and every Opus packet is one — which is what makes audio
    /// outrank video here as well as in the priority the group is opened at.
    pub(crate) fn verdict_for_one_sample(
        &self,
        sample: &EncodedMediaSample,
        now_ns: i64,
        the_tracks_open_group_is_already_being_shed: bool,
    ) -> DeliveryDeadlineVerdict {
        if sample.is_sync_point() {
            return DeliveryDeadlineVerdict::PublishIt;
        }
        let Some(longest_object_age_ns) = self.longest_object_age_ns else {
            return DeliveryDeadlineVerdict::PublishIt;
        };
        if the_tracks_open_group_is_already_being_shed {
            return DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup;
        }
        // Saturating, because a stamp ahead of the reading is an age of zero
        // rather than a wrap into lateness.
        let age_ns = now_ns.saturating_sub(sample.timestamp_ns());
        if age_ns > longest_object_age_ns {
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup
        } else {
            DeliveryDeadlineVerdict::PublishIt
        }
    }
}

/// What one track's delivery deadline threw away, named by the link an
/// operator wired rather than by the MoQ track the container happened to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectsTheDeliveryDeadlineShedOnOneTrack {
    pub(crate) inbound_link_name: String,
    pub(crate) objects_shed: u64,
    pub(crate) bytes_shed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::encoded_media_sample::EncodedVideoAccessUnit;

    const A_STAMP_NS: i64 = 5_000_000_000;

    fn a_deadline_of_100_ms() -> MoqPublisherDeliveryDeadline {
        MoqPublisherDeliveryDeadline::of_optional_milliseconds(Some(100))
    }

    fn a_delta_frame_stamped_at(timestamp_ns: i64) -> EncodedMediaSample {
        a_frame_stamped_at(timestamp_ns, false)
    }

    fn a_sync_point_stamped_at(timestamp_ns: i64) -> EncodedMediaSample {
        a_frame_stamped_at(timestamp_ns, true)
    }

    fn a_frame_stamped_at(timestamp_ns: i64, is_sync_point: bool) -> EncodedMediaSample {
        EncodedMediaSample::VideoAccessUnit(EncodedVideoAccessUnit {
            codec: "h264".to_owned(),
            annex_b_access_unit: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x41]),
            is_sync_point,
            group_index: 0,
            sequence_index: 0,
            width: 320,
            height: 180,
            color: None,
            timestamp_ns,
        })
    }

    #[test]
    fn a_sample_inside_the_deadline_is_published() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 99_000_000,
                false
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
                false
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
                false
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup
        );
    }

    #[test]
    fn a_sample_inside_the_deadline_is_still_shed_while_its_group_is_being_shed() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_delta_frame_stamped_at(A_STAMP_NS),
                A_STAMP_NS,
                true
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup
        );
    }

    #[test]
    fn a_sync_point_is_published_however_late_it_is_and_ends_the_shed() {
        assert_eq!(
            a_deadline_of_100_ms().verdict_for_one_sample(
                &a_sync_point_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 60_000_000_000,
                true
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
                false
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
                false
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
                false
            ),
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup
        );
        assert_eq!(
            shed_everything.verdict_for_one_sample(
                &a_sync_point_stamped_at(A_STAMP_NS),
                A_STAMP_NS + 1,
                false
            ),
            DeliveryDeadlineVerdict::PublishIt
        );
    }

    #[test]
    fn a_deadline_far_past_what_nanoseconds_hold_saturates_rather_than_wrapping() {
        let saturated = MoqPublisherDeliveryDeadline::of_optional_milliseconds(Some(u64::MAX));

        assert_eq!(
            saturated.verdict_for_one_sample(&a_delta_frame_stamped_at(0), i64::MAX, false),
            DeliveryDeadlineVerdict::PublishIt
        );
    }
}
