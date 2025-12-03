//! LinkInstance - Runtime materialization of a Link.
//!
//! A Link (graph) is a blueprint describing a connection between processor ports.
//! A LinkInstance (runtime) is the actual ring buffer that carries data.

use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::link_input_data_reader::LinkInputDataReader;
use super::link_output_data_writer::LinkOutputDataWriter;
use crate::core::links::graph::LinkId;
use crate::core::links::traits::{LinkBufferReadMode, LinkPortMessage};

/// Default ring buffer capacity for links.
pub const DEFAULT_LINK_CAPACITY: usize = 4;

/// Inner state of a link instance, holding the ring buffer.
pub struct LinkInstanceInner<T: LinkPortMessage> {
    producer: Mutex<Producer<T>>,
    consumer: Mutex<Consumer<T>>,
    cached_size: AtomicUsize,
    link_id: LinkId,
}

impl<T: LinkPortMessage> LinkInstanceInner<T> {
    fn new(link_id: LinkId, capacity: usize) -> Self {
        let (producer, consumer) = RingBuffer::new(capacity);
        Self {
            producer: Mutex::new(producer),
            consumer: Mutex::new(consumer),
            cached_size: AtomicUsize::new(0),
            link_id,
        }
    }

    /// Push a value into the ring buffer.
    pub fn push(&self, value: T) -> bool {
        let mut producer = self.producer.lock();
        match producer.push(value) {
            Ok(()) => {
                self.cached_size.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(rtrb::PushError::Full(_)) => {
                tracing::trace!("LinkInstance {}: buffer full, dropping frame", self.link_id);
                false
            }
        }
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

    #[inline]
    pub fn has_data(&self) -> bool {
        self.cached_size.load(Ordering::Relaxed) > 0
    }

    #[inline]
    pub fn link_id(&self) -> &LinkId {
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
    pub fn new(link_id: LinkId, capacity: usize) -> Self {
        Self {
            inner: Arc::new(LinkInstanceInner::new(link_id, capacity)),
        }
    }

    /// Create with default capacity.
    pub fn with_default_capacity(link_id: LinkId) -> Self {
        Self::new(link_id, DEFAULT_LINK_CAPACITY)
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
    pub fn link_id(&self) -> &LinkId {
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
    fn link_id(&self) -> &LinkId;
    fn has_data(&self) -> bool;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn strong_count(&self) -> usize;
    fn weak_count(&self) -> usize;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: LinkPortMessage> AnyLinkInstance for LinkInstance<T> {
    fn link_id(&self) -> &LinkId {
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
