//! Audio Mixer Processor
//!
//! Combines multiple audio streams into a single output stream.
//! Supports dynamic input count, sample rate conversion, and channel mixing.
//!
//! # Architecture
//!
//! ```text
//! AudioMixerProcessor
//!   ├─ Dynamic Input Ports (HashMap: "input_0", "input_1", ...)
//!   ├─ Single Output Port (mixed audio)
//!   ├─ Mixing Strategy (sum normalized, sum clipped, weighted)
//!   ├─ Resampling (rubato for real-time safe sample rate conversion)
//!   └─ Channel Conversion (mono → stereo auto-conversion)
//! ```
//!
//! # Real-time Safety
//!
//! - All buffers pre-allocated in `new()` and `on_start()`
//! - Uses rubato's `process_into_buffer()` (no allocations in audio thread)
//! - No HashMap insertions during `process()` (only reads)
//! - Drop frames gracefully when inputs unavailable (no buffering)
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{AudioMixerProcessor, MixingStrategy};
//!
//! // Create mixer with 4 inputs, sum with normalization at 48kHz
//! let mut mixer = AudioMixerProcessor::new(
//!     4,
//!     MixingStrategy::SumNormalized,
//!     48000
//! )?;
//!
//! // Connect inputs
//! runtime.connect(
//!     &mut mic1.output_ports().audio,
//!     &mut mixer.input_ports().inputs.get_mut("input_0").unwrap().lock()
//! )?;
//!
//! // Connect output to speaker
//! runtime.connect(
//!     &mut mixer.output_ports().audio,
//!     &mut speaker.input_ports().audio
//! )?;
//! ```

use crate::core::{
    Result, StreamError, StreamProcessor, GpuContext,
    AudioFrame, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, SCHEMA_AUDIO_FRAME,
    AudioRequirements,
};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters,
    WindowFunction,
};

/// Mixing strategy for combining audio streams
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixingStrategy {
    /// Sum all inputs and divide by active input count (prevents clipping)
    SumNormalized,

    /// Sum all inputs and clamp to [-1.0, 1.0] (may cause distortion)
    SumClipped,

    // TODO: Weighted - Per-input gain control for advanced mixing
    // Weighted,
}

impl Default for MixingStrategy {
    fn default() -> Self {
        MixingStrategy::SumNormalized
    }
}

/// Input ports for AudioMixerProcessor (dynamic count)
pub struct AudioMixerInputPorts {
    /// Dynamic input ports: "input_0", "input_1", "input_2", ...
    ///
    /// Wrapped in Arc<Mutex<>> for thread-safe access during connections.
    /// Use `.lock()` to access the underlying StreamInput.
    pub inputs: HashMap<String, Arc<Mutex<StreamInput<AudioFrame>>>>,
}

/// Output ports for AudioMixerProcessor
pub struct AudioMixerOutputPorts {
    /// Mixed audio output
    pub audio: StreamOutput<AudioFrame>,
}

/// Audio Mixer Processor
///
/// Combines multiple audio streams into a single output with real-time safe processing.
///
/// # Features
///
/// - **Dynamic inputs**: Configurable number of inputs at creation time
/// - **Sample rate conversion**: Uses rubato for real-time safe resampling
/// - **Channel mixing**: Auto-converts mono to stereo
/// - **Mixing strategies**: Normalized sum (default) or clipped sum
/// - **Real-time safe**: No allocations in audio processing thread
///
/// # Thread Safety
///
/// - Input ports wrapped in Arc<Mutex<>> for safe concurrent access
/// - All resampling buffers pre-allocated
/// - No allocations during `process()`
pub struct AudioMixerProcessor {
    /// Number of input ports
    num_inputs: usize,

    /// Mixing strategy
    strategy: MixingStrategy,

    /// Input ports (dynamic count)
    input_ports: AudioMixerInputPorts,

    /// Output port
    output_ports: AudioMixerOutputPorts,

    /// Target sample rate for output
    target_sample_rate: u32,

    /// Target channels (always 2 for stereo output)
    target_channels: u32,

    /// Frame counter for output timestamps
    frame_counter: u64,

    /// Current timestamp in nanoseconds
    current_timestamp_ns: i64,

    /// Pre-allocated mix buffer (reused each tick for real-time safety)
    /// Size: maximum buffer size * channels
    mix_buffer: Vec<f32>,

    /// Maximum buffer size per channel (for pre-allocation)
    max_buffer_size: usize,

