//! Platform-agnostic message types for stream data
//!
//! These types define the **data contracts** between processors.
//! They are platform-agnostic, with trait-based interfaces that
//! platform-specific crates (streamlib-metal, streamlib-vulkan) implement.
//!
//! This allows processors to work with concrete types (VideoFrame, AudioBuffer)
//! without coupling to specific GPU APIs.

use std::any::Any;
use std::collections::HashMap;
use crate::ports::{PortType, PortMessage};

/// Video frame message
///
/// Represents a single frame of video data on the GPU.
/// The actual GPU texture implementation is platform-specific.
///
/// # Platform-Specific Implementation
///
/// Platform crates provide concrete implementations:
/// - `streamlib-metal`: Uses `metal::Texture`
/// - `streamlib-vulkan`: Uses `vk::Image`
/// - `streamlib-wgpu`: Uses `wgpu::Texture`
///
/// # Example
///
/// ```ignore
/// use streamlib_core::messages::VideoFrame;
///
/// // In a processor
/// let frame = video_input.read_latest().unwrap();
/// println!("Frame {}x{} @ {:.3}s",
///     frame.width, frame.height, frame.timestamp);
///
/// // Access platform-specific texture
/// if let Some(metal_texture) = frame.gpu_data.downcast_ref::<metal::Texture>() {
///     // Use Metal texture directly
/// }
/// ```
#[derive(Clone)]
pub struct VideoFrame {
    /// Platform-specific GPU texture (metal::Texture, vk::Image, etc.)
    ///
    /// Use `downcast_ref()` to access the concrete type:
    /// ```ignore
    /// if let Some(metal_texture) = frame.gpu_data.downcast_ref::<metal::Texture>() {
    ///     // Use Metal-specific APIs
    /// }
    /// ```
    pub gpu_data: Box<dyn GpuData>,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Sequential frame number
    pub frame_number: u64,

    /// Frame width in pixels
    pub width: u32,

    /// Frame height in pixels
    pub height: u32,

    /// Optional metadata (detection boxes, ML results, etc.)
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl VideoFrame {
    /// Create a new video frame
    pub fn new(
        gpu_data: Box<dyn GpuData>,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            gpu_data,
            timestamp,
            frame_number,
            width,
            height,
            metadata: None,
        }
    }

    /// Create a video frame with metadata
    pub fn with_metadata(
        gpu_data: Box<dyn GpuData>,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        Self {
            gpu_data,
            timestamp,
            frame_number,
            width,
            height,
            metadata: Some(metadata),
        }
    }
}

/// Audio buffer message
///
/// Represents a chunk of audio data on the GPU.
/// The actual GPU buffer implementation is platform-specific.
///
/// # Example
///
/// ```ignore
/// use streamlib_core::messages::AudioBuffer;
///
/// // In a processor
/// let buffers = audio_input.read_all();
/// for buffer in buffers {
///     println!("Audio: {} samples @ {} Hz",
///         buffer.sample_count, buffer.sample_rate);
/// }
/// ```
#[derive(Clone)]
pub struct AudioBuffer {
    /// Platform-specific GPU buffer (metal::Buffer, vk::Buffer, etc.)
    pub gpu_data: Box<dyn GpuData>,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Number of audio samples in this buffer
    pub sample_count: usize,

    /// Sample rate in Hz (e.g., 48000)
    pub sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u32,

