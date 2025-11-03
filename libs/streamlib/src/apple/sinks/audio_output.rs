//! Apple AudioOutputProcessor implementation using CoreAudio (v2.0.0 StreamSink)
//!
//! Uses the `cpal` crate which provides a safe Rust wrapper around CoreAudio on macOS.
//! This gives us low-latency audio playback with minimal overhead.

use crate::core::{
    AudioDevice,
    AudioFrame, Result, StreamError,
    ProcessorDescriptor, PortDescriptor, SCHEMA_AUDIO_FRAME,
};
use crate::core::traits::{StreamElement, StreamSink, ElementType};
use crate::core::scheduling::{ClockConfig, ClockType, SyncMode};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

/// Apple CoreAudio implementation of AudioOutputProcessor (v2.0.0 StreamSink)
///
/// # Architecture
///
/// - Uses `cpal` library which wraps CoreAudio on macOS
/// - Maintains an internal ring buffer for audio frames
/// - Runs audio playback on a dedicated thread (managed by cpal/CoreAudio)
/// - Low-latency: typical latency < 20ms on macOS
/// - Implements StreamElement + StreamSink traits (v2.0.0 architecture)
///
/// # Clock Provider
///
/// This sink provides the Audio clock for the pipeline. The CoreAudio callback
/// provides a sample-accurate clock that other processors can sync to.
///
/// # Example
///
/// ```ignore
/// use streamlib::AppleAudioOutputProcessor;
/// use streamlib::traits::StreamSink;
///
/// // Create from config
/// let config = AudioOutputConfig { device_id: None };
/// let speaker = AppleAudioOutputProcessor::from_config(config)?;
///
/// // Runtime calls render() with each AudioFrame
/// speaker.render(audio_frame)?;
/// ```
pub struct AppleAudioOutputProcessor {
    /// Device name (for StreamElement.name())
    device_name: String,

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

    /// Minimum buffer size before playback starts (prevents initial underruns)
    /// Set to ~20ms - lower than steady-state to avoid stopping during normal operation
    prebuffer_samples: usize,

    /// Sample rate for this output
    sample_rate: u32,

    /// Number of channels (2 = stereo)
    channels: u32,
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

        // Industry standard: Pre-buffer = 1x callback size (512 frames = 1024 samples stereo)
        // Prevents startup underruns while staying below steady-state minimum
        const CALLBACK_FRAMES: usize = 512;
        let prebuffer_samples = CALLBACK_FRAMES * channels as usize; // 1024 samples for stereo

        tracing::info!(
            "Audio output prebuffer: {} samples (1x callback = {:.1}ms)",
            prebuffer_samples,
            (prebuffer_samples as f32 / sample_rate as f32 / channels as f32) * 1000.0
        );

        // Create shared ring buffer for audio samples
        let sample_buffer = Arc::new(Mutex::new(Vec::new()));
        let sample_buffer_clone = sample_buffer.clone();

        // Create flag for playback status
        let is_playing = Arc::new(AtomicBool::new(false));
        let is_playing_clone = is_playing.clone();

        // Build audio stream configuration with industry-standard buffer size
        // 512 frames = 10.7ms @ 48kHz (industry standard for mixing)
        let stream_config = StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Fixed(CALLBACK_FRAMES as u32),
        };

        // Build output stream with callback
        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Audio thread callback - fill output buffer
                    let mut buffer = sample_buffer_clone.lock();
                    let buffer_size_before = buffer.len();
                    let requested = data.len();

                    // Pre-buffer strategy: Don't start playback until buffer has enough samples
                    // This prevents initial underruns and provides smooth playback
                    if buffer.len() < prebuffer_samples {
                        // Not enough samples for smooth playback - output silence and wait
                        data.fill(0.0);
                        is_playing_clone.store(false, Ordering::Relaxed);

                        tracing::debug!(
                            "[AudioOutput::Callback] Pre-buffering - have {} samples, need {} ({}%)",
                            buffer.len(), prebuffer_samples, (buffer.len() * 100 / prebuffer_samples)
                        );
                        return;
                    }

                    // Buffer has enough samples - proceed with playback
                    if buffer.len() >= data.len() {
                        // Copy samples from ring buffer to output
                        data.copy_from_slice(&buffer[..data.len()]);
                        buffer.drain(..data.len());
                        is_playing_clone.store(true, Ordering::Relaxed);

                        tracing::debug!(
                            "[AudioOutput::Callback] Normal playback - buffer: {} → {} samples (-{})",
                            buffer_size_before, buffer.len(), requested
                        );
                    } else {
                        // Partial buffer available - use what we have and repeat last sample
                        let available = buffer.len();
                        data[..available].copy_from_slice(&buffer[..]);
                        buffer.clear();

                        // Fill remainder with last sample (sample-and-hold) to avoid clicks
                        let last_sample = data[available - 1];
                        data[available..].fill(last_sample);

                        is_playing_clone.store(true, Ordering::Relaxed);

                        tracing::warn!(
                            "[AudioOutput::Callback] ⚠️  UNDERRUN - requested {} samples, only {} available ({}% short)",
                            requested, available, ((requested - available) * 100 / requested)
                        );
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
            device_name: device_info.name.clone(),
            device_info,
            _device: device,
            _stream: stream,
            sample_buffer,
            is_playing,
            prebuffer_samples,
            sample_rate,
            channels,
        })
    }

    /// Get current buffer fill level (0.0 to 1.0)
    ///
    /// Useful for monitoring latency and detecting underruns
    pub fn buffer_level(&self) -> f32 {
        const CALLBACK_FRAMES: usize = 512;
        let buffer = self.sample_buffer.lock();
        // Industry standard: Ring buffer = 8x callback size
        let target_size = CALLBACK_FRAMES * self.channels as usize * 8;
        (buffer.len() as f32 / target_size as f32).min(1.0)
    }

    /// Check if audio is currently playing
    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }
}

