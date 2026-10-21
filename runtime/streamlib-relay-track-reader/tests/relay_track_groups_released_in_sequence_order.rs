// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The relay-track reader against an in-memory track of one-object groups
//! stamped 20 ms apart, the way per-block audio rides a relay.

use std::time::Duration;

use streamlib_relay_track_reader::{
    RelayTrackGroupsReleasedInSequenceOrder, ReleasedRelayTrackGroup,
};

const MILLISECONDS_BETWEEN_GROUPS: u64 = 20;
const ONE_HUNDRED_MILLISECONDS: Duration = Duration::from_millis(100);
/// How long a read that must release nothing is given before it is judged
/// to be holding.
const A_HOLDING_READ_IS_GIVEN: Duration = Duration::from_millis(50);

/// A track whose publisher writes group `n` stamped `20 n` ms, read by a
/// [`RelayTrackGroupsReleasedInSequenceOrder`].
struct InMemoryTrackOfStampedGroups {
    _broadcast: moq_net::broadcast::Producer,
    track: moq_net::track::Producer,
}

impl InMemoryTrackOfStampedGroups {
    fn new() -> Self {
        let broadcast = moq_net::broadcast::Info::new().produce();
        let track = broadcast
            .create_track("audio", moq_net::track::Info::default())
            .unwrap();
        Self {
            _broadcast: broadcast,
            track,
        }
    }

    async fn read_with_max_age(
        &self,
        max_age: Duration,
    ) -> RelayTrackGroupsReleasedInSequenceOrder {
        RelayTrackGroupsReleasedInSequenceOrder::subscribe(&self.track.consume(), max_age)
            .await
            .unwrap()
    }

    fn write_the_group(&self, sequence: u64) {
        self.write_the_group_stamped(sequence, sequence * MILLISECONDS_BETWEEN_GROUPS);
    }

    fn write_the_group_stamped(&self, sequence: u64, timestamp_ms: u64) {
        let group = self.open_the_group_stamped(sequence, timestamp_ms);
        group.finish().unwrap();
    }

    /// Group `sequence` with its one object written and its stream left open.
    fn open_the_group(&self, sequence: u64) -> moq_net::group::Producer {
        self.open_the_group_stamped(sequence, sequence * MILLISECONDS_BETWEEN_GROUPS)
    }

    fn open_the_group_stamped(&self, sequence: u64, timestamp_ms: u64) -> moq_net::group::Producer {
        let mut group = self.track.create_group(sequence.into()).unwrap();
        group
            .write_frame(
                moq_net::Timestamp::from_millis(timestamp_ms).unwrap(),
                the_payload_of(sequence),
            )
            .unwrap();
        group
    }

    fn write_the_groups(&self, sequences: impl IntoIterator<Item = u64>) {
        for sequence in sequences {
            self.write_the_group(sequence);
        }
    }
}

fn the_payload_of(sequence: u64) -> Vec<u8> {
    format!("block {sequence}").into_bytes()
}

/// What a release is, in a form a test compares: the group's sequence and
/// the payloads of its objects, or a count given up.
#[derive(Debug, PartialEq, Eq)]
enum Released {
    InSequence {
        sequence: u64,
        payloads: Vec<Vec<u8>>,
    },
    GivenUp {
        groups: u64,
    },
    TrackEnded,
}

fn in_sequence(sequence: u64) -> Released {
    Released::InSequence {
        sequence,
        payloads: vec![the_payload_of(sequence)],
    }
}

async fn the_next_release_of(reader: &mut RelayTrackGroupsReleasedInSequenceOrder) -> Released {
    let released = tokio::time::timeout(Duration::from_secs(5), reader.next_released_group())
        .await
        .expect("the reader releases within 5 s")
        .expect("the track is live");
    match released {
        None => Released::TrackEnded,
        Some(ReleasedRelayTrackGroup::GivenUp { groups }) => Released::GivenUp { groups },
        Some(ReleasedRelayTrackGroup::InSequence(mut group)) => {
            let sequence = group.sequence;
            let mut payloads = Vec::new();
            while let Some(object) = group.read_frame().await.expect("the group reads whole") {
                payloads.push(object.payload.to_vec());
            }
            Released::InSequence { sequence, payloads }
        }
    }
}

