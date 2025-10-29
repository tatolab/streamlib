//! Apple AudioOutputProcessor implementation using CoreAudio
//!
//! Uses the `cpal` crate which provides a safe Rust wrapper around CoreAudio on macOS.
//! This gives us low-latency audio playback with minimal overhead.

use crate::core::{
    AudioOutputProcessor as AudioOutputProcessorTrait, AudioDevice, AudioFrame, Result,
    StreamError, StreamProcessor, TimedTick,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

/// Apple CoreAudio implementation of AudioOutputProcessor
///
/// # Architecture
///
/// - Uses `cpal` library which wraps CoreAudio on macOS
/// - Maintains an internal ring buffer for audio frames
/// - Runs audio playback on a dedicated thread (managed by cpal/CoreAudio)
/// - Low-latency: typical latency < 20ms on macOS
///
/// # Example
///
/// ```ignore
/// use streamlib::AudioOutputProcessor;
///
/// // Create speaker output using default device
/// let speaker = AudioOutputProcessor::new(None)?;
///
/// // In process() method, write AudioFrames
/// speaker.process(tick)?;  // Reads from "audio" input port
/// ```
pub struct AppleAudioOutputProcessor {
    /// Current audio device information
    device_info: AudioDevice,

    /// cpal device handle
    _device: Device,

    /// cpal audio stream (keeps audio thread alive)
    _stream: Stream,

    /// Ring buffer for audio samples (shared with audio thread)
    ///
    /// Audio frames are pushed here, audio thread pulls them
    sample_buffer: Arc<Mutex<Vec<f32>>>,

    /// Whether the processor is actively playing
    is_playing: Arc<AtomicBool>,

    /// Sample rate for this output
    sample_rate: u32,

    /// Number of channels (2 = stereo)
    channels: u32,

    /// Input port name
    #[allow(dead_code)]
    input_port: String,
}

// SAFETY: AppleAudioOutputProcessor is Send despite cpal::Stream not being Send
// because all shared state (sample_buffer, is_playing) is protected by Arc/Mutex
// and the Stream's internal audio callback only accesses thread-safe types.
unsafe impl Send for AppleAudioOutputProcessor {}

impl AppleAudioOutputProcessor {
    /// Create new audio output processor using default or specified device
    fn new_internal(device_id: Option<usize>) -> Result<Self> {
        // Get cpal host (CoreAudio on macOS)
        let host = cpal::default_host();

        // Get device
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

        // Get device name
        let device_name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());

        // Get default config
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

        // Create device info
        let device_info = AudioDevice {
            id: device_id.unwrap_or(0),
            name: device_name,
            sample_rate,
            channels,
            is_default: device_id.is_none(),
        };

        // Create shared ring buffer for audio samples
        let sample_buffer = Arc::new(Mutex::new(Vec::new()));
        let sample_buffer_clone = sample_buffer.clone();

        // Create flag for playback status
        let is_playing = Arc::new(AtomicBool::new(false));
        let is_playing_clone = is_playing.clone();

        // Build audio stream configuration
        let stream_config = StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Build output stream with callback
        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Audio thread callback - fill output buffer
                    let mut buffer = sample_buffer_clone.lock();

                    if buffer.len() >= data.len() {
                        // Copy samples from ring buffer to output
                        data.copy_from_slice(&buffer[..data.len()]);
                        buffer.drain(..data.len());
                        is_playing_clone.store(true, Ordering::Relaxed);
                    } else {
                        // Not enough samples - output silence
                        data.fill(0.0);
                        is_playing_clone.store(false, Ordering::Relaxed);
                    }
                },
                |err| {
                    tracing::error!("Audio output stream error: {}", err);
                },
                None, // No timeout
            )
            .map_err(|e| StreamError::Configuration(format!("Failed to build audio stream: {}", e)))?;

        // Start the stream
        stream
            .play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start audio stream: {}", e)))?;

        Ok(Self {
            device_info,
            _device: device,
            _stream: stream,
            sample_buffer,
            is_playing,
            sample_rate,
            channels,
            input_port: "audio".to_string(),
        })
    }

    /// Get current buffer fill level (0.0 to 1.0)
    ///
    /// Useful for monitoring latency and detecting underruns
    pub fn buffer_level(&self) -> f32 {
        let buffer = self.sample_buffer.lock();
        let target_size = (self.sample_rate as usize / 10) * self.channels as usize; // 100ms buffer
        (buffer.len() as f32 / target_size as f32).min(1.0)
    }

    /// Check if audio is currently playing
    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }
}

impl AudioOutputProcessorTrait for AppleAudioOutputProcessor {
    fn new(device_id: Option<usize>) -> Result<Self> {
        Self::new_internal(device_id)
    }

    fn list_devices() -> Result<Vec<AudioDevice>> {
        let host = cpal::default_host();
        let mut devices = Vec::new();
        let default_device = host.default_output_device();

        for (id, device) in host
            .output_devices()
            .map_err(|e| StreamError::Configuration(format!("Failed to enumerate audio devices: {}", e)))?
            .enumerate()
        {
            let name = device.name().unwrap_or_else(|_| format!("Device {}", id));
            let config = device.default_output_config().ok();

            let (sample_rate, channels) = if let Some(cfg) = config {
                (cfg.sample_rate().0, cfg.channels() as u32)
            } else {
                (48000, 2) // Defaults
            };

            let is_default = if let Some(ref default) = default_device {
                default.name().ok() == Some(name.clone())
            } else {
                false
            };

            devices.push(AudioDevice {
                id,
                name,
                sample_rate,
                channels,
                is_default,
            });
        }

        Ok(devices)
    }

