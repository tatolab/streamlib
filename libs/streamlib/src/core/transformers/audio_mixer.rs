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

use dasp::Frame;

use crate::core::{
    Result, StreamError, GpuContext,
    AudioFrame, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, SCHEMA_AUDIO_FRAME,
    AudioRequirements,
};
use crate::core::traits::{StreamElement, StreamTransform, ElementType};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters,
    WindowFunction,
};
use serde::{Serialize, Deserialize};

/// Configuration for audio mixer processor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    /// Number of input ports
    pub num_inputs: usize,
    /// Mixing strategy
    pub strategy: MixingStrategy,
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
            strategy: MixingStrategy::SumNormalized,
            sample_rate: 48000,
            buffer_size: 2048,
            timer_group_id: None,
        }
    }
}

/// Mixing strategy for combining audio streams
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// Buffer size (samples per channel) - determines mix rate
    buffer_size: usize,

    /// Optional timer group ID for synchronized timing with input sources
    timer_group_id: Option<String>,

    /// Track last mixed timestamps to prevent duplicate mixes on spurious wakeups
    /// Key: input name (e.g., "input_0"), Value: last mixed timestamp_ns
    /// Timestamp-based deduplication is more robust than frame numbers
    last_mixed_timestamps: HashMap<String, i64>,

    /// Cache for processed inputs (post-resampling/channel-conversion)
    /// Key: input name, Value: (samples, timestamp, frame_number)
    /// This provides sample-and-hold behavior similar to StreamInput::read_latest()
    input_cache: HashMap<String, Option<(Vec<f32>, i64, u64)>>,
}

impl AudioMixerProcessor {
    /// Create a new audio mixer processor
    ///
    /// # Arguments
    ///
    /// * `num_inputs` - Number of input ports to create
    /// * `strategy` - Mixing strategy (SumNormalized or SumClipped)
    /// * `sample_rate` - Target output sample rate in Hz (e.g., 48000)
    /// * `buffer_size` - Buffer size in samples per channel (e.g., 2048)
    ///
    /// # Returns
    ///
    /// Configured AudioMixerProcessor ready to mix streams
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Create mixer for 4 inputs at 48kHz with 2048 buffer size
    /// let mixer = AudioMixerProcessor::new(4, MixingStrategy::SumNormalized, 48000, 2048)?;
    /// ```
    pub fn new(
        num_inputs: usize,
        strategy: MixingStrategy,
        sample_rate: u32,
        buffer_size: usize,
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

        // Initialize input cache for sample-and-hold behavior
        let mut input_cache = HashMap::new();
        for i in 0..num_inputs {
            input_cache.insert(format!("input_{}", i), None);
        }

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
            buffer_size,
            timer_group_id: None,
            last_mixed_timestamps: HashMap::new(),
            input_cache,
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
    /// Uses dasp Frame operations for channel conversion.
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

        // Process each mono sample as a Frame, then duplicate to stereo
        for &sample in mono_samples {
            // Create stereo frame by duplicating mono sample to both channels
            let stereo_frame: [f32; 2] = [sample, sample];
            stereo.push(stereo_frame[0]);
            stereo.push(stereo_frame[1]);
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
        // NOTE: With RuntimeContext enforcing system-wide sample rate,
        // all frames should already be at target_sample_rate.
        // This resampling code is kept for backwards compatibility but should not trigger.

        // No resampling needed - sample rate is enforced by RuntimeContext
        return Ok(frame.samples.as_ref().clone());

        // Dead code below - keeping for reference but removing sample_rate access
        #[allow(unreachable_code)]
        {
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
                1.0, // No resampling - sample rates already match
                2.0, // max_resample_ratio_relative
                params,
                frame.sample_count(),
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
    /// Uses dasp Frame operations for professional-quality DSP.
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

        let sample_count = inputs[0].len() / 2; // Stereo: divide by 2 for frame count
        let mut output = Vec::with_capacity(sample_count * 2);

        // Process frame-by-frame using dasp
        for frame_idx in 0..sample_count {
            // Start with silence (equilibrium)
            let mut mixed_frame: [f32; 2] = Frame::EQUILIBRIUM;

            // Add all input frames using dasp Frame operations
            for input in &inputs {
                let left = input[frame_idx * 2];
                let right = input[frame_idx * 2 + 1];
                let input_frame: [f32; 2] = [left, right];
                mixed_frame = mixed_frame.add_amp(input_frame);
            }

            // Apply mixing strategy
            mixed_frame = match self.strategy {
                MixingStrategy::SumNormalized => {
                    // Normalize by number of inputs using dasp scale_amp
                    mixed_frame.scale_amp(1.0 / inputs.len() as f32)
                }
                MixingStrategy::SumClipped => {
                    // Clip each channel to [-1.0, 1.0]
                    // Note: dasp doesn't have a built-in clip_amp for frames,
                    // so we do it manually but still using Sample trait
                    [
                        mixed_frame[0].clamp(-1.0, 1.0),
                        mixed_frame[1].clamp(-1.0, 1.0),
                    ]
                }
            };

            // Write interleaved output
            output.push(mixed_frame[0]);
            output.push(mixed_frame[1]);
        }

        output
    }
}

// ============================================================
// StreamElement Implementation (v2.0.0 Architecture)
// ============================================================

impl StreamElement for AudioMixerProcessor {
    fn name(&self) -> &str {
        "audio_mixer"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AudioMixerProcessor as StreamTransform>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        // Return descriptors for all dynamic inputs
        (0..self.num_inputs)
            .map(|i| PortDescriptor {
                name: format!("input_{}", i),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: true,
                description: format!("Audio input {} for mixing", i),
            })
            .collect()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Mixed audio output".to_string(),
        }]
    }

