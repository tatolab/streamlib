// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a processor's ports lost, counted where `graph` reads them: bags per
//! inbound link at a destination, samples a windowed port's flush discarded per
//! inbound link, and bags per output port refused at a producer's channel
//! ceiling — beside the bags each output port sent, which those losses are
//! counted against.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

/// Where every new total of one count is also written, for a reader in another
/// process — a helper process's loss-count board, which its parent reads.
///
/// Called at the site that moved the count, under whatever lock that caller
/// holds, so a mirror takes no lock but its own.
pub type LossCountMirror = Box<dyn Fn(u64) + Send + Sync>;

/// One cumulative count, and the mirror its every new total is written through
/// once one is installed.
#[derive(Default)]
struct CumulativeCount {
    total: AtomicU64,
    mirror: OnceLock<LossCountMirror>,
}

impl CumulativeCount {
    /// Add `amount`, mirror the new total, and return it.
    fn add(&self, amount: u64) -> u64 {
        let total = self
            .total
            .fetch_add(amount, Ordering::Relaxed)
            .wrapping_add(amount);
        if let Some(mirror) = self.mirror.get() {
            mirror(total);
        }
        total
    }

    fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Install `mirror` and write the total as it stands through it, or hand
    /// `mirror` back where this count already has one.
    ///
    /// The total is written after the install rather than before, so an
    /// increment landing between the two reaches the mirror either way; a
    /// mirror keeps the largest total it was handed.
    fn mirror_every_new_total_into(&self, mirror: LossCountMirror) -> Result<(), LossCountMirror> {
        self.mirror.set(mirror)?;
        if let Some(mirror) = self.mirror.get() {
            mirror(self.total());
        }
        Ok(())
    }
}

/// A count whose total `graph` renders.
trait CountWithACumulativeTotal: Default {
    fn total(&self) -> u64;
}

impl CountWithACumulativeTotal for CumulativeCount {
    fn total(&self) -> u64 {
        CumulativeCount::total(self)
    }
}

/// Named cumulative counts, each shared live with whoever records into it.
///
/// A name gets its count when it is first asked for, so a wired name that has
/// lost nothing reports zero rather than going missing, and forgetting a name
/// takes its count with it: a handle still held keeps counting into nothing.
struct CumulativeCountsByName<Count = CumulativeCount> {
    per_name: Mutex<HashMap<String, Arc<Count>>>,
}

impl<Count> Default for CumulativeCountsByName<Count> {
    fn default() -> Self {
        Self {
            per_name: Mutex::new(HashMap::new()),
        }
    }
}

impl<Count: Default> CumulativeCountsByName<Count> {
    fn count_for(&self, name: &str) -> Arc<Count> {
        Arc::clone(self.per_name.lock().entry(name.to_string()).or_default())
    }

    fn forget(&self, name: &str) {
        self.per_name.lock().remove(name);
    }

    fn snapshot_by_name_of(&self, total_of: impl Fn(&Count) -> u64) -> BTreeMap<String, u64> {
        self.per_name
            .lock()
            .iter()
            .map(|(name, count)| (name.clone(), total_of(count)))
            .collect()
    }
}

impl<Count: CountWithACumulativeTotal> CumulativeCountsByName<Count> {
    fn snapshot_by_name(&self) -> BTreeMap<String, u64> {
        self.snapshot_by_name_of(Count::total)
    }

    fn total(&self) -> u64 {
        self.per_name
            .lock()
            .values()
            .map(|count| count.total())
            .sum()
    }
}

/// One inbound link's dropped bags: the total `graph` renders, split by where
/// each was lost, and how many of them the reader has been told of.
///
/// What the reader is told also takes in the messages the link's writer
/// declared lost upstream of it, which are never part of the total: the link
/// lost none of them.
///
/// The split is what places a loss on the right message. A message lost before
/// its bag reached the mailbox is ahead of every bag queued after the loss was
/// counted, so a queued bag carries that part as it stood when it was queued. A bag the
/// mailbox evicted was the oldest queued, and a bag its reader counted lost had
/// already been taken, so each is ahead of every bag of the link still queued,
/// and a bag reads that part as it stands when it is taken. Each part lands on
/// the right bag only while every push and pop of the link's mailbox is
/// serialized, as `InputMailboxesInner` serializes them under its port map
/// lock, and a reader counts a bag lost on the thread that takes its bags.
#[derive(Default)]
struct InboundLinkDroppedBagCount {
    dropped_bags: CumulativeCount,
    lost_before_reaching_the_mailbox: AtomicU64,
    lost_ahead_of_every_bag_still_queued: AtomicU64,
    lost_ahead_of_the_latest_bag_taken_off_the_mailbox: AtomicU64,
    lost_and_told_to_the_reader: AtomicU64,
}

