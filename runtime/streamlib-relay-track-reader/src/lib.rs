// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one relay-track reader: a track's groups received in the order they
//! land and released in sequence order, holding at a missing sequence.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::select_all;

/// What a [`RelayTrackGroupsReleasedInSequenceOrder`] releases next.
pub enum ReleasedRelayTrackGroup {
    /// The next group in sequence, read from its first object.
    InSequence(Box<moq_net::group::Consumer>),
    /// Consecutive sequences given up, each group counting as one.
    GivenUp { groups: u64 },
}

/// A relay track's groups, released lowest sequence first.
///
/// A group that arrives in sequence is released at once. A missing sequence
/// is held for and given up only when its stream is reset, when the track
/// ends, or when newer content on the track has run `max_age` past the
/// missing group's reach — the first timestamp of the held group right after
/// it — measured in the track's own timestamps and never on a clock. The read
/// joins at the live edge, replaying no cached history.
///
/// [`Self::next_released_group`] is cancel-safe: dropping it mid-wait loses no
/// group, so it can sit in a `select!` beside other work.
pub struct RelayTrackGroupsReleasedInSequenceOrder {
    groups_in_arrival_order: moq_net::track::Subscriber,
    max_age: Duration,
    /// `None` until the first group arrives and places the join.
    next_sequence_to_release: Option<u64>,
    a_group_was_released: bool,
    groups_awaiting_release_by_sequence: BTreeMap<u64, RelayTrackGroupAwaitingRelease>,
    arrivals: RelayTrackArrivals,
}

/// A group received at or past the sequence the reader releases next.
struct RelayTrackGroupAwaitingRelease {
    group_to_release: moq_net::group::Consumer,
    /// A clone of the group with a cursor of its own, read for its frames'
    /// timestamps while the group waits; `None` once it ended.
    group_read_for_its_timestamps: Option<moq_net::group::Consumer>,
    first_frame_timestamp_ns: Option<u128>,
    newest_frame_timestamp_ns: Option<u128>,
    /// False once it was reset or expired, or when it was served from a later
    /// object than its first.
    releasable_whole: bool,
}

impl RelayTrackGroupAwaitingRelease {
    /// The next group in sequence, released before anything needs its timestamps.
    fn released_at_once(group: moq_net::group::Consumer) -> Self {
        Self::awaiting(group, None, true)
    }

    /// A group held behind a missing one, read for its timestamps meanwhile.
    fn held_behind_a_missing_group(group: moq_net::group::Consumer) -> Self {
        let group_read_for_its_timestamps = Some(group.clone());
        Self::awaiting(group, group_read_for_its_timestamps, true)
    }

    /// A group served from a later object than its first, given up when reached.
    fn served_part_way(group: moq_net::group::Consumer) -> Self {
        Self::awaiting(group, None, false)
    }

    fn awaiting(
        group_to_release: moq_net::group::Consumer,
        group_read_for_its_timestamps: Option<moq_net::group::Consumer>,
        releasable_whole: bool,
    ) -> Self {
        Self {
            group_to_release,
            group_read_for_its_timestamps,
            first_frame_timestamp_ns: None,
            newest_frame_timestamp_ns: None,
            releasable_whole,
        }
    }

    fn take_the_read_of_its_next_frame(
        &mut self,
        frame_read: moq_net::Result<Option<moq_net::frame::Frame>>,
    ) {
        match frame_read {
            Ok(Some(frame)) => {
                let timestamp_ns = frame.timestamp.as_nanos();
                self.first_frame_timestamp_ns.get_or_insert(timestamp_ns);
                self.newest_frame_timestamp_ns = Some(timestamp_ns);
            }
            Ok(None) => self.group_read_for_its_timestamps = None,
            Err(_reset_or_expired) => {
                self.group_read_for_its_timestamps = None;
                self.releasable_whole = false;
            }
        }
    }
}

enum RelayTrackArrivals {
    StillArriving,
    /// The track ended; its exclusive final sequence when it declared one.
    Ended {
        exclusive_final_sequence: Option<u64>,
    },
}

