//! Ring buffers for zero-copy data exchange between handlers
//!
//! This module provides fixed-size circular buffers with latest-read semantics,
//! matching professional broadcast practice (SMPTE ST 2110).
//!
//! Key properties:
//! - Fixed memory allocation (3 slots by default)
//! - Latest-read semantics (skip old data)
//! - Overwrite oldest slot when full
//! - Thread-safe (uses Mutex)
//! - No queueing, no backpressure
//!
//! This matches professional broadcast practice where old frames are
//! worthless and should be discarded in favor of new ones.

use parking_lot::Mutex;

/// Fixed-size circular buffer for data exchange
///
/// Generic ring buffer that can hold any type `T`. Thread-safe via Mutex.
///
/// # Example
///
/// ```
/// use streamlib_core::RingBuffer;
///
/// let buffer = RingBuffer::new(3);
///
/// // Write some data
/// buffer.write("frame1");
/// buffer.write("frame2");
///
/// // Read latest
/// assert_eq!(buffer.read_latest(), Some("frame2"));
/// ```
pub struct RingBuffer<T> {
    slots: usize,
    buffer: Mutex<BufferState<T>>,
}

struct BufferState<T> {
    data: Vec<Option<T>>,
    write_idx: usize,
    read_idx: usize,
    has_data: bool,
}

impl<T> RingBuffer<T> {
    /// Create a new ring buffer with specified number of slots
    ///
    /// # Arguments
    ///
    /// * `slots` - Number of buffer slots (default: 3, matches broadcast practice)
    ///
    /// # Panics
    ///
    /// Panics if slots < 1
    pub fn new(slots: usize) -> Self {
        assert!(slots >= 1, "Ring buffer must have at least 1 slot, got {}", slots);

        Self {
            slots,
            buffer: Mutex::new(BufferState {
                data: (0..slots).map(|_| None).collect(),
                write_idx: 0,
                read_idx: 0,
                has_data: false,
            }),
        }
    }

    /// Write data to ring buffer, overwriting oldest slot
    ///
    /// # Arguments
    ///
    /// * `data` - Data to write
    pub fn write(&self, data: T) {
        let mut state = self.buffer.lock();
        let idx = state.write_idx;
        state.data[idx] = Some(data);
        state.write_idx = (idx + 1) % self.slots;
        state.has_data = true;
    }

    /// Read most recent data from ring buffer
    ///
    /// Returns the most recent data written, or None if no data has been written yet.
    /// This is "latest-read semantics" - old data is skipped.
    ///
    /// # Returns
    ///
    /// Most recent data, or None if buffer is empty
    pub fn read_latest(&self) -> Option<T>
    where
        T: Clone,
    {
        let mut state = self.buffer.lock();
        if !state.has_data {
            return None;
        }

        let idx = (state.write_idx + self.slots - 1) % self.slots;
        let data = state.data[idx].clone();

        // Update read_idx to mark as read
        state.read_idx = state.write_idx;

        data
    }

    /// Read all unread data from ring buffer
    ///
    /// Returns all items that have been written since the last read.
    /// Useful for audio processing where all chunks must be processed.
    ///
    /// # Returns
    ///
    /// Vector of all unread data (may be empty)
    pub fn read_all(&self) -> Vec<T>
    where
        T: Clone,
    {
        let mut state = self.buffer.lock();
        if !state.has_data {
            return vec![];
        }

        let mut result = Vec::new();

        // Calculate how many items to read
        let count = if state.write_idx > state.read_idx {
            // Simple case: read from read_idx to write_idx
            state.write_idx - state.read_idx
        } else if state.write_idx < state.read_idx {
            // Wrapped around: read from read_idx to end, then 0 to write_idx
            (self.slots - state.read_idx) + state.write_idx
        } else {
            // write_idx == read_idx
            // This could mean either:
            // 1. No new data (we've already read everything)
            // 2. Buffer is full (we've written exactly `slots` items)
            //
            // Check if we have unread data at read_idx
            if state.data[state.read_idx].is_some() {
                // Buffer is full, read all slots
                self.slots
            } else {
                // No new data
                0
            }
        };

        if count == 0 {
            return vec![];
        }

        // Collect items and clear slots as we read
        for i in 0..count {
            let idx = (state.read_idx + i) % self.slots;
            if let Some(data) = state.data[idx].take() {
                result.push(data);
            }
        }

        // Update read_idx to current write_idx
        state.read_idx = state.write_idx;

        result
    }

    /// Check if any data has been written
    ///
    /// # Returns
    ///
    /// true if no data written yet, false otherwise
    pub fn is_empty(&self) -> bool {
        let state = self.buffer.lock();
        !state.has_data
    }

    /// Clear buffer (reset to empty state)
    pub fn clear(&self) {
        let mut state = self.buffer.lock();
        state.data = (0..self.slots).map(|_| None).collect();
        state.write_idx = 0;
        state.read_idx = 0;
        state.has_data = false;
    }

