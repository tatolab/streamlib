// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using crossbeam ArrayQueue for thread-safe access.

use crossbeam_queue::ArrayQueue;

use super::dropped_bag_counters::InboundLinkDroppedBagCounter;

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
}

impl PortMailbox {
    /// Create a new mailbox with the given history depth.
    pub fn new(history: usize) -> Self {
        let capacity = history.max(1);
        Self {
            queue: ArrayQueue::new(capacity),
            capacity,
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
        self.push_frame(PortMailboxQueuedFrame {
            payload,
            dropped_bag_counter: Some(dropped_bag_counter.clone()),
        });
    }

    /// Push a raw frame slice that no inbound link delivered — the
    /// manual-injection path, whose evictions have no link to name.
    pub fn push_frame_without_inbound_link_attribution(&self, payload: Vec<u8>) {
        self.push_frame(PortMailboxQueuedFrame {
            payload,
            dropped_bag_counter: None,
        });
    }

    fn push_frame(&self, mut frame: PortMailboxQueuedFrame) {
        // `ArrayQueue::push` hands the frame back only when the queue is full,
        // so this is the one eviction site: try, evict oldest, retry.
        while let Err(rejected) = self.queue.push(frame) {
            frame = rejected;
            if let Some(evicted) = self.queue.pop() {
                evicted.record_eviction();
            }
        }
    }

    /// Pop the oldest entry from the mailbox (FIFO).
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop(&self) -> Option<Vec<u8>> {
        self.queue.pop().map(|frame| frame.payload)
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
