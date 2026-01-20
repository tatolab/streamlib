// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using rtrb ring buffer for real-time history.

use rtrb::{Consumer, Producer, RingBuffer};

use super::FramePayload;

/// Per-port mailbox with configurable history depth.
///
/// Uses an rtrb ring buffer internally for lock-free, real-time safe access.
pub struct PortMailbox {
    producer: Producer<FramePayload>,
    consumer: Consumer<FramePayload>,
    history: usize,
}

impl PortMailbox {
    /// Create a new mailbox with the given history depth.
    pub fn new(history: usize) -> Self {
        let capacity = history.max(1);
        let (producer, consumer) = RingBuffer::new(capacity);
        Self {
            producer,
            consumer,
            history: capacity,
        }
    }

    /// Push a payload into the mailbox.
    ///
    /// If the mailbox is full, the oldest payload is dropped to make room.
    pub fn push(&mut self, payload: FramePayload) {
        // If full, pop oldest to make room
        while self.producer.is_full() {
            let _ = self.consumer.pop();
        }
        // Push should succeed now
        let _ = self.producer.push(payload);
    }

    /// Get the most recent payload without removing it.
    ///
    /// Returns None if the mailbox is empty.
    pub fn peek(&self) -> Option<&FramePayload> {
        self.consumer.peek().ok()
    }

    /// Pop the oldest payload from the mailbox (FIFO).
    pub fn pop(&mut self) -> Option<FramePayload> {
        self.consumer.pop().ok()
    }

    /// Drain buffer and return only the newest payload.
    pub fn pop_latest(&mut self) -> Option<FramePayload> {
        let mut latest = None;
        while let Ok(value) = self.consumer.pop() {
            latest = Some(value);
        }
        latest
    }

    /// Check if the mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    /// Get the number of payloads currently in the mailbox.
    pub fn len(&self) -> usize {
        self.consumer.slots()
    }

    /// Get the configured history depth.
    pub fn history(&self) -> usize {
        self.history
    }

    /// Drain all payloads from the mailbox.
    pub fn drain(&mut self) -> impl Iterator<Item = FramePayload> + '_ {
        std::iter::from_fn(move || self.consumer.pop().ok())
    }
}