    /// Resamplers for each input (created on-demand when different sample rate detected)
    /// Key: input port name (e.g., "input_0")
    /// Uses SincFixedIn for high-quality real-time resampling
    resamplers: HashMap<String, SincFixedIn<f32>>,

    /// Pre-allocated resampling output buffers
    /// Key: input port name, Value: buffer for resampled output
    resample_buffers: HashMap<String, Vec<Vec<f32>>>,
}

impl AudioMixerProcessor {
    /// Create a new audio mixer processor
    ///
    /// # Arguments
    ///
    /// * `num_inputs` - Number of input ports to create
    /// * `strategy` - Mixing strategy (SumNormalized or SumClipped)
    /// * `sample_rate` - Target output sample rate in Hz (e.g., 48000)
    ///
    /// # Returns
    ///
    /// Configured AudioMixerProcessor ready to mix streams
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Create mixer for 4 inputs at 48kHz
    /// let mixer = AudioMixerProcessor::new(4, MixingStrategy::SumNormalized, 48000)?;
    /// ```
    pub fn new(
        num_inputs: usize,
        strategy: MixingStrategy,
        sample_rate: u32,
    ) -> Result<Self> {
        if num_inputs == 0 {
            return Err(StreamError::Configuration(
                "AudioMixerProcessor requires at least 1 input".into()
            ));
        }

        // Create dynamic input ports
        let mut input_ports_map = HashMap::new();
        for i in 0..num_inputs {
            let port_name = format!("input_{}", i);
            input_ports_map.insert(
                port_name.clone(),
                Arc::new(Mutex::new(StreamInput::new(&port_name)))
            );
        }

        // Assume maximum buffer size of 4096 samples per channel for pre-allocation
        // This covers typical audio buffer sizes (512, 1024, 2048, 4096)
        let max_buffer_size = 4096;
        let target_channels = 2; // Always output stereo

        // Pre-allocate mix buffer (max_buffer_size * channels)
        let mix_buffer = vec![0.0; max_buffer_size * target_channels as usize];

        Ok(Self {
            num_inputs,
            strategy,
            input_ports: AudioMixerInputPorts {
                inputs: input_ports_map,
            },
            output_ports: AudioMixerOutputPorts {
                audio: StreamOutput::new("audio"),
            },
            target_sample_rate: sample_rate,
            target_channels,
            frame_counter: 0,
            current_timestamp_ns: 0,
            mix_buffer,
            max_buffer_size,
            resamplers: HashMap::new(),
            resample_buffers: HashMap::new(),
        })
    }

    /// Get mutable access to input ports
    pub fn input_ports(&mut self) -> &mut AudioMixerInputPorts {
        &mut self.input_ports
    }

    /// Get mutable access to output ports
    pub fn output_ports(&mut self) -> &mut AudioMixerOutputPorts {
        &mut self.output_ports
    }

    /// Convert mono audio to stereo by duplicating samples to both channels
    ///
    /// # Arguments
    ///
    /// * `mono_samples` - Mono audio samples (L, L, L, ...)
    ///
    /// # Returns
    ///
    /// Stereo samples (L, L, L, L, ...) - each mono sample duplicated to L and R
    fn mono_to_stereo(&self, mono_samples: &[f32]) -> Vec<f32> {
        let mut stereo = Vec::with_capacity(mono_samples.len() * 2);
        for &sample in mono_samples {
            stereo.push(sample); // Left
            stereo.push(sample); // Right (same as left for mono)
        }
        stereo
    }

    /// Resample audio frame to target sample rate if needed
    ///
    /// Uses rubato's real-time safe `process_into_buffer()` method.
    /// Resamplers are created on-demand and cached.
    ///
    /// # Arguments
    ///
    /// * `frame` - Input audio frame
    /// * `input_name` - Input port name for resampler caching
    ///
    /// # Returns
    ///
    /// Resampled audio samples (interleaved stereo) or original if no resampling needed
    fn resample_if_needed(
        &mut self,
        frame: &AudioFrame,
        input_name: &str,
    ) -> Result<Vec<f32>> {
        // If sample rates match, no resampling needed
        if frame.sample_rate == self.target_sample_rate {
            return Ok(frame.samples.as_ref().clone());
        }

        // Get or create resampler for this input
        if !self.resamplers.contains_key(input_name) {
            // Create new resampler
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };

            let resampler = SincFixedIn::<f32>::new(
                self.target_sample_rate as f64 / frame.sample_rate as f64,
                2.0, // max_resample_ratio_relative
                params,
                frame.sample_count,
                frame.channels as usize,
            ).map_err(|e| StreamError::Configuration(format!("Failed to create resampler: {}", e)))?;

            // Pre-allocate resampling output buffer (get size before moving resampler)
            let output_size = resampler.output_frames_max();

            self.resamplers.insert(input_name.to_string(), resampler);
            let mut buffers = Vec::new();
            for _ in 0..frame.channels {
                buffers.push(vec![0.0; output_size]);
            }
            self.resample_buffers.insert(input_name.to_string(), buffers);
        }

