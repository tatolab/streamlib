
use crate::core::{StreamOutput, AudioFrame, Result};
use crate::core::traits::{StreamElement, StreamProcessor};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioCaptureConfig {
    pub device_id: Option<String>,
    pub sample_rate: u32,
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

#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    pub id: usize,

    pub name: String,

    pub sample_rate: u32,

    pub channels: u32,

    pub is_default: bool,
}

pub trait AudioCaptureProcessor: StreamElement + StreamProcessor<Config = AudioCaptureConfig> {
    fn new(device_id: Option<usize>, sample_rate: u32, channels: u32) -> Result<Self>
    where
        Self: Sized;

    fn list_devices() -> Result<Vec<AudioInputDevice>>;

    fn current_device(&self) -> &AudioInputDevice;

    fn current_level(&self) -> f32 {
        0.0 // Default implementation
    }

    fn output_ports(&mut self) -> &mut AudioCaptureOutputPorts;
}

pub struct AudioCaptureOutputPorts {
    pub audio: StreamOutput<AudioFrame<1>>,
}