impl CountWithACumulativeTotal for InboundLinkDroppedBagCount {
    fn total(&self) -> u64 {
        self.dropped_bags.total()
    }
}

/// One inbound link's cumulative count of bags lost.
///
/// A bag the subscriber ring overwrote, one the port's mailbox evicted, one
/// dropped at receive, and one its reader read and then counted lost all land
/// here. Handed to every mailbox entry that arrived
/// on that link, so an eviction is attributed to the link whose bag was lost
/// rather than to the link that happened to push. Cloning shares the count.
///
/// The total is never zeroed. A reader is told of each loss once, on the first
/// message it reads from the link after the lost bag, through
/// [`Self::take_lost_messages_the_reader_has_not_been_told_of`].
#[derive(Clone, Default)]
pub struct InboundLinkDroppedBagCounter(Arc<InboundLinkDroppedBagCount>);

impl InboundLinkDroppedBagCounter {
    /// Record one bag of this link's, lost before it reached the mailbox.
    pub fn record_one_dropped_bag(&self) {
        self.record_dropped_bags(1);
    }

    /// Record `dropped_bag_count` of this link's bags, lost before they reached
    /// the mailbox.
    pub fn record_dropped_bags(&self, dropped_bag_count: u64) {
        self.0
            .lost_before_reaching_the_mailbox
            .fetch_add(dropped_bag_count, Ordering::Relaxed);
        self.0.dropped_bags.add(dropped_bag_count);
    }

    /// Record `lost_message_count` messages the link's writer declared lost
    /// upstream of it ahead of the bag now arriving: told to the reader with
    /// that bag, never counted as this link's drop.
    pub(super) fn record_messages_the_writer_declared_lost_upstream(
        &self,
        lost_message_count: u64,
    ) {
        self.0
            .lost_before_reaching_the_mailbox
            .fetch_add(lost_message_count, Ordering::Relaxed);
    }

    /// Record one bag of this link's that the mailbox evicted to make room.
    pub(super) fn record_one_bag_evicted_from_the_mailbox(&self) {
        self.record_one_bag_lost_ahead_of_every_bag_still_queued();
    }

    /// Record one bag of this link's that its reader took and then counted
    /// lost, because what the bag named was gone by the time it was read.
    pub(super) fn record_one_bag_its_reader_counted_lost_after_taking_it(&self) {
        self.record_one_bag_lost_ahead_of_every_bag_still_queued();
    }

    fn record_one_bag_lost_ahead_of_every_bag_still_queued(&self) {
        self.0
            .lost_ahead_of_every_bag_still_queued
            .fetch_add(1, Ordering::Relaxed);
        self.0.dropped_bags.add(1);
    }

    /// How many of this link's bags have been lost since it was wired.
    pub fn dropped_bag_count(&self) -> u64 {
        self.0.dropped_bags.total()
    }

    /// The messages lost before reaching the mailbox, as the count stands now —
    /// what a bag queued now has ahead of it from that cause.
    pub(super) fn lost_before_reaching_the_mailbox(&self) -> u64 {
        self.0
            .lost_before_reaching_the_mailbox
            .load(Ordering::Relaxed)
    }

    /// Note a bag taken off the mailbox that had
    /// `lost_before_reaching_the_mailbox_when_queued` ahead of it when it was
    /// queued; every eviction and every bag its reader counted lost so far is
    /// ahead of it too.
    pub(super) fn note_a_bag_taken_off_the_mailbox(
        &self,
        lost_before_reaching_the_mailbox_when_queued: u64,
    ) {
        let lost_ahead_of_it = lost_before_reaching_the_mailbox_when_queued
            + self
                .0
                .lost_ahead_of_every_bag_still_queued
                .load(Ordering::Relaxed);
        self.0
            .lost_ahead_of_the_latest_bag_taken_off_the_mailbox
            .fetch_max(lost_ahead_of_it, Ordering::Relaxed);
    }