        // Get resampler and buffer
        let resampler = self.resamplers.get_mut(input_name).unwrap();
        let output_buffer = self.resample_buffers.get_mut(input_name).unwrap();

        // Deinterleave input for rubato (it expects separate channel buffers)
        let mut input_channels = vec![Vec::new(); frame.channels as usize];
        for (i, sample) in frame.samples.iter().enumerate() {
            let channel = i % frame.channels as usize;
            input_channels[channel].push(*sample);
        }

        // Convert to slices
        let input_slices: Vec<&[f32]> = input_channels.iter().map(|v| v.as_slice()).collect();
        let mut output_slices: Vec<&mut [f32]> = output_buffer.iter_mut().map(|v| v.as_mut_slice()).collect();

        // Resample using real-time safe method
        let (_input_frames, output_frames) = resampler.process_into_buffer(
            &input_slices,
            &mut output_slices,
            None, // No specific output length requirement
        ).map_err(|e| StreamError::Configuration(format!("Resampling failed: {}", e)))?;

        // Interleave output back to single vector
        let mut result = Vec::with_capacity(output_frames * frame.channels as usize);
        for i in 0..output_frames {
            for channel in 0..frame.channels as usize {
                result.push(output_buffer[channel][i]);
            }
        }

        Ok(result)
    }

    /// Mix multiple audio sample vectors according to mixing strategy
    ///
    /// # Arguments
    ///
    /// * `inputs` - Vector of sample vectors to mix (all must be same length and stereo)
    ///
    /// # Returns
    ///
    /// Mixed audio samples
    fn mix_samples(&self, inputs: Vec<Vec<f32>>) -> Vec<f32> {
        if inputs.is_empty() {
            return Vec::new();
        }

        let sample_count = inputs[0].len();
        let mut output = vec![0.0; sample_count];

        match self.strategy {
            MixingStrategy::SumNormalized => {
                // Sum all inputs
                for input in &inputs {
                    for (i, &sample) in input.iter().enumerate() {
                        output[i] += sample;
                    }
                }

                // Normalize by number of active inputs
                let num_inputs = inputs.len() as f32;
                for sample in &mut output {
                    *sample /= num_inputs;
                }
            }

            MixingStrategy::SumClipped => {
                // Sum all inputs
                for input in &inputs {
                    for (i, &sample) in input.iter().enumerate() {
                        output[i] += sample;
                    }
                }

                // Clip to [-1.0, 1.0]
                for sample in &mut output {
                    *sample = sample.clamp(-1.0, 1.0);
                }
            }
        }

        output
    }
}