    /// Get the number of slots in this buffer
    pub fn slots(&self) -> usize {
        self.slots
    }
}

impl<T> Default for RingBuffer<T> {
    fn default() -> Self {
        Self::new(3)
    }
}

// Implement Send + Sync for RingBuffer when T is Send
// (Mutex handles the thread safety)
unsafe impl<T: Send> Send for RingBuffer<T> {}
unsafe impl<T: Send> Sync for RingBuffer<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer() {
        let buffer: RingBuffer<i32> = RingBuffer::new(3);
        assert_eq!(buffer.slots(), 3);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_default_buffer() {
        let buffer: RingBuffer<i32> = RingBuffer::default();
        assert_eq!(buffer.slots(), 3);
        assert!(buffer.is_empty());
    }

    #[test]
    #[should_panic(expected = "Ring buffer must have at least 1 slot")]
    fn test_zero_slots_panics() {
        let _buffer: RingBuffer<i32> = RingBuffer::new(0);
    }

    #[test]
    fn test_write_and_read_latest() {
        let buffer = RingBuffer::new(3);

        buffer.write(1);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.read_latest(), Some(1));

        buffer.write(2);
        buffer.write(3);
        assert_eq!(buffer.read_latest(), Some(3));
    }

    #[test]
    fn test_read_latest_empty() {
        let buffer: RingBuffer<i32> = RingBuffer::new(3);
        assert_eq!(buffer.read_latest(), None);
    }

    #[test]
    fn test_overwrite_oldest() {
        let buffer = RingBuffer::new(3);

        // Fill buffer
        buffer.write(1);
        buffer.write(2);
        buffer.write(3);

        // Overwrite oldest (1)
        buffer.write(4);

        // Should get latest
        assert_eq!(buffer.read_latest(), Some(4));
    }

    #[test]
    fn test_read_all_empty() {
        let buffer: RingBuffer<i32> = RingBuffer::new(3);
        assert_eq!(buffer.read_all(), Vec::<i32>::new());
    }

    #[test]
    fn test_read_all_single() {
        let buffer = RingBuffer::new(3);
        buffer.write(1);

        let data = buffer.read_all();
        assert_eq!(data, vec![1]);

        // Second read should be empty (already read)
        let data2 = buffer.read_all();
        assert_eq!(data2, Vec::<i32>::new());
    }

    #[test]
    fn test_read_all_multiple() {
        let buffer = RingBuffer::new(3);

        buffer.write(1);
        buffer.write(2);
        buffer.write(3);

        let data = buffer.read_all();
        assert_eq!(data, vec![1, 2, 3]);

        // Second read should be empty
        let data2 = buffer.read_all();
        assert_eq!(data2, Vec::<i32>::new());
    }

    #[test]
    fn test_read_all_wrapped() {
        let buffer = RingBuffer::new(3);

        // Fill buffer
        buffer.write(1);
        buffer.write(2);
        buffer.write(3);

        // Read all (marks as read)
        let _ = buffer.read_all();

        // Write more (will wrap around)
        buffer.write(4);
        buffer.write(5);

        let data = buffer.read_all();
        assert_eq!(data, vec![4, 5]);
    }

    #[test]
    fn test_read_all_incremental() {
        let buffer = RingBuffer::new(5);

        buffer.write(1);
        buffer.write(2);

        // Read first batch
        let data1 = buffer.read_all();
        assert_eq!(data1, vec![1, 2]);

        buffer.write(3);
        buffer.write(4);

        // Read second batch (should only get new data)
        let data2 = buffer.read_all();
        assert_eq!(data2, vec![3, 4]);
    }

    #[test]
    fn test_clear() {
        let buffer = RingBuffer::new(3);

        buffer.write(1);
        buffer.write(2);
        assert!(!buffer.is_empty());

        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.read_latest(), None);
    }

    #[test]
    fn test_latest_read_semantics() {
        let buffer = RingBuffer::new(3);

        // Write several values
        buffer.write(1);
        buffer.write(2);
        buffer.write(3);

        // read_latest should skip old data and get latest
        assert_eq!(buffer.read_latest(), Some(3));

        // Write more
        buffer.write(4);
        assert_eq!(buffer.read_latest(), Some(4));
    }

    #[test]
    fn test_with_strings() {
        let buffer = RingBuffer::new(3);

        buffer.write("frame1".to_string());
        buffer.write("frame2".to_string());

        assert_eq!(buffer.read_latest(), Some("frame2".to_string()));
    }

    #[test]
    fn test_thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let buffer = Arc::new(RingBuffer::new(10));
        let buffer_clone = Arc::clone(&buffer);

        // Writer thread
        let writer = thread::spawn(move || {
            for i in 0..100 {
                buffer_clone.write(i);
            }
        });

        // Reader thread
        let reader = thread::spawn(move || {
            for _ in 0..100 {
                let _ = buffer.read_latest();
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}
