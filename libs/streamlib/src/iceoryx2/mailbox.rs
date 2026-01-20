// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using crossbeam ArrayQueue for thread-safe access.

use crossbeam_queue::ArrayQueue;

use super::FramePayload;

/// Per-port mailbox with configurable history depth.
///
/// Uses a crossbeam ArrayQueue internally for lock-free, thread-safe access.
/// Multiple threads can push and pop concurrently (MPMC).
pub struct PortMailbox {
    queue: ArrayQueue<FramePayload>,
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

    /// Push a payload into the mailbox.
    ///
    /// If the mailbox is full, the oldest payload is dropped to make room.
    /// Thread-safe: can be called from any thread.
    pub fn push(&self, payload: FramePayload) {
        // If full, pop oldest to make room
        while self.queue.is_full() {
            let _ = self.queue.pop();
        }
        // Push should succeed now (may fail if another thread filled it, retry)
        while self.queue.push(payload).is_err() {
            let _ = self.queue.pop();
        }
    }

    /// Pop the oldest payload from the mailbox (FIFO).
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop(&self) -> Option<FramePayload> {
        self.queue.pop()
    }

    /// Drain buffer and return only the newest payload.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop_latest(&self) -> Option<FramePayload> {
        let mut latest = None;
        while let Some(value) = self.queue.pop() {
            latest = Some(value);
        }
        latest
    }

    /// Check if the mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Get the number of payloads currently in the mailbox.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Get the configured capacity (history depth).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drain all payloads from the mailbox.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn drain(&self) -> impl Iterator<Item = FramePayload> + '_ {
        std::iter::from_fn(move || self.queue.pop())
    }
}