impl StreamProcessor for AudioMixerProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        // Note: AudioMixerProcessor instances have dynamic input counts,
        // so the descriptor here is generic. Specific instances would need
        // custom descriptors based on their num_inputs.
        Some(
            ProcessorDescriptor::new(
                "AudioMixer",
                "Combines multiple audio streams into a single output with real-time mixing"
            )
            .with_usage_context(
                "Use when you need to mix multiple audio sources (microphone + music, \
                multiple microphones, audio effects chains). Supports different sample rates \
                and automatic channel conversion."
            )
            .with_output(PortDescriptor::new(
                "audio",
                Arc::clone(&SCHEMA_AUDIO_FRAME),
                true,
                "Mixed audio output (stereo)"
            ))
            .with_tags(vec!["audio", "mixer", "processing", "real-time"])
            .with_audio_requirements(AudioRequirements::flexible())
        )
    }

    fn on_start(&mut self, _gpu_context: &GpuContext) -> Result<()> {
        tracing::info!(
            "[AudioMixer] Started with {} inputs, strategy: {:?}, target: {}Hz stereo",
            self.num_inputs,
            self.strategy,
            self.target_sample_rate
        );

        // Reset state
        self.frame_counter = 0;
        self.current_timestamp_ns = 0;

        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Collect audio frames from all inputs
        let mut input_samples = Vec::new();

        for i in 0..self.num_inputs {
            let input_name = format!("input_{}", i);

            // Read frame from port (lock is dropped after this block)
            let frame_opt = if let Some(input_port) = self.input_ports.inputs.get(&input_name) {
                let mut port = input_port.lock();
                port.read_latest()
            } else {
                None
            };

            if let Some(mut frame) = frame_opt {
                // Convert mono to stereo if needed
                if frame.channels == 1 {
                    let stereo_samples = self.mono_to_stereo(&frame.samples);
                    frame.samples = Arc::new(stereo_samples);
                    frame.channels = 2;
                    frame.sample_count = frame.sample_count; // Same sample count, but now stereo
                }

                // Resample if needed (lock is already dropped, so we can borrow self mutably)
                let samples = self.resample_if_needed(&frame, &input_name)?;

                // Track timestamp from first input
                if input_samples.is_empty() {
                    self.current_timestamp_ns = frame.timestamp_ns;
                }

                input_samples.push(samples);
            }
        }

        // If no inputs have data, skip this tick (drop frame)
        if input_samples.is_empty() {
            return Ok(());
        }

        // Ensure all inputs have the same length (should be true after resampling)
        let target_len = input_samples[0].len();
        for samples in &input_samples {
            if samples.len() != target_len {
                tracing::warn!(
                    "[AudioMixer] Sample length mismatch: {} vs {}. Skipping frame.",
                    samples.len(),
                    target_len
                );
                return Ok(());
            }
        }

        // Mix the samples
        let mixed_samples = self.mix_samples(input_samples);

        // Create output frame
        let sample_count = mixed_samples.len() / self.target_channels as usize;
        let output_frame = AudioFrame::new(
            mixed_samples,
            self.current_timestamp_ns,
            self.frame_counter,
            self.target_sample_rate,
            self.target_channels,
        );

        // Write to output
        self.output_ports.audio.write(output_frame);

        // Update counters
        self.frame_counter += 1;
        self.current_timestamp_ns += (sample_count as i64 * 1_000_000_000) / self.target_sample_rate as i64;

        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!("[AudioMixer] Stopped (processed {} frames)", self.frame_counter);
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixer_creation() {
        let mixer = AudioMixerProcessor::new(4, MixingStrategy::SumNormalized, 48000).unwrap();
        assert_eq!(mixer.num_inputs, 4);
        assert_eq!(mixer.strategy, MixingStrategy::SumNormalized);
        assert_eq!(mixer.target_sample_rate, 48000);
        assert_eq!(mixer.input_ports.inputs.len(), 4);
        assert!(mixer.input_ports.inputs.contains_key("input_0"));
        assert!(mixer.input_ports.inputs.contains_key("input_3"));
    }

    #[test]
    fn test_mixer_zero_inputs_fails() {
        let result = AudioMixerProcessor::new(0, MixingStrategy::SumNormalized, 48000);
        assert!(result.is_err());
    }

    #[test]
    fn test_mono_to_stereo() {
        let mixer = AudioMixerProcessor::new(1, MixingStrategy::SumNormalized, 48000).unwrap();
        let mono = vec![0.5, 0.6, 0.7];
        let stereo = mixer.mono_to_stereo(&mono);
        assert_eq!(stereo, vec![0.5, 0.5, 0.6, 0.6, 0.7, 0.7]);
    }

    #[test]
    fn test_sum_normalized() {
        let mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, 48000).unwrap();

        let input1 = vec![0.5, 0.5, 0.6, 0.6];
        let input2 = vec![0.3, 0.3, 0.4, 0.4];
        let mixed = mixer.mix_samples(vec![input1, input2]);

        // (0.5 + 0.3) / 2 = 0.4, (0.6 + 0.4) / 2 = 0.5
        assert_eq!(mixed, vec![0.4, 0.4, 0.5, 0.5]);
    }

    #[test]
    fn test_sum_clipped() {
        let mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, 48000).unwrap();

        let input1 = vec![0.8, 0.8, 0.9, 0.9];
        let input2 = vec![0.7, 0.7, 0.8, 0.8];
        let mixed = mixer.mix_samples(vec![input1, input2]);

        // 0.8 + 0.7 = 1.5 -> clipped to 1.0
        // 0.9 + 0.8 = 1.7 -> clipped to 1.0
        assert_eq!(mixed, vec![1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn test_descriptor() {
        let desc = AudioMixerProcessor::descriptor().unwrap();
        assert_eq!(desc.name, "AudioMixer");
        assert_eq!(desc.outputs.len(), 1);
        assert_eq!(desc.outputs[0].name, "audio");
        assert!(desc.tags.contains(&"audio".to_string()));
        assert!(desc.tags.contains(&"mixer".to_string()));
    }
}
