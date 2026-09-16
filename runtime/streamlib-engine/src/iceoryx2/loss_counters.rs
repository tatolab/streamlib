// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a processor's ports lost, counted where `graph` reads them: bags per
//! inbound link at a destination, samples a windowed port's flush discarded per
//! inbound link, and bags per output port refused at a producer's channel
//! ceiling.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// Named cumulative counts, each shared live with whoever records into it.
///
/// A name gets its count when it is first asked for, so a wired name that has
/// lost nothing reports zero rather than going missing, and forgetting a name
/// takes its count with it: a handle still held keeps counting into nothing.
#[derive(Default)]
struct CumulativeCountsByName {
    per_name: Mutex<HashMap<String, Arc<AtomicU64>>>,
}

impl CumulativeCountsByName {
    fn count_for(&self, name: &str) -> Arc<AtomicU64> {
        Arc::clone(self.per_name.lock().entry(name.to_string()).or_default())
    }

    fn forget(&self, name: &str) {
        self.per_name.lock().remove(name);
    }

    fn snapshot_by_name(&self) -> BTreeMap<String, u64> {
        self.per_name
            .lock()
            .iter()
            .map(|(name, count)| (name.clone(), count.load(Ordering::Relaxed)))
            .collect()
    }

    fn total(&self) -> u64 {
        self.per_name
            .lock()
            .values()
            .map(|count| count.load(Ordering::Relaxed))
            .sum()
    }
}

/// One inbound link's cumulative count of bags lost before anything read them.
///
/// A bag the subscriber ring overwrote, one the port's mailbox evicted, and one
/// dropped at receive all land here. Handed to every mailbox entry that arrived
/// on that link, so an eviction is attributed to the link whose bag was lost
/// rather than to the link that happened to push. Cloning shares the count.
#[derive(Clone, Default)]
pub struct InboundLinkDroppedBagCounter(Arc<AtomicU64>);

impl InboundLinkDroppedBagCounter {
    /// Record one bag of this link's, lost before anything read it.
    pub fn record_one_dropped_bag(&self) {
        self.record_dropped_bags(1);
    }

    /// Record `dropped_bag_count` of this link's bags, lost before anything read
    /// them.
    pub fn record_dropped_bags(&self, dropped_bag_count: u64) {
        self.0.fetch_add(dropped_bag_count, Ordering::Relaxed);
    }

    /// How many of this link's bags have been lost since it was wired.
    pub fn dropped_bag_count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Every inbound link's dropped-bag counter for one destination processor.
///
/// A link gets its counter when its subscriber binds, so a wired link that has
/// lost nothing reports zero rather than going missing. A count is cumulative
/// for the life of one wiring, not of the link id: disconnect takes it with the
/// link, and reconnecting the same id starts from zero, because a count
/// outliving its link would name something `graph` no longer has.
#[derive(Default)]
pub struct DroppedBagCountsByInboundLink {
    per_inbound_link: CumulativeCountsByName,
}

impl DroppedBagCountsByInboundLink {
    /// The counter for `inbound_link_id`, minting a zeroed one on first ask.
    pub fn counter_for_inbound_link(&self, inbound_link_id: &str) -> InboundLinkDroppedBagCounter {
        InboundLinkDroppedBagCounter(self.per_inbound_link.count_for(inbound_link_id))
    }

    /// Forget a disconnected link's count. Entries still queued from it keep
    /// their counter handle alive and bump it on eviction; nothing reads it.
    pub fn forget_inbound_link(&self, inbound_link_id: &str) {
        self.per_inbound_link.forget(inbound_link_id);
    }

    /// Every live inbound link's count as it stands right now, ordered by link
    /// id so a rendering is stable across snapshots. The returned map is a
    /// value and is stale the moment the next loss lands; the counters
    /// themselves stay the record.
    pub fn dropped_bag_count_snapshot_by_inbound_link(&self) -> BTreeMap<String, u64> {
        self.per_inbound_link.snapshot_by_name()
    }

    /// Every live inbound link's counts summed — what a reader means by "this
    /// processor's total dropped bags". The per-link counts stay the record;
    /// this is derived from them and never counted separately.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.per_inbound_link.total()
    }
}

