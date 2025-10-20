//! GPU-first ports for StreamHandler inputs/outputs
//!
//! All ports operate on GPU data types (textures, buffers).
//! This design ensures zero-copy GPU pipelines throughout.
//!
//! Follows the GPU-first architecture: simple, opinionated, GPU-only.

use crate::buffers::RingBuffer;
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
/// Ports are GPU-only - all data must be GPU textures/buffers.
/// Runtime provides shared GPU context for resource management.
///
/// # Example
///
/// ```
/// use streamlib_core::ports::{StreamOutput, PortType};
/// use streamlib_core::texture::GpuTexture;
///
/// let output: StreamOutput<GpuTexture> = StreamOutput::new("video", PortType::Video);
/// ```
pub struct StreamOutput<T> {
    /// Port name (e.g., "video", "audio", "out")
    name: String,
    /// Port type
    port_type: PortType,
    /// Ring buffer for zero-copy data exchange
    buffer: Arc<RingBuffer<T>>,
}

impl<T> StreamOutput<T> {
    /// Create a new output port with default buffer size
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio", "out")
    /// * `port_type` - Port type (Video, Audio, Data)
    pub fn new(name: impl Into<String>, port_type: PortType) -> Self {
        Self::with_slots(name, port_type, port_type.default_slots())
    }

