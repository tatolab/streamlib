// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{HeldV4l2Buffer, NextFilledV4l2Buffer, V4l2CaptureBufferQueue};

const BUFFER_COUNT: u32 = 4;
const CAMERA_NAME: &str = "withholding camera";

/// A capture queue that keeps videobuf2's rules: a buffer the driver holds,
/// queued or filled, cannot be queued again (`EINVAL`), and a dequeue with no
/// filled buffer would block (`EAGAIN`). Each wait fills the oldest queued
/// buffer with the next frame, unless that frame is being withheld; a frame
/// whose sequence is listed comes back marked damaged.
struct CaptureQueueKeepingVideobuf2sRules {
    damaged_frame_sequences: Vec<u32>,
    queued: VecDeque<u32>,
    filled: VecDeque<(u32, u32)>,
    next_frame_sequence: u32,
    empty_waits_before_each_frame: VecDeque<u32>,
    empty_waits: u32,
    queues_refused_for_a_buffer_the_driver_held: u32,
    stop_after_this_many_empty_waits: Option<(u32, Arc<AtomicBool>)>,
}

impl CaptureQueueKeepingVideobuf2sRules {
    fn withholding_frames_for(
        empty_waits_before_each_frame: impl IntoIterator<Item = u32>,
    ) -> Self {
        let mut queue = Self {
            damaged_frame_sequences: Vec::new(),
            queued: VecDeque::new(),
            filled: VecDeque::new(),
            next_frame_sequence: 0,
            empty_waits_before_each_frame: empty_waits_before_each_frame.into_iter().collect(),
            empty_waits: 0,
            queues_refused_for_a_buffer_the_driver_held: 0,
            stop_after_this_many_empty_waits: None,
        };
        for index in 0..BUFFER_COUNT {
            queue
                .queue_a_held_buffer(HeldV4l2Buffer::not_yet_queued(index))
                .expect("every buffer starts out with the capture");
        }
        queue
    }

    fn holds(&self, index: u32) -> bool {
        self.queued.contains(&index) || self.filled.iter().any(|&(held, _)| held == index)
    }
}

impl V4l2CaptureBufferQueue for CaptureQueueKeepingVideobuf2sRules {
    fn wait_for_a_filled_buffer(&mut self, _wait: Duration) -> io::Result<bool> {
        if !self.filled.is_empty() {
            return Ok(true);
        }
        let withheld = self.empty_waits_before_each_frame.front_mut();
        if let Some(empty_waits_left) = withheld.filter(|left| **left > 0) {
            *empty_waits_left -= 1;
            self.empty_waits += 1;
            if let Some((stop_after, is_capturing)) = &self.stop_after_this_many_empty_waits
                && self.empty_waits >= *stop_after
            {
                is_capturing.store(false, Ordering::Release);
            }
            return Ok(false);
        }
        self.empty_waits_before_each_frame.pop_front();
        let Some(index) = self.queued.pop_front() else {
            return Ok(false);
        };
        self.filled.push_back((index, self.next_frame_sequence));
        self.next_frame_sequence += 1;
        Ok(true)
    }

    fn dequeue_a_filled_buffer(&mut self) -> io::Result<HeldV4l2Buffer> {
        let (index, sequence) = self
            .filled
            .pop_front()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EAGAIN))?;
        let mut buffer = HeldV4l2Buffer::not_yet_queued(index);
        buffer.0.sequence = sequence;
        if self.damaged_frame_sequences.contains(&sequence) {
            buffer.0.flags |= v4l::buffer::Flags::ERROR.bits();
        }
        Ok(buffer)
    }

    fn queue_a_held_buffer(&mut self, buffer: HeldV4l2Buffer) -> io::Result<()> {
        if self.holds(buffer.index()) {
            self.queues_refused_for_a_buffer_the_driver_held += 1;
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        self.queued.push_back(buffer.index());
        Ok(())
    }
}

/// Read `frame_count` frames the way the capture thread does, answering the
/// sequence of each.
fn capture_frames(
    queue: &mut CaptureQueueKeepingVideobuf2sRules,
    is_capturing: &AtomicBool,
    frame_count: usize,
) -> Vec<u32> {
    let mut sequences = Vec::new();
    while sequences.len() < frame_count {
        match queue.wait_for_the_next_filled_buffer(CAMERA_NAME, is_capturing) {
            NextFilledV4l2Buffer::Filled(buffer) => {
                sequences.push(buffer.sequence());
                queue.requeue_a_read_buffer(CAMERA_NAME, buffer);
            }
            other => panic!("capture ended before frame {}: {other:?}", sequences.len()),
        }
    }
    sequences
}