/// One inbound link's cumulative count of samples its windowed port's flushes
/// discarded, in per-channel samples at the port's declared rate. Cloning shares
/// the count.
#[derive(Clone, Default)]
pub struct InboundLinkDiscardedSampleCounter(Arc<AtomicU64>);

impl InboundLinkDiscardedSampleCounter {
    /// Record `discarded_sample_count` of this link's samples, discarded by a
    /// flush.
    pub fn record_discarded_samples(&self, discarded_sample_count: u64) {
        self.0.fetch_add(discarded_sample_count, Ordering::Relaxed);
    }
}

/// Every windowed inbound link's discarded-sample counter for one destination
/// processor.
///
/// Only a link into a windowed port is given a counter, so a link into any other
/// port carries no sample count rather than a zero. A count lives as long as one
/// wiring, as a link's dropped-bag count does.
#[derive(Default)]
pub struct DiscardedSampleCountsByInboundLink {
    per_inbound_link: CumulativeCountsByName,
}

impl DiscardedSampleCountsByInboundLink {
    /// The counter for `inbound_link_id`, minting a zeroed one on first ask.
    pub fn counter_for_inbound_link(
        &self,
        inbound_link_id: &str,
    ) -> InboundLinkDiscardedSampleCounter {
        InboundLinkDiscardedSampleCounter(self.per_inbound_link.count_for(inbound_link_id))
    }

    /// Forget a disconnected link's count.
    pub fn forget_inbound_link(&self, inbound_link_id: &str) {
        self.per_inbound_link.forget(inbound_link_id);
    }

    /// Every windowed inbound link's count as it stands right now, ordered by
    /// link id.
    pub fn discarded_sample_count_snapshot_by_inbound_link(&self) -> BTreeMap<String, u64> {
        self.per_inbound_link.snapshot_by_name()
    }
}

/// One output port's cumulative count of bags refused at its channel's payload
/// ceiling. Cloning shares the count.
#[derive(Clone, Default)]
pub struct OutputPortRefusedBagCounter(Arc<AtomicU64>);

impl OutputPortRefusedBagCounter {
    /// Record one bag this port refused, and return the port's total after it.
    pub fn record_one_refused_bag(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// How many bags this port has refused since its channel was opened.
    pub fn refused_bag_count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Every output port's refused-bag counter for one producing processor.
///
/// A refusal happens before the bag belongs to any link, so it is counted on the
/// port that refused it. A port gets its counter when its channel publisher is
/// installed and loses it when the publisher is released with the port's last
/// link, the same life a link's count has.
#[derive(Default)]
pub struct RefusedBagCountsByOutputPort {
    per_output_port: CumulativeCountsByName,
}

impl RefusedBagCountsByOutputPort {
    /// The counter for `output_port`, minting a zeroed one on first ask.
    pub fn counter_for_output_port(&self, output_port: &str) -> OutputPortRefusedBagCounter {
        OutputPortRefusedBagCounter(self.per_output_port.count_for(output_port))
    }

    /// Forget a port whose channel publisher was released.
    pub fn forget_output_port(&self, output_port: &str) {
        self.per_output_port.forget(output_port);
    }

    /// Every output port with a channel, and its count as it stands right now,
    /// ordered by port name.
    pub fn refused_bag_count_snapshot_by_output_port(&self) -> BTreeMap<String, u64> {
        self.per_output_port.snapshot_by_name()
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

    #[test]
    fn a_released_output_ports_refusals_leave_with_it_and_a_reopened_port_starts_from_zero() {
        let counts = RefusedBagCountsByOutputPort::default();
        let first_channel = counts.counter_for_output_port("video");
        assert_eq!(first_channel.record_one_refused_bag(), 1);
        assert_eq!(first_channel.record_one_refused_bag(), 2);
        assert_eq!(
            counts.refused_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 2)])
        );

        counts.forget_output_port("video");
        assert!(
            counts
                .refused_bag_count_snapshot_by_output_port()
                .is_empty()
        );

        let reopened_channel = counts.counter_for_output_port("video");
        assert_eq!(reopened_channel.refused_bag_count(), 0);
        first_channel.record_one_refused_bag();
        assert_eq!(
            counts.refused_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 0)]),
            "a refusal on the released channel's handle must reach no reader"
        );
    }
}
