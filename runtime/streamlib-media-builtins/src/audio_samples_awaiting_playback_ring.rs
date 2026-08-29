// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The bounded hand-off between the thread draining an input port and the
//! device's playback callback.
//!
//! It is the capture ring's mirror, and the asymmetry is the point. On capture
//! the device is the producer and must never wait, so a full ring drops its
//! oldest block. On playback the *graph* is the producer, its port declares
//! `lossless`, and the one thing that must never wait is the device callback —
//! so this ring makes the drain thread wait for room, and takes whatever is
//! there when the callback asks.
//!
//! The wait backs the drain thread's mailbox up, which is as far as
//! backpressure reaches today: `PortMailbox::push_frame_from_inbound_link`
//! evicts its oldest entry when full whatever a port's profile says, so a
//! producer racing far enough ahead loses blocks there rather than being held.
//! Nothing here can close that — it is a transport-layer gap this ring is
//! downstream of, and one the port counts per link now rather than swallowing.
//!
//! Samples rather than blocks, because a device period and a published block
//! are different sizes and neither divides the other. What a callback needs is
//! a period's worth of the stream in order; where the block boundaries fell is
//! not something playback can act on.
//!
//! Playback does not begin until a few periods are queued. Without that a
//! device fed by another device runs in lockstep with it and no cushion ever
//! forms, so every scheduling jitter costs a whole period — measured at four
//! lost periods in fourteen on a microphone wired to a speaker. The pre-roll is
//! silence like an underrun is, and is counted separately from one, because a
//! stream that has not started yet and a stream that fell behind need different
//! answers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What the drain thread and the device callback share under one lock, so the
/// drain's wait and the end of playback cannot race.
#[derive(Debug)]
struct AudioSamplesAwaitingPlayback {
    interleaved_sample_bytes: VecDeque<u8>,
    playback_has_ended: bool,
    /// False until enough was queued to start on. The device's own request
    /// size is what "enough" is measured in, so the cushion is stated in the
    /// device's periods rather than guessed at from the format.
    the_cushion_has_filled: bool,
}

/// What handing samples over produced.
///
/// `#[must_use]` because dropping it is a silent bug: a drain loop that ignores
/// `PlaybackEnded` keeps reading its port and queueing into a ring nothing will
/// ever take from again.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub enum AudioSamplesHandOffOutcome {
    /// Every byte is queued, in order, for the device to take.
    Queued,
    /// Playback ended before every byte was queued; the tail was not played.
    PlaybackEnded,
}

/// Device periods that must be queued before the first one is served.
///
/// Two rather than one: one leaves the cushion exactly empty the instant it is
/// spent, so the very next jitter is an underrun again. Two costs one more
/// period of latency — about 21 ms at a PipeWire quantum, against a
/// conversational budget an LLM already dominates — and buys a stream that
/// survives a late block.
const DEVICE_PERIODS_QUEUED_BEFORE_PLAYBACK_BEGINS: usize = 2;

/// A bounded, in-order queue of interleaved sample bytes from the graph to a
/// playback device's callback.
#[derive(Debug)]
pub struct AudioSamplesAwaitingPlaybackRing {
    samples_awaiting_playback: Mutex<AudioSamplesAwaitingPlayback>,
    room_was_made_or_playback_ended: Condvar,
    byte_capacity: usize,
    /// How many bytes of silence the device had to be given because the graph
    /// fell behind after playback had begun.
    underrun_byte_count: AtomicU64,
    /// How many bytes of silence the device was given while the cushion was
    /// still filling. Counted apart from an underrun because it is the cost of
    /// starting, not a fault.
    silence_played_before_the_cushion_filled_byte_count: AtomicU64,
}

impl AudioSamplesAwaitingPlaybackRing {
    /// A ring holding at most `byte_capacity` bytes of queued samples. A
    /// capacity of zero could never accept a sample, so one is the floor.
    pub fn with_byte_capacity(byte_capacity: usize) -> Self {
        let byte_capacity = byte_capacity.max(1);
        Self {
            // Allocated up front rather than grown: every doubling would
            // otherwise happen inside the lock the device callback contends
            // on, and the growth window is exactly the first seconds of
            // playback — the window the cushion exists to protect. The bound
            // is enforced either way, so this reserves nothing the ring would
            // not reach.
            samples_awaiting_playback: Mutex::new(AudioSamplesAwaitingPlayback {
                interleaved_sample_bytes: VecDeque::with_capacity(byte_capacity),
                playback_has_ended: false,
                the_cushion_has_filled: false,
            }),
            room_was_made_or_playback_ended: Condvar::new(),
            byte_capacity,
            underrun_byte_count: AtomicU64::new(0),
            silence_played_before_the_cushion_filled_byte_count: AtomicU64::new(0),
        }
    }