/// What one wait on the track produced.
#[expect(
    clippy::large_enum_variant,
    reason = "built and matched on the stack, never stored"
)]
enum RelayTrackEvent {
    GroupArrived(moq_net::group::Consumer),
    TrackEnded,
    HeldGroupFrameRead {
        sequence: u64,
        frame_read: moq_net::Result<Option<moq_net::frame::Frame>>,
    },
}

impl RelayTrackGroupsReleasedInSequenceOrder {
    /// Subscribe to `track` asking the relay for `max_age` of history, and
    /// hold a missing group for up to `max_age` of newer content, clamped to
    /// the track's declared retention.
    pub async fn subscribe(
        track: &moq_net::track::Consumer,
        max_age: Duration,
    ) -> moq_net::Result<Self> {
        let groups_in_arrival_order = track
            .subscribe(moq_net::track::Subscription::default().with_max_age(max_age))
            .await?;
        let max_age = max_age.min(groups_in_arrival_order.info().max_age);
        Ok(Self {
            groups_in_arrival_order,
            max_age,
            next_sequence_to_release: None,
            a_group_was_released: false,
            groups_awaiting_release_by_sequence: BTreeMap::new(),
            arrivals: RelayTrackArrivals::StillArriving,
        })
    }

    /// Subscribe to `track` holding a missing group for as long as the track's
    /// publisher declares it keeps one, as a reader dumping every group does.
    pub async fn subscribe_waiting_the_tracks_retention(
        track: &moq_net::track::Consumer,
    ) -> moq_net::Result<Self> {
        let the_tracks_retention = track.query().await?.max_age;
        Self::subscribe(track, the_tracks_retention).await
    }

    /// The next group in sequence or the run of sequences given up before it;
    /// `None` once the track ended and everything before its end is released.
    pub async fn next_released_group(
        &mut self,
    ) -> moq_net::Result<Option<ReleasedRelayTrackGroup>> {
        loop {
            if let Some(released) = self.release_what_is_due() {
                return Ok(Some(released));
            }
            if matches!(self.arrivals, RelayTrackArrivals::Ended { .. }) {
                return Ok(None);
            }
            match self.the_next_event().await? {
                RelayTrackEvent::GroupArrived(group) => self.take_the_arrival_of(group),
                RelayTrackEvent::TrackEnded => {
                    let exclusive_final_sequence = self
                        .groups_in_arrival_order
                        .finished()
                        .now_or_never()
                        .and_then(Result::ok);
                    self.arrivals = RelayTrackArrivals::Ended {
                        exclusive_final_sequence,
                    };
                }
                RelayTrackEvent::HeldGroupFrameRead {
                    sequence,
                    frame_read,
                } => {
                    if let Some(held_group) =
                        self.groups_awaiting_release_by_sequence.get_mut(&sequence)
                    {
                        held_group.take_the_read_of_its_next_frame(frame_read);
                    }
                }
            }
        }
    }

    /// Wait for a group to land or a held group to read a frame. Both waits
    /// are poll-driven, so the one not taken loses nothing when dropped.
    async fn the_next_event(&mut self) -> moq_net::Result<RelayTrackEvent> {
        let Self {
            groups_in_arrival_order,
            groups_awaiting_release_by_sequence,
            ..
        } = self;
        let held_group_frame_reads: Vec<_> = groups_awaiting_release_by_sequence
            .iter_mut()
            .filter_map(|(sequence, held_group)| {
                let group = held_group.group_read_for_its_timestamps.as_mut()?;
                let sequence = *sequence;
                Some(Box::pin(
                    async move { (sequence, group.read_frame().await) },
                ))
            })
            .collect();
        let the_next_held_group_frame_read = async {
            if held_group_frame_reads.is_empty() {
                std::future::pending().await
            } else {
                select_all(held_group_frame_reads).await.0
            }
        };
        tokio::select! {
            biased;
            arrival = groups_in_arrival_order.recv_group() => Ok(match arrival? {
                Some(group) => RelayTrackEvent::GroupArrived(group),
                None => RelayTrackEvent::TrackEnded,
            }),
            (sequence, frame_read) = the_next_held_group_frame_read => {
                Ok(RelayTrackEvent::HeldGroupFrameRead { sequence, frame_read })
            }
        }
    }

