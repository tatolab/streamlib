use parking_lot::Mutex;
use std::borrow::Cow;
use std::cell::UnsafeCell;

use super::connection::{OwnedConsumer, OwnedProducer};
use crate::core::runtime::{ProcessorId, WakeupEvent};

/// Strongly-typed port address combining processor ID and port name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortAddress {
    pub processor_id: ProcessorId,
    pub port_name: Cow<'static, str>,
}

impl PortAddress {
    /// Create a new port address
    pub fn new(processor: impl Into<ProcessorId>, port: impl Into<Cow<'static, str>>) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: port.into(),
        }
    }

    /// Create a port address with a static string port name (zero allocation)
    pub fn with_static(processor: impl Into<ProcessorId>, port: &'static str) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: Cow::Borrowed(port),
        }
    }

    /// Get the full address as "processor_id.port_name"
    pub fn full_address(&self) -> String {
        format!("{}.{}", self.processor_id, self.port_name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    Video,
    Audio1,
    Audio2,
    Audio4,
    Audio6,
    Audio8,
    Data,
}

/// Sealed trait pattern - only known frame types can implement PortMessage
pub mod sealed {
    pub trait Sealed {}
}

/// Consumption strategy for reading from ports
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumptionStrategy {
    /// Read the latest item, discarding older ones (optimal for video)
    Latest,
    /// Read items sequentially in order (required for audio)
    Sequential,
}

/// Trait for types that can be sent through ports
/// This is a sealed trait - only types in this crate can implement it
pub trait PortMessage: sealed::Sealed + Clone + Send + 'static {
    fn port_type() -> PortType;
    fn schema() -> std::sync::Arc<crate::core::Schema>;
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }

    /// Determines how this type should be consumed from the ring buffer
    /// - Video frames: Latest (skip old frames to show newest)
    /// - Audio frames: Sequential (must play every frame in order)
    fn consumption_strategy() -> ConsumptionStrategy {
        // Default to Latest for backwards compatibility with video
        ConsumptionStrategy::Latest
    }
}

impl PortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            PortType::Video => 3,
            PortType::Audio1
            | PortType::Audio2
            | PortType::Audio4
            | PortType::Audio6
            | PortType::Audio8 => 32,
            PortType::Data => 16,
        }
    }

    pub fn compatible_with(&self, other: &PortType) -> bool {
        self == other
    }
}

/// Lock-free output port using OwnedProducer
///
/// SAFETY: The producers and downstream_wakeup are wrapped in UnsafeCell to avoid
/// mutex overhead in the hot path (write()). This is safe because:
/// 1. Ports are owned by a single processor
/// 2. Only that processor's thread calls write() during process()
/// 3. Connection setup (add_producer, set_downstream_wakeup) happens during
///    initialization before the processor thread starts, or with external synchronization
pub struct StreamOutput<T: PortMessage> {
    name: String,
    port_type: PortType,
    /// Lock-free producers - each write is atomic
    /// UnsafeCell for zero-cost abstraction in hot path
    producers: UnsafeCell<Vec<OwnedProducer<T>>>,
    /// Wakeup channels for downstream processors (supports fan-out to multiple Push mode processors)
    /// UnsafeCell for zero-cost abstraction in hot path
    downstream_wakeups: UnsafeCell<Vec<crossbeam_channel::Sender<WakeupEvent>>>,
    /// Mutex for connection setup (cold path only)
    setup_lock: Mutex<()>,
}

// SAFETY: StreamOutput can be safely shared between threads because:
// 1. The setup_lock Mutex ensures connection setup is synchronized
// 2. The processor thread has exclusive access during write() (single owner pattern)
// 3. OwnedProducer and Sender<WakeupEvent> are already Send+Sync
unsafe impl<T: PortMessage> Sync for StreamOutput<T> {}

