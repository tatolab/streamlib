// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-inbound-link counts of bags evicted at a destination processor's ports.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// One inbound link's cumulative count of bags evicted at the port it feeds.
///
/// Handed to every mailbox entry that arrived on that link, so an eviction is
/// attributed to the link whose bag was lost rather than to the link that
/// happened to push. Cloning shares the count.
#[derive(Clone, Default)]
pub struct InboundLinkDroppedBagCounter(Arc<AtomicU64>);

impl InboundLinkDroppedBagCounter {
    /// Record one bag of this link's, evicted before anything read it.
    pub fn record_one_dropped_bag(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// How many of this link's bags have been evicted since it was wired.
    pub fn dropped_bag_count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Every inbound link's dropped-bag counter for one destination processor.
///
/// A link gets its counter when its subscriber binds, so a wired link that has
/// lost nothing reports zero rather than going missing, and loses it when the
/// link disconnects — a count outliving its link would name something `graph`
/// no longer has.
#[derive(Default)]
pub struct DroppedBagCountsByInboundLink {
    per_inbound_link: Mutex<HashMap<String, InboundLinkDroppedBagCounter>>,
}

impl DroppedBagCountsByInboundLink {
    /// The counter for `inbound_link_id`, minting a zeroed one on first ask.
    pub fn counter_for_inbound_link(&self, inbound_link_id: &str) -> InboundLinkDroppedBagCounter {
        self.per_inbound_link
            .lock()
            .entry(inbound_link_id.to_string())
            .or_default()
            .clone()
    }

    /// Forget a disconnected link's count. Entries still queued from it keep
    /// their counter handle alive and bump it on eviction; nothing reads it.
    pub fn forget_inbound_link(&self, inbound_link_id: &str) {
        self.per_inbound_link.lock().remove(inbound_link_id);
    }

    /// Every live inbound link's count as it stands right now, ordered by link
    /// id so a rendering is stable across snapshots. The returned map is a
    /// value and is stale the moment the next eviction lands; the counters
    /// themselves stay the record.
    pub fn dropped_bag_count_snapshot_by_inbound_link(&self) -> BTreeMap<String, u64> {
        self.per_inbound_link
            .lock()
            .iter()
            .map(|(inbound_link_id, counter)| {
                (inbound_link_id.clone(), counter.dropped_bag_count())
            })
            .collect()
    }

    /// Every live inbound link's counts summed — what a reader means by "this
    /// processor's total dropped bags". The per-link counts stay the record;
    /// this is derived from them and never counted separately.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.per_inbound_link
            .lock()
            .values()
            .map(InboundLinkDroppedBagCounter::dropped_bag_count)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_twice_for_one_links_counter_shares_the_count() {
        let counts = DroppedBagCountsByInboundLink::default();
        counts
            .counter_for_inbound_link("L-shared")
            .record_one_dropped_bag();
        counts
            .counter_for_inbound_link("L-shared")
            .record_one_dropped_bag();

        assert_eq!(
            counts
                .counter_for_inbound_link("L-shared")
                .dropped_bag_count(),
            2,
            "the second ask must reach the same counter, not mint a fresh one"
        );
    }

    #[test]
    fn a_disconnected_links_count_leaves_with_it() {
        let counts = DroppedBagCountsByInboundLink::default();
        let departing = counts.counter_for_inbound_link("L-gone");
        departing.record_one_dropped_bag();

        counts.forget_inbound_link("L-gone");

        assert!(
            counts
                .dropped_bag_count_snapshot_by_inbound_link()
                .is_empty()
        );
        assert_eq!(counts.total_dropped_bag_count(), 0);
        departing.record_one_dropped_bag();
        assert!(
            counts
                .dropped_bag_count_snapshot_by_inbound_link()
                .is_empty(),
            "an entry still queued from a departed link must reach no reader"
        );
    }
}
