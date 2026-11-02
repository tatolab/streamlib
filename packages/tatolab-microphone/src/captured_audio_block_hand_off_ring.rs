// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The bounded hand-off between a device's capture callback and the thread
//! that publishes.
//!
//! A device callback runs on the device's own thread and cannot wait for
//! anything downstream of it. So the callback only ever hands off: when a
//! stalled consumer fills the ring the oldest block is dropped at the device
//! edge and counted, and the gap stays derivable from the timestamps and
//! sample counts of the blocks either side of it. Nothing is interpolated and
//! no sample is invented.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// One captured block the ring owns until it is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedAudioBlockAwaitingPublish {
    /// Interleaved little-endian scalars, in the capture stream's format.
    pub interleaved_sample_bytes: Vec<u8>,
    /// Per-channel sample count.
    pub sample_count: u32,
    /// Monotonic timestamp of the block's first sample as the device timed it.
    pub first_sample_timestamp_ns: i64,
}

/// What the device callback and the publishing thread share under one lock,
/// so the publisher's wait and the hand-off's end cannot race.
#[derive(Debug, Default)]
struct CapturedAudioBlocksAwaitingPublish {
    blocks: VecDeque<CapturedAudioBlockAwaitingPublish>,
    hand_off_has_ended: bool,
}

/// What a publisher's wait produced.
///
/// Three answers rather than an `Option`, because collapsing "nothing arrived
/// yet" into "nothing will ever arrive again" leaves the caller's loop to tell
/// them apart — and a caller that gets that wrong spins at full tilt instead
/// of ending.
#[derive(Debug)]
pub enum NextCapturedAudioBlockToPublish {
    /// The oldest block that was awaiting publication.
    Block(CapturedAudioBlockAwaitingPublish),
    /// Nothing arrived inside the wait; the device may still be capturing.
    WaitTimedOut,
    /// No further block will ever be handed off, and none is left.
    HandOffEnded,
}

/// A bounded drop-oldest queue from a device capture callback to the thread
/// that publishes what it captured.
#[derive(Debug)]
pub struct CapturedAudioBlockHandOffRing {
    blocks_awaiting_publish: Mutex<CapturedAudioBlocksAwaitingPublish>,
    a_block_arrived_or_the_hand_off_ended: Condvar,
    block_capacity: usize,
    dropped_block_count: AtomicU64,
}

impl CapturedAudioBlockHandOffRing {
    /// A ring holding at most `block_capacity` blocks before it starts
    /// dropping the oldest. A capacity of zero would drop every block, so one
    /// is the floor.
    pub fn with_capacity(block_capacity: usize) -> Self {
        Self {
            blocks_awaiting_publish: Mutex::new(CapturedAudioBlocksAwaitingPublish::default()),
            a_block_arrived_or_the_hand_off_ended: Condvar::new(),
            block_capacity: block_capacity.max(1),
            dropped_block_count: AtomicU64::new(0),
        }
    }

    /// Hand a captured block to the publishing thread.
    ///
    /// Never waits on the publisher: at capacity the oldest block is dropped
    /// and counted, because a device callback that waited on a stalled
    /// consumer would stall the device itself.
    pub fn hand_off_from_device_callback(&self, block: CapturedAudioBlockAwaitingPublish) {
        let mut awaiting = self.lock_blocks_awaiting_publish();
        while awaiting.blocks.len() >= self.block_capacity {
            awaiting.blocks.pop_front();
            self.dropped_block_count.fetch_add(1, Ordering::Relaxed);
        }
        awaiting.blocks.push_back(block);
        drop(awaiting);
        self.a_block_arrived_or_the_hand_off_ended.notify_one();
    }

