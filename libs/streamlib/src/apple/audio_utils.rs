use crate::core::{AudioDevice, Result, StreamError};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Stream, StreamConfig};

pub struct AudioOutputSetup {
    pub stream: Stream,
    pub device: Device,
    pub device_info: AudioDevice,
    pub sample_rate: u32,
    pub channels: u32,
}

pub fn setup_audio_output<F>(
    device_id: Option<usize>,
    buffer_size: usize,
    mut callback: F,
) -> Result<AudioOutputSetup>
where
    F: FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static,
{
    let host = cpal::default_host();

    let device = if let Some(id) = device_id {
        let devices: Vec<_> = host
            .output_devices()
            .map_err(|e| StreamError::Configuration(format!("Failed to enumerate audio devices: {}", e)))?
            .collect();
        devices
            .get(id)
            .ok_or_else(|| StreamError::Configuration(format!("Audio device {} not found", id)))?
            .clone()
    } else {
        host.default_output_device()
            .ok_or_else(|| StreamError::Configuration("No default audio output device".into()))?
    };

    let device_name = device
        .name()
        .unwrap_or_else(|_| "Unknown Device".to_string());

    let config = device
        .default_output_config()
        .map_err(|e| StreamError::Configuration(format!("Failed to get audio config: {}", e)))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as u32;

    tracing::info!(
        "Audio output device: {} ({}Hz, {} channels)",
        device_name,
        sample_rate,
        channels
    );

    let device_info = AudioDevice {
        id: device_id.unwrap_or(0),
        name: device_name.clone(),
        sample_rate,
        channels,
        is_default: device_id.is_none(),
    };

    tracing::info!(
        "Audio output buffer size: {} frames ({:.2} ms at {}Hz)",
        buffer_size,
        buffer_size as f64 / sample_rate as f64 * 1000.0,
        sample_rate
    );

    let stream_config = StreamConfig {
        channels: channels as u16,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Fixed(buffer_size as u32),
    };

    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                callback(data, info);
            },
            |err| {
                tracing::error!("Audio output stream error: {}", err);
            },
            None,
        )
        .map_err(|e| StreamError::Configuration(format!("Failed to build audio stream: {}", e)))?;

    Ok(AudioOutputSetup {
        stream,
        device,
        device_info,
        sample_rate,
        channels,
    })
}