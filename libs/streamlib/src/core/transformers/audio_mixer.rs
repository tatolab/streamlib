use crate::core::{
    Result, StreamError,
    AudioFrame, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, SCHEMA_AUDIO_FRAME,
    AudioRequirements,
};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    pub num_inputs: usize,
    pub strategy: MixingStrategy,
    pub channel_mode: ChannelMode,
}

impl Default for AudioMixerConfig {
    fn default() -> Self {
        Self {
            num_inputs: 2,
            strategy: MixingStrategy::SumNormalized,
            channel_mode: ChannelMode::MixUp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MixingStrategy {
    Sum,
    SumNormalized,
    SumClipped,
}

impl Default for MixingStrategy {
    fn default() -> Self {
        MixingStrategy::SumNormalized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChannelMode {
    MixUp,
    MixDown,
}

impl Default for ChannelMode {
    fn default() -> Self {
        ChannelMode::MixUp
    }
}

pub struct AudioMixerInputPorts {
    pub inputs: HashMap<String, Arc<Mutex<StreamInput<AudioFrame>>>>,
}

pub struct AudioMixerOutputPorts {
    pub audio: StreamOutput<AudioFrame>,
}

pub struct AudioMixerProcessor {
    num_inputs: usize,
    strategy: MixingStrategy,
    channel_mode: ChannelMode,
    input_ports: AudioMixerInputPorts,
    output_ports: AudioMixerOutputPorts,
    target_sample_rate: u32,
    target_channels: u32,
    frame_counter: u64,
    current_timestamp_ns: i64,
    mix_buffer: Vec<f32>,
    max_buffer_size: usize,
    buffer_size: usize,
    last_mixed_timestamps: HashMap<String, i64>,
    input_cache: HashMap<String, Option<AudioFrame>>,
    cached_output_channels: Option<u32>,
    cache_last_updated: u64,
    cache_ttl_frames: u64,
}

impl AudioMixerProcessor {
    pub fn new(
        num_inputs: usize,
        strategy: MixingStrategy,
        channel_mode: ChannelMode,
    ) -> Result<Self> {
        let sample_rate = 48000;
        let buffer_size = 2048;
        if num_inputs == 0 {
            return Err(StreamError::Configuration(
                "AudioMixerProcessor requires at least 1 input".into()
            ));
        }

        let mut input_ports_map = HashMap::new();
        for i in 0..num_inputs {
            let port_name = format!("input_{}", i);
            input_ports_map.insert(
                port_name.clone(),
                Arc::new(Mutex::new(StreamInput::new(port_name.clone())))
            );
        }

        let max_buffer_size = 4096;
        let target_channels = 2;
        let mix_buffer = vec![0.0; max_buffer_size * target_channels as usize];

        let mut input_cache = HashMap::new();
        for i in 0..num_inputs {
            input_cache.insert(format!("input_{}", i), None);
        }

        Ok(Self {
            num_inputs,
            strategy,
            channel_mode,
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
            buffer_size,
            last_mixed_timestamps: HashMap::new(),
            input_cache,
            cached_output_channels: None,
            cache_last_updated: 0,
            cache_ttl_frames: 60,
        })
    }

    pub fn input_ports(&mut self) -> &mut AudioMixerInputPorts {
        &mut self.input_ports
    }

    pub fn output_ports(&mut self) -> &mut AudioMixerOutputPorts {
        &mut self.output_ports
    }

    fn determine_output_channels(&mut self, inputs: &[&AudioFrame]) -> u32 {
        let cache_expired = self.frame_counter - self.cache_last_updated >= self.cache_ttl_frames;

        if let Some(cached) = self.cached_output_channels {
            if !cache_expired {
                return cached;
            }
        }

        if inputs.is_empty() {
            return self.target_channels;
        }

        let mut channel_counts: Vec<u32> = Vec::new();
        for input in inputs {
            if input.channels > 0 {
                channel_counts.push(input.channels);
            }
        }

        if channel_counts.is_empty() {
            return self.target_channels;
        }

        let output_channels = match self.channel_mode {
            ChannelMode::MixUp => *channel_counts.iter().max().unwrap(),
            ChannelMode::MixDown => *channel_counts.iter().min().unwrap(),
        };

        self.cached_output_channels = Some(output_channels);
        self.cache_last_updated = self.frame_counter;

        tracing::debug!(
            "[AudioMixer] Channel detection: mode={:?}, detected={} channels (cache TTL={})",
            self.channel_mode, output_channels, self.cache_ttl_frames
        );

        output_channels
    }

    fn mix_samples(&mut self, inputs: Vec<&AudioFrame>) -> Result<AudioFrame> {
        if inputs.is_empty() {
            return Err(StreamError::Configuration("No inputs to mix".into()));
        }

        let timestamp_ns = inputs[0].timestamp_ns;
        let frame_number = self.frame_counter;

        let output_channels = match self.channel_mode {
            ChannelMode::MixUp => inputs.iter().map(|f| f.channels as usize).max().unwrap(),
            ChannelMode::MixDown => inputs.iter().map(|f| f.channels as usize).min().unwrap(),
        };

        let loop_count = inputs.iter().map(|f| f.samples.len() / f.channels as usize).max().unwrap();

        let num_inputs = inputs.len() as f32;
        let output_frames: Vec<Vec<f32>> = (0..loop_count)
            .map(|frame_idx| {
                (0..output_channels)
                    .map(|ch_idx| {
                        let sum = inputs.iter()
                            .filter_map(|input| {
                                let input_channels = input.channels as usize;
                                let input_frame_count = input.samples.len() / input_channels;
                                if frame_idx < input_frame_count {
                                    let in_ch = ch_idx % input_channels;
                                    let sample_idx = frame_idx * input_channels + in_ch;
                                    Some(input.samples[sample_idx])
                                } else {
                                    None
                                }
                            })
                            .sum::<f32>();

                        match self.strategy {
                            MixingStrategy::Sum => sum,
                            MixingStrategy::SumNormalized => sum / num_inputs,
                            MixingStrategy::SumClipped => sum.clamp(-1.0, 1.0),
                        }
                    })
                    .collect()
            })
            .collect();

        let output_samples: Vec<f32> = output_frames.into_iter().flatten().collect();
        Ok(AudioFrame::new(output_samples, timestamp_ns, frame_number, output_channels as u32))
    }
}

impl StreamElement for AudioMixerProcessor {
    fn name(&self) -> &str {
        "audio_mixer"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AudioMixerProcessor as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
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

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.target_sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

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
        for value in self.input_cache.values_mut() {
            *value = None;
        }
        tracing::info!("AudioMixer: Stopped");
        Ok(())
    }
}

impl StreamProcessor for AudioMixerProcessor {
    type Config = crate::core::AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Self::new(
            config.num_inputs,
            config.strategy,
            config.channel_mode,
        )
    }

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
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: Some(2),
            })
            .with_tags(vec!["audio", "mixer", "transform", "multi-input"])
        )
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[AudioMixer] process() called");

        for i in 0..self.num_inputs {
            let input_name = format!("input_{}", i);

            let frame_opt = if let Some(input_port) = self.input_ports.inputs.get(&input_name) {
                let mut port = input_port.lock();
                port.read_latest()
            } else {
                None
            };

            if let Some(frame) = frame_opt {
                tracing::debug!(
                    "[AudioMixer] Received NEW frame from {} - {} samples, {} channels, frame #{}",
                    input_name, frame.sample_count(), frame.channels, frame.frame_number
                );

                if let Some(slot) = self.input_cache.get_mut(&input_name) {
                    *slot = Some(frame);
                }
            }
        }

        let all_ready = self.input_cache.values().all(|v| v.is_some());
        if !all_ready {
            tracing::debug!("[AudioMixer] Not all inputs have data yet (cold start), skipping mix");
            return Ok(());
        }

        let mut all_inputs_named: HashMap<String, AudioFrame> = HashMap::new();
        for (name, value_opt) in &self.input_cache {
            if let Some(value) = value_opt {
                all_inputs_named.insert(name.clone(), value.clone());
            }
        }

        let mut all_inputs_unchanged = true;
        for (input_name, frame) in &all_inputs_named {
            if let Some(&last_ts) = self.last_mixed_timestamps.get(input_name) {
                if frame.timestamp_ns != last_ts {
                    all_inputs_unchanged = false;
                    break;
                }
            } else {
                all_inputs_unchanged = false;
                break;
            }
        }

        if all_inputs_unchanged {
            tracing::debug!("[AudioMixer] Skipping duplicate mix - all inputs have same timestamps as last mix");
            return Ok(());
        }

        tracing::debug!(
            "[AudioMixer] Mixing ALL {} input streams (cached, sample-and-hold pattern)",
            all_inputs_named.len()
        );

        let mut sorted_names: Vec<_> = all_inputs_named.keys().collect();
        sorted_names.sort();

        let input_frames: Vec<&AudioFrame> = sorted_names.iter()
            .map(|name| all_inputs_named.get(*name).unwrap())
            .collect();

        let output_frame = self.mix_samples(input_frames)?;

        self.output_ports.audio.write(output_frame.clone());
        tracing::debug!(
            "[AudioMixer] Wrote mixed frame #{} - {} samples, {} channels, {} Hz",
            output_frame.frame_number, output_frame.sample_count(), output_frame.channels, self.target_sample_rate
        );

        self.frame_counter += 1;
        self.current_timestamp_ns = output_frame.timestamp_ns + output_frame.duration_ns(self.target_sample_rate);

        for (input_name, frame) in all_inputs_named {
            self.last_mixed_timestamps.insert(input_name, frame.timestamp_ns);
        }

        Ok(())
    }

    fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::traits::PortConsumer> {
        if port_name == "audio" {
            self.output_ports.audio
                .consumer_holder()
                .lock()
                .take()
                .map(|consumer| crate::core::traits::PortConsumer::Audio(consumer))
        } else {
            None
        }
    }

    fn connect_input_consumer(&mut self, port_name: &str, consumer: crate::core::traits::PortConsumer) -> bool {
        // Check if port_name matches any of our dynamic input ports
        if let Some(input_arc) = self.input_ports.inputs.get(port_name) {
            match consumer {
                crate::core::traits::PortConsumer::Audio(c) => {
                    let mut input = input_arc.lock();
                    input.connect_consumer(c);
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mix_samples_sum_normalized_stereo() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![2.0, 3.0, 4.0, 5.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![3.0, 4.0, 5.0, 6.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 2.5);
        assert_eq!(result.samples[1], 3.5);
        assert_eq!(result.samples[2], 4.5);
        assert_eq!(result.samples[3], 5.5);
    }

    #[test]
    fn test_mix_samples_sum_clipped_stereo() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.8, 0.9, 0.5, 0.6], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.5, 0.6, 0.3, 0.2], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 1.0);
        assert_eq!(result.samples[1], 1.0);
        assert_eq!(result.samples[2], 0.8);
        assert_eq!(result.samples[3], 0.8);
    }