    /// Take the oldest block awaiting publication, waiting up to `timeout` for
    /// one to arrive.
    ///
    /// Blocks already handed off outlive the end of the hand-off: `HandOffEnded`
    /// only comes back once the ring is also empty, so a publisher that drains
    /// until it sees that one loses nothing the device captured.
    pub fn wait_for_next_block_to_publish(
        &self,
        timeout: Duration,
    ) -> NextCapturedAudioBlockToPublish {
        let mut awaiting = self.lock_blocks_awaiting_publish();
        loop {
            if let Some(block) = awaiting.blocks.pop_front() {
                return NextCapturedAudioBlockToPublish::Block(block);
            }
            if awaiting.hand_off_has_ended {
                return NextCapturedAudioBlockToPublish::HandOffEnded;
            }
            let (next_awaiting, wait) = self
                .a_block_arrived_or_the_hand_off_ended
                .wait_timeout(awaiting, timeout)
                .unwrap_or_else(PoisonError::into_inner);
            awaiting = next_awaiting;
            if wait.timed_out() {
                return match awaiting.blocks.pop_front() {
                    Some(block) => NextCapturedAudioBlockToPublish::Block(block),
                    None => NextCapturedAudioBlockToPublish::WaitTimedOut,
                };
            }
        }
    }

    /// Declare that no further block will be handed off, waking a publisher
    /// that is waiting for one.
    pub fn end_hand_off(&self) {
        self.lock_blocks_awaiting_publish().hand_off_has_ended = true;
        self.a_block_arrived_or_the_hand_off_ended.notify_all();
    }

    /// How many blocks the device edge has dropped rather than let a callback
    /// wait.
    pub fn dropped_block_count(&self) -> u64 {
        self.dropped_block_count.load(Ordering::Relaxed)
    }

    /// A poisoned ring is still a well-formed queue — a panicking publisher
    /// must not turn every later hand-off into a panic in a device callback.
    fn lock_blocks_awaiting_publish(&self) -> MutexGuard<'_, CapturedAudioBlocksAwaitingPublish> {
        self.blocks_awaiting_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    const A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM: Duration = Duration::from_secs(5);
    const A_WAIT_SHORT_ENOUGH_TO_TIME_OUT_IN: Duration = Duration::from_millis(50);

    fn block_stamped(first_sample_timestamp_ns: i64) -> CapturedAudioBlockAwaitingPublish {
        CapturedAudioBlockAwaitingPublish {
            interleaved_sample_bytes: vec![0u8; 8],
            sample_count: 2,
            first_sample_timestamp_ns,
        }
    }

    fn timestamps_drained_from(ring: &CapturedAudioBlockHandOffRing) -> Vec<i64> {
        let mut stamps = Vec::new();
        while let NextCapturedAudioBlockToPublish::Block(block) =
            ring.wait_for_next_block_to_publish(Duration::ZERO)
        {
            stamps.push(block.first_sample_timestamp_ns);
        }
        stamps
    }

    #[test]
    fn blocks_are_published_in_the_order_the_device_captured_them() {
        let ring = CapturedAudioBlockHandOffRing::with_capacity(8);
        for stamp in [10, 20, 30] {
            ring.hand_off_from_device_callback(block_stamped(stamp));
        }
        assert_eq!(timestamps_drained_from(&ring), vec![10, 20, 30]);
        assert_eq!(ring.dropped_block_count(), 0);
    }

    /// The loss is at the device edge and counted there. What survives is the
    /// newest audio, and the gap is visible in the timestamps either side.
    #[test]
    fn a_full_ring_drops_its_oldest_block_and_counts_it() {
        let ring = CapturedAudioBlockHandOffRing::with_capacity(2);
        for stamp in [10, 20, 30, 40] {
            ring.hand_off_from_device_callback(block_stamped(stamp));
        }
        assert_eq!(ring.dropped_block_count(), 2);
        assert_eq!(timestamps_drained_from(&ring), vec![30, 40]);
    }

    /// The invariant the whole ring exists for: a consumer that never drains
    /// must not turn a hand-off into a wait.
    #[test]
    fn a_publisher_that_never_drains_never_makes_a_hand_off_wait() {
        let ring = CapturedAudioBlockHandOffRing::with_capacity(4);
        let handing_off_began = Instant::now();
        for stamp in 0..10_000 {
            ring.hand_off_from_device_callback(block_stamped(stamp));
        }
        assert!(
            handing_off_began.elapsed() < A_WAIT_SHORT_ENOUGH_TO_TIME_OUT_IN,
            "10 000 hand-offs into a ring nobody drains took {:?} — a hand-off waited",
            handing_off_began.elapsed()
        );
        assert_eq!(ring.dropped_block_count(), 10_000 - 4);
    }

