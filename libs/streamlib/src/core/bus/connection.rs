//! Phase 2: Lock-Free Connection Architecture
//!
//! This module provides true lock-free connections using rtrb's lock-free ring buffer.
//! NO Arc<Mutex> - just atomic operations for maximum performance.

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Lock-free producer for writing data to a connection
///
/// Wraps rtrb::Producer directly (no Arc<Mutex>) for true lock-free writes
/// using only atomic operations.
pub struct OwnedProducer<T: Clone + Send + 'static> {
    inner: Producer<T>,
    cached_size: Arc<AtomicUsize>,
}

impl<T: Clone + Send + 'static> OwnedProducer<T> {
    pub fn new(producer: Producer<T>, cached_size: Arc<AtomicUsize>) -> Self {
        Self {
            inner: producer,
            cached_size,
        }
    }

    /// Write data to the connection with drop-on-full semantics (lock-free)
    ///
    /// If the buffer is full, the data is dropped (acceptable for real-time streaming).
    /// This is necessary because we can't access the consumer from the producer side
    /// in the lock-free architecture.
    pub fn write(&mut self, data: T) {
        match self.inner.push(data) {
            Ok(()) => {
                self.cached_size.fetch_add(1, Ordering::Relaxed);
            }
            Err(rtrb::PushError::Full(_dropped)) => {
                // In lock-free mode, we can't pop from consumer side
                // Data is dropped on overflow (acceptable for real-time)
                tracing::warn!("OwnedProducer: Buffer full, dropping data");
            }
        }
    }

    /// Try to write data, returning the data back if buffer is full (lock-free)
    pub fn try_write(&mut self, data: T) -> Result<(), T> {
        match self.inner.push(data) {
            Ok(()) => {
                self.cached_size.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(rtrb::PushError::Full(data)) => Err(data),
        }
    }

    /// Check if buffer has space (lock-free, atomic read)
    pub fn has_space(&self) -> bool {
        self.inner.slots() > 0
    }
}

/// Lock-free consumer for reading data from a connection
///
/// Wraps rtrb::Consumer directly (no Arc<Mutex>) for true lock-free reads
/// using only atomic operations.
pub struct OwnedConsumer<T: Clone + Send + 'static> {
    inner: Consumer<T>,
    cached_size: Arc<AtomicUsize>,
}

impl<T: Clone + Send + 'static> OwnedConsumer<T> {
    pub fn new(consumer: Consumer<T>, cached_size: Arc<AtomicUsize>) -> Self {
        Self {
            inner: consumer,
            cached_size,
        }
    }

    /// Read and return the most recent item, discarding all older items (lock-free)
    ///
    /// Optimal for video frames where you want the latest frame and don't care about
    /// missed intermediate frames.
    pub fn read_latest(&mut self) -> Option<T> {
        let mut latest = None;
        while let Ok(item) = self.inner.pop() {
            self.cached_size.fetch_sub(1, Ordering::Relaxed);
            latest = Some(item);
        }
        latest
    }

    /// Read the next item sequentially (lock-free)
    ///
    /// Returns None if buffer is empty. Use this for audio or data where you need
    /// every item in order.
    pub fn read(&mut self) -> Option<T> {
        match self.inner.pop() {
            Ok(item) => {
                self.cached_size.fetch_sub(1, Ordering::Relaxed);
                Some(item)
            }
            Err(_) => None,
        }
    }

    /// Check if data is available (lock-free, atomic read)
    pub fn has_data(&self) -> bool {
        self.cached_size.load(Ordering::Relaxed) > 0
    }

    /// Peek at the next item without consuming it (lock-free)
    pub fn peek(&self) -> Option<T> {
        self.inner.peek().ok().cloned()
    }
}

/// Create a pair of owned producer/consumer for lock-free communication
///
/// This is the primary way to create connections in Phase 2.
///
/// # Example
///
/// ```
/// use streamlib::core::bus::create_owned_connection;
///
/// let (mut producer, mut consumer) = create_owned_connection::<i32>(32);
///
/// // Lock-free write
/// producer.write(42);
///
/// // Lock-free read
/// assert_eq!(consumer.read(), Some(42));
/// ```
pub fn create_owned_connection<T: Clone + Send + 'static>(
    capacity: usize,
) -> (OwnedProducer<T>, OwnedConsumer<T>) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let cached_size = Arc::new(AtomicUsize::new(0));

    (
        OwnedProducer::new(producer, cached_size.clone()),
        OwnedConsumer::new(consumer, cached_size),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_owned_write_read() {
        let (mut producer, mut consumer) = create_owned_connection::<i32>(10);

        producer.write(42);
        assert_eq!(consumer.read(), Some(42));
        assert_eq!(consumer.read(), None);
    }

    #[test]
    fn test_owned_read_latest() {
        let (mut producer, mut consumer) = create_owned_connection::<i32>(10);

        producer.write(1);
        producer.write(2);
        producer.write(3);

        // read_latest should skip 1 and 2, return only 3
        assert_eq!(consumer.read_latest(), Some(3));
        assert_eq!(consumer.read_latest(), None);
    }

    #[test]
    fn test_owned_has_data() {
        let (mut producer, consumer) = create_owned_connection::<i32>(10);

        assert!(!consumer.has_data());

        producer.write(42);
        assert!(consumer.has_data());
    }

    #[test]
    fn test_owned_try_write() {
        let (mut producer, mut consumer) = create_owned_connection::<i32>(2);

        assert!(producer.try_write(1).is_ok());
        assert!(producer.try_write(2).is_ok());

        // Buffer full
        assert!(producer.try_write(3).is_err());

        // Make space
        consumer.read();

        // Should work now
        assert!(producer.try_write(3).is_ok());
    }

    #[test]
    fn test_owned_peek() {
        let (mut producer, consumer) = create_owned_connection::<i32>(10);

        producer.write(42);

        // Peek doesn't consume
        assert_eq!(consumer.peek(), Some(42));
        assert_eq!(consumer.peek(), Some(42));

        // Still there for read
        let mut consumer = consumer;
        assert_eq!(consumer.read(), Some(42));
    }
}