async fn the_next_releases_of(
    reader: &mut RelayTrackGroupsReleasedInSequenceOrder,
    count: usize,
) -> Vec<Released> {
    let mut releases = Vec::new();
    for _ in 0..count {
        releases.push(the_next_release_of(reader).await);
    }
    releases
}

async fn assert_the_reader_holds(reader: &mut RelayTrackGroupsReleasedInSequenceOrder) {
    assert!(
        tokio::time::timeout(A_HOLDING_READ_IS_GIVEN, reader.next_released_group())
            .await
            .is_err(),
        "the reader holds at the missing group"
    );
}

/// The track, read from its first group 0, so later writes land after the join.
async fn a_track_read_from_its_first_group(
    max_age: Duration,
) -> (
    InMemoryTrackOfStampedGroups,
    RelayTrackGroupsReleasedInSequenceOrder,
) {
    let track = InMemoryTrackOfStampedGroups::new();
    let mut reader = track.read_with_max_age(max_age).await;
    track.write_the_group(0);
    assert_eq!(the_next_release_of(&mut reader).await, in_sequence(0));
    (track, reader)
}

#[tokio::test]
async fn a_burst_of_complete_groups_is_released_whole_and_in_sequence() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_groups(1..=5);
    assert_eq!(
        the_next_releases_of(&mut reader, 5).await,
        (1..=5).map(in_sequence).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_group_whose_stream_lands_after_its_successors_is_released_before_it() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_group(2);
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(1);
    assert_eq!(
        the_next_releases_of(&mut reader, 2).await,
        [in_sequence(1), in_sequence(2)]
    );
}

#[tokio::test]
async fn a_missing_group_is_held_while_newer_content_stays_within_max_age() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    // Group 2 starts at 40 ms, so group 6 at 120 ms is 80 ms past group 1's reach.
    track.write_the_groups(2..=6);
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(1);
    assert_eq!(
        the_next_releases_of(&mut reader, 6).await,
        (1..=6).map(in_sequence).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_missing_group_is_given_up_once_newer_content_runs_max_age_past_it() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_groups(2..=6);
    assert_the_reader_holds(&mut reader).await;
    // Group 7 at 140 ms is 100 ms past group 1's reach at 40 ms.
    track.write_the_group(7);
    let mut expected = vec![Released::GivenUp { groups: 1 }];
    expected.extend((2..=7).map(in_sequence));
    assert_eq!(the_next_releases_of(&mut reader, 7).await, expected);
}

#[tokio::test]
async fn a_missing_group_is_never_given_up_while_nothing_newer_arrives() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_group(2);
    assert!(
        tokio::time::timeout(ONE_HUNDRED_MILLISECONDS * 3, reader.next_released_group())
            .await
            .is_err(),
        "three times max_age on the clock gives nothing up"
    );
    track.write_the_group(1);
    assert_eq!(
        the_next_releases_of(&mut reader, 2).await,
        [in_sequence(1), in_sequence(2)]
    );
}

#[tokio::test]
async fn a_reset_group_is_given_up_at_once() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    let group_reset_while_held = track.open_the_group(2);
    track.write_the_group(3);
    assert_the_reader_holds(&mut reader).await;
    group_reset_while_held
        .abort(moq_net::Error::Cancel)
        .unwrap();
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(1);
    assert_eq!(
        the_next_releases_of(&mut reader, 3).await,
        [
            in_sequence(1),
            Released::GivenUp { groups: 1 },
            in_sequence(3)
        ]
    );
}

