//! Processor Configuration Types
//!
//! This module defines configuration types for all processors in streamlib.
//! Each processor has an associated Config type that is passed to its constructor.

use std::path::PathBuf;

/// Empty configuration for processors that don't need configuration
#[derive(Debug, Clone, Default)]
pub struct EmptyConfig;

/// Configuration for camera processors
#[derive(Debug, Clone)]
pub struct CameraConfig {
    /// Optional device ID to use (e.g., "0x1234" on macOS)
    /// If None, uses the default camera
    pub device_id: Option<String>,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self { device_id: None }
    }
}

impl From<()> for CameraConfig {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

/// Configuration for display processors
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    /// Window width in pixels
    pub width: u32,
    /// Window height in pixels
    pub height: u32,
    /// Optional window title
    pub title: Option<String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            title: None,
        }
    }
}

/// Configuration for audio capture processors
#[derive(Debug, Clone)]
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

/// Configuration for audio output processors
#[derive(Debug, Clone)]
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

/// Configuration for CLAP effect processors
#[derive(Debug, Clone)]
pub struct ClapEffectConfig {
    /// Path to the CLAP plugin file
    pub plugin_path: PathBuf,
    /// Optional plugin name (if bundle contains multiple)
    pub plugin_name: Option<String>,
    /// Sample rate for audio processing
    pub sample_rate: u32,
    /// Buffer size for audio processing
    pub buffer_size: usize,
}

impl Default for ClapEffectConfig {
    fn default() -> Self {
        Self {
            plugin_path: PathBuf::new(), // Empty path - must be set by user
            plugin_name: None,
            sample_rate: 48000,  // Standard audio sample rate
            buffer_size: 2048,   // Standard CLAP buffer size
        }
    }
}

/// Configuration for test tone generator
#[derive(Debug, Clone)]
pub struct TestToneConfig {
    /// Frequency in Hz
    pub frequency: f64,
    /// Amplitude (0.0 to 1.0)
    pub amplitude: f64,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Optional timer group ID for synchronized timing with other processors
    pub timer_group_id: Option<String>,
}

impl Default for TestToneConfig {
    fn default() -> Self {
        Self {
            frequency: 440.0,
            amplitude: 0.5,
            sample_rate: 48000,
            timer_group_id: None,
        }
    }
}

/// Configuration for audio mixer processor
#[derive(Debug, Clone)]
pub struct AudioMixerConfig {
    /// Number of input ports
    pub num_inputs: usize,
    /// Mixing strategy
    pub strategy: crate::core::processors::MixingStrategy,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Buffer size in samples per channel
    pub buffer_size: usize,
    /// Optional timer group ID (same as input sources for deterministic ordering)
    pub timer_group_id: Option<String>,
}

impl Default for AudioMixerConfig {
    fn default() -> Self {
        Self {
            num_inputs: 2,
            strategy: crate::core::processors::MixingStrategy::SumNormalized,
            sample_rate: 48000,
            buffer_size: 2048,
            timer_group_id: None,
        }
    }
}

/// Configuration for performance overlay processor (debug-overlay feature)
#[derive(Debug, Clone, Default)]
pub struct PerformanceOverlayConfig {
    // No configuration needed - overlay automatically displays performance metrics
}