    #[test]
    fn waiting_on_an_empty_ring_times_out_rather_than_hanging() {
        let ring = CapturedAudioBlockHandOffRing::with_capacity(4);
        let waiting_began = Instant::now();
        assert!(matches!(
            ring.wait_for_next_block_to_publish(A_WAIT_SHORT_ENOUGH_TO_TIME_OUT_IN),
            NextCapturedAudioBlockToPublish::WaitTimedOut
        ));
        assert!(waiting_began.elapsed() >= A_WAIT_SHORT_ENOUGH_TO_TIME_OUT_IN);
    }

    #[test]
    fn ending_the_hand_off_wakes_a_publisher_that_is_waiting() {
        let ring = Arc::new(CapturedAudioBlockHandOffRing::with_capacity(4));
        let waiting_publisher = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let waiting_began = Instant::now();
                let block =
                    ring.wait_for_next_block_to_publish(A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM);
                (block, waiting_began.elapsed())
            })
        };

        std::thread::sleep(Duration::from_millis(20));
        ring.end_hand_off();

        let (waited_for, waited) = waiting_publisher.join().expect("publisher thread panicked");
        assert!(matches!(
            waited_for,
            NextCapturedAudioBlockToPublish::HandOffEnded
        ));
        assert!(
            waited < A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM,
            "the publisher sat out its whole timeout ({waited:?}) instead of being woken"
        );
    }

    /// The loss is explicit in both directions: the counter says how many
    /// blocks went, and the timestamps either side say when. Nothing is
    /// interpolated and no sample is invented, so the gap is arithmetic a
    /// consumer can do rather than a silence it cannot see.
    #[test]
    fn the_gap_a_drop_leaves_is_visible_in_the_timestamps_around_it() {
        const ONE_BLOCK_NS: i64 = 10_000_000;
        let ring = CapturedAudioBlockHandOffRing::with_capacity(2);

        ring.hand_off_from_device_callback(block_stamped(0));
        let published_before_the_stall = timestamps_drained_from(&ring);

        // The publisher stalls; the device keeps capturing regardless, which
        // is the whole situation the ring is for.
        for block_index in 1..=3 {
            ring.hand_off_from_device_callback(block_stamped(block_index * ONE_BLOCK_NS));
        }
        let published_after_the_stall = timestamps_drained_from(&ring);

        assert_eq!(published_before_the_stall, vec![0]);
        assert_eq!(
            published_after_the_stall,
            vec![2 * ONE_BLOCK_NS, 3 * ONE_BLOCK_NS]
        );
        assert_eq!(ring.dropped_block_count(), 1);
        assert_eq!(
            published_after_the_stall[0] - published_before_the_stall[0],
            (ring.dropped_block_count() as i64 + 1) * ONE_BLOCK_NS,
            "the jump across the gap is the blocks that were lost plus the one that \
             survived — which is what makes the loss derivable downstream"
        );
    }

    /// Ending the hand-off is not a discard: whatever the device already
    /// captured still reaches the link, and only once the ring is empty does
    /// the publisher learn there is nothing more.
    #[test]
    fn blocks_already_handed_off_survive_the_end_of_the_hand_off() {
        let ring = CapturedAudioBlockHandOffRing::with_capacity(4);
        ring.hand_off_from_device_callback(block_stamped(10));
        ring.end_hand_off();

        assert!(matches!(
            ring.wait_for_next_block_to_publish(Duration::ZERO),
            NextCapturedAudioBlockToPublish::Block(block) if block.first_sample_timestamp_ns == 10
        ));
        assert!(matches!(
            ring.wait_for_next_block_to_publish(Duration::ZERO),
            NextCapturedAudioBlockToPublish::HandOffEnded
        ));
    }
}
