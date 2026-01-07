// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! LinkInstance - Runtime materialization of a Link.
//!
//! A Link (graph) is a blueprint describing a connection between processor ports.
//! A LinkInstance (runtime) is the actual ring buffer that carries data.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::link_input_data_reader::LinkInputDataReader;
use super::link_output_data_writer::LinkOutputDataWriter;
use crate::core::graph::{LinkCapacity, LinkUniqueId};
use crate::core::links::traits::{LinkBufferReadMode, LinkPortMessage};

/// Inner state of a link instance, holding the ring buffer.
pub struct LinkInstanceInner<T: LinkPortMessage> {
    producer: Mutex<Producer<T>>,
    consumer: Mutex<Consumer<T>>,
    cached_size: AtomicUsize,
    link_id: LinkUniqueId,
    read_mode: LinkBufferReadMode,
    /// Condvar for wait_read() - signaled when data is pushed.
    data_available_condvar: Condvar,
    /// Mutex for the condvar (parking_lot requires separate mutex).
    data_available_mutex: Mutex<()>,
}

impl<T: LinkPortMessage> LinkInstanceInner<T> {
    fn new(capacity: LinkCapacity) -> Self {
        let (producer, consumer) = RingBuffer::new(capacity.into());
        Self {
            producer: Mutex::new(producer),
            consumer: Mutex::new(consumer),
            cached_size: AtomicUsize::new(0),
            link_id: LinkUniqueId::new(),
            read_mode: T::link_read_behavior(),
            data_available_condvar: Condvar::new(),
            data_available_mutex: Mutex::new(()),
        }
    }