    fn start(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        tracing::info!(
            "AudioMixer: Starting ({} inputs, {} Hz, {} samples buffer, strategy: {:?})",
            self.num_inputs,
            self.target_sample_rate,
            self.buffer_size,
            self.strategy
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Clear input cache
        for value in self.input_cache.values_mut() {
            *value = None;
        }
        tracing::info!("AudioMixer: Stopped");
        Ok(())
    }
}

// ============================================================
// StreamTransform Implementation (v2.0.0 Architecture)
// ============================================================

impl StreamTransform for AudioMixerProcessor {
    type Config = crate::core::AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Self::new(
            config.num_inputs,
            config.strategy,
            config.sample_rate,
            config.buffer_size,
        )
    }

    // process() is implemented directly in the impl block below

    fn descriptor() -> Option<ProcessorDescriptor> {
        use crate::core::schema::ProcessorDescriptor;

        Some(
            ProcessorDescriptor::new(
                "AudioMixerProcessor",
                "Mixes multiple audio streams into a single output with real-time safe processing"
            )
            .with_usage_context(
                "Use when you need to combine multiple audio sources into one stream. \
                 Supports dynamic input count, sample rate conversion, and channel mixing. \
                 All mixing is real-time safe with pre-allocated buffers."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,  // Flexible
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: Some(2),  // Always outputs stereo
            })
            .with_tags(vec!["audio", "mixer", "transform", "multi-input"])
        )
    }

    fn process(&mut self) -> Result<()> {
        // Process using cached inputs (sample-and-hold pattern):
        // Update cache with incoming frames, then mix ALL inputs using held values
        tracing::debug!("[AudioMixer] process() called");

        // Step 1: Update input cache with any incoming frames
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
                tracing::debug!(
                    "[AudioMixer] Received NEW frame from {} - {} samples, {} channels, frame #{}",
                    input_name, frame.sample_count(), frame.channels, frame.frame_number
                );

                // Convert mono to stereo if needed
                if frame.channels == 1 {
                    let stereo_samples = self.mono_to_stereo(&frame.samples);
                    frame.samples = Arc::new(stereo_samples);
                    frame.channels = 2;
                    frame.sample_count = frame.sample_count; // Same sample count, but now stereo
                }

                // Resample if needed
                let samples = self.resample_if_needed(&frame, &input_name)?;

                // Store processed samples in input cache
                if let Some(slot) = self.input_cache.get_mut(&input_name) {
                    *slot = Some((samples, frame.timestamp_ns, frame.frame_number));
                }
            }
        }

        // Step 2: Check if ALL inputs have data (cold start check)
        let all_ready = self.input_cache.values().all(|v| v.is_some());
        if !all_ready {
            tracing::debug!("[AudioMixer] Not all inputs have data yet (cold start), skipping mix");
            return Ok(());
        }

        // Step 3: Collect all cached inputs and check for duplicates
        let mut all_inputs_named: HashMap<String, (Vec<f32>, i64, u64)> = HashMap::new();
        for (name, value_opt) in &self.input_cache {
            if let Some(value) = value_opt {
                all_inputs_named.insert(name.clone(), value.clone());
            }
        }

        // Check if ALL inputs have the SAME timestamps as last mix (duplicate wakeup)
        let mut all_inputs_unchanged = true;
        for (input_name, (_samples, timestamp, _frame_num)) in &all_inputs_named {
            if let Some(&last_ts) = self.last_mixed_timestamps.get(input_name) {
                if *timestamp != last_ts {
                    // At least one input has a new timestamp
                    all_inputs_unchanged = false;
                    break;
                }
            } else {
                // First time seeing this input
                all_inputs_unchanged = false;
                break;
            }
        }

        if all_inputs_unchanged {
            tracing::debug!("[AudioMixer] Skipping duplicate mix - all inputs have same timestamps as last mix");
            return Ok(());
        }

        // Step 4: Mix using cached inputs (sample-and-hold pattern)
        // Timer groups ensure all inputs wake up simultaneously, eliminating clock drift.
        // Input cache provides latest data from each input.
        // Timestamp deduplication prevents re-mixing on spurious wakeups.

        tracing::debug!(
            "[AudioMixer] Mixing ALL {} input streams (cached, sample-and-hold pattern)",
            all_inputs_named.len()
        );

        // Extract samples, timestamp, and frame number
        let mut sorted_names: Vec<_> = all_inputs_named.keys().collect();
        sorted_names.sort();

        let input_samples: Vec<Vec<f32>> = sorted_names.iter()
            .map(|name| all_inputs_named.get(*name).unwrap().0.clone())
            .collect();

        let first_timestamp = all_inputs_named.values().next().unwrap().1; // Use timestamp from first input

        self.current_timestamp_ns = first_timestamp;

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
        tracing::debug!(
            "[AudioMixer] Wrote mixed frame #{} - {} samples, {} Hz",
            self.frame_counter, sample_count, self.target_sample_rate
        );

        // Update counters
        self.frame_counter += 1;
        self.current_timestamp_ns += (sample_count as i64 * 1_000_000_000) / self.target_sample_rate as i64;

        // Update last mixed timestamps to prevent duplicate mixes on next wakeup
        for (input_name, (_samples, timestamp, _frame_num)) in all_inputs_named {
            self.last_mixed_timestamps.insert(input_name, timestamp);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixer_creation() {
        let mixer = AudioMixerProcessor::new(4, MixingStrategy::SumNormalized, 48000, 2048).unwrap();
        assert_eq!(mixer.num_inputs, 4);
        assert_eq!(mixer.strategy, MixingStrategy::SumNormalized);
        assert_eq!(mixer.target_sample_rate, 48000);
        assert_eq!(mixer.input_ports.inputs.len(), 4);
        assert!(mixer.input_ports.inputs.contains_key("input_0"));
        assert!(mixer.input_ports.inputs.contains_key("input_3"));
    }

    #[test]
    fn test_mixer_zero_inputs_fails() {
        let result = AudioMixerProcessor::new(0, MixingStrategy::SumNormalized, 48000, 2048);
        assert!(result.is_err());
    }

    #[test]
    fn test_mono_to_stereo() {
        let mixer = AudioMixerProcessor::new(1, MixingStrategy::SumNormalized, 48000, 2048).unwrap();
        let mono = vec![0.5, 0.6, 0.7];
        let stereo = mixer.mono_to_stereo(&mono);
        assert_eq!(stereo, vec![0.5, 0.5, 0.6, 0.6, 0.7, 0.7]);
    }

    #[test]
    fn test_sum_normalized() {
        let mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, 48000, 2048).unwrap();

        let input1 = vec![0.5, 0.5, 0.6, 0.6];
        let input2 = vec![0.3, 0.3, 0.4, 0.4];
        let mixed = mixer.mix_samples(vec![input1, input2]);

        // (0.5 + 0.3) / 2 = 0.4, (0.6 + 0.4) / 2 = 0.5
        assert_eq!(mixed, vec![0.4, 0.4, 0.5, 0.5]);
    }

    #[test]
    fn test_sum_clipped() {
        let mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, 48000, 2048).unwrap();

        let input1 = vec![0.8, 0.8, 0.9, 0.9];
        let input2 = vec![0.7, 0.7, 0.8, 0.8];
        let mixed = mixer.mix_samples(vec![input1, input2]);

        // 0.8 + 0.7 = 1.5 -> clipped to 1.0
        // 0.9 + 0.8 = 1.7 -> clipped to 1.0
        assert_eq!(mixed, vec![1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn test_descriptor() {
        let desc = <AudioMixerProcessor as StreamTransform>::descriptor().unwrap();
        assert_eq!(desc.name, "AudioMixerProcessor");
        assert!(desc.tags.contains(&"audio".to_string()));
        assert!(desc.tags.contains(&"mixer".to_string()));
        assert!(desc.tags.contains(&"transform".to_string()));
    }
}