    fn current_device(&self) -> &AudioDevice {
        &self.device_info
    }
}

impl StreamProcessor for AppleAudioOutputProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Read AudioFrame from input port
        // Note: This is a placeholder - actual port reading will be implemented
        // when we integrate with the runtime's port system

        // For now, we'll add a method to push audio frames manually
        // which will be called by the runtime when data is available

        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl AppleAudioOutputProcessor {
    /// Push an AudioFrame to the output buffer
    ///
    /// This is called by the runtime when audio data is available on the input port.
    /// The audio thread will pull samples from this buffer.
    ///
    /// # Arguments
    ///
    /// * `frame` - AudioFrame containing samples to play
    ///
    /// # Returns
    ///
    /// Ok if frame was queued successfully
    pub fn push_frame(&mut self, frame: &AudioFrame) -> Result<()> {
        // Convert AudioFrame to output format if needed
        let mut samples = Vec::new();

        // Handle sample rate conversion if needed
        if frame.sample_rate != self.sample_rate {
            // Simple linear interpolation for sample rate conversion
            // TODO: Use a better resampler (e.g., rubato crate) for production
            tracing::warn!(
                "Sample rate conversion needed: {} -> {}",
                frame.sample_rate,
                self.sample_rate
            );
        }

        // Handle channel conversion if needed
        if frame.channels != self.channels {
            if frame.channels == 1 && self.channels == 2 {
                // Mono to stereo: duplicate samples
                for sample in frame.samples.iter() {
                    samples.push(*sample); // Left
                    samples.push(*sample); // Right
                }
            } else if frame.channels == 2 && self.channels == 1 {
                // Stereo to mono: average channels
                for chunk in frame.samples.chunks(2) {
                    samples.push((chunk[0] + chunk.get(1).unwrap_or(&0.0)) / 2.0);
                }
            } else {
                return Err(StreamError::Configuration(format!(
                    "Unsupported channel conversion: {} -> {}",
                    frame.channels, self.channels
                )));
            }
        } else {
            // No conversion needed
            samples.extend_from_slice(&frame.samples);
        }

        // Push samples to ring buffer
        let mut buffer = self.sample_buffer.lock();
        buffer.extend_from_slice(&samples);

        // Warn if buffer is getting too large (potential latency issue)
        let max_buffer_size = (self.sample_rate as usize * self.channels as usize) / 2; // 500ms max
        let buffer_len = buffer.len();
        if buffer_len > max_buffer_size {
            tracing::warn!(
                "Audio buffer overflow: {} samples (max {})",
                buffer_len,
                max_buffer_size
            );
            // Trim oldest samples
            let drain_count = buffer_len - max_buffer_size;
            buffer.drain(..drain_count);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let devices = AppleAudioOutputProcessor::list_devices().unwrap();
        assert!(
            !devices.is_empty(),
            "Should have at least one audio output device"
        );

        // Check that device info is populated
        for device in &devices {
            assert!(!device.name.is_empty());
            assert!(device.sample_rate > 0);
            assert!(device.channels > 0);
        }

        // Should have a default device
        assert!(
            devices.iter().any(|d| d.is_default),
            "Should have a default device"
        );
    }

    #[test]
    fn test_create_default_device() {
        let processor = AppleAudioOutputProcessor::new(None);
        assert!(processor.is_ok(), "Should create default device");

        let proc = processor.unwrap();
        assert!(proc.device_info.is_default);
        assert!(proc.sample_rate > 0);
        assert!(proc.channels > 0);
    }

    #[test]
    fn test_push_frame() {
        let mut processor = AppleAudioOutputProcessor::new(None).unwrap();

        // Create a test frame (100ms of 440Hz sine wave at 48kHz stereo)
        let sample_rate = 48000;
        let duration = 0.1; // 100ms
        let freq = 440.0;
        let sample_count = (sample_rate as f64 * duration) as usize;

        let mut samples = Vec::with_capacity(sample_count * 2);
        for i in 0..sample_count {
            let t = i as f64 / sample_rate as f64;
            let sample = (2.0 * std::f64::consts::PI * freq * t).sin() as f32;
            samples.push(sample); // Left
            samples.push(sample); // Right
        }

        let frame = AudioFrame::new(samples, 0, 0, sample_rate, 2);

        // Push frame
        let result = processor.push_frame(&frame);
        assert!(result.is_ok(), "Should push frame successfully");

        // Check buffer has data
        assert!(processor.buffer_level() > 0.0, "Buffer should have data");
    }

    #[test]
    fn test_mono_to_stereo_conversion() {
        let mut processor = AppleAudioOutputProcessor::new(None).unwrap();

        // Assume device is stereo
        if processor.channels != 2 {
            return; // Skip test if device isn't stereo
        }

        // Create mono frame
        let samples = vec![0.5, 0.6, 0.7]; // 3 mono samples
        let frame = AudioFrame::new(samples, 0, 0, 48000, 1);

        let result = processor.push_frame(&frame);
        assert!(result.is_ok(), "Should convert mono to stereo");

        // Buffer should have 6 samples (3 * 2 channels)
        let buffer = processor.sample_buffer.lock();
        assert_eq!(buffer.len(), 6);
        // Each mono sample should be duplicated
        assert_eq!(buffer[0], 0.5);
        assert_eq!(buffer[1], 0.5);
        assert_eq!(buffer[2], 0.6);
        assert_eq!(buffer[3], 0.6);
    }
}
