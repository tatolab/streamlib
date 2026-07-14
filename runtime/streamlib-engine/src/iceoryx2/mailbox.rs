// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-port mailbox using crossbeam ArrayQueue for thread-safe access.

use crossbeam_queue::ArrayQueue;

/// Per-port mailbox with configurable history depth.
///
/// Stores raw wire-format `[u8]` slices (header + data) as `Vec<u8>`.
/// Uses a crossbeam ArrayQueue internally for lock-free, thread-safe access.
/// Multiple threads can push and pop concurrently (MPMC).
pub struct PortMailbox {
    queue: ArrayQueue<Vec<u8>>,
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

    /// Push a raw frame slice into the mailbox.
    ///
    /// If the mailbox is full, the oldest entry is dropped to make room.
    /// Thread-safe: can be called from any thread.
    pub fn push(&self, payload: Vec<u8>) {
        // If full, pop oldest to make room
        while self.queue.is_full() {
            let _ = self.queue.pop();
        }
        // Push should succeed now (may fail if another thread filled it, retry)
        let mut val = payload;
        while let Err(v) = self.queue.push(val) {
            val = v;
            let _ = self.queue.pop();
        }
    }

    /// Pop the oldest entry from the mailbox (FIFO).
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop(&self) -> Option<Vec<u8>> {
        self.queue.pop()
    }

    /// Drain buffer and return only the newest entry.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn pop_latest(&self) -> Option<Vec<u8>> {
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
        std::iter::from_fn(move || self.queue.pop())
    }
}