#[tokio::test]
async fn a_given_up_group_arriving_late_is_discarded() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    let group_reset_while_held = track.open_the_group(2);
    track.write_the_group(3);
    assert_the_reader_holds(&mut reader).await;
    group_reset_while_held
        .abort(moq_net::Error::Cancel)
        .unwrap();
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(1);
    assert_eq!(
        the_next_releases_of(&mut reader, 3).await,
        [
            in_sequence(1),
            Released::GivenUp { groups: 1 },
            in_sequence(3)
        ]
    );
    // Served again within max_age of the track's newest content, so the
    // stack hands it over; its loss is already counted.
    track.write_the_group(2);
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(5);
    track.track.finish().unwrap();
    assert_eq!(
        the_next_releases_of(&mut reader, 3).await,
        [
            Released::GivenUp { groups: 1 },
            in_sequence(5),
            Released::TrackEnded
        ]
    );
}

#[tokio::test]
async fn a_group_served_after_its_first_object_is_skipped_to_the_next_groups_start() {
    let track = InMemoryTrackOfStampedGroups::new();
    let mut reader = track.read_with_max_age(ONE_HUNDRED_MILLISECONDS).await;
    let mut group_joined_part_way = track.track.create_group(0_u64.into()).unwrap();
    group_joined_part_way.start_at(1).unwrap();
    group_joined_part_way
        .write_frame(
            moq_net::Timestamp::from_millis(0).unwrap(),
            b"its second object".to_vec(),
        )
        .unwrap();
    group_joined_part_way.finish().unwrap();
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(1);
    assert_eq!(the_next_release_of(&mut reader).await, in_sequence(1));
}

#[tokio::test]
async fn a_track_whose_timestamps_never_advance_gives_up_a_hole_only_on_its_reset_or_end() {
    let track = InMemoryTrackOfStampedGroups::new();
    let mut reader = track.read_with_max_age(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_group_stamped(0, 0);
    assert_eq!(the_next_release_of(&mut reader).await, in_sequence(0));
    track.write_the_group_stamped(2, 0);
    let group_reset_while_held = track.open_the_group_stamped(3, 0);
    for sequence in 4..=12 {
        track.write_the_group_stamped(sequence, 0);
    }
    assert_the_reader_holds(&mut reader).await;
    group_reset_while_held
        .abort(moq_net::Error::Cancel)
        .unwrap();
    assert_the_reader_holds(&mut reader).await;
    track.track.finish().unwrap();
    let mut expected = vec![
        Released::GivenUp { groups: 1 },
        in_sequence(2),
        Released::GivenUp { groups: 1 },
    ];
    expected.extend((4..=12).map(in_sequence));
    expected.push(Released::TrackEnded);
    assert_eq!(
        the_next_releases_of(&mut reader, expected.len()).await,
        expected
    );
}

#[tokio::test]
async fn a_track_that_ends_gives_up_its_missing_groups() {
    let (track, mut reader) = a_track_read_from_its_first_group(ONE_HUNDRED_MILLISECONDS).await;
    track.write_the_group(3);
    assert_the_reader_holds(&mut reader).await;
    track.track.finish().unwrap();
    assert_eq!(
        the_next_releases_of(&mut reader, 3).await,
        [
            Released::GivenUp { groups: 2 },
            in_sequence(3),
            Released::TrackEnded
        ]
    );
}

#[tokio::test]
async fn a_reader_joining_a_track_with_cached_history_starts_at_the_live_edge() {
    let track = InMemoryTrackOfStampedGroups::new();
    track.write_the_groups(0..=4);
    let mut reader = track.read_with_max_age(Duration::from_secs(5)).await;
    assert_eq!(the_next_release_of(&mut reader).await, in_sequence(4));
    track.write_the_group(5);
    assert_eq!(the_next_release_of(&mut reader).await, in_sequence(5));
}

#[tokio::test]
async fn at_a_max_age_of_zero_a_missing_group_is_given_up_only_once_newer_content_runs_past_it() {
    let (track, mut reader) = a_track_read_from_its_first_group(Duration::ZERO).await;
    track.write_the_group(2);
    assert_the_reader_holds(&mut reader).await;
    track.write_the_group(3);
    assert_eq!(
        the_next_releases_of(&mut reader, 3).await,
        [
            Released::GivenUp { groups: 1 },
            in_sequence(2),
            in_sequence(3)
        ]
    );
}
