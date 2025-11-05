
use crate::core::{
    AudioCaptureProcessor as AudioCaptureProcessorTrait, AudioInputDevice, AudioCaptureOutputPorts,
    AudioFrame, Result, StreamError, StreamOutput, ProcessorDescriptor,
    PortDescriptor,
};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
use crate::core::bus::PortMessage;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

pub struct AppleAudioCaptureProcessor {
    device_info: AudioInputDevice,

    _device: Device,

    _stream: Stream,

    sample_buffer: Arc<Mutex<Vec<f32>>>,

    #[allow(dead_code)]
    is_capturing: Arc<AtomicBool>,

    current_level: Arc<Mutex<f32>>,

    frame_counter: Arc<AtomicU64>,

    sample_rate: u32,

    channels: u32,

    pub ports: AudioCaptureOutputPorts,

    wakeup_tx: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>>,
}

// SAFETY: AppleAudioCaptureProcessor is Send despite cpal::Stream not being Send
unsafe impl Send for AppleAudioCaptureProcessor {}

impl AppleAudioCaptureProcessor {
    fn new_internal(device_id: Option<usize>, sample_rate: u32, channels: u32) -> Result<Self> {
        let host = cpal::default_host();

        let device = if let Some(id) = device_id {
            let devices: Vec<_> = host
                .input_devices()
                .map_err(|e| StreamError::Configuration(format!("Failed to enumerate audio input devices: {}", e)))?
                .collect();
            devices
                .get(id)
                .ok_or_else(|| StreamError::Configuration(format!("Audio input device {} not found", id)))?
                .clone()
        } else {
            host.default_input_device()
                .ok_or_else(|| StreamError::Configuration("No default audio input device".into()))?
        };

        let device_name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());

        let default_config = device
            .default_input_config()
            .map_err(|e| StreamError::Configuration(format!("Failed to get audio config: {}", e)))?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels() as u32;

        tracing::info!(
            "Audio input device: {} ({}Hz, {} channels, requesting {}Hz {} channels)",
            device_name,
            device_sample_rate,
            device_channels,
            sample_rate,
            channels
        );

        let device_info = AudioInputDevice {
            id: device_id.unwrap_or(0),
            name: device_name,
            sample_rate: device_sample_rate,
            channels: device_channels,
            is_default: device_id.is_none(),
        };

        let sample_buffer = Arc::new(Mutex::new(Vec::new()));
        let sample_buffer_clone = sample_buffer.clone();

        let is_capturing = Arc::new(AtomicBool::new(false));
        let is_capturing_clone = is_capturing.clone();

        let current_level = Arc::new(Mutex::new(0.0f32));
        let current_level_clone = current_level.clone();

        let frame_counter = Arc::new(AtomicU64::new(0));

        let wakeup_tx: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>> =
            Arc::new(Mutex::new(None));
        let wakeup_tx_clone = wakeup_tx.clone();

        let stream_config = StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mut buffer = sample_buffer_clone.lock();

                    buffer.extend_from_slice(data);

                    let peak = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                    *current_level_clone.lock() = peak;

                    is_capturing_clone.store(true, Ordering::Relaxed);

                    if let Some(tx) = wakeup_tx_clone.lock().as_ref() {
                        let _ = tx.send(crate::core::runtime::WakeupEvent::DataAvailable);
                    }
                },
                move |err| {
                    tracing::error!("Audio capture error: {}", err);
                },
                None, // None = blocking mode
            )
            .map_err(|e| StreamError::Configuration(format!("Failed to build audio stream: {}", e)))?;

        stream
            .play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start audio stream: {}", e)))?;

        let ports = AudioCaptureOutputPorts {
            audio: StreamOutput::new("audio".to_string()),
        };

        Ok(Self {
            device_info,
            _device: device,
            _stream: stream,
            sample_buffer,
            is_capturing,
            current_level,
            frame_counter,
            sample_rate,
            channels,
            ports,
            wakeup_tx: Arc::new(Mutex::new(None)),  // Will be set by runtime via set_wakeup_channel()
        })
    }
}

impl AudioCaptureProcessorTrait for AppleAudioCaptureProcessor {
    fn new(device_id: Option<usize>, sample_rate: u32, channels: u32) -> Result<Self> {
        Self::new_internal(device_id, sample_rate, channels)
    }

    fn list_devices() -> Result<Vec<AudioInputDevice>> {
        let host = cpal::default_host();
        let devices: Result<Vec<AudioInputDevice>> = host
            .input_devices()
            .map_err(|e| StreamError::Configuration(format!("Failed to enumerate audio input devices: {}", e)))?
            .enumerate()
            .map(|(id, device)| {
                let name = device.name().unwrap_or_else(|_| "Unknown Device".to_string());

                let config = device
                    .default_input_config()
                    .map_err(|e| StreamError::Configuration(format!("Failed to get device config: {}", e)))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as u32;

                let is_default = if let Some(default_device) = host.default_input_device() {
                    device.name().ok() == default_device.name().ok()
                } else {
                    false
                };

                Ok(AudioInputDevice {
                    id,
                    name,
                    sample_rate,
                    channels,
                    is_default,
                })
            })
            .collect();

        devices
    }

