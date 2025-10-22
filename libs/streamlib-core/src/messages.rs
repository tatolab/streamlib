//! Message types for stream data
//!
//! These types define the **data contracts** between processors.
//! All GPU data uses **WebGPU (wgpu)** as the intermediate representation.
//!
//! Platform-specific crates (streamlib-apple, streamlib-linux) convert
//! their native GPU types (Metal, Vulkan) to/from WebGPU internally.
//!
//! This provides:
//! - Zero-copy GPU operations (via wgpu-hal bridges)
//! - Platform-agnostic shader effects
//! - Simple, concrete types (no trait objects)

use crate::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;

/// Video frame message
///
/// Represents a single frame of video data as a WebGPU texture.
/// Platform-specific processors convert their native GPU types internally.
///
/// # Architecture
///
/// - Camera processors (streamlib-apple): Metal → WebGPU
/// - Display processors (streamlib-apple): WebGPU → Metal (if needed)
/// - Effect processors: Work directly with WebGPU shaders
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
/// // Access WebGPU texture directly
/// let texture: &wgpu::Texture = &frame.texture;
/// ```
#[derive(Clone)]
pub struct VideoFrame {
    /// WebGPU texture containing the frame data
    ///
    /// This is the universal GPU representation. Platform processors
    /// convert to/from their native types (Metal, Vulkan) internally.
    pub texture: Arc<wgpu::Texture>,

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
        texture: Arc<wgpu::Texture>,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            texture,
            timestamp,
            frame_number,
            width,
            height,
            metadata: None,
        }
    }

    /// Create a video frame with metadata
    pub fn with_metadata(
        texture: Arc<wgpu::Texture>,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        Self {
            texture,
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
/// Represents a chunk of audio data as a WebGPU buffer.
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
    /// WebGPU buffer containing audio samples
    pub buffer: Arc<wgpu::Buffer>,

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
        buffer: Arc<wgpu::Buffer>,
        timestamp: f64,
        sample_count: usize,
        sample_rate: u32,
        channels: u32,
    ) -> Self {
        Self {
            buffer,
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
/// Uses WebGPU buffer for GPU-resident data.
///
/// # Example
///
/// ```ignore
/// use streamlib_core::messages::DataMessage;
///
/// // ML detection results in GPU buffer
/// let detections = DataMessage::new(
///     detection_buffer,
///     timestamp,
///     Some(hashmap!{ "model".into() => "yolov8".into() })
/// );
/// ```
#[derive(Clone)]
pub struct DataMessage {
    /// WebGPU buffer containing custom data
    pub buffer: Arc<wgpu::Buffer>,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Optional metadata
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl DataMessage {
    /// Create a new data message
    pub fn new(
        buffer: Arc<wgpu::Buffer>,
        timestamp: f64,
        metadata: Option<HashMap<String, MetadataValue>>,
    ) -> Self {
        Self {
            buffer,
            timestamp,
            metadata,
        }
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

    #[test]
    fn test_metadata_value_conversions() {
        let _str_val: MetadataValue = "test".into();
        let _int_val: MetadataValue = 42i64.into();
        let _float_val: MetadataValue = 2.71f64.into();
        let _bool_val: MetadataValue = true.into();
    }

    // Note: VideoFrame and AudioBuffer tests require actual wgpu::Device
    // to create textures/buffers. Integration tests in platform crates
    // (streamlib-apple, etc.) test the full pipeline.
}
