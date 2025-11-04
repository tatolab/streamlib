//! Bus system for connecting processor ports with fan-out capability
//!
//! A bus is a one-to-many communication channel where one output can feed multiple inputs.
//! Think of it like a patch cable splitter in a modular synthesizer.
//!
//! # Architecture
//!
//! - **Output creates bus**: When an output port is first used, it creates a bus
//! - **Inputs get readers**: Each input port connecting to the bus gets its own reader
//! - **Independent reads**: Each reader tracks its own position (fan-out pattern)
//! - **Type-specific implementations**:
//!   - Audio: Uses dasp's SignalBus (lazy signal combinators)
//!   - Video: Custom ring buffer (GPU textures, drop old frames)
//!   - Data: Custom message queue (metadata, control messages)
//!
//! # Example
//!
//! ```ignore
//! // Runtime creates buses when connecting processors
//! runtime.connect(
//!     mixer.output_port::<StereoSignal>("audio"),
//!     reverb.input_port::<StereoSignal>("audio"),
//! )?;
//! runtime.connect(
//!     mixer.output_port::<StereoSignal>("audio"),  // Same output!
//!     speaker.input_port::<StereoSignal>("audio"), // Different input
//! )?;
//! // One bus, two readers - fan-out achieved!
//! ```

use crate::core::{VideoFrame, AudioFrame, DataFrame};
use std::any::Any;
use std::fmt;
use std::sync::Arc;

pub mod audio;
pub mod video;
pub mod data;

pub use audio::{AudioBus, AudioBusReader};
pub use video::{VideoBus, VideoBusReader};
pub use data::{DataBus, DataBusReader};

/// Unique identifier for a bus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BusId(u64);

impl BusId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for BusId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bus({})", self.0)
    }
}

/// Trait for types that can flow through buses
///
/// This is automatically implemented for VideoFrame, AudioFrame, DataFrame,
/// and AudioSignal types.
pub trait BusMessage: Send + 'static {
    /// Clone the message (required for fan-out)
    fn clone_message(&self) -> Box<dyn BusMessage>;

    /// Downcast to concrete type
    fn as_any(&self) -> &dyn Any;
}

// Implement BusMessage for our core types
impl BusMessage for VideoFrame {
    fn clone_message(&self) -> Box<dyn BusMessage> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl BusMessage for AudioFrame {
    fn clone_message(&self) -> Box<dyn BusMessage> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl BusMessage for DataFrame {
    fn clone_message(&self) -> Box<dyn BusMessage> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// AudioSignal types implement BusMessage
impl<const CHANNELS: usize> BusMessage for crate::core::frames::AudioSignal<CHANNELS> {
    fn clone_message(&self) -> Box<dyn BusMessage> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A typed bus that can create readers for multiple consumers
///
/// This is the main trait for bus implementations. Each message type
/// (AudioSignal, VideoFrame, DataFrame) has its own Bus implementation
/// with type-specific behavior.
pub trait Bus<T: BusMessage>: Send + Sync {
    /// Get unique identifier for this bus
    fn id(&self) -> BusId;

    /// Create a new reader for this bus
    ///
    /// Each reader maintains independent position, enabling fan-out.
    /// Multiple readers can exist for the same bus without interfering.
    fn create_reader(&self) -> Box<dyn BusReader<T>>;

    /// Write a message to the bus
    ///
    /// This makes the message available to all readers.
    /// Behavior depends on implementation:
    /// - Audio: Advances signal position
    /// - Video: Pushes frame to ring buffer (drops oldest if full)
    /// - Data: Enqueues message
    fn write(&self, message: T);
}

/// A reader for consuming messages from a bus
///
/// Each reader tracks its own position independently, allowing
/// multiple consumers to read from the same bus at different rates.
pub trait BusReader<T: BusMessage>: Send {
    /// Read the latest message available
    ///
    /// Returns None if no new data since last read.
    /// Each reader tracks position independently.
    fn read_latest(&mut self) -> Option<T>;

    /// Check if data is available without consuming
    fn has_data(&self) -> bool;

    /// Clone this reader (for port cloning)
    fn clone_reader(&self) -> Box<dyn BusReader<T>>;
}

/// Type-erased bus for runtime storage
///
/// The runtime stores all buses in a HashMap<BusId, Arc<dyn AnyBus>>.
/// This trait provides the type erasure layer needed for heterogeneous storage.
pub trait AnyBus: Send + Sync {
    /// Get the bus ID
    fn id(&self) -> BusId;

    /// Downcast to concrete bus type
    fn as_any(&self) -> &dyn Any;

    /// Clone as Arc<dyn AnyBus>
    fn clone_arc(&self) -> Arc<dyn AnyBus>;
}

/// Wrapper that makes any Bus into an AnyBus
pub struct TypedBus<T: BusMessage> {
    id: BusId,
    inner: Arc<dyn Bus<T>>,
}

impl<T: BusMessage> TypedBus<T> {
    pub fn new(inner: Arc<dyn Bus<T>>) -> Self {
        let id = inner.id();
        Self { id, inner }
    }

    pub fn inner(&self) -> &Arc<dyn Bus<T>> {
        &self.inner
    }
}

impl<T: BusMessage> AnyBus for TypedBus<T> {
    fn id(&self) -> BusId {
        self.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn clone_arc(&self) -> Arc<dyn AnyBus> {
        Arc::new(Self {
            id: self.id,
            inner: Arc::clone(&self.inner),
        })
    }
}
