//! Audio capture (microphone) processor trait
//!
//! This module defines the platform-agnostic trait for audio capture processors.
//! Captures audio from microphones/line-in and outputs AudioFrames.
//!
//! # Platform Implementations
//!
//! - **macOS/iOS**: CoreAudio (`AppleAudioCaptureProcessor`)
//! - **Linux**: ALSA/PulseAudio (future)
//! - **Windows**: WASAPI (future)
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{AudioCaptureProcessor, AudioOutputProcessor, StreamRuntime};
//!
//! // Create audio capture from default microphone
//! let mut mic = AudioCaptureProcessor::new(None, 48000, 2)?;
//! let mut speaker = AudioOutputProcessor::new(None)?;
//!
//! // Type-safe connection
//! runtime.connect(
//!     &mut mic.output_ports().audio,
//!     &mut speaker.input_ports().audio
//! )?;
//! ```

use crate::core::{StreamOutput, AudioFrame, Result};
use crate::core::traits::{StreamElement, StreamSource};

/// Configuration for audio capture processors
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioCaptureConfig {
    /// Optional device ID/name
    /// If None, uses the default input device
    pub device_id: Option<String>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u32,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            device_id: None,
            sample_rate: 48000,
            channels: 2,
        }
    }
}

/// Audio input device information
///
/// Shares same structure as AudioDevice from audio_output module for consistency.
#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    /// Platform-specific device ID
    pub id: usize,

    /// Human-readable device name (e.g., "MacBook Pro Microphone")
    pub name: String,

    /// Default sample rate supported by device
    pub sample_rate: u32,

    /// Number of input channels (1 = mono, 2 = stereo)
    pub channels: u32,

    /// Whether this is the system default input device
    pub is_default: bool,
}

/// Audio capture processor trait
///
/// Platform-specific implementations capture audio from input devices
/// and output AudioFrames through an output port.
///
/// # Architecture
///
/// - **Input ports**: None (audio comes from hardware)
/// - **Output port**: `audio` (AudioFrame)
/// - **Processing**: Capture from device → AudioFrame conversion → output
///
/// # Platform Notes
///
/// **macOS/iOS (CoreAudio)**:
/// - Uses `AVAudioEngine` or `AudioUnit` for low-latency capture
/// - Requires microphone permission (requested automatically)
/// - Handles sample rate conversion automatically
/// - Target latency: < 20ms
///
/// **Linux (ALSA/PulseAudio)**:
/// - Uses `cpal` crate for cross-platform audio
/// - Requires proper PulseAudio/PipeWire setup
/// - May need udev rules for device access
///
/// **Windows (WASAPI)**:
/// - Uses `cpal` crate
/// - Exclusive mode for lowest latency
pub trait AudioCaptureProcessor: StreamElement + StreamSource<Output = AudioFrame, Config = AudioCaptureConfig> {
    /// Create new audio capture processor
    ///
    /// # Arguments
    ///
    /// * `device_id` - Optional device ID from `list_devices()`. If `None`, uses system default.
    /// * `sample_rate` - Desired sample rate (e.g., 48000). Device will convert if needed.
    /// * `channels` - Number of channels (1 = mono, 2 = stereo)
    ///
    /// # Returns
    ///
    /// Audio capture processor ready to output AudioFrames
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Use default microphone at 48kHz stereo
    /// let mic = AudioCaptureProcessor::new(None, 48000, 2)?;
    ///
    /// // Use specific device at 44.1kHz mono (voice quality)
    /// let devices = AudioCaptureProcessor::list_devices()?;
    /// let usb_mic = AudioCaptureProcessor::new(Some(devices[1].id), 44100, 1)?;
    /// ```
    fn new(device_id: Option<usize>, sample_rate: u32, channels: u32) -> Result<Self>
    where
        Self: Sized;

    /// List available audio input devices
    ///
    /// # Returns
    ///
    /// Vector of available audio input devices with their capabilities
    ///
    /// # Example
    ///
    /// ```ignore
    /// let devices = AudioCaptureProcessor::list_devices()?;
    /// for device in devices {
    ///     println!("{}: {} ({}Hz, {} channels)",
    ///         device.id, device.name, device.sample_rate, device.channels);
    /// }
    /// ```
    fn list_devices() -> Result<Vec<AudioInputDevice>>;

    /// Get the currently selected device
    ///
    /// Returns information about the device this processor is using
    fn current_device(&self) -> &AudioInputDevice;

    /// Get current audio level (0.0 to 1.0)
    ///
    /// Useful for monitoring input levels, detecting voice activity, etc.
    ///
    /// # Returns
    ///
    /// Peak audio level from recent samples (0.0 = silence, 1.0 = maximum)
    fn current_level(&self) -> f32 {
        0.0 // Default implementation
    }

    /// Get mutable access to output ports
    ///
    /// Required for type-safe connections between processors.
    fn output_ports(&mut self) -> &mut AudioCaptureOutputPorts;
}

/// Output ports for AudioCaptureProcessor
pub struct AudioCaptureOutputPorts {
    /// Audio output port (sends AudioFrame)
    pub audio: StreamOutput<AudioFrame>,
}
