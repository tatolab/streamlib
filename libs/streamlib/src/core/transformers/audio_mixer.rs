use crate::core::{
    Result, StreamInput, StreamOutput,
    ProcessorDescriptor, AudioRequirements,
};
use crate::core::frames::AudioFrame;
use crate::core::bus::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::schema::PortDescriptor;
use serde::{Serialize, Deserialize};
use dasp::Signal;
use streamlib_macros::StreamProcessor;

// Re-export for macro use
use crate as streamlib;

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

#[derive(StreamProcessor)]
pub struct AudioMixerProcessor {
    // Port fields - annotated!
    #[input]
    left: StreamInput<AudioFrame<1>>,

    #[input]
    right: StreamInput<AudioFrame<1>>,

    #[output]
    audio: StreamOutput<AudioFrame<2>>,

    // Config fields
    strategy: MixingStrategy,
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: u64,
}

impl AudioMixerProcessor {
    pub fn new(strategy: MixingStrategy) -> Self {
        Self {
            // Ports
            left: StreamInput::new("left"),
            right: StreamInput::new("right"),
            audio: StreamOutput::new("audio"),

            // Config fields
            strategy,
            sample_rate: 48000,
            buffer_size: 128,
            frame_counter: 0,
        }
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
        <Self as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![
            PortDescriptor {
                name: "left".to_string(),
                schema: AudioFrame::<1>::schema(),
                required: true,
                description: "Left channel mono audio input".to_string(),
            },
            PortDescriptor {
                name: "right".to_string(),
                schema: AudioFrame::<1>::schema(),
                required: true,
                description: "Right channel mono audio input".to_string(),
            },
        ]
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
            "AudioMixer: Starting ({} Hz, {} samples buffer, strategy: {:?})",
            self.sample_rate,
            self.buffer_size,
            self.strategy
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("AudioMixer: Stopped");
        Ok(())
    }
}

impl StreamProcessor for AudioMixerProcessor {
    type Config = AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self::new(config.strategy))
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "AudioMixerProcessor",
                "Mixes two mono signals (left and right) into a stereo signal"
            )
            .with_usage_context(
                "Use when you need to combine two mono audio sources into a stereo stream. \
                 Left input becomes left channel, right input becomes right channel. \
                 Mixing is performed using dasp signal combinators."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: Some(2),
            })
            .with_tags(vec!["audio", "mixer", "transform", "stereo", "dasp"])
        )
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[AudioMixer] process() called");

        // Check if both inputs have data
        let left_frame = match self.left.read_latest() {
            Some(f) => f,
            None => {
                tracing::debug!("[AudioMixer] Left input has no data");
                return Ok(());
            }
        };

        let right_frame = match self.right.read_latest() {
            Some(f) => f,
            None => {
                tracing::debug!("[AudioMixer] Right input has no data");
                return Ok(());
            }
        };

        // Use the newer timestamp
        let timestamp_ns = left_frame.timestamp_ns.max(right_frame.timestamp_ns);

        // Create dasp signals from both inputs
        let mut left_signal = left_frame.read();
        let mut right_signal = right_frame.read();

        // Interleave left and right samples into stereo
        let mut stereo_samples = Vec::with_capacity(self.buffer_size * 2);

        for _ in 0..self.buffer_size {
            let left_sample = left_signal.next()[0];
            let right_sample = right_signal.next()[0];

            // Apply mixing strategy (in case inputs need combining)
            let (final_left, final_right) = match self.strategy {
                MixingStrategy::Sum => (left_sample, right_sample),
                MixingStrategy::SumNormalized => (left_sample, right_sample),
                MixingStrategy::SumClipped => (
                    left_sample.clamp(-1.0, 1.0),
                    right_sample.clamp(-1.0, 1.0),
                ),
            };

            stereo_samples.push(final_left);   // Left channel
            stereo_samples.push(final_right);  // Right channel
        }

        let output_frame = AudioFrame::<2>::new(stereo_samples, timestamp_ns, self.frame_counter);
        self.audio.write(output_frame);

        tracing::debug!("[AudioMixer] Wrote mixed stereo frame");
        self.frame_counter += 1;

        Ok(())
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    // Delegate to macro-generated methods
    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_output_port_type_impl(port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_input_port_type_impl(port_name)
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        self.wire_output_connection_impl(port_name, connection)
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        self.wire_input_connection_impl(port_name, connection)
    }
}