    #[test]
    fn test_mix_samples_different_lengths() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![2.0, 3.0, 4.0, 5.0, 6.0, 7.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![1.0, 2.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.sample_count(), 3);
        assert_eq!(result.samples[0], 1.5);
        assert_eq!(result.samples[1], 2.5);
        assert_eq!(result.samples[2], 2.0);
        assert_eq!(result.samples[3], 2.5);
        assert_eq!(result.samples[4], 3.0);
        assert_eq!(result.samples[5], 3.5);
    }

    #[test]
    fn test_mix_samples_mono_to_stereo_mixup() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let mono_input = AudioFrame::new(vec![2.0, 4.0], 0, 0, 1);
        let stereo_input = AudioFrame::new(vec![3.0, 5.0, 6.0, 8.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&mono_input, &stereo_input]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 2.5);
        assert_eq!(result.samples[1], 3.5);
        assert_eq!(result.samples[2], 5.0);
        assert_eq!(result.samples[3], 6.0);
    }

    #[test]
    fn test_mix_samples_stereo_to_mono_mixdown() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixDown).unwrap();

        let mono_input = AudioFrame::new(vec![2.0, 4.0], 0, 0, 1);
        let stereo_input = AudioFrame::new(vec![3.0, 5.0, 6.0, 8.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&mono_input, &stereo_input]).unwrap();

        assert_eq!(result.channels, 1);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 2.5);
        assert_eq!(result.samples[1], 5.0);
    }

    #[test]
    fn test_basic_addition() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![2.0, 3.0], 0, 0, 1);
        let input2 = AudioFrame::new(vec![3.0, 2.0], 0, 0, 1);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.samples[0], 2.5);
        assert_eq!(result.samples[1], 2.5);
    }

    #[test]
    fn test_stereo_to_quad_mixup_with_modulo() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let stereo_input = AudioFrame::new(vec![0.1, 0.2], 0, 0, 2);
        let quad_input = AudioFrame::new(vec![0.1, 0.1, 0.2, 0.3], 0, 0, 4);

        let result = mixer.mix_samples(vec![&stereo_input, &quad_input]).unwrap();

        assert_eq!(result.channels, 4);
        assert_eq!(result.sample_count(), 1);
        assert_eq!(result.samples[0], 0.2);
        assert_eq!(result.samples[1], 0.3);
        assert_eq!(result.samples[2], 0.3);
        assert_eq!(result.samples[3], 0.5);
    }

    #[test]
    fn test_empty_input_mixed_with_data() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let empty_input = AudioFrame::new(vec![], 0, 0, 2);
        let data_input = AudioFrame::new(vec![0.5, 0.6], 0, 0, 2);

        let result = mixer.mix_samples(vec![&empty_input, &data_input]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 1);
        assert_eq!(result.samples[0], 0.5);
        assert_eq!(result.samples[1], 0.6);
    }

    #[test]
    fn test_mismatched_lengths_short_and_long() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let short_input = AudioFrame::new(vec![0.1, 0.2], 0, 0, 2);
        let long_input = AudioFrame::new(vec![0.3, 0.4, 0.5, 0.6, 0.7, 0.8], 0, 0, 2);

        let result = mixer.mix_samples(vec![&short_input, &long_input]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 3);
        assert_eq!(result.samples[0], 0.4);
        assert_eq!(result.samples[1], 0.6);
        assert_eq!(result.samples[2], 0.5);
        assert_eq!(result.samples[3], 0.6);
        assert_eq!(result.samples[4], 0.7);
        assert_eq!(result.samples[5], 0.8);
    }

    #[test]
    fn test_three_inputs_different_lengths() {
        let mut mixer = AudioMixerProcessor::new(3, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.1, 0.2], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.2, 0.3, 0.4, 0.5], 0, 0, 2);
        let input3 = AudioFrame::new(vec![0.3, 0.4, 0.5, 0.6, 0.7, 0.8], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2, &input3]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 3);
        assert_eq!(result.samples[0], 0.6);
        assert_eq!(result.samples[1], 0.9);
        assert_eq!(result.samples[2], 0.6);
        assert_eq!(result.samples[3], 0.8);
        assert_eq!(result.samples[4], 0.7);
        assert_eq!(result.samples[5], 0.8);
    }

    #[test]
    fn test_mono_stereo_quad_mixup() {
        let mut mixer = AudioMixerProcessor::new(3, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let mono = AudioFrame::new(vec![0.1, 0.2], 0, 0, 1);
        let stereo = AudioFrame::new(vec![0.2, 0.3], 0, 0, 2);
        let quad = AudioFrame::new(vec![0.1, 0.1, 0.1, 0.1], 0, 0, 4);

        let result = mixer.mix_samples(vec![&mono, &stereo, &quad]).unwrap();

        assert_eq!(result.channels, 4);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 0.4);
        assert_eq!(result.samples[1], 0.5);
        assert_eq!(result.samples[2], 0.3);
        assert_eq!(result.samples[3], 0.2);
    }

    #[test]
    fn test_mono_stereo_quad_mixdown() {
        let mut mixer = AudioMixerProcessor::new(3, MixingStrategy::Sum, ChannelMode::MixDown).unwrap();

        let mono = AudioFrame::new(vec![0.1, 0.2], 0, 0, 1);
        let stereo = AudioFrame::new(vec![0.2, 0.3], 0, 0, 2);
        let quad = AudioFrame::new(vec![0.1, 0.1, 0.1, 0.1], 0, 0, 4);

        let result = mixer.mix_samples(vec![&mono, &stereo, &quad]).unwrap();

        assert_eq!(result.channels, 1);
        assert_eq!(result.sample_count(), 2);
        assert_eq!(result.samples[0], 0.4);
        assert_eq!(result.samples[1], 0.5);
    }

    #[test]
    fn test_single_sample_multiple_inputs() {
        let mut mixer = AudioMixerProcessor::new(4, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![1.0, 2.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![3.0, 4.0], 0, 0, 2);
        let input3 = AudioFrame::new(vec![5.0, 6.0], 0, 0, 2);
        let input4 = AudioFrame::new(vec![7.0, 8.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2, &input3, &input4]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 1);
        assert_eq!(result.samples[0], 16.0);
        assert_eq!(result.samples[1], 20.0);
    }

    #[test]
    fn test_values_exceeding_one() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.8, 0.9], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.7, 0.8], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 1.5);
        assert_eq!(result.samples[1], 1.7);
    }

    #[test]
    fn test_negative_values() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![-0.5, 0.3], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.2, -0.4], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], -0.3);
        assert_eq!(result.samples[1], -0.1);
    }

    #[test]
    fn test_all_zero_inputs() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.0, 0.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.0, 0.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 0.0);
        assert_eq!(result.samples[1], 0.0);
    }

    #[test]
    fn test_very_long_input() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let long_samples: Vec<f32> = (0..1000).map(|i| (i as f32) * 0.001).collect();
        let short_samples = vec![0.1, 0.2];

        let long_input = AudioFrame::new(long_samples, 0, 0, 2);
        let short_input = AudioFrame::new(short_samples, 0, 0, 2);

        let result = mixer.mix_samples(vec![&long_input, &short_input]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.sample_count(), 500);
        assert_eq!(result.samples[0], 0.1);
        assert_eq!(result.samples[1], 0.201);
    }

    #[test]
    fn test_5_1_surround_mixing() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();

        let stereo = AudioFrame::new(vec![0.1, 0.2], 0, 0, 2);
        let surround_5_1 = AudioFrame::new(vec![0.1, 0.1, 0.1, 0.1, 0.1, 0.1], 0, 0, 6);

        let result = mixer.mix_samples(vec![&stereo, &surround_5_1]).unwrap();

        assert_eq!(result.channels, 6);
        assert_eq!(result.sample_count(), 1);
        assert_eq!(result.samples[0], 0.2);
        assert_eq!(result.samples[1], 0.3);
        assert_eq!(result.samples[2], 0.2);
        assert_eq!(result.samples[3], 0.3);
        assert_eq!(result.samples[4], 0.2);
        assert_eq!(result.samples[5], 0.3);
    }

    #[test]
    fn test_normalized_strategy() {
        let mut mixer = AudioMixerProcessor::new(3, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.9, 1.2], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.6, 0.9], 0, 0, 2);
        let input3 = AudioFrame::new(vec![0.3, 0.6], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2, &input3]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 0.6);
        assert_eq!(result.samples[1], 0.9);
    }

    #[test]
    fn test_clipped_strategy() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.8, -0.7], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.5, -0.6], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 1.0);
        assert_eq!(result.samples[1], -1.0);
    }

    #[test]
    fn test_normalized_with_two_inputs() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.4, 0.6], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.2, 0.4], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 0.3);
        assert_eq!(result.samples[1], 0.5);
    }

    #[test]
    fn test_normalized_prevents_clipping() {
        let mut mixer = AudioMixerProcessor::new(3, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.9, 0.8], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.9, 0.8], 0, 0, 2);
        let input3 = AudioFrame::new(vec![0.9, 0.8], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2, &input3]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 0.9);
        assert_eq!(result.samples[1], 0.8);
    }

    #[test]
    fn test_clipped_no_clipping_needed() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.2, 0.3], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.3, 0.4], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 0.5);
        assert_eq!(result.samples[1], 0.7);
    }

    #[test]
    fn test_clipped_extreme_positive() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![5.0, 3.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![2.0, 4.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], 1.0);
        assert_eq!(result.samples[1], 1.0);
    }

    #[test]
    fn test_clipped_extreme_negative() {
        let mut mixer = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![-5.0, -3.0], 0, 0, 2);
        let input2 = AudioFrame::new(vec![-2.0, -4.0], 0, 0, 2);

        let result = mixer.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result.channels, 2);
        assert_eq!(result.samples[0], -1.0);
        assert_eq!(result.samples[1], -1.0);
    }

    #[test]
    fn test_sum_vs_normalized_comparison() {
        let mut mixer_sum = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();
        let mut mixer_norm = AudioMixerProcessor::new(2, MixingStrategy::SumNormalized, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.4, 0.6], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.2, 0.4], 0, 0, 2);

        let result_sum = mixer_sum.mix_samples(vec![&input1, &input2]).unwrap();
        let result_norm = mixer_norm.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result_sum.samples[0], 0.6);
        assert_eq!(result_sum.samples[1], 1.0);
        assert_eq!(result_norm.samples[0], 0.3);
        assert_eq!(result_norm.samples[1], 0.5);
    }

    #[test]
    fn test_sum_vs_clipped_comparison() {
        let mut mixer_sum = AudioMixerProcessor::new(2, MixingStrategy::Sum, ChannelMode::MixUp).unwrap();
        let mut mixer_clip = AudioMixerProcessor::new(2, MixingStrategy::SumClipped, ChannelMode::MixUp).unwrap();

        let input1 = AudioFrame::new(vec![0.8, 0.9], 0, 0, 2);
        let input2 = AudioFrame::new(vec![0.5, 0.6], 0, 0, 2);

        let result_sum = mixer_sum.mix_samples(vec![&input1, &input2]).unwrap();
        let result_clip = mixer_clip.mix_samples(vec![&input1, &input2]).unwrap();

        assert_eq!(result_sum.samples[0], 1.3);
        assert_eq!(result_sum.samples[1], 1.5);
        assert_eq!(result_clip.samples[0], 1.0);
        assert_eq!(result_clip.samples[1], 1.0);
    }
}
