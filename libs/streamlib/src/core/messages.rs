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

use super::ports::{PortMessage, PortType};
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

    /// Texture format (Rgba8Unorm or Bgra8Unorm)
    ///
    /// Cached for performance. Should always match `texture.format()`.
    /// - **Internal pipeline standard**: Rgba8Unorm (used by most processors)
    /// - **Platform edges**: Bgra8Unorm (camera input, display output on some platforms)
    pub format: wgpu::TextureFormat,

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
        format: wgpu::TextureFormat,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            texture,
            format,
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
        format: wgpu::TextureFormat,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp,
            frame_number,
            width,
            height,
            metadata: Some(metadata),
        }
    }

    /// Create example 720p video frame metadata for MCP/macro use
    ///
    /// Returns a JSON representation suitable for ProcessorExample.
    /// Note: This is metadata only (no actual texture), used by MCP for documentation.
    pub fn example_720p() -> serde_json::Value {
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example 1080p video frame metadata for MCP/macro use
    pub fn example_1080p() -> serde_json::Value {
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example 4K video frame metadata for MCP/macro use
    pub fn example_4k() -> serde_json::Value {
        serde_json::json!({
            "width": 3840,
            "height": 2160,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }
}

/// Audio sample format
///
/// Tracks the original audio format for conversion purposes.
/// All samples are converted to f32 internally for processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// 32-bit floating point (-1.0 to 1.0)
    F32,
    /// 16-bit signed integer
    I16,
    /// 24-bit signed integer
    I24,
    /// 32-bit signed integer
    I32,
}

/// Audio frame message
///
/// Represents a chunk of audio data with CPU-first architecture.
/// Unlike VideoFrame (GPU-first), audio is primarily CPU-based for:
/// - Mixing (combining multiple sources)
/// - Effects processing (reverb, EQ, compression)
/// - Encoding (AAC, Opus for network transport)
///
/// GPU buffer is optional for specialized GPU audio processing.
///
/// # Architecture
///
/// - **CPU storage**: `Arc<Vec<f32>>` - Main storage, interleaved samples
/// - **GPU storage**: `Option<Arc<wgpu::Buffer>>` - Optional for GPU effects
/// - **Sample format**: Always f32 internally, tracks original format
/// - **Channel layout**: Interleaved (L,R,L,R,... for stereo)
///
/// # Example
///
/// ```ignore
/// use streamlib_core::messages::AudioFrame;
///
/// // Create stereo audio frame at 48kHz
/// let samples = vec![0.0, 0.0, 0.1, -0.1, 0.2, -0.2]; // 3 frames, 2 channels
/// let frame = AudioFrame::new(samples, 0, 0, 48000, 2);
///
/// assert_eq!(frame.sample_count, 3);
/// assert_eq!(frame.duration(), 0.0000625); // 3 / 48000 seconds
/// ```
#[derive(Clone)]
pub struct AudioFrame {
    /// CPU buffer containing interleaved audio samples
    ///
    /// Format: f32 samples in range [-1.0, 1.0]
    /// Layout: Interleaved [L,R,L,R,...] for stereo
    /// Size: sample_count * channels (total f32 values)
    pub samples: Arc<Vec<f32>>,

    /// Optional GPU buffer for specialized processing
    ///
    /// Most audio processors use CPU only. GPU buffer is created
    /// on-demand for GPU-accelerated effects (FFT, convolution, etc.)
    pub gpu_buffer: Option<Arc<wgpu::Buffer>>,

    /// Timestamp in nanoseconds since stream start
    ///
    /// Uses i64 for precise A/V synchronization (sub-microsecond accuracy).
    /// VideoFrame uses f64 seconds for backward compatibility.
    pub timestamp_ns: i64,

    /// Sequential frame number
    ///
    /// Matches VideoFrame numbering for correlation.
    /// Useful for detecting dropped audio or sync issues.
    pub frame_number: u64,

    /// Number of audio samples per channel
    ///
    /// Total f32 values in samples = sample_count * channels
    pub sample_count: usize,

    /// Sample rate in Hz (e.g., 48000)
    ///
    /// Common rates: 48000 (pro), 44100 (CD), 16000 (voice)
    pub sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u32,

    /// Original sample format before conversion to f32
    ///
    /// Tracks source format for debugging and format negotiation.
    /// All samples are stored as f32 internally.
    pub format: AudioFormat,

    /// Optional metadata (ML results, speaker labels, etc.)
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl AudioFrame {
    /// Create a new audio frame
    ///
    /// # Arguments
    ///
    /// * `samples` - Interleaved f32 samples in range [-1.0, 1.0]
    /// * `timestamp_ns` - Timestamp in nanoseconds since stream start
    /// * `frame_number` - Sequential frame number
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    /// * `channels` - Number of channels (1 = mono, 2 = stereo)
    ///
    /// # Panics
    ///
    /// Panics if samples.len() is not evenly divisible by channels
    pub fn new(
        samples: Vec<f32>,
        timestamp_ns: i64,
        frame_number: u64,
        sample_rate: u32,
        channels: u32,
    ) -> Self {
        let sample_count = samples.len() / channels as usize;
        assert_eq!(
            samples.len(),
            sample_count * channels as usize,
            "samples.len() must be divisible by channels"
        );

        Self {
            samples: Arc::new(samples),
            gpu_buffer: None,
            timestamp_ns,
            frame_number,
            sample_count,
            sample_rate,
            channels,
            format: AudioFormat::F32,
            metadata: None,
        }
    }

    /// Get duration in seconds
    pub fn duration(&self) -> f64 {
        self.sample_count as f64 / self.sample_rate as f64
    }

    /// Get duration in nanoseconds
    pub fn duration_ns(&self) -> i64 {
        (self.sample_count as i64 * 1_000_000_000) / self.sample_rate as i64
    }

    /// Extract samples for a specific channel
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0 = left, 1 = right for stereo)
    ///
    /// # Returns
    ///
    /// Vector of samples for the specified channel
    ///
    /// # Panics
    ///
    /// Panics if channel >= self.channels
    pub fn channel_samples(&self, channel: u32) -> Vec<f32> {
        assert!(
            channel < self.channels,
            "channel {} out of range (0..{})",
            channel,
            self.channels
        );

        self.samples
            .iter()
            .skip(channel as usize)
            .step_by(self.channels as usize)
            .copied()
            .collect()
    }

    /// Get timestamp in seconds (for compatibility with VideoFrame)
    pub fn timestamp_seconds(&self) -> f64 {
        self.timestamp_ns as f64 / 1_000_000_000.0
    }

    /// Create example stereo 48kHz audio frame metadata for MCP/macro use
    ///
    /// Returns a JSON representation suitable for ProcessorExample.
    /// Note: This is metadata only (no actual samples), used by MCP for documentation.
    pub fn example_stereo_48k() -> serde_json::Value {
        serde_json::json!({
            "sample_count": 2048,
            "sample_rate": 48000,
            "channels": 2,
            "format": "F32",
            "timestamp_ns": 0,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example mono 48kHz audio frame metadata for MCP/macro use
    pub fn example_mono_48k() -> serde_json::Value {
        serde_json::json!({
            "sample_count": 2048,
            "sample_rate": 48000,
            "channels": 1,
            "format": "F32",
            "timestamp_ns": 0,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example stereo 44.1kHz audio frame metadata for MCP/macro use
    pub fn example_stereo_44_1k() -> serde_json::Value {
        serde_json::json!({
            "sample_count": 2048,
            "sample_rate": 44100,
            "channels": 2,
            "format": "F32",
            "timestamp_ns": 0,
            "frame_number": 1,
            "metadata": {}
        })
    }
}

/// Generic data message
///
/// For custom data types that don't fit VideoFrame or AudioFrame.
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

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_VIDEO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("720p video", Self::example_720p()),
            ("1080p video", Self::example_1080p()),
            ("4K video", Self::example_4k()),
        ]
    }
}

impl PortMessage for AudioFrame {
    fn port_type() -> PortType {
        PortType::Audio
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_AUDIO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("Stereo 48kHz", Self::example_stereo_48k()),
            ("Mono 48kHz", Self::example_mono_48k()),
            ("Stereo 44.1kHz", Self::example_stereo_44_1k()),
        ]
    }
}


impl PortMessage for DataMessage {
    fn port_type() -> PortType {
        PortType::Data
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE)
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

    #[test]
    fn test_audioframe_creation() {
        // Create stereo audio frame with 480 samples at 48kHz (10ms)
        let samples = vec![0.0; 480 * 2]; // 480 samples per channel
        let frame = AudioFrame::new(samples, 0, 0, 48000, 2);

        assert_eq!(frame.sample_count, 480);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.samples.len(), 480 * 2);
    }

    #[test]
    fn test_audioframe_duration() {
        // 480 samples at 48kHz = 10ms
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 0, 0, 48000, 2);

        assert_eq!(frame.duration(), 0.01); // 10ms
        assert_eq!(frame.duration_ns(), 10_000_000); // 10ms in ns
    }

    #[test]
    fn test_audioframe_channel_extraction() {
        // Create stereo frame with distinct L/R channels
        let samples = vec![
            1.0, -1.0, // Sample 0: L=1.0, R=-1.0
            2.0, -2.0, // Sample 1: L=2.0, R=-2.0
            3.0, -3.0, // Sample 2: L=3.0, R=-3.0
        ];
        let frame = AudioFrame::new(samples, 0, 0, 48000, 2);

        let left = frame.channel_samples(0);
        let right = frame.channel_samples(1);

        assert_eq!(left, vec![1.0, 2.0, 3.0]);
        assert_eq!(right, vec![-1.0, -2.0, -3.0]);
    }

    #[test]
    fn test_audioframe_timestamp_conversion() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 1_500_000_000, 0, 48000, 2); // 1.5 seconds

        assert_eq!(frame.timestamp_seconds(), 1.5);
    }

    #[test]
    #[should_panic(expected = "samples.len() must be divisible by channels")]
    fn test_audioframe_invalid_sample_count() {
        // Odd number of samples for stereo should panic
        let samples = vec![0.0; 5]; // 5 samples for 2 channels = invalid
        AudioFrame::new(samples, 0, 0, 48000, 2);
    }

    #[test]
    #[should_panic(expected = "channel 2 out of range")]
    fn test_audioframe_invalid_channel() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 0, 0, 48000, 2);
        let _ = frame.channel_samples(2); // Invalid channel for stereo
    }

    // Note: VideoFrame and AudioBuffer tests require actual wgpu::Device
    // to create textures/buffers. Integration tests in platform crates
    // (streamlib-apple, etc.) test the full pipeline.
}
