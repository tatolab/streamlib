// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The lock-free hand-off between the device thread and the one thread that drains it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;

/// One device period, as read off the device.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CapturedAudioBlock {
    pub(crate) interleaved_f32_sample_bytes: Vec<u8>,
    pub(crate) sample_count: u32,
    pub(crate) first_sample_timestamp_ns: i64,
}

/// What a wait for the next block came back with.
#[derive(Debug, PartialEq)]
pub(crate) enum NextCapturedAudioBlock {
    Block(CapturedAudioBlock),
    WaitTimedOut,
    HandOffEnded,
}

/// A second thread asked to drain a ring that wakes only the first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RingDrainedFromASecondThread;

/// Bounded single-producer ring: the device thread never waits, and a full ring drops
/// its oldest block, counted.
///
/// The draining thread is woken by `unpark`, so the producer takes no lock; the first
/// thread to wait is the only one woken, and any other is refused.
pub(crate) struct CapturedAudioBlockRing {
    queue: ArrayQueue<CapturedAudioBlock>,
    dropped_block_count: AtomicU64,
    hand_off_ended: AtomicBool,
    draining_thread: OnceLock<Thread>,
}

impl CapturedAudioBlockRing {
    pub(crate) fn with_capacity(capacity_in_blocks: usize) -> Self {
        Self {
            queue: ArrayQueue::new(capacity_in_blocks),
            dropped_block_count: AtomicU64::new(0),
            hand_off_ended: AtomicBool::new(false),
            draining_thread: OnceLock::new(),
        }
    }

    /// Called on the device thread; never blocks.
    pub(crate) fn hand_off_from_device_thread(&self, block: CapturedAudioBlock) {
        if self.queue.force_push(block).is_some() {
            self.dropped_block_count.fetch_add(1, Ordering::Relaxed);
        }
        self.wake_the_draining_thread();
    }

    /// No further block will arrive; a waiting drain returns once the ring is empty.
    pub(crate) fn end_hand_off(&self) {
        self.hand_off_ended.store(true, Ordering::Release);
        self.wake_the_draining_thread();
    }

    pub(crate) fn dropped_block_count(&self) -> u64 {
        self.dropped_block_count.load(Ordering::Relaxed)
    }

    /// The oldest block, waiting up to `timeout` for one.
    pub(crate) fn wait_for_next_block(
        &self,
        timeout: Duration,
    ) -> Result<NextCapturedAudioBlock, RingDrainedFromASecondThread> {
        let current_thread = std::thread::current();
        let draining_thread = self.draining_thread.get_or_init(|| current_thread.clone());
        if draining_thread.id() != current_thread.id() {
            return Err(RingDrainedFromASecondThread);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(block) = self.queue.pop() {
                return Ok(NextCapturedAudioBlock::Block(block));
            }
            if self.hand_off_ended.load(Ordering::Acquire) {
                return Ok(NextCapturedAudioBlock::HandOffEnded);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(NextCapturedAudioBlock::WaitTimedOut);
            }
            // A hand-off between the pop above and this park leaves the unpark token
            // set, so the park returns at once rather than missing the block.
            std::thread::park_timeout(deadline - now);
        }
    }

    fn wake_the_draining_thread(&self) {
        if let Some(draining_thread) = self.draining_thread.get() {
            draining_thread.unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn block_stamped(first_sample_timestamp_ns: i64) -> CapturedAudioBlock {
        CapturedAudioBlock {
            interleaved_f32_sample_bytes: vec![0; 8],
            sample_count: 2,
            first_sample_timestamp_ns,
        }
    }

    #[test]
    fn blocks_come_out_in_the_order_they_went_in() {
        let ring = CapturedAudioBlockRing::with_capacity(4);
        ring.hand_off_from_device_thread(block_stamped(1));
        ring.hand_off_from_device_thread(block_stamped(2));

        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::Block(block_stamped(1)))
        );
        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::Block(block_stamped(2)))
        );
        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::WaitTimedOut)
        );
    }

    #[test]
    fn a_full_ring_drops_its_oldest_block_and_counts_it() {
        let ring = CapturedAudioBlockRing::with_capacity(2);
        for stamp in 1..=3 {
            ring.hand_off_from_device_thread(block_stamped(stamp));
        }

        assert_eq!(ring.dropped_block_count(), 1);
        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::Block(block_stamped(2)))
        );
    }

    #[test]
    fn an_ended_hand_off_still_yields_what_it_already_holds() {
        let ring = CapturedAudioBlockRing::with_capacity(2);
        ring.hand_off_from_device_thread(block_stamped(1));
        ring.end_hand_off();

        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::Block(block_stamped(1)))
        );
        assert_eq!(
            ring.wait_for_next_block(Duration::from_secs(5)),
            Ok(NextCapturedAudioBlock::HandOffEnded)
        );
    }

    #[test]
    fn a_waiting_drain_wakes_on_a_hand_off_long_before_its_timeout() {
        let ring = Arc::new(CapturedAudioBlockRing::with_capacity(2));
        let draining_ring = Arc::clone(&ring);
        let drain = std::thread::spawn(move || {
            let started = Instant::now();
            let next = draining_ring.wait_for_next_block(Duration::from_secs(10));
            (next, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        ring.hand_off_from_device_thread(block_stamped(7));

        let (next, waited) = drain.join().expect("drain thread");
        assert_eq!(next, Ok(NextCapturedAudioBlock::Block(block_stamped(7))));
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    }

    #[test]
    fn a_second_draining_thread_is_refused() {
        let ring = Arc::new(CapturedAudioBlockRing::with_capacity(2));
        assert_eq!(
            ring.wait_for_next_block(Duration::ZERO),
            Ok(NextCapturedAudioBlock::WaitTimedOut)
        );
        let other_ring = Arc::clone(&ring);

        let refusal = std::thread::spawn(move || other_ring.wait_for_next_block(Duration::ZERO))
            .join()
            .expect("second thread");

        assert_eq!(refusal, Err(RingDrainedFromASecondThread));
    }
}