    /// Create a new output port with custom buffer size
    ///
    /// # Arguments
    ///
    /// * `name` - Port name
    /// * `port_type` - Port type
    /// * `slots` - Ring buffer size
    pub fn with_slots(name: impl Into<String>, port_type: PortType, slots: usize) -> Self {
        Self {
            name: name.into(),
            port_type,
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
/// Ports are GPU-only - expects all data as GPU textures/buffers.
/// Runtime provides shared GPU context for resource management.
///
/// # Example
///
/// ```
/// use streamlib_core::ports::{StreamInput, PortType};
/// use streamlib_core::texture::GpuTexture;
///
/// let input: StreamInput<GpuTexture> = StreamInput::new("video", PortType::Video);
/// ```
pub struct StreamInput<T> {
    /// Port name (e.g., "video", "audio", "in")
    name: String,
    /// Port type
    port_type: PortType,
    /// Connected upstream ring buffer (None until connected)
    buffer: Option<Arc<RingBuffer<T>>>,
}

impl<T> StreamInput<T> {
    /// Create a new input port
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio", "in")
    /// * `port_type` - Port type (Video, Audio, Data)
    pub fn new(name: impl Into<String>, port_type: PortType) -> Self {
        Self {
            name: name.into(),
            port_type,
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

impl<T> std::fmt::Debug for StreamInput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .field("connected", &self.is_connected())
            .finish()
    }
}

// Convenience constructors for common port types

/// Create a video output port (GPU textures only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "video", "out")
/// * `slots` - Ring buffer size (default: 3)
///
/// # Example
///
/// ```
/// use streamlib_core::ports::video_output;
/// use streamlib_core::texture::GpuTexture;
///
/// let output = video_output::<GpuTexture>("video", None);
/// ```
pub fn video_output<T>(name: impl Into<String>, slots: Option<usize>) -> StreamOutput<T> {
    match slots {
        Some(s) => StreamOutput::with_slots(name, PortType::Video, s),
        None => StreamOutput::new(name, PortType::Video),
    }
}

/// Create a video input port (GPU textures only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "video", "in")
///
/// # Example
///
/// ```
/// use streamlib_core::ports::video_input;
/// use streamlib_core::texture::GpuTexture;
///
/// let input = video_input::<GpuTexture>("video");
/// ```
pub fn video_input<T>(name: impl Into<String>) -> StreamInput<T> {
    StreamInput::new(name, PortType::Video)
}

/// Create an audio output port (GPU buffers only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "audio", "out")
/// * `slots` - Ring buffer size (default: 32 for audio buffering)
///
/// # Example
///
/// ```
/// use streamlib_core::ports::audio_output;
///
/// // AudioBuffer type would be defined in platform-specific crate
/// let output = audio_output::<u8>("audio", None);
/// ```
pub fn audio_output<T>(name: impl Into<String>, slots: Option<usize>) -> StreamOutput<T> {
    match slots {
        Some(s) => StreamOutput::with_slots(name, PortType::Audio, s),
        None => StreamOutput::new(name, PortType::Audio),
    }
}

/// Create an audio input port (GPU buffers only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "audio", "in")
///
/// # Example
///
/// ```
/// use streamlib_core::ports::audio_input;
///
/// let input = audio_input::<u8>("audio");
/// ```
pub fn audio_input<T>(name: impl Into<String>) -> StreamInput<T> {
    StreamInput::new(name, PortType::Audio)
}

/// Create a generic data output port (GPU buffers only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "data", "out")
/// * `slots` - Ring buffer size (default: 3)
///
/// # Example
///
/// ```
/// use streamlib_core::ports::data_output;
///
/// let output = data_output::<Vec<u8>>("data", None);
/// ```
pub fn data_output<T>(name: impl Into<String>, slots: Option<usize>) -> StreamOutput<T> {
    match slots {
        Some(s) => StreamOutput::with_slots(name, PortType::Data, s),
        None => StreamOutput::new(name, PortType::Data),
    }
}

/// Create a generic data input port (GPU buffers only)
///
/// # Arguments
///
/// * `name` - Port name (e.g., "data", "in")
///
/// # Example
///
/// ```
/// use streamlib_core::ports::data_input;
///
/// let input = data_input::<Vec<u8>>("data");
/// ```
pub fn data_input<T>(name: impl Into<String>) -> StreamInput<T> {
    StreamInput::new(name, PortType::Data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_type_defaults() {
        assert_eq!(PortType::Video.default_slots(), 3);
        assert_eq!(PortType::Audio.default_slots(), 32);
        assert_eq!(PortType::Data.default_slots(), 3);
    }

    #[test]
    fn test_output_creation() {
        let output: StreamOutput<i32> = StreamOutput::new("video", PortType::Video);
        assert_eq!(output.name(), "video");
        assert_eq!(output.port_type(), PortType::Video);
    }

    #[test]
    fn test_output_with_custom_slots() {
        let output: StreamOutput<i32> = StreamOutput::with_slots("audio", PortType::Audio, 64);
        assert_eq!(output.name(), "audio");
        assert_eq!(output.port_type(), PortType::Audio);
        assert_eq!(output.buffer().slots(), 64);
    }

    #[test]
    fn test_input_creation() {
        let input: StreamInput<i32> = StreamInput::new("video", PortType::Video);
        assert_eq!(input.name(), "video");
        assert_eq!(input.port_type(), PortType::Video);
        assert!(!input.is_connected());
    }

    #[test]
    fn test_write_and_read() {
        let output: StreamOutput<i32> = StreamOutput::new("test", PortType::Data);
        let mut input: StreamInput<i32> = StreamInput::new("test", PortType::Data);

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
        let output: StreamOutput<i32> = StreamOutput::new("test", PortType::Data);
        let mut input: StreamInput<i32> = StreamInput::new("test", PortType::Data);

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
        let input: StreamInput<i32> = StreamInput::new("test", PortType::Data);
        assert_eq!(input.read_latest(), None);
        assert_eq!(input.read_all(), Vec::<i32>::new());
    }

    #[test]
    fn test_video_helpers() {
        let output = video_output::<i32>("video", None);
        assert_eq!(output.port_type(), PortType::Video);
        assert_eq!(output.buffer().slots(), 3);

        let output_custom = video_output::<i32>("video", Some(10));
        assert_eq!(output_custom.buffer().slots(), 10);

        let input = video_input::<i32>("video");
        assert_eq!(input.port_type(), PortType::Video);
    }

    #[test]
    fn test_audio_helpers() {
        let output = audio_output::<i32>("audio", None);
        assert_eq!(output.port_type(), PortType::Audio);
        assert_eq!(output.buffer().slots(), 32);

        let input = audio_input::<i32>("audio");
        assert_eq!(input.port_type(), PortType::Audio);
    }

    #[test]
    fn test_data_helpers() {
        let output = data_output::<i32>("data", None);
        assert_eq!(output.port_type(), PortType::Data);
        assert_eq!(output.buffer().slots(), 3);

        let input = data_input::<i32>("data");
        assert_eq!(input.port_type(), PortType::Data);
    }

    #[test]
    fn test_debug_output() {
        let output: StreamOutput<i32> = StreamOutput::new("test", PortType::Video);
        let debug_str = format!("{:?}", output);
        assert!(debug_str.contains("StreamOutput"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("Video"));
    }

    #[test]
    fn test_debug_input() {
        let input: StreamInput<i32> = StreamInput::new("test", PortType::Audio);
        let debug_str = format!("{:?}", input);
        assert!(debug_str.contains("StreamInput"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("Audio"));
        assert!(debug_str.contains("connected"));
    }
}
