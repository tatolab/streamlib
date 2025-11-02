//! Apple AudioCaptureProcessor implementation using CoreAudio
//!
//! Uses the `cpal` crate which provides a safe Rust wrapper around CoreAudio on macOS.
//! This gives us low-latency audio capture from microphones with minimal overhead.

use crate::core::{
    AudioCaptureProcessor as AudioCaptureProcessorTrait, AudioInputDevice, AudioCaptureOutputPorts,
    AudioFrame, Result, StreamError, StreamProcessor, StreamOutput, ProcessorDescriptor,
    PortDescriptor, SCHEMA_AUDIO_FRAME,
};
use crate::core::traits::{StreamElement, StreamSource, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

/// Apple CoreAudio implementation of AudioCaptureProcessor
///
/// # Architecture
///
/// - Uses `cpal` library which wraps CoreAudio on macOS
/// - Maintains an internal ring buffer for captured audio samples
/// - Runs audio capture on a dedicated thread (managed by cpal/CoreAudio)
/// - Low-latency: typical latency < 20ms on macOS
///
/// # Example
///
/// ```ignore
/// use streamlib::AppleAudioCaptureProcessor;
///
/// // Create microphone input using default device at 48kHz stereo
/// let mic = AppleAudioCaptureProcessor::new(None, 48000, 2)?;
///
/// // Connect to audio pipeline
/// runtime.add_processor(Box::new(mic));
/// runtime.connect("mic.audio", "plugin.audio")?;
/// ```
pub struct AppleAudioCaptureProcessor {
    /// Current audio device information
    device_info: AudioInputDevice,

    /// cpal device handle
    _device: Device,

    /// cpal audio stream (keeps audio thread alive)
    _stream: Stream,

    /// Ring buffer for captured audio samples (shared with audio thread)
    ///
    /// Audio thread writes captured samples here, process() reads them
    sample_buffer: Arc<Mutex<Vec<f32>>>,

    /// Whether the processor is actively capturing
    #[allow(dead_code)]
    is_capturing: Arc<AtomicBool>,

    /// Current audio level (peak)
    current_level: Arc<Mutex<f32>>,

    /// Frame counter for generating AudioFrame metadata
    frame_counter: Arc<AtomicU64>,

    /// Sample rate for this input
    sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    channels: u32,

    /// Output ports
    pub ports: AudioCaptureOutputPorts,

    /// Wakeup channel for push-based operation (Phase 3)
    /// When audio data arrives, we trigger processing immediately
    /// Wrapped in Arc<Mutex> so audio callback can access it
    wakeup_tx: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>>,
}

// SAFETY: AppleAudioCaptureProcessor is Send despite cpal::Stream not being Send
// because all shared state (sample_buffer, is_capturing, etc.) is protected by Arc/Mutex
// and the Stream's internal audio callback only accesses thread-safe types.
unsafe impl Send for AppleAudioCaptureProcessor {}

impl AppleAudioCaptureProcessor {
    /// Create new audio capture processor using default or specified device
    fn new_internal(device_id: Option<usize>, sample_rate: u32, channels: u32) -> Result<Self> {
        // Get cpal host (CoreAudio on macOS)
        let host = cpal::default_host();

        // Get device
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

        // Get device name
        let device_name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());

        // Get default config from device
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

        // Create device info
        let device_info = AudioInputDevice {
            id: device_id.unwrap_or(0),
            name: device_name,
            sample_rate: device_sample_rate,
            channels: device_channels,
            is_default: device_id.is_none(),
        };

        // Create shared ring buffer for captured audio samples
        let sample_buffer = Arc::new(Mutex::new(Vec::new()));
        let sample_buffer_clone = sample_buffer.clone();

        // Create flag for capture status
        let is_capturing = Arc::new(AtomicBool::new(false));
        let is_capturing_clone = is_capturing.clone();

        // Create current level tracking
        let current_level = Arc::new(Mutex::new(0.0f32));
        let current_level_clone = current_level.clone();

        // Create frame counter
        let frame_counter = Arc::new(AtomicU64::new(0));

        // Create wakeup channel holder (Phase 3: Push-based operation)
        let wakeup_tx: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>> =
            Arc::new(Mutex::new(None));
        let wakeup_tx_clone = wakeup_tx.clone();

        // Build audio stream configuration
        // Note: We request the desired sample_rate and channels, but cpal may use device defaults
        let stream_config = StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Build input stream with callback
        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Audio thread callback - capture input buffer
                    let mut buffer = sample_buffer_clone.lock();

                    // Append captured samples to ring buffer
                    buffer.extend_from_slice(data);

                    // Calculate peak level for this chunk
                    let peak = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                    *current_level_clone.lock() = peak;

                    is_capturing_clone.store(true, Ordering::Relaxed);

                    // Phase 3: Trigger push-based wakeup when data arrives
                    if let Some(tx) = wakeup_tx_clone.lock().as_ref() {
                        // Send non-blocking wakeup event
                        let _ = tx.send(crate::core::runtime::WakeupEvent::DataAvailable);
                    }
                },
                move |err| {
                    tracing::error!("Audio capture error: {}", err);
                },
                None, // None = blocking mode
            )
            .map_err(|e| StreamError::Configuration(format!("Failed to build audio stream: {}", e)))?;

        // Start the stream immediately
        stream
            .play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start audio stream: {}", e)))?;

        // Create output ports
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

                // Get default config to determine capabilities
                let config = device
                    .default_input_config()
                    .map_err(|e| StreamError::Configuration(format!("Failed to get device config: {}", e)))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as u32;

                // Check if this is the default device
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