impl<T: PortMessage> StreamOutput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            producers: UnsafeCell::new(Vec::new()),
            downstream_wakeups: UnsafeCell::new(Vec::new()),
            setup_lock: Mutex::new(()),
        }
    }

    /// Write data to all connected outputs (truly lock-free hot path)
    ///
    /// This always succeeds - each producer uses lock-free atomic operations.
    /// If a buffer is full, data is dropped (acceptable for real-time).
    ///
    /// Fan-out behavior: Each destination gets an independent OwnedProducer
    /// with its own RTRB buffer (N buffers for N destinations).
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own output ports during process().
    pub fn write(&self, data: T) {
        unsafe {
            let producers = &mut *self.producers.get();

            // Write to all producers - lock-free atomic operations
            for producer in producers.iter_mut() {
                producer.write(data.clone());
            }

            // Notify all downstream Push mode processors that data is available
            if !producers.is_empty() {
                let wakeups = &*self.downstream_wakeups.get();
                if !wakeups.is_empty() {
                    tracing::trace!(
                        "[StreamOutput] Sending {} wakeup events to downstream processors",
                        wakeups.len()
                    );
                    for wakeup_tx in wakeups.iter() {
                        if let Err(e) = wakeup_tx.send(WakeupEvent::DataAvailable) {
                            tracing::error!("[StreamOutput] Failed to send wakeup: {}", e);
                        }
                    }
                } else {
                    tracing::warn!(
                        "[StreamOutput] Producers exist but no wakeup channels configured!"
                    );
                }
            }
        }
    }

    /// Add a producer during connection setup (cold path)
    ///
    /// SAFETY: Uses setup_lock to ensure this only happens during initialization
    /// before processor threads start, or with external synchronization
    pub fn add_producer(&self, producer: OwnedProducer<T>) {
        let _lock = self.setup_lock.lock();
        unsafe {
            (*self.producers.get()).push(producer);
        }
    }

    /// Compatibility method for wire_output_producer
    pub fn add_connection(&self, producer: OwnedProducer<T>) {
        self.add_producer(producer);
    }

    pub fn producer_count(&self) -> usize {
        let _lock = self.setup_lock.lock();
        unsafe { (*self.producers.get()).len() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }

    /// Set downstream wakeup channel during connection setup (cold path)
    ///
    /// SAFETY: Uses setup_lock to ensure this only happens during initialization
    /// before processor threads start, or with external synchronization
    pub fn set_downstream_wakeup(&self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        let _lock = self.setup_lock.lock();
        tracing::info!("[StreamOutput] Adding downstream wakeup channel");
        unsafe {
            (*self.downstream_wakeups.get()).push(wakeup_tx);
        }
    }
}

impl<T: PortMessage> Clone for StreamOutput<T> {
    fn clone(&self) -> Self {
        let _lock = self.setup_lock.lock();
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            producers: UnsafeCell::new(Vec::new()), // Cannot clone producers (owned)
            downstream_wakeups: unsafe {
                UnsafeCell::new((*self.downstream_wakeups.get()).clone())
            },
            setup_lock: Mutex::new(()),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOutput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .field("producer_count", &self.producer_count())
            .finish()
    }
}

/// Lock-free input port using OwnedConsumer
///
/// SAFETY: The consumer is wrapped in UnsafeCell to avoid mutex overhead in the hot path (read_latest()).
/// This is safe because:
/// 1. Ports are owned by a single processor
/// 2. Only that processor's thread calls read_latest() during process()
/// 3. Connection setup (set_consumer) happens during initialization before processor threads start
pub struct StreamInput<T: PortMessage> {
    name: String,
    port_type: PortType,
    /// Lock-free consumer - each read is atomic
    /// UnsafeCell for zero-cost abstraction in hot path
    consumer: UnsafeCell<Option<OwnedConsumer<T>>>,
    /// Mutex for connection setup (cold path only)
    setup_lock: Mutex<()>,
}

// SAFETY: StreamInput can be safely shared between threads because:
// 1. The setup_lock Mutex ensures connection setup is synchronized
// 2. The processor thread has exclusive access during read_latest() (single owner pattern)
// 3. OwnedConsumer is already Send+Sync
unsafe impl<T: PortMessage> Sync for StreamInput<T> {}

impl<T: PortMessage> StreamInput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            consumer: UnsafeCell::new(None),
            setup_lock: Mutex::new(()),
        }
    }

    /// Set consumer during connection setup (cold path)
    ///
    /// SAFETY: Uses setup_lock to ensure this only happens during initialization
    /// before processor threads start, or with external synchronization
    pub fn set_consumer(&self, consumer: OwnedConsumer<T>) {
        let _lock = self.setup_lock.lock();
        unsafe {
            *self.consumer.get() = Some(consumer);
        }
    }

    /// Compatibility method for wire_input_consumer
    pub fn set_connection(&self, consumer: OwnedConsumer<T>) {
        self.set_consumer(consumer);
    }

    /// Read the latest available data (truly lock-free hot path)
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own input ports during process().
    pub fn read_latest(&self) -> Option<T> {
        unsafe { (*self.consumer.get()).as_mut()?.read_latest() }
    }

    /// Read all available data (truly lock-free hot path)
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own input ports during process().
    pub fn read_all(&self) -> Vec<T> {
        unsafe {
            if let Some(consumer) = (*self.consumer.get()).as_mut() {
                let mut items = Vec::new();
                while let Some(item) = consumer.read_latest() {
                    items.push(item);
                }
                items
            } else {
                Vec::new()
            }
        }
    }

    /// Check if data is available (truly lock-free hot path)
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own input ports during process().
    pub fn has_data(&self) -> bool {
        unsafe {
            (*self.consumer.get())
                .as_ref()
                .map(|c| c.has_data())
                .unwrap_or(false)
        }
    }

    /// Peek at data without consuming (truly lock-free hot path)
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own input ports during process().
    pub fn peek(&self) -> Option<T> {
        unsafe { (*self.consumer.get()).as_mut().and_then(|c| c.peek()) }
    }

    /// Read item using type-appropriate strategy (truly lock-free hot path)
    ///
    /// The consumption strategy is determined by the type:
    /// - Video frames: Latest (skips old frames to show newest)
    /// - Audio frames: Sequential (reads every frame in order)
    ///
    /// SAFETY: This is safe because the processor thread has exclusive access
    /// to its own input ports during process().
    pub fn read(&self) -> Option<T> {
        unsafe {
            (*self.consumer.get())
                .as_mut()
                .and_then(|c| match T::consumption_strategy() {
                    ConsumptionStrategy::Latest => c.read_latest(),
                    ConsumptionStrategy::Sequential => c.read(),
                })
        }
    }

    pub fn is_connected(&self) -> bool {
        let _lock = self.setup_lock.lock();
        unsafe { (*self.consumer.get()).is_some() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }
}