    fn current_device(&self) -> &AudioInputDevice {
        &self.device_info
    }

    fn current_level(&self) -> f32 {
        *self.current_level.lock()
    }

    fn output_ports(&mut self) -> &mut AudioCaptureOutputPorts {
        &mut self.ports
    }
}

impl StreamElement for AppleAudioCaptureProcessor {
    fn name(&self) -> &str {
        &self.device_info.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleAudioCaptureProcessor as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new() // Sources have no inputs
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: AudioFrame::<1>::schema(),
            required: true,
            description: "Captured mono audio frames from the microphone".to_string(),
        }]
    }

    fn start(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        tracing::info!("AudioCapture {}: Starting ({}Hz, {} channels)",
            self.device_info.name, self.sample_rate, self.channels);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("AudioCapture {}: Stopping (captured {} frames)",
            self.device_info.name, self.frame_counter.load(Ordering::Relaxed));
        Ok(())
    }
}

impl StreamProcessor for AppleAudioCaptureProcessor {
    type Config = crate::core::AudioCaptureConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id, config.sample_rate, config.channels)
    }

    fn process(&mut self) -> Result<()> {
        let samples = {
            let mut buffer = self.sample_buffer.lock();

            let min_chunk_size = 512 * self.channels as usize;

            if buffer.len() >= min_chunk_size {
                let samples: Vec<f32> = buffer.drain(..).collect();
                samples
            } else {
                return Err(StreamError::Runtime(
                    format!("Not enough samples available ({} < {})", buffer.len(), min_chunk_size)
                ));
            }
        };

        let frame_number = self.frame_counter.fetch_add(1, Ordering::Relaxed);
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        let mono_samples: Vec<f32> = if self.channels == 2 {
            samples.chunks_exact(2)
                .map(|chunk| (chunk[0] + chunk[1]) / 2.0)
                .collect()
        } else {
            samples
        };

        let frame = AudioFrame::<1>::new(
            mono_samples,
            timestamp_ns,
            frame_number,
        );

        self.ports.audio.write(frame);
        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,
            priority: ThreadPriority::RealTime,
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> where Self: Sized {
        Some(
            ProcessorDescriptor::new(
                "AppleAudioCaptureProcessor",
                "Captures mono audio from macOS microphones using CoreAudio via cpal"
            )
            .with_usage_context(
                "Automatically uses default microphone if device_id not specified. \
                 Outputs mono AudioFrame<1> at system sample rate (typically 48kHz). \
                 Converts stereo hardware input to mono by averaging channels. \
                 Callback-driven for low latency."
            )
            .with_tags(vec!["audio", "source", "microphone", "coreaudio", "macos", "mono"])
        )
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        match port_name {
            "audio" => Some(crate::core::bus::PortType::Audio1),
            _ => None,
        }
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<1>>>>() {
            if port_name == "audio" {
                self.ports.audio.add_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let devices = AppleAudioCaptureProcessor::list_devices();

        assert!(devices.is_ok());

        if let Ok(devices) = devices {
            println!("Found {} audio input devices:", devices.len());
            for device in &devices {
                println!(
                    "  [{}] {}: {}Hz, {} channels{}",
                    device.id,
                    device.name,
                    device.sample_rate,
                    device.channels,
                    if device.is_default { " (default)" } else { "" }
                );
            }

            assert!(devices.len() > 0, "Expected at least one audio input device");
        }
    }

    #[test]
    fn test_create_default_device() {
        let result = AppleAudioCaptureProcessor::new(None, 48000, 2);

        match result {
            Ok(processor) => {
                let device = processor.current_device();
                println!("Created audio capture: {}", device.name);
                assert_eq!(processor.sample_rate, 48000);
                assert_eq!(processor.channels, 2);
                assert!(device.is_default);
            }
            Err(e) => {
                println!("Note: Could not create audio capture (may require permissions): {}", e);
            }
        }
    }

    #[test]
    fn test_capture_audio() {
        let result = AppleAudioCaptureProcessor::new(None, 48000, 2);

        if let Ok(mut processor) = result {
            std::thread::sleep(std::time::Duration::from_millis(100));

            let result = processor.process();
            if result.is_ok() {
                println!("Successfully processed captured audio");

                let level = processor.current_level();
                println!("Current audio level: {:.3}", level);
                assert!(level >= 0.0 && level <= 1.0);
            } else {
                println!("Note: Audio processing returned: {:?}", result);
            }
        } else {
            println!("Note: Could not create audio capture (may require permissions)");
        }
    }
}