    fn take_the_arrival_of(&mut self, mut group: moq_net::group::Consumer) {
        let sequence = group.sequence;
        // A group whose stream opened past its first object is trimmed at its
        // producer; clamping the cursor to what it holds shows it in `index`.
        group.set_frames(0..);
        let next_sequence_to_release = *self.next_sequence_to_release.get_or_insert_with(|| {
            // The newest group this copy holds once the first one lands is the
            // live edge: moq-lite delivers newest first. The floor is local,
            // never the subscription's start, which a relay aggregates across
            // its subscribers.
            let live_edge = self
                .groups_in_arrival_order
                .latest()
                .map_or(sequence, |latest| latest.max(sequence));
            self.groups_in_arrival_order.set_groups(live_edge..);
            live_edge
        });
        if sequence < next_sequence_to_release
            || self
                .groups_awaiting_release_by_sequence
                .contains_key(&sequence)
        {
            return;
        }
        let served_from_its_first_object = group.index() == 0;
        if !served_from_its_first_object
            && !self.a_group_was_released
            && sequence == next_sequence_to_release
        {
            self.next_sequence_to_release = Some(sequence + 1);
            return;
        }
        let group_awaiting_release = if !served_from_its_first_object {
            RelayTrackGroupAwaitingRelease::served_part_way(group)
        } else if sequence == next_sequence_to_release {
            RelayTrackGroupAwaitingRelease::released_at_once(group)
        } else {
            RelayTrackGroupAwaitingRelease::held_behind_a_missing_group(group)
        };
        self.groups_awaiting_release_by_sequence
            .insert(sequence, group_awaiting_release);
    }

    fn release_what_is_due(&mut self) -> Option<ReleasedRelayTrackGroup> {
        let next_sequence_to_release = self.next_sequence_to_release?;
        if let Some(group_awaiting_release) = self
            .groups_awaiting_release_by_sequence
            .remove(&next_sequence_to_release)
        {
            self.next_sequence_to_release = Some(next_sequence_to_release + 1);
            if !group_awaiting_release.releasable_whole {
                return Some(ReleasedRelayTrackGroup::GivenUp { groups: 1 });
            }
            self.a_group_was_released = true;
            return Some(ReleasedRelayTrackGroup::InSequence(Box::new(
                group_awaiting_release.group_to_release,
            )));
        }
        let lowest_held_sequence = self
            .groups_awaiting_release_by_sequence
            .keys()
            .next()
            .copied();
        let the_missing_run_ends_before = match self.arrivals {
            RelayTrackArrivals::Ended {
                exclusive_final_sequence,
            } => lowest_held_sequence
                .or(exclusive_final_sequence)
                .filter(|end| *end > next_sequence_to_release)?,
            RelayTrackArrivals::StillArriving => lowest_held_sequence
                .filter(|_| self.newer_content_has_run_max_age_past_the_missing_groups_reach())?,
        };
        self.next_sequence_to_release = Some(the_missing_run_ends_before);
        Some(ReleasedRelayTrackGroup::GivenUp {
            groups: the_missing_run_ends_before - next_sequence_to_release,
        })
    }

    /// moq-net's own staleness rule applied to a missing group: its reach is
    /// where the first releasable held group starts, and the newest content is
    /// the newest frame of the highest releasable held group, so a later group
    /// whose timestamps step back convicts nothing. Content must run strictly
    /// past the reach, so a group arriving first never convicts the one before
    /// it and a track whose timestamps never advance ages nothing.
    fn newer_content_has_run_max_age_past_the_missing_groups_reach(&self) -> bool {
        let Some(reach_ns) = self
            .groups_awaiting_release_releasable_whole()
            .next()
            .and_then(|held_group| held_group.first_frame_timestamp_ns)
        else {
            return false;
        };
        let Some(newest_ns) = self
            .groups_awaiting_release_releasable_whole()
            .rev()
            .find_map(|held_group| held_group.newest_frame_timestamp_ns)
        else {
            return false;
        };
        newest_ns > reach_ns && newest_ns - reach_ns >= self.max_age.as_nanos()
    }

    fn groups_awaiting_release_releasable_whole(
        &self,
    ) -> impl DoubleEndedIterator<Item = &RelayTrackGroupAwaitingRelease> {
        self.groups_awaiting_release_by_sequence
            .values()
            .filter(|group_awaiting_release| group_awaiting_release.releasable_whole)
    }
}
