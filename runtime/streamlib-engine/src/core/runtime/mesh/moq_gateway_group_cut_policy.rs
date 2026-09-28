// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! When the MoQ gateway closes one group on a track and opens the next.
//!
//! A group is what a subscriber joins at, so an encoded stream cuts one at
//! every sync point — a joiner then enters at a decodable frame. Anything else
//! rolls a new group every second or every sixty objects, so a subscriber
//! joining mid-stream replays at most that much and a relay never holds an
//! unbounded group.

use std::time::{Duration, Instant};

/// The most objects one group holds on a track that is not cut by sync points.
pub(crate) const MOST_OBJECTS_IN_ONE_MOQ_GATEWAY_GROUP: usize = 60;

/// The oldest a group gets on a track that is not cut by sync points.
pub(crate) const OLDEST_A_MOQ_GATEWAY_GROUP_GETS: Duration = Duration::from_secs(1);

/// One track's open group, as the cut policy reads it.
#[derive(Debug, Default)]
pub(crate) struct WhenTheMoqGatewayCutsAGroup {
    objects_in_the_open_group: usize,
    the_open_group_opened_at: Option<Instant>,
}

impl WhenTheMoqGatewayCutsAGroup {
    /// Whether the object about to be written opens a new group, noting it
    /// against whichever group it lands in.
    pub(crate) fn this_object_opens_a_new_group(
        &mut self,
        it_is_an_encoded_sync_point: bool,
        now: Instant,
    ) -> bool {
        let opens_a_new_group = match self.the_open_group_opened_at {
            None => true,
            Some(opened_at) => {
                it_is_an_encoded_sync_point
                    || self.objects_in_the_open_group >= MOST_OBJECTS_IN_ONE_MOQ_GATEWAY_GROUP
                    || now.saturating_duration_since(opened_at) >= OLDEST_A_MOQ_GATEWAY_GROUP_GETS
            }
        };
        if opens_a_new_group {
            self.objects_in_the_open_group = 0;
            self.the_open_group_opened_at = Some(now);
        }
        self.objects_in_the_open_group += 1;
        opens_a_new_group
    }

    /// Forget the open group, so the next object opens one whatever it is.
    pub(crate) fn forget_the_open_group(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_object_always_opens_a_group() {
        let mut policy = WhenTheMoqGatewayCutsAGroup::default();
        assert!(policy.this_object_opens_a_new_group(false, Instant::now()));
    }

    #[test]
    fn every_encoded_sync_point_opens_a_group_and_nothing_between_them_does() {
        let mut policy = WhenTheMoqGatewayCutsAGroup::default();
        let start = Instant::now();
        let opened: Vec<bool> = [true, false, false, true, false]
            .into_iter()
            .enumerate()
            .map(|(index, sync)| {
                policy.this_object_opens_a_new_group(
                    sync,
                    start + Duration::from_millis(33 * index as u64),
                )
            })
            .collect();
        assert_eq!(opened, [true, false, false, true, false]);
    }

    #[test]
    fn sixty_objects_fill_a_group_and_the_sixty_first_opens_the_next() {
        let mut policy = WhenTheMoqGatewayCutsAGroup::default();
        let now = Instant::now();
        let opened: Vec<usize> = (0..121)
            .filter(|_| policy.this_object_opens_a_new_group(false, now))
            .map(|_| 0)
            .collect();
        assert_eq!(opened.len(), 3, "objects 0, 60 and 120 each open a group");
    }

    #[test]
    fn a_group_a_second_old_is_cut_by_the_next_object() {
        let mut policy = WhenTheMoqGatewayCutsAGroup::default();
        let start = Instant::now();
        assert!(policy.this_object_opens_a_new_group(false, start));
        assert!(!policy.this_object_opens_a_new_group(false, start + Duration::from_millis(999)));
        assert!(policy.this_object_opens_a_new_group(false, start + Duration::from_millis(1_000)));
        assert!(!policy.this_object_opens_a_new_group(false, start + Duration::from_millis(1_500)));
    }

    #[test]
    fn a_forgotten_group_is_reopened_by_the_next_object() {
        let mut policy = WhenTheMoqGatewayCutsAGroup::default();
        let now = Instant::now();
        policy.this_object_opens_a_new_group(false, now);
        policy.forget_the_open_group();
        assert!(policy.this_object_opens_a_new_group(false, now));
    }
}
