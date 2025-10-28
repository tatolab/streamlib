//! GPU-first ports for StreamHandler inputs/outputs
//!
//! All ports operate on GPU data types (textures, buffers).
//! This design ensures zero-copy GPU pipelines throughout.
//!
//! Follows the GPU-first architecture: simple, opinionated, GPU-only.

use super::buffers::RingBuffer;
use std::sync::Arc;

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
/// This trait associates a message type with its port type,
/// allowing automatic port type inference from the generic type parameter.
///
/// # Example
///
/// ```ignore
/// impl PortMessage for VideoFrame {
///     fn port_type() -> PortType {
///         PortType::Video
///     }
/// }
///
/// // Now ports can infer their type from T:
/// let input = StreamInput::<VideoFrame>::new("video");  // Automatically PortType::Video!
/// ```
pub trait PortMessage: Clone + Send + 'static {
    /// Returns the port type for this message type
    fn port_type() -> PortType;
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
    /// Ring buffer for zero-copy data exchange
    buffer: Arc<RingBuffer<T>>,
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
        Self {
            name: name.into(),
            port_type: T::port_type(),
            buffer: Arc::new(RingBuffer::new(slots)),
        }
    }

    /// Write data to ring buffer (zero-copy reference)
    ///
    /// # Arguments
    ///
    /// * `data` - Data to write (GpuTexture, AudioBuffer, etc.)
    pub fn write(&self, data: T) {
        self.buffer.write(data);
    }

    /// Get the port name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the port type
    pub fn port_type(&self) -> PortType {
        self.port_type
    }

    /// Get a reference to the underlying ring buffer
    ///
    /// This is used by the runtime to connect ports.
    pub fn buffer(&self) -> &Arc<RingBuffer<T>> {
        &self.buffer
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
    /// Connected upstream ring buffer (None until connected)
    buffer: Option<Arc<RingBuffer<T>>>,
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
            buffer: None,
        }
    }

    /// Connect to upstream ring buffer
    ///
    /// # Arguments
    ///
    /// * `buffer` - Ring buffer from upstream output port
    ///
    /// Note: This is called by StreamRuntime.connect()
    pub fn connect(&mut self, buffer: Arc<RingBuffer<T>>) {
        self.buffer = Some(buffer);
    }

    /// Read latest data from ring buffer (zero-copy reference)
    ///
    /// # Returns
    ///
    /// Most recent data (GpuTexture, AudioBuffer, etc.), or None if no data yet
    ///
    /// Note: Returns reference to data in ring buffer, not a copy.
    pub fn read_latest(&self) -> Option<T>
    where
        T: Clone,
    {
        self.buffer.as_ref()?.read_latest()
    }

    /// Read all unread data from ring buffer (zero-copy references)
    ///
    /// Returns all items that have been written since last read.
    /// Useful for audio processing where all chunks must be processed.
    ///
    /// # Returns
    ///
    /// Vector of all unread data (may be empty)
    ///
    /// Note: Returns references to data in ring buffer, not copies.
    pub fn read_all(&self) -> Vec<T>
    where
        T: Clone,
    {
        match &self.buffer {
            Some(buffer) => buffer.read_all(),
            None => vec![],
        }
    }

    /// Check if this input is connected to an upstream output
    pub fn is_connected(&self) -> bool {
        self.buffer.is_some()
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
        assert_eq!(output.buffer().slots(), 64);
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

        // Connect input to output
        input.connect(output.buffer().clone());
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

        input.connect(output.buffer().clone());

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
        let input = StreamInput::<i32>::new("test");
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
        // Test custom slot sizes
        let output = StreamOutput::<i32>::with_slots("test", 10);
        assert_eq!(output.buffer().slots(), 10);
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
