
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

// Ports are now defined directly on platform-specific implementations
// No shared port struct needed - each implementation uses #[derive(StreamProcessor)]
