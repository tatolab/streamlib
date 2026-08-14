// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using crossbeam ArrayQueue for thread-safe access.

use std::sync::{Arc, OnceLock};

use crossbeam_queue::ArrayQueue;

/// Why a bag left a mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedBagDeparture {
    /// Handed to the processor to read.
    DeliveredToProcessor,
    /// Dropped by the queue without ever reaching the processor — evicted to
    /// make room, skipped past by a latest-wins read, or discarded with the
    /// mailbox itself.
    DiscardedUnread,
}

/// What a mailbox tells its host about the bags passing through it.
///
/// A helper child uses this to claim the GPU surfaces a queued bag names as
/// soon as the bag is queued, so a bag waiting its turn is protected for the
/// whole wait rather than from whenever user code reaches for it. Every
/// enqueue is paired with exactly one departure — including the ones the queue
/// itself throws away — because a claim with no departure to answer it pins a
/// producer's pool slot forever.
///
/// Optional and unset by default: a native in-process processor installs none
/// and pays nothing.
pub trait QueuedBagObserver: Send + Sync {
    /// A bag has entered the queue. Called before it becomes visible to a
    /// reader.
    fn bag_queued(&self, wire_frame: &[u8]);

    /// A bag has left the queue, for the stated reason.
    fn bag_departed(&self, wire_frame: &[u8], departure: QueuedBagDeparture);
}

/// Per-port mailbox with configurable history depth.
///
/// Stores raw wire-format `[u8]` slices (header + data) as `Vec<u8>`.
/// Uses a crossbeam ArrayQueue internally for lock-free, thread-safe access.
/// Multiple threads can push and pop concurrently (MPMC).
pub struct PortMailbox {
    queue: ArrayQueue<Vec<u8>>,
    capacity: usize,
    /// Shared with the destination that owns this mailbox, so an observer
    /// installed after the ports are wired reaches every one of them. A
    /// `OnceLock` rather than a lock because this is read on the receive path
    /// and written at most once, while the processor is being set up.
    queued_bag_observer: Arc<OnceLock<Arc<dyn QueuedBagObserver>>>,
}

impl PortMailbox {
    /// Create a new mailbox with the given history depth.
    pub fn new(history: usize) -> Self {
        Self::new_observed_by(history, Arc::new(OnceLock::new()))
    }

    /// As [`Self::new`], but reporting every bag that enters and leaves to
    /// whatever observer `queued_bag_observer` ends up holding.
    pub fn new_observed_by(
        history: usize,
        queued_bag_observer: Arc<OnceLock<Arc<dyn QueuedBagObserver>>>,
    ) -> Self {
        let capacity = history.max(1);
        Self {
            queue: ArrayQueue::new(capacity),
            capacity,
            queued_bag_observer,
        }
    }

    fn report_departure(&self, wire_frame: &[u8], departure: QueuedBagDeparture) {
        if let Some(observer) = self.queued_bag_observer.get() {
            observer.bag_departed(wire_frame, departure);
        }
    }

    /// Push a raw frame slice into the mailbox.
    ///
    /// If the mailbox is full, the oldest entry is dropped to make room.
    /// Thread-safe: can be called from any thread.
    pub fn push(&self, payload: Vec<u8>) {
        // Announced before it is reachable, so a bag is never readable ahead
        // of the claim protecting it.
        if let Some(observer) = self.queued_bag_observer.get() {
            observer.bag_queued(&payload);
        }

        // If full, pop oldest to make room
        while self.queue.is_full() {
            if let Some(evicted) = self.queue.pop() {
                self.report_departure(&evicted, QueuedBagDeparture::DiscardedUnread);
            }
        }
        // Push should succeed now (may fail if another thread filled it, retry)
        let mut val = payload;
        while let Err(v) = self.queue.push(val) {
            val = v;
            if let Some(evicted) = self.queue.pop() {
                self.report_departure(&evicted, QueuedBagDeparture::DiscardedUnread);
            }
        }
    }

    /// Pop the oldest entry from the mailbox (FIFO).
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop(&self) -> Option<Vec<u8>> {
        let popped = self.queue.pop();
        if let Some(delivered) = &popped {
            self.report_departure(delivered, QueuedBagDeparture::DeliveredToProcessor);
        }
        popped
    }

    /// Drain buffer and return only the newest entry.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop_latest(&self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(value) = self.queue.pop() {
            if let Some(skipped_past) = latest.replace(value) {
                self.report_departure(&skipped_past, QueuedBagDeparture::DiscardedUnread);
            }
        }
        if let Some(delivered) = &latest {
            self.report_departure(delivered, QueuedBagDeparture::DeliveredToProcessor);
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
        std::iter::from_fn(move || {
            let popped = self.queue.pop();
            if let Some(delivered) = &popped {
                self.report_departure(delivered, QueuedBagDeparture::DeliveredToProcessor);
            }
            popped
        })
    }
}