    /// The messages lost ahead of the latest bag taken off the mailbox that no
    /// earlier call returned — the `lost_since_last_read` of the message that
    /// bag is handed to the reader as.
    pub(super) fn take_lost_messages_the_reader_has_not_been_told_of(&self) -> u64 {
        let lost_ahead = self
            .0
            .lost_ahead_of_the_latest_bag_taken_off_the_mailbox
            .load(Ordering::Relaxed);
        let already_told = self
            .0
            .lost_and_told_to_the_reader
            .fetch_max(lost_ahead, Ordering::Relaxed);
        lost_ahead.saturating_sub(already_told)
    }

    /// Write this count's every new total through `mirror`, or hand it back
    /// where the count is already mirrored.
    pub(crate) fn mirror_every_new_total_into(
        &self,
        mirror: LossCountMirror,
    ) -> Result<(), LossCountMirror> {
        self.0.dropped_bags.mirror_every_new_total_into(mirror)
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
    per_inbound_link: CumulativeCountsByName<InboundLinkDroppedBagCount>,
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
pub struct InboundLinkDiscardedSampleCounter(Arc<CumulativeCount>);

impl InboundLinkDiscardedSampleCounter {
    /// Record `discarded_sample_count` of this link's samples, discarded by a
    /// flush.
    pub fn record_discarded_samples(&self, discarded_sample_count: u64) {
        self.0.add(discarded_sample_count);
    }

    /// Write this count's every new total through `mirror`, or hand it back
    /// where the count is already mirrored.
    pub(crate) fn mirror_every_new_total_into(
        &self,
        mirror: LossCountMirror,
    ) -> Result<(), LossCountMirror> {
        self.0.mirror_every_new_total_into(mirror)
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

/// One output port's cumulative counts: bags refused at its channel's payload
/// ceiling, bags sent on its channel, and groups its writer declared given up
/// upstream of it.
#[derive(Default)]
struct OutputPortRefusedWrittenAndGivenUpCounts {
    refused_bags: CumulativeCount,
    written_bags: CumulativeCount,
    given_up_groups: CumulativeCount,
}

/// One output port's bag counters. Cloning shares the counts.
#[derive(Clone, Default)]
pub struct OutputPortBagCounters(Arc<OutputPortRefusedWrittenAndGivenUpCounts>);

impl OutputPortBagCounters {
    /// Record one bag this port refused, and return the port's refused total
    /// after it.
    pub fn record_one_refused_bag(&self) -> u64 {
        self.0.refused_bags.add(1)
    }

    /// Record one bag this port sent.
    pub fn record_one_written_bag(&self) {
        self.0.written_bags.add(1);
    }

    /// Record `given_up_group_count` groups this port's writer gave up upstream
    /// of it.
    pub fn record_given_up_groups(&self, given_up_group_count: u64) {
        self.0.given_up_groups.add(given_up_group_count);
    }

    /// Write every new total of the refused and written counts through its
    /// mirror, or refuse where they are already mirrored. Given-up groups are
    /// not mirrored: only an engine-internal writer in this process declares
    /// one.
    pub(crate) fn mirror_every_new_total_into(
        &self,
        refused_bags_mirror: LossCountMirror,
        written_bags_mirror: LossCountMirror,
    ) -> Result<(), OutputPortBagCountsAlreadyMirrored> {
        self.0
            .refused_bags
            .mirror_every_new_total_into(refused_bags_mirror)
            .map_err(|_| OutputPortBagCountsAlreadyMirrored)?;
        self.0
            .written_bags
            .mirror_every_new_total_into(written_bags_mirror)
            .map_err(|_| OutputPortBagCountsAlreadyMirrored)
    }
}

/// A second mirror refused on an output port's counts, which already have one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutputPortBagCountsAlreadyMirrored;

/// Every output port's bag counters for one producing processor.
///
/// A refusal happens before the bag belongs to any link, so it is counted on the
/// port that refused it, as a sent bag is. A port gets its counters when its
/// channel publisher is installed and loses them when the publisher is released
/// with the port's last link, the same life a link's count has.
#[derive(Default)]
pub struct BagCountsByOutputPort {
    per_output_port: CumulativeCountsByName<OutputPortRefusedWrittenAndGivenUpCounts>,
}

impl BagCountsByOutputPort {
    /// The counters for `output_port`, minting zeroed ones on first ask.
    pub fn counters_for_output_port(&self, output_port: &str) -> OutputPortBagCounters {
        OutputPortBagCounters(self.per_output_port.count_for(output_port))
    }

    /// Forget a port whose channel publisher was released.
    pub fn forget_output_port(&self, output_port: &str) {
        self.per_output_port.forget(output_port);
    }

    /// Every output port with a channel, and its refused total as it stands
    /// right now, ordered by port name.
    pub fn refused_bag_count_snapshot_by_output_port(&self) -> BTreeMap<String, u64> {
        self.per_output_port
            .snapshot_by_name_of(|count| count.refused_bags.total())
    }

    /// Every output port with a channel, and its sent total as it stands right
    /// now, ordered by port name.
    pub fn written_bag_count_snapshot_by_output_port(&self) -> BTreeMap<String, u64> {
        self.per_output_port
            .snapshot_by_name_of(|count| count.written_bags.total())
    }

    /// Every output port whose writer has given up a group upstream of it, and
    /// its given-up total as it stands right now, ordered by port name. A port
    /// whose writer has not given one up yet is absent rather than zero.
    pub fn given_up_group_count_snapshot_by_output_port(&self) -> BTreeMap<String, u64> {
        let mut given_up_groups_by_output_port = self
            .per_output_port
            .snapshot_by_name_of(|count| count.given_up_groups.total());
        given_up_groups_by_output_port.retain(|_, given_up_groups| *given_up_groups > 0);
        given_up_groups_by_output_port
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
    fn a_mirrored_count_writes_the_total_it_had_and_every_new_total_through_its_mirror() {
        let counts = DroppedBagCountsByInboundLink::default();
        let counter = counts.counter_for_inbound_link("L-mirrored");
        counter.record_dropped_bags(3);
        let mirrored_totals = Arc::new(Mutex::new(Vec::new()));
        let mirror_sink = Arc::clone(&mirrored_totals);

        assert!(
            counter
                .mirror_every_new_total_into(Box::new(move |total| mirror_sink.lock().push(total)))
                .is_ok()
        );
        counter.record_one_dropped_bag();
        counts
            .counter_for_inbound_link("L-mirrored")
            .record_dropped_bags(2);

        assert_eq!(
            *mirrored_totals.lock(),
            [3, 4, 6],
            "the total at install, then each total a record reached, through any handle"
        );
        assert!(
            counter
                .mirror_every_new_total_into(Box::new(|_| {}))
                .is_err(),
            "a count takes one mirror, and a second is handed back rather than replacing it"
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
    fn a_released_output_ports_counts_leave_with_it_and_a_reopened_port_starts_from_zero() {
        let counts = BagCountsByOutputPort::default();
        let first_channel = counts.counters_for_output_port("video");
        assert_eq!(first_channel.record_one_refused_bag(), 1);
        assert_eq!(first_channel.record_one_refused_bag(), 2);
        first_channel.record_one_written_bag();
        assert_eq!(
            counts.refused_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 2)])
        );
        assert_eq!(
            counts.written_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 1)])
        );

        counts.forget_output_port("video");
        assert!(
            counts
                .refused_bag_count_snapshot_by_output_port()
                .is_empty()
        );

        let _reopened_channel = counts.counters_for_output_port("video");
        first_channel.record_one_refused_bag();
        first_channel.record_one_written_bag();
        assert_eq!(
            counts.refused_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 0)]),
            "a refusal on the released channel's handle must reach no reader"
        );
        assert_eq!(
            counts.written_bag_count_snapshot_by_output_port(),
            BTreeMap::from([("video".to_string(), 0)]),
            "a send on the released channel's handle must reach no reader"
        );
    }

    #[test]
    fn a_ports_counters_install_their_mirrors_once() {
        let counters = BagCountsByOutputPort::default().counters_for_output_port("video");
        assert_eq!(
            counters.mirror_every_new_total_into(Box::new(|_| {}), Box::new(|_| {})),
            Ok(())
        );
        assert_eq!(
            counters.mirror_every_new_total_into(Box::new(|_| {}), Box::new(|_| {})),
            Err(OutputPortBagCountsAlreadyMirrored)
        );
    }
}
