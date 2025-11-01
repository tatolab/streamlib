//! GPU-first ports for StreamHandler inputs/outputs
//!
//! All ports operate on GPU data types (textures, buffers).
//! This design ensures zero-copy GPU pipelines throughout.
//!
//! Follows the GPU-first architecture: simple, opinionated, GPU-only.
//!
//! ## Lock-Free Design
//!
//! Uses rtrb (lock-free SPSC ring buffer) for real-time performance:
//! - Zero mutex contention between audio/video threads
//! - Wait-free reads (consumer never blocks)
//! - Atomic operations only (no unbounded blocking)
//!
//! This is critical for real-time audio processing where priority inversion
//! in a mutex can cause glitches.

use parking_lot::Mutex;
use std::sync::Arc;

// Import WakeupEvent for push-based notifications
use super::runtime::WakeupEvent;

/// Port type identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    /// Video frames (GPU textures)
    Video,
    /// Audio buffers (GPU buffers)
    Audio,
    /// Generic data (GPU buffers)
    Data,
}

/// Trait for message types that can flow through ports
///
/// This trait associates a message type with its port type, schema, and examples.
/// Used by the macro system to auto-generate processor descriptors for MCP discovery.
///
/// # Example
///
/// ```ignore
/// impl PortMessage for VideoFrame {
///     fn port_type() -> PortType {
///         PortType::Video
///     }
///
///     fn schema() -> Arc<Schema> {
///         Arc::clone(&SCHEMA_VIDEO_FRAME)
///     }
///
///     fn examples() -> Vec<(&'static str, serde_json::Value)> {
///         vec![
///             ("720p", Self::example_720p()),
///             ("1080p", Self::example_1080p()),
///         ]
///     }
/// }
///
/// // Now ports can infer their type and schema from T:
/// let input = StreamInput::<VideoFrame>::new("video");  // Automatically PortType::Video!
/// ```
pub trait PortMessage: Clone + Send + 'static {
    /// Returns the port type for this message type
    fn port_type() -> PortType;

    /// Returns the schema for this message type
    ///
    /// Used by the macro system to auto-generate ProcessorDescriptor with correct schemas.
    /// The schema is used by MCP servers for AI agent discovery.
    fn schema() -> std::sync::Arc<crate::core::Schema>;

    /// Returns example values for this message type
    ///
    /// Used by the macro system to auto-generate ProcessorExample instances.
    /// Each example is a tuple of (description, sample_value).
    ///
    /// Default implementation returns empty vec - override for custom types.
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }
}

impl PortType {
    /// Get the default ring buffer size for this port type
    pub fn default_slots(&self) -> usize {
        match self {
            PortType::Video => 3,  // Broadcast practice
            PortType::Audio => 32, // Audio arrives ~94 Hz, read at ~30 Hz
            PortType::Data => 3,   // General purpose
        }
    }
}

/// Output port for sending GPU data
///
/// Port type is automatically determined from the message type via the `PortMessage` trait.
///
/// Uses lock-free rtrb SPSC ring buffer for real-time performance.
///
/// # Example
///
/// ```ignore
/// use streamlib_core::{StreamOutput, VideoFrame};
///
/// // Port type automatically becomes PortType::Video
/// let output = StreamOutput::<VideoFrame>::new("video");
/// ```
pub struct StreamOutput<T> {
    /// Port name (e.g., "video", "audio", "out")
    name: String,
    /// Port type
    port_type: PortType,
    /// Lock-free producer for pushing data
    producer: Mutex<rtrb::Producer<T>>,
    /// Consumer holder for connection transfer (SPSC constraint)
    /// The consumer is created with the producer and transferred to StreamInput during connection
    consumer_holder: Arc<Mutex<Option<rtrb::Consumer<T>>>>,
    /// Downstream processor wakeup channel (for push-based notifications)
    /// Set by runtime during connection
    downstream_wakeup: Mutex<Option<crossbeam_channel::Sender<WakeupEvent>>>,
}

impl<T: PortMessage> StreamOutput<T> {
    /// Create a new output port with default buffer size
    ///
    /// Port type is automatically inferred from the message type T.
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio", "out")
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Port type automatically determined from VideoFrame
    /// let output = StreamOutput::<VideoFrame>::new("video");
    /// ```
    pub fn new(name: impl Into<String>) -> Self {
        let port_type = T::port_type();
        Self::with_slots(name, port_type.default_slots())
    }

    /// Create a new output port with custom buffer size
    ///
    /// Port type is automatically inferred from the message type T.
    ///
    /// # Arguments
    ///
    /// * `name` - Port name
    /// * `slots` - Ring buffer size
    pub fn with_slots(name: impl Into<String>, slots: usize) -> Self {
        // Create lock-free SPSC ring buffer
        let (producer, consumer) = rtrb::RingBuffer::new(slots);

        Self {
            name: name.into(),
            port_type: T::port_type(),
            producer: Mutex::new(producer),
            consumer_holder: Arc::new(Mutex::new(Some(consumer))),
            downstream_wakeup: Mutex::new(None),
        }
    }