impl Drop for PortMailbox {
    /// A port unwired with bags still queued owes a departure for each of
    /// them, or their claims outlive the queue that made them.
    fn drop(&mut self) {
        if self.queued_bag_observer.get().is_none() {
            return;
        }
        while let Some(abandoned) = self.queue.pop() {
            self.report_departure(&abandoned, QueuedBagDeparture::DiscardedUnread);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct RecordingQueuedBagObserver {
        queued: Mutex<Vec<u8>>,
        departed: Mutex<Vec<(u8, QueuedBagDeparture)>>,
    }

    impl QueuedBagObserver for RecordingQueuedBagObserver {
        fn bag_queued(&self, wire_frame: &[u8]) {
            self.queued.lock().push(wire_frame[0]);
        }

        fn bag_departed(&self, wire_frame: &[u8], departure: QueuedBagDeparture) {
            self.departed.lock().push((wire_frame[0], departure));
        }
    }

    impl RecordingQueuedBagObserver {
        fn unanswered_claims(&self) -> usize {
            self.queued.lock().len() - self.departed.lock().len()
        }
    }

    fn observed_mailbox(history: usize) -> (Arc<RecordingQueuedBagObserver>, PortMailbox) {
        let observer = Arc::new(RecordingQueuedBagObserver::default());
        let slot: Arc<OnceLock<Arc<dyn QueuedBagObserver>>> = Arc::new(OnceLock::new());
        slot.set(Arc::clone(&observer) as Arc<dyn QueuedBagObserver>)
            .ok()
            .expect("the observer slot is empty");
        (observer, PortMailbox::new_observed_by(history, slot))
    }

    /// The install order the helper host actually uses: links are wired
    /// before the processor's contexts are built, so the observer arrives
    /// after the mailboxes do and must still reach them.
    #[test]
    fn an_observer_installed_after_the_mailbox_still_sees_its_bags() {
        let observer = Arc::new(RecordingQueuedBagObserver::default());
        let slot: Arc<OnceLock<Arc<dyn QueuedBagObserver>>> = Arc::new(OnceLock::new());
        let mailbox = PortMailbox::new_observed_by(4, Arc::clone(&slot));

        slot.set(Arc::clone(&observer) as Arc<dyn QueuedBagObserver>)
            .ok()
            .expect("the observer slot is empty");

        mailbox.push(vec![9]);
        mailbox.pop();
        assert_eq!(*observer.queued.lock(), vec![9]);
        assert_eq!(observer.unanswered_claims(), 0);
    }

    /// The pairing invariant, on the read path.
    #[test]
    fn a_bag_read_out_is_reported_as_delivered() {
        let (observer, mailbox) = observed_mailbox(4);
        mailbox.push(vec![7]);
        assert_eq!(observer.unanswered_claims(), 1);

        mailbox.pop();
        assert_eq!(
            *observer.departed.lock(),
            vec![(7, QueuedBagDeparture::DeliveredToProcessor)]
        );
    }

    /// The leak the observer exists to avoid: a queue at capacity throws
    /// bags away, and each one owes a departure even though nobody read it.
    #[test]
    fn a_bag_evicted_to_make_room_is_reported_as_discarded() {
        let (observer, mailbox) = observed_mailbox(2);
        mailbox.push(vec![1]);
        mailbox.push(vec![2]);
        mailbox.push(vec![3]);

        assert_eq!(
            *observer.departed.lock(),
            vec![(1, QueuedBagDeparture::DiscardedUnread)]
        );
        assert_eq!(observer.unanswered_claims(), 2);
    }

    /// A latest-wins read throws away everything it skipped past. Those bags
    /// are departures too — the common case for a lagging consumer, and the
    /// one that would otherwise leak a claim per dropped frame.
    #[test]
    fn a_latest_wins_read_reports_every_bag_it_skipped_past() {
        let (observer, mailbox) = observed_mailbox(4);
        mailbox.push(vec![1]);
        mailbox.push(vec![2]);
        mailbox.push(vec![3]);

        assert_eq!(mailbox.pop_latest(), Some(vec![3]));
        assert_eq!(
            *observer.departed.lock(),
            vec![
                (1, QueuedBagDeparture::DiscardedUnread),
                (2, QueuedBagDeparture::DiscardedUnread),
                (3, QueuedBagDeparture::DeliveredToProcessor),
            ]
        );
        assert_eq!(observer.unanswered_claims(), 0);
    }

    /// Unwiring a port with bags still in it settles them too.
    #[test]
    fn dropping_the_mailbox_reports_every_bag_still_queued() {
        let (observer, mailbox) = observed_mailbox(4);
        mailbox.push(vec![1]);
        mailbox.push(vec![2]);

        drop(mailbox);
        assert_eq!(observer.unanswered_claims(), 0);
        assert!(
            observer
                .departed
                .lock()
                .iter()
                .all(|(_, departure)| *departure == QueuedBagDeparture::DiscardedUnread)
        );
    }

    /// An unobserved mailbox is the native in-process path: same behavior,
    /// no bookkeeping.
    #[test]
    fn an_unobserved_mailbox_still_queues_and_pops() {
        let mailbox = PortMailbox::new(2);
        mailbox.push(vec![1]);
        mailbox.push(vec![2]);
        mailbox.push(vec![3]);

        assert_eq!(mailbox.len(), 2);
        assert_eq!(mailbox.pop(), Some(vec![2]));
        assert_eq!(mailbox.pop_latest(), Some(vec![3]));
        assert!(mailbox.is_empty());
    }
}
