//! Audio frame message type
//!
//! Represents a chunk of audio data with CPU-first architecture.
//! Unlike VideoFrame (GPU-first), audio is primarily CPU-based.

use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;

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
/// use streamlib::AudioFrame;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
