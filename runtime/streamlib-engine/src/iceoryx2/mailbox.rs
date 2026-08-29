// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using crossbeam ArrayQueue for thread-safe access.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_queue::ArrayQueue;

use super::dropped_bag_counters::InboundLinkDroppedBagCounter;

/// A per-frame measure a port may install so it can ask what its mailbox holds
/// without consuming any of it.
///
/// `ArrayQueue` admits no peek — pushing and popping is the whole of its API —
/// so a port that needs to reason about queued content takes the measure once,
/// as the frame arrives, and the mailbox keeps the running total across every
/// push, pop and eviction. Only the audio window contract's readiness gate
/// installs one; a port without one pays nothing.
pub type PortMailboxQueuedFrameMeasure = Arc<dyn Fn(&[u8]) -> u64 + Send + Sync>;

/// One queued frame and the inbound link it arrived on.
///
/// The tag rides the entry rather than the port so a port fanning in N links
/// attributes an eviction to the link whose bag was lost, not to the link whose
/// push made room. `None` on the manual-injection path
/// ([`PortMailbox::push_frame_without_inbound_link_attribution`]), which
/// synthesizes a frame with no link behind it.
struct PortMailboxQueuedFrame {
    payload: Vec<u8>,
    dropped_bag_counter: Option<InboundLinkDroppedBagCounter>,
    /// This frame's share of [`PortMailbox::queued_frame_measure_total`], taken
    /// when it was pushed. Zero on a mailbox with no measure installed.
    measure: u64,
}

impl PortMailboxQueuedFrame {
    /// Count this frame as lost against the link it came in on.
    fn record_eviction(self) {
        if let Some(counter) = self.dropped_bag_counter {
            counter.record_one_dropped_bag();
        }
    }
}

/// Per-port mailbox with configurable history depth.
///
/// Stores raw wire-format `[u8]` slices (header + data) as `Vec<u8>`.
/// Uses a crossbeam ArrayQueue internally for lock-free, thread-safe access.
/// Multiple threads can push and pop concurrently (MPMC).
pub struct PortMailbox {
    queue: ArrayQueue<PortMailboxQueuedFrame>,
    capacity: usize,
    measure: Option<PortMailboxQueuedFrameMeasure>,
    queued_frame_measure_total: AtomicU64,
}

impl PortMailbox {
    /// Create a new mailbox with the given history depth.
    pub fn new(history: usize) -> Self {
        Self::new_measuring(history, None)
    }

    /// Create a new mailbox that keeps a running total of `measure` over every
    /// frame it holds, so its port can read what is queued without popping it.
    pub fn new_measuring(history: usize, measure: Option<PortMailboxQueuedFrameMeasure>) -> Self {
        let capacity = history.max(1);
        Self {
            queue: ArrayQueue::new(capacity),
            capacity,
            measure,
            queued_frame_measure_total: AtomicU64::new(0),
        }
    }

    /// The installed measure summed over every frame currently queued; zero on
    /// a mailbox with no measure.
    pub fn queued_frame_measure_total(&self) -> u64 {
        self.queued_frame_measure_total.load(Ordering::Relaxed)
    }