    /// Optional metadata
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl AudioBuffer {
    /// Create a new audio buffer
    pub fn new(
        gpu_data: Box<dyn GpuData>,
        timestamp: f64,
        sample_count: usize,
        sample_rate: u32,
        channels: u32,
    ) -> Self {
        Self {
            gpu_data,
            timestamp,
            sample_count,
            sample_rate,
            channels,
            metadata: None,
        }
    }
}

/// Generic data message
///
/// For custom data types that don't fit VideoFrame or AudioBuffer.
///
/// # Example
///
/// ```ignore
/// use streamlib_core::messages::DataMessage;
///
/// // ML detection results
/// let detections = DataMessage::new(
///     Box::new(detection_data),
///     timestamp,
///     Some(hashmap!{ "model".into() => "yolov8".into() })
/// );
/// ```
#[derive(Clone)]
pub struct DataMessage {
    /// Platform-specific GPU data or CPU data
    pub data: Box<dyn GpuData>,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Optional metadata
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl DataMessage {
    /// Create a new data message
    pub fn new(
        data: Box<dyn GpuData>,
        timestamp: f64,
        metadata: Option<HashMap<String, MetadataValue>>,
    ) -> Self {
        Self {
            data,
            timestamp,
            metadata,
        }
    }
}

/// Trait for platform-specific GPU data
///
/// Platform crates implement this for their GPU types:
/// - Metal: implement for `metal::Texture`, `metal::Buffer`
/// - Vulkan: implement for `vk::Image`, `vk::Buffer`
/// - wgpu: implement for `wgpu::Texture`, `wgpu::Buffer`
///
/// This allows zero-copy access to platform-specific GPU resources
/// while maintaining type safety.
///
/// # Safety
///
/// Implementors must ensure:
/// - `as_any()` returns a reference to the concrete GPU type
/// - The GPU resource is valid for the lifetime of the object
/// - Thread-safety guarantees match the underlying GPU API
pub trait GpuData: Send + Sync {
    /// Downcast to concrete GPU type (metal::Texture, vk::Image, etc.)
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(metal_texture) = gpu_data.as_any().downcast_ref::<metal::Texture>() {
    ///     // Use Metal-specific APIs
    /// }
    /// ```
    fn as_any(&self) -> &dyn Any;

    /// Clone the GPU data (may be reference-counted, not a deep copy)
    fn clone_box(&self) -> Box<dyn GpuData>;
}

// Implement Clone for Box<dyn GpuData>
impl Clone for Box<dyn GpuData> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Metadata value types
///
/// Supports common metadata value types for flexibility.
#[derive(Debug, Clone)]
pub enum MetadataValue {
    /// String value
    String(String),
    /// Integer value
    Int(i64),
    /// Float value
    Float(f64),
    /// Boolean value
    Bool(bool),
    /// Nested metadata
    Map(HashMap<String, MetadataValue>),
    /// Array of values
    Array(Vec<MetadataValue>),
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<i64> for MetadataValue {
    fn from(i: i64) -> Self {
        MetadataValue::Int(i)
    }
}

impl From<f64> for MetadataValue {
    fn from(f: f64) -> Self {
        MetadataValue::Float(f)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Bool(b)
    }
}

// Implement PortMessage trait for message types
impl PortMessage for VideoFrame {
    fn port_type() -> PortType {
        PortType::Video
    }
}

impl PortMessage for AudioBuffer {
    fn port_type() -> PortType {
        PortType::Audio
    }
}

impl PortMessage for DataMessage {
    fn port_type() -> PortType {
        PortType::Data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock GPU data for testing
    #[derive(Clone)]
    struct MockTexture {
        id: u32,
    }

    impl GpuData for MockTexture {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn clone_box(&self) -> Box<dyn GpuData> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn test_video_frame_creation() {
        let texture = Box::new(MockTexture { id: 42 });
        let frame = VideoFrame::new(texture, 1.0, 30, 1920, 1080);

        assert_eq!(frame.timestamp, 1.0);
        assert_eq!(frame.frame_number, 30);
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert!(frame.metadata.is_none());
    }

    #[test]
    fn test_video_frame_with_metadata() {
        let texture = Box::new(MockTexture { id: 42 });
        let mut metadata = HashMap::new();
        metadata.insert("detections".to_string(), MetadataValue::Int(5));

        let frame = VideoFrame::with_metadata(texture, 1.0, 30, 1920, 1080, metadata);

        assert!(frame.metadata.is_some());
        if let Some(ref meta) = frame.metadata {
            if let Some(MetadataValue::Int(count)) = meta.get("detections") {
                assert_eq!(*count, 5);
            } else {
                panic!("Expected Int metadata");
            }
        }
    }

    #[test]
    fn test_downcast_gpu_data() {
        let texture = MockTexture { id: 42 };
        let gpu_data: Box<dyn GpuData> = Box::new(texture);

        // Downcast to concrete type
        if let Some(mock_texture) = gpu_data.as_any().downcast_ref::<MockTexture>() {
            assert_eq!(mock_texture.id, 42);
        } else {
            panic!("Failed to downcast");
        }
    }

    #[test]
    fn test_audio_buffer_creation() {
        let buffer = Box::new(MockTexture { id: 100 });
        let audio = AudioBuffer::new(buffer, 0.5, 1024, 48000, 2);

        assert_eq!(audio.timestamp, 0.5);
        assert_eq!(audio.sample_count, 1024);
        assert_eq!(audio.sample_rate, 48000);
        assert_eq!(audio.channels, 2);
    }

    #[test]
    fn test_metadata_value_conversions() {
        let _str_val: MetadataValue = "test".into();
        let _int_val: MetadataValue = 42i64.into();
        let _float_val: MetadataValue = 3.14f64.into();
        let _bool_val: MetadataValue = true.into();
    }

    #[test]
    fn test_clone_video_frame() {
        let texture = Box::new(MockTexture { id: 42 });
        let frame1 = VideoFrame::new(texture, 1.0, 30, 1920, 1080);
        let frame2 = frame1.clone();

        assert_eq!(frame1.timestamp, frame2.timestamp);
        assert_eq!(frame1.frame_number, frame2.frame_number);
        assert_eq!(frame1.width, frame2.width);
        assert_eq!(frame1.height, frame2.height);
    }
}