/// The bug was a retry after an empty wait queuing a buffer the driver still
/// held, which videobuf2 refuses and which ended capture for good.
#[test]
fn a_frame_that_comes_after_several_empty_waits_is_captured_and_capture_goes_on() {
    let mut queue = CaptureQueueKeepingVideobuf2sRules::withholding_frames_for([0, 3, 0, 5, 0, 0]);
    let is_capturing = AtomicBool::new(true);

    let sequences = capture_frames(&mut queue, &is_capturing, 6);

    assert_eq!(sequences, vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(queue.empty_waits, 8);
    assert_eq!(
        queue.queues_refused_for_a_buffer_the_driver_held, 0,
        "a wait that ended empty queued a buffer the driver already held"
    );
}

#[test]
fn a_stop_while_a_frame_is_overdue_ends_the_wait_at_the_next_look() {
    let is_capturing = Arc::new(AtomicBool::new(true));
    let mut queue = CaptureQueueKeepingVideobuf2sRules::withholding_frames_for([0, u32::MAX]);
    queue.stop_after_this_many_empty_waits = Some((2, Arc::clone(&is_capturing)));

    assert_eq!(capture_frames(&mut queue, &is_capturing, 1), vec![0]);
    let after_the_stop = queue.wait_for_the_next_filled_buffer(CAMERA_NAME, &is_capturing);

    assert!(
        matches!(after_the_stop, NextFilledV4l2Buffer::CaptureStopped),
        "{after_the_stop:?}"
    );
    assert_eq!(
        queue.empty_waits, 2,
        "the stop was not noticed at the first look after it"
    );
}

#[test]
fn a_ready_device_with_no_filled_buffer_is_waited_on_again() {
    struct ReadyOnceWithNothingThenAFrame {
        dequeues: u32,
    }
    impl V4l2CaptureBufferQueue for ReadyOnceWithNothingThenAFrame {
        fn wait_for_a_filled_buffer(&mut self, _wait: Duration) -> io::Result<bool> {
            Ok(true)
        }
        fn dequeue_a_filled_buffer(&mut self) -> io::Result<HeldV4l2Buffer> {
            self.dequeues += 1;
            if self.dequeues == 1 {
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            Ok(HeldV4l2Buffer::not_yet_queued(0))
        }
        fn queue_a_held_buffer(&mut self, _buffer: HeldV4l2Buffer) -> io::Result<()> {
            Ok(())
        }
    }
    let mut queue = ReadyOnceWithNothingThenAFrame { dequeues: 0 };

    let next = queue.wait_for_the_next_filled_buffer(CAMERA_NAME, &AtomicBool::new(true));

    assert!(matches!(next, NextFilledV4l2Buffer::Filled(_)), "{next:?}");
    assert_eq!(queue.dequeues, 2);
}

#[test]
fn a_device_whose_poll_fails_ends_capture_with_the_reason() {
    struct PollFails;
    impl V4l2CaptureBufferQueue for PollFails {
        fn wait_for_a_filled_buffer(&mut self, _wait: Duration) -> io::Result<bool> {
            Err(io::Error::from_raw_os_error(libc::ENODEV))
        }
        fn dequeue_a_filled_buffer(&mut self) -> io::Result<HeldV4l2Buffer> {
            unreachable!("nothing is dequeued from a device whose poll failed")
        }
        fn queue_a_held_buffer(&mut self, _buffer: HeldV4l2Buffer) -> io::Result<()> {
            Ok(())
        }
    }

    let next = PollFails.wait_for_the_next_filled_buffer(CAMERA_NAME, &AtomicBool::new(true));

    let NextFilledV4l2Buffer::DeviceFailed(reason) = next else {
        panic!("expected the device to have failed: {next:?}");
    };
    assert!(reason.starts_with("polling the device failed"), "{reason}");
}

/// The driver hands a damaged frame back through a successful dequeue; it is
/// given back and never delivered, and capture goes on.
#[test]
fn a_frame_the_driver_marked_damaged_is_given_back_and_capture_goes_on() {
    let mut queue = CaptureQueueKeepingVideobuf2sRules::withholding_frames_for([0; 6]);
    queue.damaged_frame_sequences = vec![1, 2];
    let is_capturing = AtomicBool::new(true);

    let sequences = capture_frames(&mut queue, &is_capturing, 4);

    assert_eq!(sequences, vec![0, 3, 4, 5]);
    assert_eq!(queue.queues_refused_for_a_buffer_the_driver_held, 0);
}