impl<T: PortMessage> Clone for StreamInput<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            consumer: UnsafeCell::new(None), // Cannot clone consumer (owned)
            setup_lock: Mutex::new(()),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamInput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .field("connected", &self.is_connected())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl sealed::Sealed for i32 {}

    impl PortMessage for i32 {
        fn port_type() -> PortType {
            PortType::Data
        }

        fn schema() -> std::sync::Arc<crate::core::Schema> {
            use crate::core::{Field, FieldType, Schema, SemanticVersion, SerializationFormat};
            std::sync::Arc::new(Schema::new(
                "i32",
                SemanticVersion::new(1, 0, 0),
                vec![Field::new("value", FieldType::Int32)],
                SerializationFormat::Bincode,
            ))
        }
    }

    #[test]
    fn test_port_type_defaults() {
        assert_eq!(PortType::Video.default_capacity(), 3);
        assert_eq!(PortType::Audio1.default_capacity(), 4);
        assert_eq!(PortType::Data.default_capacity(), 16);
    }

    #[test]
    fn test_output_creation() {
        let output = StreamOutput::<i32>::new("test");
        assert_eq!(output.name(), "test");
        assert_eq!(output.port_type(), PortType::Data);
    }

    #[test]
    fn test_input_creation() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.name(), "test");
        assert_eq!(input.port_type(), PortType::Data);
        assert!(!input.is_connected());
    }

    #[test]
    fn test_write_and_read() {
        use super::super::connection::create_owned_connection;

        let output = StreamOutput::<i32>::new("output");
        let input = StreamInput::<i32>::new("input");

        let (producer, consumer) = create_owned_connection::<i32>(4);

        output.add_producer(producer);
        input.set_consumer(consumer);

        assert!(input.is_connected());

        output.write(42);
        output.write(100);

        assert_eq!(input.read_latest(), Some(100));
    }

    #[test]
    fn test_fan_out() {
        use super::super::connection::create_owned_connection;

        let output = StreamOutput::<i32>::new("output");
        let input1 = StreamInput::<i32>::new("input1");
        let input2 = StreamInput::<i32>::new("input2");

        let (producer1, consumer1) = create_owned_connection::<i32>(4);
        let (producer2, consumer2) = create_owned_connection::<i32>(4);

        output.add_producer(producer1);
        output.add_producer(producer2);
        input1.set_consumer(consumer1);
        input2.set_consumer(consumer2);

        output.write(42);

        assert_eq!(input1.read_latest(), Some(42));
        assert_eq!(input2.read_latest(), Some(42));
    }

    #[test]
    fn test_read_all() {
        use super::super::connection::create_owned_connection;

        let output = StreamOutput::<i32>::new("output");
        let input = StreamInput::<i32>::new("input");

        let (producer, consumer) = create_owned_connection::<i32>(4);

        output.add_producer(producer);
        input.set_consumer(consumer);

        output.write(1);
        output.write(2);
        output.write(3);

        let data = input.read_all();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], 3);

        let data2 = input.read_all();
        assert_eq!(data2.len(), 0);
    }

    #[test]
    fn test_read_from_unconnected() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.read_latest(), None);
        assert_eq!(input.read_all().len(), 0);
    }

    #[test]
    fn test_port_address_creation() {
        let addr = PortAddress::new("processor_1", "audio_out");
        assert_eq!(addr.processor_id, "processor_1");
        assert_eq!(addr.port_name, "audio_out");
    }

    #[test]
    fn test_port_address_static() {
        let addr = PortAddress::with_static("processor_1", "audio_out");
        assert_eq!(addr.processor_id, "processor_1");
        assert_eq!(addr.port_name, "audio_out");
        // Verify it's borrowed (zero allocation)
        assert!(matches!(addr.port_name, Cow::Borrowed(_)));
    }

    #[test]
    fn test_port_address_full_address() {
        let addr = PortAddress::new("proc_123", "video");
        assert_eq!(addr.full_address(), "proc_123.video");
    }

    #[test]
    fn test_port_address_equality() {
        let addr1 = PortAddress::new("proc", "port");
        let addr2 = PortAddress::with_static("proc", "port");
        assert_eq!(addr1, addr2);
    }

    #[test]
    fn test_port_address_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let addr1 = PortAddress::new("proc", "port");
        let addr2 = PortAddress::with_static("proc", "port");

        map.insert(addr1.clone(), 42);
        assert_eq!(map.get(&addr2), Some(&42));
    }
}
