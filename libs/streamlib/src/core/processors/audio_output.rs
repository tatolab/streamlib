
use crate::core::{StreamInput, Result};
use crate::core::frames::AudioFrame;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioOutputConfig {
    pub device_id: Option<String>,
}

impl Default for AudioOutputConfig {
    fn default() -> Self {
        Self { device_id: None }
    }
}

#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub id: usize,

    pub name: String,

    pub sample_rate: u32,

    pub channels: u32,

    pub is_default: bool,
}

pub trait AudioOutputProcessor {
    fn new(device_id: Option<usize>) -> Result<Self>
    where
        Self: Sized;

    fn list_devices() -> Result<Vec<AudioDevice>>;

    fn current_device(&self) -> &AudioDevice;
}

// Platform-specific processor re-exports
// This allows `use streamlib::core::AudioOutputProcessor` to work across platforms
#[cfg(target_os = "macos")]
pub use crate::apple::processors::audio_output::AppleAudioOutputProcessor;

// TODO: Add Linux/Windows implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::audio_output::LinuxAudioOutputProcessor;