impl StreamProcessor for AppleAudioCaptureProcessor {
    type Config = crate::core::AudioCaptureConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        // Parse device_id string to usize if provided
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id, config.sample_rate, config.channels)
    }

    fn process(&mut self) -> Result<()> {
        // Phase 6: Delegate to StreamSource::generate() for clean separation
        // generate() produces the frame, process() writes it to the output port
        match <Self as StreamSource>::generate(self) {
            Ok(frame) => {
                // Write to output port (this will trigger downstream via push notification)
                self.ports.audio.write(frame);
                Ok(())
            }
            Err(StreamError::Runtime(msg)) if msg.contains("Not enough samples") => {
                // Not enough samples yet - this is normal for callback-driven sources
                // at startup or during audio gaps
                Ok(())
            }
            Err(e) => {
                tracing::error!("AudioCapture: Error generating frame: {:?}", e);
                Err(e)
            }
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "AppleAudioCaptureProcessor",
                "Captures audio from microphones/line-in using CoreAudio. Outputs AudioFrames at the configured sample rate.",
            )
            .with_usage_context(
                "Use when you need live audio input from a microphone or line-in source. This is typically a source \
                 processor in an audio pipeline. Use list_devices() to enumerate available input devices, or pass None \
                 for device_id to use the system default microphone.",
            )
            .with_output(PortDescriptor::new(
                "audio",
                Arc::clone(&SCHEMA_AUDIO_FRAME),
                true,
                "Captured audio frames. Each frame contains samples at the configured sample rate and channel count. \
                 Frames are produced at the runtime's tick rate (typically 60 FPS).",
            ))
            .with_tags(vec!["source", "audio", "microphone", "input", "capture"])
        )
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn set_wakeup_channel(&mut self, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        *self.wakeup_tx.lock() = Some(wakeup_tx);
        tracing::debug!("AudioCaptureProcessor: Push-based wakeup enabled");
    }
}

// StreamElement implementation - GStreamer-inspired base trait
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
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Captured audio frames from the microphone".to_string(),
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

// StreamSource implementation - GStreamer-inspired source trait
impl StreamSource for AppleAudioCaptureProcessor {
    type Output = AudioFrame;
    type Config = crate::core::AudioCaptureConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        // Parse device_id string to usize if provided
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id, config.sample_rate, config.channels)
    }

    fn generate(&mut self) -> Result<Self::Output> {
        // Read captured samples from ring buffer
        let samples = {
            let mut buffer = self.sample_buffer.lock();

            // Check if we have enough samples for a reasonable chunk
            // Use 512 samples per channel as minimum (good balance for audio processing)
            let min_chunk_size = 512 * self.channels as usize;

            if buffer.len() >= min_chunk_size {
                // Extract all available samples
                let samples: Vec<f32> = buffer.drain(..).collect();
                samples
            } else {
                // Not enough samples yet - return error so runtime can retry
                return Err(StreamError::Runtime(
                    format!("Not enough samples available ({} < {})", buffer.len(), min_chunk_size)
                ));
            }
        };

        // Create AudioFrame from captured samples
        let frame_number = self.frame_counter.fetch_add(1, Ordering::Relaxed);
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        Ok(AudioFrame::new(
            samples,
            timestamp_ns,
            frame_number,
            self.sample_rate,
            self.channels,
        ))
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        // Audio capture is callback-driven - CoreAudio/cpal callback triggers processing
        SchedulingConfig {
            mode: SchedulingMode::Callback,
            priority: ThreadPriority::RealTime,  // Audio I/O requires real-time priority
            clock: ClockSource::Audio, // Audio hardware provides timing
            rate_hz: None, // Not applicable for callback mode
            provide_clock: false, // Capture doesn't provide pipeline clock
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> where Self: Sized {
        <AppleAudioCaptureProcessor as StreamProcessor>::descriptor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let devices = AppleAudioCaptureProcessor::list_devices();

        // Should succeed even if no devices (though macOS usually has built-in mic)
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

            // macOS should have at least one input device (built-in mic)
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
        // Try to create processor and capture some audio
        let result = AppleAudioCaptureProcessor::new(None, 48000, 2);

        if let Ok(mut processor) = result {
            // Let it capture for a bit
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Try to process captured audio
            let result = processor.process();
            if result.is_ok() {
                println!("Successfully processed captured audio");

                // Check audio level
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