    /// Saturating, because the push adds to the total only after the queue
    /// accepted the frame: a pop landing in between would otherwise wrap the
    /// total to near `u64::MAX`, and every consumer of it reads the total as an
    /// upper bound on what is queued. Under-reporting is the direction that
    /// costs nothing — a reader waits for one more bag.
    fn take_out_of_the_total(&self, frame: &PortMailboxQueuedFrame) {
        if frame.measure != 0 {
            let _ = self.queued_frame_measure_total.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |total| Some(total.saturating_sub(frame.measure)),
            );
        }
    }

    /// Push a raw frame slice that arrived on `dropped_bag_counter`'s inbound link.
    ///
    /// If the mailbox is full, the oldest entry is evicted to make room and
    /// counted against the link *it* arrived on. Thread-safe: can be called
    /// from any thread.
    pub fn push_frame_from_inbound_link(
        &self,
        payload: Vec<u8>,
        dropped_bag_counter: &InboundLinkDroppedBagCounter,
    ) {
        let measure = self.measure_of(&payload);
        self.push_frame(PortMailboxQueuedFrame {
            payload,
            dropped_bag_counter: Some(dropped_bag_counter.clone()),
            measure,
        });
    }

    /// Push a raw frame slice that no inbound link delivered — the
    /// manual-injection path, whose evictions have no link to name.
    pub fn push_frame_without_inbound_link_attribution(&self, payload: Vec<u8>) {
        let measure = self.measure_of(&payload);
        self.push_frame(PortMailboxQueuedFrame {
            payload,
            dropped_bag_counter: None,
            measure,
        });
    }

    fn measure_of(&self, payload: &[u8]) -> u64 {
        self.measure
            .as_ref()
            .map(|measure| measure(payload))
            .unwrap_or(0)
    }

    fn push_frame(&self, mut frame: PortMailboxQueuedFrame) {
        // `ArrayQueue::push` hands the frame back only when the queue is full,
        // so this is the one eviction site: try, evict oldest, retry.
        loop {
            let measure = frame.measure;
            match self.queue.push(frame) {
                Ok(()) => {
                    if measure != 0 {
                        self.queued_frame_measure_total
                            .fetch_add(measure, Ordering::Relaxed);
                    }
                    return;
                }
                Err(rejected) => {
                    frame = rejected;
                    if let Some(evicted) = self.queue.pop() {
                        self.take_out_of_the_total(&evicted);
                        evicted.record_eviction();
                    }
                }
            }
        }
    }

    /// Pop the oldest entry from the mailbox (FIFO).
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop(&self) -> Option<Vec<u8>> {
        self.queue.pop().map(|frame| {
            self.take_out_of_the_total(&frame);
            frame.payload
        })
    }

    /// Drain buffer and return only the newest entry.
    ///
    /// The bags passed over are the `newest` read policy working, not loss at
    /// the port, and are not counted.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop_latest(&self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(frame) = self.queue.pop() {
            self.take_out_of_the_total(&frame);
            latest = Some(frame.payload);
        }
        latest
    }

    /// Check if the mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Get the number of entries currently in the mailbox.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Get the configured capacity (history depth).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drain all entries from the mailbox.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn drain(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        std::iter::from_fn(move || self.pop())
    }

    /// Hand every queued frame over to the mailbox replacing this one, oldest
    /// first, each re-measured by the replacement's own measure.
    ///
    /// A port whose window contract settles at `setup()` is re-sized from that
    /// contract, and `ArrayQueue` has a fixed capacity — so the replacement is
    /// a new queue and the frames already in flight have to move into it. They
    /// move rather than being dropped because a bag lost here would be lost
    /// where nothing counts it: eviction is charged to the link a frame arrived
    /// on, and a frame discarded by the swap arrived on a link that is still
    /// wired and still delivering. Each frame keeps the inbound link it came in
    /// on, so a later eviction is still charged to the right one, and any that
    /// overrun the replacement's depth are evicted and counted by it exactly as
    /// a burst would be.
    pub fn hand_every_queued_frame_over_to(&self, replacement: &PortMailbox) {
        while let Some(frame) = self.queue.pop() {
            self.take_out_of_the_total(&frame);
            let measure = replacement.measure_of(&frame.payload);
            replacement.push_frame(PortMailboxQueuedFrame {
                measure,
                ..frame
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iceoryx2::dropped_bag_counters::DroppedBagCountsByInboundLink;

    #[test]
    fn an_eviction_is_counted_against_the_link_whose_bag_was_lost() {
        let counts = DroppedBagCountsByInboundLink::default();
        let from_first_link = counts.counter_for_inbound_link("L-a");
        let from_second_link = counts.counter_for_inbound_link("L-b");
        let mailbox = PortMailbox::new(1);

        mailbox.push_frame_from_inbound_link(vec![1], &from_first_link);
        mailbox.push_frame_from_inbound_link(vec![2], &from_second_link);

        assert_eq!(
            from_first_link.dropped_bag_count(),
            1,
            "the evicted bag was the first link's, so the first link must carry the loss"
        );
        assert_eq!(
            from_second_link.dropped_bag_count(),
            0,
            "the link that made room lost nothing and must not be charged for it"
        );
        assert_eq!(mailbox.pop(), Some(vec![2]));
    }

    #[test]
    fn a_mailbox_with_room_counts_nothing() {
        let counts = DroppedBagCountsByInboundLink::default();
        let counter = counts.counter_for_inbound_link("L-roomy");
        let mailbox = PortMailbox::new(4);

        for byte in 0..4u8 {
            mailbox.push_frame_from_inbound_link(vec![byte], &counter);
        }

        assert_eq!(counter.dropped_bag_count(), 0);
        assert_eq!(mailbox.len(), 4);
    }

    #[test]
    fn every_bag_a_sustained_overrun_evicts_is_counted() {
        let counts = DroppedBagCountsByInboundLink::default();
        let counter = counts.counter_for_inbound_link("L-overrun");
        let mailbox = PortMailbox::new(2);

        for byte in 0..10u8 {
            mailbox.push_frame_from_inbound_link(vec![byte], &counter);
        }
        let delivered = mailbox.drain().count() as u64;

        assert_eq!(delivered, 2);
        assert_eq!(
            counter.dropped_bag_count(),
            10 - delivered,
            "published minus delivered must be exactly what the counter reports"
        );
    }

    #[test]
    fn passing_over_bags_to_reach_the_newest_is_not_a_drop_at_the_port() {
        let counts = DroppedBagCountsByInboundLink::default();
        let counter = counts.counter_for_inbound_link("L-newest");
        let mailbox = PortMailbox::new(4);

        for byte in 0..4u8 {
            mailbox.push_frame_from_inbound_link(vec![byte], &counter);
        }

        assert_eq!(mailbox.pop_latest(), Some(vec![3]));
        assert_eq!(
            counter.dropped_bag_count(),
            0,
            "the `newest` read policy passing over bags is the profile working, never loss"
        );
    }

    #[test]
    fn a_manually_injected_frame_evicts_with_no_link_to_charge() {
        let mailbox = PortMailbox::new(1);

        mailbox.push_frame_without_inbound_link_attribution(vec![1]);
        mailbox.push_frame_without_inbound_link_attribution(vec![2]);

        assert_eq!(mailbox.pop(), Some(vec![2]));
    }
}