    /// Push a value into the ring buffer.
    ///
    /// Writes ALWAYS succeed. When the buffer is full, behavior depends on read mode:
    /// - **SkipToLatest** (video): Circular overwrite - drop oldest unread frame, write new
    /// - **ReadNextInOrder** (audio): Sliding window - drop oldest frame, append new to end
    ///
    /// Returns `true` if no frame was dropped, `false` if an old frame was evicted.
    pub fn push(&self, value: T) -> bool {
        let mut producer = self.producer.lock();
        let result = match producer.push(value) {
            Ok(()) => {
                self.cached_size.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(rtrb::PushError::Full(rejected_value)) => {
                // Buffer is full - make room by dropping oldest frame
                // Both modes handle this the same way: pop oldest, push new
                // The difference is only in how reads work (latest vs sequential)
                let mut consumer = self.consumer.lock();
                if consumer.pop().is_ok() {
                    // Don't decrement cached_size here - we're about to add one back
                    // (net effect is no change to size)
                    drop(consumer); // Release consumer lock before pushing

                    // Now push should succeed
                    match producer.push(rejected_value) {
                        Ok(()) => {
                            // Size stays the same (popped one, pushed one)
                            tracing::trace!(
                                "LinkInstance {}: buffer full, evicted oldest frame ({:?} mode)",
                                self.link_id,
                                self.read_mode
                            );
                        }
                        Err(_) => {
                            // This shouldn't happen - we just made room
                            tracing::error!(
                                "LinkInstance {}: push failed after making room",
                                self.link_id
                            );
                            self.cached_size.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                } else {
                    // Buffer was full but pop failed? Shouldn't happen
                    tracing::error!("LinkInstance {}: buffer full but pop failed", self.link_id);
                }
                false // Indicate a frame was dropped
            }
        };

        // Wake up any threads waiting in wait_read()
        self.data_available_condvar.notify_one();

        result
    }

    /// Read using the frame type's link read behavior.
    pub fn read(&self) -> Option<T> {
        match T::link_read_behavior() {
            LinkBufferReadMode::SkipToLatest => self.read_latest(),
            LinkBufferReadMode::ReadNextInOrder => self.read_sequential(),
        }
    }

    fn read_sequential(&self) -> Option<T> {
        let mut consumer = self.consumer.lock();
        match consumer.pop() {
            Ok(value) => {
                self.cached_size.fetch_sub(1, Ordering::Relaxed);
                Some(value)
            }
            Err(_) => None,
        }
    }

    fn read_latest(&self) -> Option<T> {
        let mut consumer = self.consumer.lock();
        let mut latest = None;
        while let Ok(value) = consumer.pop() {
            self.cached_size.fetch_sub(1, Ordering::Relaxed);
            latest = Some(value);
        }
        latest
    }

    /// Blocking read with timeout.
    ///
    /// First attempts a non-blocking read. If no data is available, waits up to
    /// `timeout` for data to arrive. Returns `None` if timeout expires without data.
    ///
    /// Use at sync points (e.g., display processors) where waiting for the next
    /// frame is preferable to busy-polling or sleeping. Most realtime processors
    /// should use non-blocking `read()` instead.
    pub fn wait_read(&self, timeout: Duration) -> Option<T> {
        // Fast path: data already available
        if let Some(value) = self.read() {
            return Some(value);
        }

        // Slow path: wait for data
        let mut guard = self.data_available_mutex.lock();
        let wait_result = self.data_available_condvar.wait_for(&mut guard, timeout);

        // Try to read regardless of timeout (data may have arrived)
        drop(guard);
        if wait_result.timed_out() {
            // Still try one more read - data could have arrived just before timeout
            self.read()
        } else {
            self.read()
        }
    }

    #[inline]
    pub fn has_data(&self) -> bool {
        self.cached_size.load(Ordering::Relaxed) > 0
    }

    #[inline]
    pub fn link_id(&self) -> &LinkUniqueId {
        &self.link_id
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.cached_size.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Runtime instance of a Link.
///
/// Owns the ring buffer via `Arc<LinkInstanceInner>`. When this is dropped,
/// all handles will gracefully degrade (writes silently drop, reads return None).
pub struct LinkInstance<T: LinkPortMessage> {
    inner: Arc<LinkInstanceInner<T>>,
}

impl<T: LinkPortMessage> LinkInstance<T> {
    /// Create a new LinkInstance with the given capacity.
    pub fn new(capacity: LinkCapacity) -> Self {
        Self {
            inner: Arc::new(LinkInstanceInner::new(capacity)),
        }
    }

    /// Create with default capacity.
    pub fn with_default_capacity() -> Self {
        Self::new(LinkCapacity::default())
    }

    /// Create a data writer for LinkOutput to use.
    pub fn create_link_output_data_writer(&self) -> LinkOutputDataWriter<T> {
        LinkOutputDataWriter::new(Arc::downgrade(&self.inner))
    }

    /// Create a data reader for LinkInput to use.
    pub fn create_link_input_data_reader(&self) -> LinkInputDataReader<T> {
        LinkInputDataReader::new(Arc::downgrade(&self.inner))
    }

    #[inline]
    pub fn link_id(&self) -> &LinkUniqueId {
        self.inner.link_id()
    }

    #[inline]
    pub fn has_data(&self) -> bool {
        self.inner.has_data()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    pub fn weak_count(&self) -> usize {
        Arc::weak_count(&self.inner)
    }
}

impl<T: LinkPortMessage> Clone for LinkInstance<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

// ============================================================================
// Type-erased storage for LinkInstance
// ============================================================================

/// Type-erased LinkInstance for storage in collections.
pub trait AnyLinkInstance: Send + Sync {
    fn link_id(&self) -> &LinkUniqueId;
    fn has_data(&self) -> bool;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn strong_count(&self) -> usize;
    fn weak_count(&self) -> usize;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: LinkPortMessage> AnyLinkInstance for LinkInstance<T> {
    fn link_id(&self) -> &LinkUniqueId {
        self.link_id()
    }

    fn has_data(&self) -> bool {
        self.has_data()
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn strong_count(&self) -> usize {
        self.strong_count()
    }

    fn weak_count(&self) -> usize {
        self.weak_count()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Boxed type-erased LinkInstance.
pub type BoxedLinkInstance = Box<dyn AnyLinkInstance>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::frames::{AudioChannelCount, AudioFrame};

    #[test]
    fn test_push_under_capacity() {
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(4));

        // Push 3 frames into capacity-4 buffer
        for i in 0..3 {
            let frame = AudioFrame::new(vec![0.0; 480], AudioChannelCount::One, i, i as u64, 48000);
            let no_drop = link.inner.push(frame);
            assert!(no_drop, "Should not drop when under capacity");
        }

        assert_eq!(link.len(), 3);
    }

    #[test]
    fn test_push_at_capacity_evicts_oldest_read_next_in_order() {
        // AudioFrame uses ReadNextInOrder (sliding window)
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(3));

        // Fill buffer to capacity with timestamps 0, 1, 2
        for i in 0..3 {
            let frame = AudioFrame::new(
                vec![i as f32; 480],
                AudioChannelCount::One,
                i,
                i as u64,
                48000,
            );
            link.inner.push(frame);
        }
        assert_eq!(link.len(), 3);

        // Push 4th frame with timestamp 99 - should evict oldest (timestamp 0)
        let frame4 = AudioFrame::new(vec![99.0; 480], AudioChannelCount::One, 99, 99, 48000);
        let no_drop = link.inner.push(frame4);
        assert!(!no_drop, "Should indicate a frame was dropped");
        assert_eq!(link.len(), 3, "Size should remain at capacity");

        // Read in order - should get frames with timestamps 1, 2, 99 (timestamp 0 was evicted)
        let read1 = link.inner.read().expect("Should have frame");
        assert_eq!(
            read1.timestamp_ns, 1,
            "First read should be timestamp 1 (oldest remaining)"
        );

        let read2 = link.inner.read().expect("Should have frame");
        assert_eq!(read2.timestamp_ns, 2, "Second read should be timestamp 2");

        let read3 = link.inner.read().expect("Should have frame");
        assert_eq!(
            read3.timestamp_ns, 99,
            "Third read should be timestamp 99 (newest)"
        );

        assert!(link.inner.read().is_none(), "Buffer should be empty");
    }

    #[test]
    fn test_skip_to_latest_drains_all_returns_newest() {
        // Test SkipToLatest behavior using read_latest directly
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(3));

        // Fill buffer with timestamps 10, 20, 30
        for i in 1..=3 {
            let frame = AudioFrame::new(
                vec![i as f32; 480],
                AudioChannelCount::One,
                i * 10, // timestamps: 10, 20, 30
                i as u64,
                48000,
            );
            link.inner.push(frame);
        }
        assert_eq!(link.len(), 3);

        // SkipToLatest should drain ALL frames and return only the newest (timestamp 30)
        let read = link.inner.read_latest().expect("Should have frame");
        assert_eq!(
            read.timestamp_ns, 30,
            "Should get newest frame (timestamp 30)"
        );

        // Buffer should now be empty - all frames were drained
        assert_eq!(link.len(), 0, "Buffer should be empty after skip-to-latest");
        assert!(
            link.inner.read_latest().is_none(),
            "No more frames available"
        );
    }

    #[test]
    fn test_skip_to_latest_temporal_gate_no_going_back_in_time() {
        // Once you read timestamp T, all frames with timestamp < T are inaccessible
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(5));

        // Push frames with timestamps 100, 200, 300
        for ts in [100, 200, 300] {
            let frame =
                AudioFrame::new(vec![0.0; 480], AudioChannelCount::One, ts, ts as u64, 48000);
            link.inner.push(frame);
        }

        // Read latest - gets timestamp 300
        let read1 = link.inner.read_latest().expect("Should have frame");
        assert_eq!(read1.timestamp_ns, 300);

        // Push more frames with timestamps 150, 250, 400
        // Note: 150 and 250 are "older" than what we already read (300)
        for ts in [150, 250, 400] {
            let frame =
                AudioFrame::new(vec![0.0; 480], AudioChannelCount::One, ts, ts as u64, 48000);
            link.inner.push(frame);
        }

        // Read latest again - gets timestamp 400
        // The temporal gate means 150 and 250 are drained even though they're "older" timestamps
        let read2 = link.inner.read_latest().expect("Should have frame");
        assert_eq!(
            read2.timestamp_ns, 400,
            "Should get newest (400), skipping 150 and 250"
        );

        // Buffer is now empty
        assert!(link.inner.read_latest().is_none());
    }

    #[test]
    fn test_continuous_overflow_skip_to_latest() {
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(2));

        // Push 100 frames into capacity-2 buffer - should never block
        // Timestamps will be 0, 1, 2, ..., 99
        for i in 0..100i64 {
            let frame = AudioFrame::new(
                vec![i as f32; 480],
                AudioChannelCount::One,
                i,
                i as u64,
                48000,
            );
            link.inner.push(frame); // Should always succeed
        }

        // Buffer should have last 2 frames (timestamps 98, 99)
        assert_eq!(link.len(), 2);

        // SkipToLatest should return only timestamp 99
        let read = link.inner.read_latest().expect("Should have frame");
        assert_eq!(
            read.timestamp_ns, 99,
            "Should skip to latest (timestamp 99)"
        );

        // Buffer should be empty after skip-to-latest
        assert!(link.inner.read_latest().is_none(), "Buffer should be empty");
    }

    #[test]
    fn test_continuous_overflow_read_next_in_order() {
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(2));

        // Push 100 frames into capacity-2 buffer - should never block
        for i in 0..100i64 {
            let frame = AudioFrame::new(
                vec![i as f32; 480],
                AudioChannelCount::One,
                i,
                i as u64,
                48000,
            );
            link.inner.push(frame); // Should always succeed
        }

        // Buffer should have last 2 frames (timestamps 98, 99)
        assert_eq!(link.len(), 2);

        // ReadNextInOrder returns oldest first
        let read1 = link.inner.read_sequential().expect("Should have frame");
        assert_eq!(read1.timestamp_ns, 98, "First read should be timestamp 98");

        let read2 = link.inner.read_sequential().expect("Should have frame");
        assert_eq!(read2.timestamp_ns, 99, "Second read should be timestamp 99");

        assert!(
            link.inner.read_sequential().is_none(),
            "Buffer should be empty"
        );
    }

    #[test]
    fn test_interleaved_read_write_sequential() {
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(3));

        // Write 2 frames with timestamps 0, 1
        link.inner.push(AudioFrame::new(
            vec![0.0; 480],
            AudioChannelCount::One,
            0,
            0,
            48000,
        ));
        link.inner.push(AudioFrame::new(
            vec![0.0; 480],
            AudioChannelCount::One,
            1,
            1,
            48000,
        ));
        assert_eq!(link.len(), 2);

        // Read 1 (gets timestamp 0)
        let _ = link.inner.read_sequential();
        assert_eq!(link.len(), 1);

        // Write 3 more with timestamps 2, 3, 4 (should overflow once, evicting timestamp 1)
        link.inner.push(AudioFrame::new(
            vec![0.0; 480],
            AudioChannelCount::One,
            2,
            2,
            48000,
        ));
        link.inner.push(AudioFrame::new(
            vec![0.0; 480],
            AudioChannelCount::One,
            3,
            3,
            48000,
        ));
        link.inner.push(AudioFrame::new(
            vec![0.0; 480],
            AudioChannelCount::One,
            4,
            4,
            48000,
        ));

        // Should have frames with timestamps 2, 3, 4 (timestamp 1 was evicted)
        assert_eq!(link.len(), 3);

        let read1 = link.inner.read_sequential().expect("frame");
        assert_eq!(read1.timestamp_ns, 2, "Should get timestamp 2 first");
    }

    #[test]
    fn test_empty_buffer_returns_none() {
        let link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(4));

        // Empty buffer - both read modes should return None
        assert!(link.inner.read_sequential().is_none());
        assert!(link.inner.read_latest().is_none());
        assert!(link.inner.read().is_none());
    }

    #[test]
    fn test_read_mode_matches_frame_type() {
        // AudioFrame should use ReadNextInOrder
        let audio_link = LinkInstance::<AudioFrame>::new(LinkCapacity::from(4));
        assert_eq!(
            audio_link.inner.read_mode,
            LinkBufferReadMode::ReadNextInOrder
        );
    }
}