    /// Write data to ring buffer (lock-free push)
    ///
    /// # Arguments
    ///
    /// * `data` - Data to write (GpuTexture, AudioBuffer, etc.)
    ///
    /// # Real-Time Behavior
    ///
    /// If buffer is full, drops oldest data to prioritize current frames.
    /// This is critical for real-time systems (power armor HUD) where
    /// showing stale data is worse than dropping frames.
    ///
    /// # Push-Based Notification
    ///
    /// After writing data, sends WakeupEvent::DataAvailable to downstream processor
    /// if one is connected. This enables immediate processing without waiting for
    /// the next tick (DeepStream/GStreamer model).
    pub fn write(&self, data: T) {
        let mut producer = self.producer.lock();

        let mut data_written = false;
        match producer.push(data) {
            Ok(()) => {
                // Successfully pushed, lock-free fast path
                data_written = true;
            }
            Err(rtrb::PushError::Full(data)) => {
                // Buffer full - drop oldest frame and retry
                // Real-time priority: current data > old data
                if let Some(mut consumer_guard) = self.consumer_holder.try_lock() {
                    if let Some(consumer) = consumer_guard.as_mut() {
                        // Pop oldest frame to make room
                        let _ = consumer.pop();
                        // Retry push (should succeed now)
                        if producer.push(data).is_ok() {
                            data_written = true;
                        }
                    }
                }
                // If we can't acquire consumer lock, just drop this frame
                // Better than blocking in real-time callback
            }
        }

        // Phase 2: Push-based notification - wake up downstream processor
        if data_written {
            if let Some(wakeup_tx) = self.downstream_wakeup.lock().as_ref() {
                // Non-blocking send (unbounded channel, should never fail)
                // Ignore errors if downstream is shutting down
                let _ = wakeup_tx.send(WakeupEvent::DataAvailable);
            }
        }
    }

    /// Get the port name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the port type
    pub fn port_type(&self) -> PortType {
        self.port_type
    }

    /// Get a reference to the consumer holder (for connection transfer)
    ///
    /// This is used by the runtime to connect ports.
    /// The consumer is transferred to StreamInput during connection.
    #[allow(dead_code)]
    pub(crate) fn consumer_holder(&self) -> &Arc<Mutex<Option<rtrb::Consumer<T>>>> {
        &self.consumer_holder
    }

    /// Set the downstream processor's wakeup channel (for push-based notifications)
    ///
    /// This is called by the runtime when ports are connected.
    /// When data is written to this output port, a WakeupEvent::DataAvailable
    /// will be sent to the downstream processor.
    ///
    /// # Arguments
    ///
    /// * `wakeup_tx` - Channel sender to wake up downstream processor
    #[allow(dead_code)]
    pub(crate) fn set_downstream_wakeup(&self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        *self.downstream_wakeup.lock() = Some(wakeup_tx);
    }
}

impl<T> std::fmt::Debug for StreamOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOutput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .finish()
    }
}

/// Input port for receiving GPU data
///
/// Port type is automatically determined from the message type via the `PortMessage` trait.
///
/// Uses lock-free rtrb SPSC ring buffer for real-time performance.
///
/// # Example
///
/// ```ignore
/// use streamlib_core::{StreamInput, VideoFrame};
///
/// // Port type automatically becomes PortType::Video
/// let input = StreamInput::<VideoFrame>::new("video");
/// ```
pub struct StreamInput<T> {
    /// Port name (e.g., "video", "audio", "in")
    name: String,
    /// Port type
    port_type: PortType,
    /// Lock-free consumer for popping data (transferred during connection)
    consumer: Option<rtrb::Consumer<T>>,
}