    /// Queue every byte for the device, waiting for room as many times as it
    /// takes.
    ///
    /// The wait is the backpressure `lossless` names: a caller held here is a
    /// caller not draining its mailbox, which is what makes the producer block
    /// rather than this ring drop. Bytes are queued in pieces as room appears,
    /// which changes nothing about what is played — samples are a stream, and
    /// where one block ended is not something a device acts on.
    ///
    /// `room_wait_poll_interval` only bounds how long a wait sits before it
    /// re-reads the state; ending playback is what releases a caller for good.
    pub fn hand_off_for_playback(
        &self,
        interleaved_sample_bytes: &[u8],
        room_wait_poll_interval: Duration,
    ) -> AudioSamplesHandOffOutcome {
        let mut still_to_queue = interleaved_sample_bytes;
        let mut awaiting = self.lock_samples_awaiting_playback();
        loop {
            if awaiting.playback_has_ended {
                return AudioSamplesHandOffOutcome::PlaybackEnded;
            }
            let room = self
                .byte_capacity
                .saturating_sub(awaiting.interleaved_sample_bytes.len());
            let queued_now = room.min(still_to_queue.len());
            awaiting
                .interleaved_sample_bytes
                .extend(&still_to_queue[..queued_now]);
            still_to_queue = &still_to_queue[queued_now..];
            if still_to_queue.is_empty() {
                return AudioSamplesHandOffOutcome::Queued;
            }
            awaiting = self
                .room_was_made_or_playback_ended
                .wait_timeout(awaiting, room_wait_poll_interval)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Fill a device period with whatever is queued, returning how many bytes
    /// had to be silence instead.
    ///
    /// Never waits: a callback that waited on a stalled graph would stall the
    /// device itself, which is the whole reason this ring exists. What it
    /// cannot fill it zeroes and counts — a period left partly unwritten would
    /// replay whatever the mapping last held, and silence that nothing counted
    /// is a fault nobody can see.
    ///
    /// Until the cushion has filled the answer is a whole period of silence,
    /// counted as the cost of starting rather than as an underrun.
    pub fn fill_one_device_period(&self, interleaved_sample_bytes_to_fill: &mut [u8]) -> usize {
        let mut awaiting = self.lock_samples_awaiting_playback();
        if !awaiting.the_cushion_has_filled {
            // Never more than the ring can hold: a cushion larger than the
            // capacity is one that never fills, and a device that never starts
            // is worse than one that starts with no cushion.
            let bytes_to_start_on = (interleaved_sample_bytes_to_fill.len()
                * DEVICE_PERIODS_QUEUED_BEFORE_PLAYBACK_BEGINS)
                .min(self.byte_capacity);
            if awaiting.interleaved_sample_bytes.len() < bytes_to_start_on {
                drop(awaiting);
                interleaved_sample_bytes_to_fill.fill(0);
                self.silence_played_before_the_cushion_filled_byte_count
                    .fetch_add(
                        interleaved_sample_bytes_to_fill.len() as u64,
                        Ordering::Relaxed,
                    );
                return interleaved_sample_bytes_to_fill.len();
            }
            awaiting.the_cushion_has_filled = true;
        }

        let filled = awaiting
            .interleaved_sample_bytes
            .len()
            .min(interleaved_sample_bytes_to_fill.len());
        // Copied as the one or two contiguous runs a `VecDeque` actually is,
        // rather than popped a byte at a time: the lock is held for a memcpy
        // instead of eight thousand bounds-checked pops, and the drain thread
        // is blocked on that same lock. `drain` of a front range advances the
        // head, so it moves no element either.
        {
            let (oldest_run, newer_run) = awaiting.interleaved_sample_bytes.as_slices();
            let from_oldest = oldest_run.len().min(filled);
            interleaved_sample_bytes_to_fill[..from_oldest]
                .copy_from_slice(&oldest_run[..from_oldest]);
            interleaved_sample_bytes_to_fill[from_oldest..filled]
                .copy_from_slice(&newer_run[..filled - from_oldest]);
        }
        awaiting.interleaved_sample_bytes.drain(..filled);
        drop(awaiting);
        self.room_was_made_or_playback_ended.notify_one();

        let underran_by = interleaved_sample_bytes_to_fill.len() - filled;
        if underran_by > 0 {
            interleaved_sample_bytes_to_fill[filled..].fill(0);
            self.underrun_byte_count
                .fetch_add(underran_by as u64, Ordering::Relaxed);
        }
        underran_by
    }

    /// Declare that nothing further will be played, releasing a caller waiting
    /// for room.
    pub fn end_playback(&self) {
        self.lock_samples_awaiting_playback().playback_has_ended = true;
        self.room_was_made_or_playback_ended.notify_all();
    }

    /// How many bytes of silence the device has been given because the graph
    /// fell behind after playback began.
    pub fn underrun_byte_count(&self) -> u64 {
        self.underrun_byte_count.load(Ordering::Relaxed)
    }

    /// How many bytes of silence the device was given while the cushion was
    /// still filling — the cost of starting, not a fault.
    pub fn silence_played_before_the_cushion_filled_byte_count(&self) -> u64 {
        self.silence_played_before_the_cushion_filled_byte_count
            .load(Ordering::Relaxed)
    }

    /// A poisoned ring is still a well-formed queue — a panicking drain thread
    /// must not turn every later device callback into a panic.
    fn lock_samples_awaiting_playback(&self) -> MutexGuard<'_, AudioSamplesAwaitingPlayback> {
        self.samples_awaiting_playback
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    const A_POLL_SHORT_ENOUGH_TO_LOOP_IN: Duration = Duration::from_millis(10);
    const A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM: Duration = Duration::from_secs(5);

    /// A ring, already past its pre-roll, so a test about serving is not also a
    /// test about starting.
    fn a_playing_ring(
        byte_capacity: usize,
        device_period_bytes: usize,
    ) -> AudioSamplesAwaitingPlaybackRing {
        let ring = AudioSamplesAwaitingPlaybackRing::with_byte_capacity(byte_capacity);
        let cushion = vec![0u8; device_period_bytes * DEVICE_PERIODS_QUEUED_BEFORE_PLAYBACK_BEGINS];
        let _ = ring.hand_off_for_playback(&cushion, A_POLL_SHORT_ENOUGH_TO_LOOP_IN);
        let mut period = vec![0u8; device_period_bytes];
        while !ring.lock_samples_awaiting_playback().the_cushion_has_filled {
            ring.fill_one_device_period(&mut period);
        }
        // Drain whatever the cushion left, so a test starts from an empty queue.
        while !ring
            .lock_samples_awaiting_playback()
            .interleaved_sample_bytes
            .is_empty()
        {
            ring.fill_one_device_period(&mut period);
        }
        ring
    }

    #[test]
    fn samples_reach_the_device_in_the_order_the_graph_queued_them() {
        let ring = a_playing_ring(64, 6);
        assert_eq!(
            ring.hand_off_for_playback(&[1, 2, 3, 4], A_POLL_SHORT_ENOUGH_TO_LOOP_IN),
            AudioSamplesHandOffOutcome::Queued
        );
        assert_eq!(
            ring.hand_off_for_playback(&[5, 6], A_POLL_SHORT_ENOUGH_TO_LOOP_IN),
            AudioSamplesHandOffOutcome::Queued
        );

        let mut period = [0u8; 6];
        assert_eq!(ring.fill_one_device_period(&mut period), 0);
        assert_eq!(period, [1, 2, 3, 4, 5, 6]);
        assert_eq!(ring.underrun_byte_count(), 0);
    }

    /// A device period and a published block are different sizes, so a period
    /// routinely straddles a block boundary. The stream has to be continuous
    /// across it — a boundary that reset anything would click every block.
    #[test]
    fn a_device_period_is_served_across_block_boundaries() {
        let ring = a_playing_ring(64, 4);
        let _ = ring.hand_off_for_playback(&[1, 2, 3], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);
        let _ = ring.hand_off_for_playback(&[4, 5, 6], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);

        let mut first_period = [0u8; 4];
        ring.fill_one_device_period(&mut first_period);
        let mut second_period = [0u8; 2];
        ring.fill_one_device_period(&mut second_period);

        assert_eq!(first_period, [1, 2, 3, 4]);
        assert_eq!(second_period, [5, 6]);
    }

    /// The invariant the whole ring exists for: an empty queue costs the device
    /// counted silence, never a wait.
    #[test]
    fn a_device_callback_finding_nothing_queued_is_given_counted_silence() {
        let ring = a_playing_ring(64, 8);
        let mut period = [0xAAu8; 8];

        let filling_began = Instant::now();
        assert_eq!(ring.fill_one_device_period(&mut period), 8);
        assert!(
            filling_began.elapsed() < A_POLL_SHORT_ENOUGH_TO_LOOP_IN,
            "filling a period from an empty ring waited {:?} — a device callback blocked",
            filling_began.elapsed()
        );

        assert_eq!(period, [0; 8], "what could not be filled is silence");
        assert_eq!(
            ring.underrun_byte_count(),
            8,
            "silence the graph did not supply is counted, never invented quietly"
        );
    }

    /// A partly-served period is the ordinary case at the end of a stream: what
    /// is there plays, and only the remainder is counted as underrun.
    #[test]
    fn a_partly_served_period_counts_only_the_silence_it_had_to_invent() {
        let ring = a_playing_ring(64, 5);
        let _ = ring.hand_off_for_playback(&[7, 7, 7], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);

        let mut period = [0xAAu8; 5];
        assert_eq!(ring.fill_one_device_period(&mut period), 2);
        assert_eq!(period, [7, 7, 7, 0, 0]);
        assert_eq!(ring.underrun_byte_count(), 2);
    }

    /// The pre-roll, and why it is not an underrun: a device asks the moment
    /// its stream connects, and the graph cannot have published anything yet.
    ///
    /// Mental revert: serve the first period from a half-filled queue and a
    /// speaker fed by a microphone runs with no cushion at all, losing a whole
    /// period to every scheduling jitter — four in fourteen, measured.
    #[test]
    fn nothing_is_served_until_a_cushion_of_periods_has_been_queued() {
        const PERIOD_BYTES: usize = 4;
        let ring = AudioSamplesAwaitingPlaybackRing::with_byte_capacity(64);
        let mut period = [0xAAu8; PERIOD_BYTES];

        // One period queued is not enough to start on.
        let _ = ring.hand_off_for_playback(&[1, 2, 3, 4], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);
        assert_eq!(ring.fill_one_device_period(&mut period), PERIOD_BYTES);
        assert_eq!(period, [0; PERIOD_BYTES], "the cushion is still filling");
        assert_eq!(
            ring.underrun_byte_count(),
            0,
            "a stream that has not started is not a stream that fell behind"
        );
        assert_eq!(
            ring.silence_played_before_the_cushion_filled_byte_count(),
            PERIOD_BYTES as u64,
            "the cost of starting is counted too — nothing is silent and uncounted"
        );

        // The second period fills the cushion, so the first one queued plays.
        let _ = ring.hand_off_for_playback(&[5, 6, 7, 8], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);
        assert_eq!(ring.fill_one_device_period(&mut period), 0);
        assert_eq!(period, [1, 2, 3, 4]);
    }

    /// Once playback has begun it does not stop to refill: a stream that
    /// re-primed after every underrun would answer a single late block with a
    /// gap several periods long.
    #[test]
    fn a_started_stream_does_not_pre_roll_again_after_an_underrun() {
        let ring = a_playing_ring(64, 4);
        let mut period = [0u8; 4];

        assert_eq!(ring.fill_one_device_period(&mut period), 4, "starved");
        let _ = ring.hand_off_for_playback(&[1, 2, 3, 4], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);
        assert_eq!(
            ring.fill_one_device_period(&mut period),
            0,
            "one period queued is enough to serve once the stream is playing"
        );
        assert_eq!(period, [1, 2, 3, 4]);
    }

    /// A ring too small to hold the whole cushion still starts. A device that
    /// never starts is worse than one that starts without a cushion.
    #[test]
    fn a_ring_smaller_than_the_cushion_still_begins_playing() {
        let ring = AudioSamplesAwaitingPlaybackRing::with_byte_capacity(4);
        let _ = ring.hand_off_for_playback(&[1, 2, 3, 4], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);

        let mut period = [0u8; 4];
        assert_eq!(ring.fill_one_device_period(&mut period), 0);
        assert_eq!(period, [1, 2, 3, 4]);
    }

    /// The backpressure `lossless` names: a full ring holds the drain thread,
    /// which stops it draining its mailbox, which is what makes the producer
    /// block. A ring that dropped here instead would make `lossless` a lie no
    /// test downstream could catch.
    #[test]
    fn a_full_ring_holds_the_graph_rather_than_dropping_what_it_could_not_take() {
        let ring = Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(4));
        let _ = ring.hand_off_for_playback(&[1, 2, 3, 4], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);

        let handing_off = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                ring.hand_off_for_playback(&[5, 6, 7, 8], A_POLL_SHORT_ENOUGH_TO_LOOP_IN)
            })
        };

        // Whatever the drain thread managed to queue is bounded by the ring,
        // so the device sees the first bytes and only then the rest.
        let mut first_period = [0u8; 4];
        let mut played = Vec::new();
        while played.len() < 8 {
            let underran_by = ring.fill_one_device_period(&mut first_period);
            played.extend_from_slice(&first_period[..first_period.len() - underran_by]);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(
            handing_off.join().expect("the drain thread panicked"),
            AudioSamplesHandOffOutcome::Queued
        );
        assert_eq!(
            played,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            "every byte reached the device, in order and exactly once"
        );
    }

    /// A block larger than the ring is queued in pieces rather than deadlocking
    /// or being refused: what a caller hands over is a stretch of a stream, and
    /// the ring's size is a buffering choice that must not become a limit on
    /// what a graph may publish.
    #[test]
    fn a_block_larger_than_the_whole_ring_is_still_played_in_full() {
        let ring = Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(4));
        let block: Vec<u8> = (1..=16).collect();

        let handing_off = {
            let ring = Arc::clone(&ring);
            let block = block.clone();
            std::thread::spawn(move || {
                ring.hand_off_for_playback(&block, A_POLL_SHORT_ENOUGH_TO_LOOP_IN)
            })
        };

        let mut period = [0u8; 3];
        let mut played = Vec::new();
        while played.len() < block.len() {
            let underran_by = ring.fill_one_device_period(&mut period);
            played.extend_from_slice(&period[..period.len() - underran_by]);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(
            handing_off.join().expect("the drain thread panicked"),
            AudioSamplesHandOffOutcome::Queued
        );
        assert_eq!(played, block);
    }

    #[test]
    fn ending_playback_releases_a_drain_thread_waiting_for_room() {
        let ring = Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(2));
        let _ = ring.hand_off_for_playback(&[1, 2], A_POLL_SHORT_ENOUGH_TO_LOOP_IN);

        let handing_off = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let waiting_began = Instant::now();
                let outcome =
                    ring.hand_off_for_playback(&[3, 4], A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM);
                (outcome, waiting_began.elapsed())
            })
        };

        std::thread::sleep(Duration::from_millis(20));
        ring.end_playback();

        let (outcome, waited) = handing_off.join().expect("the drain thread panicked");
        assert_eq!(outcome, AudioSamplesHandOffOutcome::PlaybackEnded);
        assert!(
            waited < A_WAIT_LONG_ENOUGH_TO_BE_WOKEN_FROM,
            "the drain thread sat out its whole poll interval ({waited:?}) instead of being woken"
        );
    }

    /// Ending playback while the ring has room is not a wait at all — a stop
    /// that arrives between blocks must not queue another one.
    #[test]
    fn nothing_is_queued_once_playback_has_ended() {
        let ring = a_playing_ring(64, 3);
        ring.end_playback();

        assert_eq!(
            ring.hand_off_for_playback(&[1, 2, 3], A_POLL_SHORT_ENOUGH_TO_LOOP_IN),
            AudioSamplesHandOffOutcome::PlaybackEnded
        );
        let mut period = [0xAAu8; 3];
        assert_eq!(
            ring.fill_one_device_period(&mut period),
            3,
            "nothing was queued, so the device gets silence"
        );
    }
}
