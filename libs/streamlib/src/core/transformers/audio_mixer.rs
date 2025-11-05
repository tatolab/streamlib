use crate::core::{
    Result, StreamError, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, AudioRequirements,
};
use crate::core::frames::AudioFrame;
use crate::core::ports::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use serde::{Serialize, Deserialize};
use dasp::Signal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    pub strategy: MixingStrategy,
}

impl Default for AudioMixerConfig {
    fn default() -> Self {
        Self {
            strategy: MixingStrategy::SumNormalized,
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

pub struct AudioMixerProcessor<const N: usize> {
    strategy: MixingStrategy,
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: u64,

    pub input_ports: [StreamInput<AudioFrame<1>>; N],

    pub output_ports: AudioMixerOutputPorts,
}

pub struct AudioMixerOutputPorts {
    pub audio: StreamOutput<AudioFrame<2>>,
}

impl<const N: usize> AudioMixerProcessor<N> {
    pub fn new(strategy: MixingStrategy) -> Result<Self> {
        if N == 0 {
            return Err(StreamError::Configuration(
                "AudioMixerProcessor requires at least 1 input".into()
            ));
        }

        let input_ports: [StreamInput<AudioFrame<1>>; N] = std::array::from_fn(|i| {
            StreamInput::new(format!("input_{}", i))
        });

        Ok(Self {
            strategy,
            sample_rate: 48000,
            buffer_size: 2048,
            frame_counter: 0,
            input_ports,
            output_ports: AudioMixerOutputPorts {
                audio: StreamOutput::new("audio"),
            },
        })
    }
}

impl<const N: usize> StreamElement for AudioMixerProcessor<N> {
    fn name(&self) -> &str {
        "audio_mixer"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <Self as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        (0..N)
            .map(|i| PortDescriptor {
                name: format!("input_{}", i),
                schema: AudioFrame::<1>::schema(),
                required: true,
                description: format!("Mono audio input {} for mixing", i),
            })
            .collect()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Mixed stereo audio output".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;
        self.frame_counter = 0;

        tracing::info!(
            "AudioMixer<{}>: Starting ({} Hz, {} samples buffer, strategy: {:?})",
            N,
            self.sample_rate,
            self.buffer_size,
            self.strategy
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("AudioMixer<{}>: Stopped", N);
        Ok(())
    }
}

impl<const N: usize> StreamProcessor for AudioMixerProcessor<N> {
    type Config = AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Self::new(config.strategy)
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                &format!("AudioMixerProcessor<{}>", N),
                &format!("Mixes {} mono signals into a stereo signal using dasp", N)
            )
            .with_usage_context(
                "Use when you need to combine multiple mono audio sources into a stereo stream. \
                 All mixing is performed using lazy dasp signal combinators - zero-copy until samples are consumed. \
                 Input channels are compile-time constant for type safety."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: Some(2),
            })
            .with_tags(vec!["audio", "mixer", "transform", "multi-input", "dasp"])
        )
    }

    fn process(&mut self) -> Result<()> {
        tracing::info!("[AudioMixer<{}>] process() called", N);

        let mut input_frames: Vec<Option<AudioFrame<1>>> = Vec::with_capacity(N);
        for input in &self.input_ports {
            input_frames.push(input.read_latest());
        }

        let all_ready = input_frames.iter().all(|frame| frame.is_some());
        if !all_ready {
            tracing::debug!("[AudioMixer<{}>] Not all inputs have data yet, skipping", N);
            return Ok(());
        }

        let timestamp_ns = input_frames[0].as_ref().unwrap().timestamp_ns;

        let mut signals: Vec<_> = input_frames.iter()
            .filter_map(|opt| opt.as_ref())
            .map(|frame| frame.read())
            .collect();

        let mut mixed_samples = Vec::with_capacity(self.buffer_size * 2); // *2 for stereo

        for _ in 0..self.buffer_size {
            let mut mixed_mono = 0.0f32;
            for signal in &mut signals {
                mixed_mono += signal.next()[0];
            }

            let final_sample = match self.strategy {
                MixingStrategy::Sum => mixed_mono,
                MixingStrategy::SumNormalized => mixed_mono / N as f32,
                MixingStrategy::SumClipped => mixed_mono.clamp(-1.0, 1.0),
            };

            mixed_samples.push(final_sample);  // Left
            mixed_samples.push(final_sample);  // Right
        }

        let output_frame = AudioFrame::<2>::new(mixed_samples, timestamp_ns, self.frame_counter);

        self.output_ports.audio.write(output_frame);
        tracing::debug!("[AudioMixer<{}>] Wrote mixed stereo frame", N);

        self.frame_counter += 1;

        Ok(())
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "audio" => Some(crate::core::ports::PortType::Audio2),
            _ => None,
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        if let Some(index_str) = port_name.strip_prefix("input_") {
            if let Ok(index) = index_str.parse::<usize>() {
                if index < N {
                    return Some(crate::core::ports::PortType::Audio1);
                }
            }
        }
        None
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::connection::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.output_ports.audio.add_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::connection::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<1>>>>() {
            if let Some(index_str) = port_name.strip_prefix("input_") {
                if let Ok(index) = index_str.parse::<usize>() {
                    if index < N {
                        self.input_ports[index].set_connection(std::sync::Arc::clone(&typed_conn));
                        return true;
                    }
                }
            }
        }
        false
    }
}