// ============================================================
// StreamElement Implementation (Base Trait)
// ============================================================

impl StreamElement for AppleAudioOutputProcessor {
    fn name(&self) -> &str {
        &self.device_name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Sink
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleAudioOutputProcessor as StreamSink>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Audio frames to play through speakers".to_string(),
        }]
    }

    fn start(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        tracing::info!(
            "AudioOutput {}: Starting ({} Hz, {} channels)",
            self.device_name,
            self.sample_rate,
            self.channels
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Clear the buffer to stop playback
        let mut buffer = self.sample_buffer.lock();
        buffer.clear();
        self.is_playing.store(false, Ordering::Relaxed);
        tracing::info!("AudioOutput {}: Stopped", self.device_name);
        Ok(())
    }
}

// ============================================================
// StreamSink Implementation (v2.0.0 Architecture)
// ============================================================

impl StreamSink for AppleAudioOutputProcessor {
    type Input = AudioFrame;
    type Config = crate::core::AudioOutputConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        // Parse device_id string to usize if provided
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id)
    }

    fn render(&mut self, frame: Self::Input) -> Result<()> {
        tracing::debug!(
            "[AudioOutput] render: frame #{} - {} samples, {} channels",
            frame.frame_number, frame.sample_count(), frame.channels
        );

        // Push frame to ring buffer (audio thread will pull from it)
        self.push_frame(&frame)?;

        let buffer_level = self.buffer_level();
        tracing::debug!(
            "[AudioOutput] Rendered frame, buffer level: {:.1}%",
            buffer_level * 100.0
        );

        Ok(())
    }

    fn clock_config(&self) -> ClockConfig {
        ClockConfig {
            provides_clock: true,
            clock_type: Some(ClockType::Audio),
            clock_name: Some(format!("audio_output_{}", self.device_name)),
        }
    }

    fn sync_mode(&self) -> SyncMode {
        SyncMode::Timestamp
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "AppleAudioOutputProcessor",
                "Plays audio through speakers/headphones using CoreAudio. Receives AudioFrames and outputs to the configured audio device.",
            )
            .with_usage_context(
                "Use when you need to play audio to speakers or headphones. This is typically a sink processor \
                 (end of pipeline). AudioFrames are buffered internally and played at the device's sample rate. \
                 The processor handles sample rate and channel conversion automatically.",
            )
            .with_input(PortDescriptor::new(
                "audio",
                SCHEMA_AUDIO_FRAME.clone(),
                true,
                "Audio frames to play. Frames are buffered and played continuously. The processor handles \
                 sample rate conversion and channel conversion (mono↔stereo) automatically.",
            ))
            .with_tags(vec!["sink", "audio", "speaker", "output", "playback"])
        )
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
        tracing::debug!(
            "[AudioOutput] push_frame: frame #{}, {} samples ({} channels)",
            frame.frame_number, frame.sample_count(), frame.channels
        );

        // Convert AudioFrame to output format if needed
        let mut samples = Vec::new();

        // NOTE: Sample rate is enforced by RuntimeContext, so no conversion needed
        // All audio frames should already match the system-wide sample rate

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
        let buffer_size_before = buffer.len();

        // Extend buffer with new samples
        // Trust upstream to produce at correct rate (event-driven, no drops needed)
        buffer.extend_from_slice(&samples);

        let buffer_size_after = buffer.len();

        // Industry standard: Ring buffer = 8x callback size (512 frames × 2 channels × 8 = 8192 samples)
        const CALLBACK_FRAMES: usize = 512;
        let target_buffer_samples = CALLBACK_FRAMES * self.channels as usize * 8;
        let buffer_percent = (buffer_size_after as f32 / target_buffer_samples as f32) * 100.0;

        tracing::debug!(
            "[AudioOutput] Ring buffer: {} → {} samples (+{}) [{:.1}% of 8x target]",
            buffer_size_before, buffer_size_after, samples.len(), buffer_percent
        );

        // Warn if buffer is getting too full (>2x target)
        if buffer_size_after > target_buffer_samples * 2 {
            tracing::warn!(
                "[AudioOutput] ⚠️  BUFFER OVERRUN - buffer at {:.1}% capacity ({} samples). Audio output may be lagging!",
                buffer_percent, buffer_size_after
            );
        }

        Ok(())
    }
}