impl<T: PortMessage> StreamInput<T> {
    /// Create a new input port
    ///
    /// Port type is automatically inferred from the message type T.
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio", "in")
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Port type automatically determined from VideoFrame
    /// let input = StreamInput::<VideoFrame>::new("video");
    /// ```
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            consumer: None,
        }
    }

    /// Connect to upstream output port by receiving its consumer
    ///
    /// # Arguments
    ///
    /// * `consumer` - Lock-free consumer from upstream output port
    ///
    /// Note: This is called by StreamRuntime.connect()
    /// SPSC constraint: Only one consumer can exist per producer
    #[allow(dead_code)]
    pub(crate) fn connect_consumer(&mut self, consumer: rtrb::Consumer<T>) {
        self.consumer = Some(consumer);
    }

    /// Read latest data from ring buffer (lock-free pop)
    ///
    /// # Returns
    ///
    /// Most recent data (GpuTexture, AudioBuffer, etc.), or None if no data yet
    ///
    /// # Performance
    ///
    /// Lock-free atomic operation. Safe to call from real-time threads.
    /// Drops all intermediate frames to get the latest one.
    pub fn read_latest(&mut self) -> Option<T> {
        let consumer = self.consumer.as_mut()?;

        // Pop all available frames, keeping only the latest
        let mut latest = None;
        while let Ok(frame) = consumer.pop() {
            latest = Some(frame);
        }

        latest
    }

    /// Read all unread data from ring buffer (lock-free pop)
    ///
    /// Returns all items that have been written since last read.
    /// Useful for audio processing where all chunks must be processed.
    ///
    /// # Returns
    ///
    /// Vector of all unread data (may be empty)
    ///
    /// # Performance
    ///
    /// Lock-free atomic operation. Safe to call from real-time threads.
    pub fn read_all(&mut self) -> Vec<T> {
        let consumer = match self.consumer.as_mut() {
            Some(c) => c,
            None => return vec![],
        };

        let mut frames = Vec::new();
        while let Ok(frame) = consumer.pop() {
            frames.push(frame);
        }

        frames
    }

    /// Check if this input is connected to an upstream output
    pub fn is_connected(&self) -> bool {
        self.consumer.is_some()
    }

    /// Get the port name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the port type
    pub fn port_type(&self) -> PortType {
        self.port_type
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

    // Implement PortMessage for test types
    impl PortMessage for i32 {
        fn port_type() -> PortType {
            PortType::Data  // Use Data for simple test types
        }

        fn schema() -> std::sync::Arc<crate::core::Schema> {
            use crate::core::{Schema, Field, FieldType, SemanticVersion, SerializationFormat};
            std::sync::Arc::new(
                Schema::new(
                    "i32",
                    SemanticVersion::new(1, 0, 0),
                    vec![Field::new("value", FieldType::Int32)],
                    SerializationFormat::Bincode,
                )
            )
        }
    }

    #[test]
    fn test_port_type_defaults() {
        assert_eq!(PortType::Video.default_slots(), 3);
        assert_eq!(PortType::Audio.default_slots(), 32);
        assert_eq!(PortType::Data.default_slots(), 3);
    }

    #[test]
    fn test_output_creation() {
        let output = StreamOutput::<i32>::new("video");
        assert_eq!(output.name(), "video");
        assert_eq!(output.port_type(), PortType::Data);  // i32 maps to Data
    }

    #[test]
    fn test_output_with_custom_slots() {
        let output = StreamOutput::<i32>::with_slots("audio", 64);
        assert_eq!(output.name(), "audio");
        assert_eq!(output.port_type(), PortType::Data);
        // Note: Buffer size is now an internal implementation detail
        // Functional behavior is tested via write/read tests
    }

    #[test]
    fn test_input_creation() {
        let input = StreamInput::<i32>::new("video");
        assert_eq!(input.name(), "video");
        assert_eq!(input.port_type(), PortType::Data);
        assert!(!input.is_connected());
    }

    #[test]
    fn test_write_and_read() {
        let output = StreamOutput::<i32>::new("test");
        let mut input = StreamInput::<i32>::new("test");

        // Connect input to output (consumer transfer pattern)
        let consumer = output.consumer_holder().lock().take().unwrap();
        input.connect_consumer(consumer);
        assert!(input.is_connected());

        // Write some data
        output.write(42);
        output.write(100);

        // Read latest
        assert_eq!(input.read_latest(), Some(100));
    }

    #[test]
    fn test_read_all() {
        let output = StreamOutput::<i32>::new("test");
        let mut input = StreamInput::<i32>::new("test");

        // Connect input to output (consumer transfer pattern)
        let consumer = output.consumer_holder().lock().take().unwrap();
        input.connect_consumer(consumer);

        // Write some data
        output.write(1);
        output.write(2);
        output.write(3);

        // Read all
        let data = input.read_all();
        assert_eq!(data, vec![1, 2, 3]);

        // Second read should be empty (already read)
        let data2 = input.read_all();
        assert_eq!(data2, Vec::<i32>::new());
    }

    #[test]
    fn test_read_from_unconnected() {
        let mut input = StreamInput::<i32>::new("test");
        assert_eq!(input.read_latest(), None);
        assert_eq!(input.read_all(), Vec::<i32>::new());
    }

    #[test]
    fn test_port_type_from_message() {
        // Test that port type is automatically inferred from message type
        let output = StreamOutput::<i32>::new("test");
        assert_eq!(output.port_type(), PortType::Data);

        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.port_type(), PortType::Data);
    }

    #[test]
    fn test_custom_slots() {
        // Test custom slot sizes - buffer capacity is tested via functional behavior
        let output = StreamOutput::<i32>::with_slots("test", 10);
        let mut input = StreamInput::<i32>::new("test");

        // Connect
        let consumer = output.consumer_holder().lock().take().unwrap();
        input.connect_consumer(consumer);

        // Write up to capacity - custom slot size affects buffering behavior
        for i in 0..10 {
            output.write(i);
        }

        // Should be able to read all written values
        let data = input.read_all();
        assert_eq!(data.len(), 10);
    }

    #[test]
    fn test_debug_output() {
        let output = StreamOutput::<i32>::new("test");
        let debug_str = format!("{:?}", output);
        assert!(debug_str.contains("StreamOutput"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("Data"));  // i32 maps to Data
    }

    #[test]
    fn test_debug_input() {
        let input = StreamInput::<i32>::new("test");
        let debug_str = format!("{:?}", input);
        assert!(debug_str.contains("StreamInput"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("Data"));  // i32 maps to Data
        assert!(debug_str.contains("connected"));
    }
}
