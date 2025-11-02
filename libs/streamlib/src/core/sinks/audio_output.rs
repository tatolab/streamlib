//! Audio output (speaker) processor trait
//!
//! This module defines the platform-agnostic trait for audio output processors.
//! Receives AudioFrames and plays them through speakers/headphones.
//!
//! # Platform Implementations
//!
//! - **macOS/iOS**: CoreAudio (`AppleAudioOutputProcessor`)
//! - **Linux**: ALSA/PulseAudio (future)
//! - **Windows**: WASAPI (future)
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{AudioCaptureProcessor, AudioOutputProcessor, StreamRuntime};
//!
//! // Create audio output to default device
//! let mut mic = AudioCaptureProcessor::new(None, 48000, 2)?;
//! let mut speaker = AudioOutputProcessor::new(None)?;
//!
//! // Type-safe connection
//! runtime.connect(
//!     &mut mic.output_ports().audio,
//!     &mut speaker.input_ports().audio
//! )?;
//! ```

use crate::core::{StreamProcessor, StreamInput, AudioFrame, Result};

/// Configuration for audio output processors
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioOutputConfig {
    /// Optional device ID/name
    /// If None, uses the default output device
    pub device_id: Option<String>,
}

impl Default for AudioOutputConfig {
    fn default() -> Self {
        Self { device_id: None }
    }
}

/// Audio output device information
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Platform-specific device ID
    pub id: usize,

    /// Human-readable device name (e.g., "MacBook Pro Speakers")
    pub name: String,

    /// Default sample rate supported by device
    pub sample_rate: u32,

    /// Number of output channels (2 = stereo, 6 = 5.1, etc.)
    pub channels: u32,

    /// Whether this is the system default device
    pub is_default: bool,
}

/// Audio output processor trait
///
/// Platform-specific implementations receive AudioFrames from an input port
/// and play them through the system's audio output device.
///
/// # Architecture
///
/// - **Input port**: `audio` (AudioFrame)
/// - **Output ports**: None (audio goes to hardware)
/// - **Processing**: Convert AudioFrame → platform audio buffer, play through speakers
///
/// # Platform Notes
///
/// **macOS/iOS (CoreAudio)**:
/// - Uses `AVAudioEngine` or `AudioUnit` for low-latency playback
/// - Handles sample rate conversion automatically
/// - Target latency: < 20ms
///
/// **Linux (ALSA/PulseAudio)**:
/// - Uses `cpal` crate for cross-platform audio
/// - Requires proper PulseAudio/PipeWire setup
///
/// **Windows (WASAPI)**:
/// - Uses `cpal` crate
/// - Exclusive mode for lowest latency
pub trait AudioOutputProcessor: StreamProcessor {
    /// Create new audio output processor
    ///
    /// # Arguments
    ///
    /// * `device_id` - Optional device ID from `list_devices()`. If `None`, uses system default.
    ///
    /// # Returns
    ///
    /// Audio output processor ready to receive AudioFrames
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Use default device
    /// let speaker = AudioOutputProcessor::new(None)?;
    ///
    /// // Use specific device
    /// let devices = AudioOutputProcessor::list_devices()?;
    /// let headphones = AudioOutputProcessor::new(Some(devices[1].id))?;
    /// ```
    fn new(device_id: Option<usize>) -> Result<Self>
    where
        Self: Sized;

    /// List available audio output devices
    ///
    /// # Returns
    ///
    /// Vector of available audio output devices with their capabilities
    ///
    /// # Example
    ///
    /// ```ignore
    /// let devices = AudioOutputProcessor::list_devices()?;
    /// for device in devices {
    ///     println!("{}: {} ({}Hz, {} channels)",
    ///         device.id, device.name, device.sample_rate, device.channels);
    /// }
    /// ```
    fn list_devices() -> Result<Vec<AudioDevice>>;

    /// Get the currently selected device
    ///
    /// Returns information about the device this processor is using
    fn current_device(&self) -> &AudioDevice;

    /// Get mutable access to input ports
    ///
    /// Required for type-safe connections between processors.
    fn input_ports(&mut self) -> &mut AudioOutputInputPorts;
}

/// Input ports for AudioOutputProcessor
pub struct AudioOutputInputPorts {
    /// Audio input port (receives AudioFrame)
    pub audio: StreamInput<AudioFrame>,
}
