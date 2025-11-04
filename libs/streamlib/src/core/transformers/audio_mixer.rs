use crate::core::{
    Result, StreamError, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, AudioRequirements,
};
use crate::core::frames::{MonoSignal, StereoSignal};
use crate::core::ports::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use std::sync::Arc;
use serde::{Serialize, Deserialize};

/// Configuration for AudioMixerProcessor
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
    /// Simple sum (can exceed [-1, 1])
    Sum,
    /// Divide by number of inputs (prevents clipping)
    SumNormalized,
    /// Clamp result to [-1, 1]
    SumClipped,
}

impl Default for MixingStrategy {
    fn default() -> Self {
        MixingStrategy::SumNormalized
    }
}

/// AudioMixerProcessor - Mixes N MonoSignal inputs into StereoSignal output
///
/// # Type Parameters
/// - `N`: Number of mono input channels (compile-time constant)
///
/// # Example
/// ```ignore
/// // Mix 3 mono sources into stereo
/// let mixer = AudioMixerProcessor::<3>::new(MixingStrategy::SumNormalized)?;
/// runtime.connect(&mut tone1.output_ports().audio, &mut mixer.input_ports[0])?;
/// runtime.connect(&mut tone2.output_ports().audio, &mut mixer.input_ports[1])?;
/// runtime.connect(&mut tone3.output_ports().audio, &mut mixer.input_ports[2])?;
/// runtime.connect(&mut mixer.output_ports.audio, &mut speaker.input_ports.audio)?;
/// ```
pub struct AudioMixerProcessor<const N: usize> {
    strategy: MixingStrategy,
    sample_rate: u32,
    buffer_size: usize,

    /// N mono input ports
    pub input_ports: [StreamInput<MonoSignal>; N],

    /// Stereo output port
    pub output_ports: AudioMixerOutputPorts,
}

pub struct AudioMixerOutputPorts {
    pub audio: StreamOutput<StereoSignal>,
}

impl<const N: usize> AudioMixerProcessor<N> {
    pub fn new(strategy: MixingStrategy) -> Result<Self> {
        if N == 0 {
            return Err(StreamError::Configuration(
                "AudioMixerProcessor requires at least 1 input".into()
            ));
        }

        // Create array of input ports
        let input_ports: [StreamInput<MonoSignal>; N] = std::array::from_fn(|i| {
            StreamInput::new(format!("input_{}", i))
        });

        Ok(Self {
            strategy,
            sample_rate: 48000,
            buffer_size: 2048,
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
                schema: MonoSignal::schema(),
                required: true,
                description: format!("Mono audio input {} for mixing", i),
            })
            .collect()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: StereoSignal::schema(),
            required: true,
            description: "Mixed stereo audio output".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

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
        tracing::debug!("[AudioMixer<{}>] process() called", N);

        // Read all input signals (these are continuous streams, created once upstream)
        let mut input_signals: Vec<Option<MonoSignal>> = Vec::with_capacity(N);
        for input in &mut self.input_ports {
            input_signals.push(input.read_latest());
        }

        // Check if all inputs have data
        let all_ready = input_signals.iter().all(|sig| sig.is_some());
        if !all_ready {
            tracing::debug!("[AudioMixer<{}>] Not all inputs have data yet, skipping", N);
            return Ok(());
        }

        // Create dasp signals from each input (one signal per process() call)
        use dasp::signal::Signal;
        use dasp::frame::Frame;
        use crate::core::frames::BufferGenerator;

        let timestamp_ns = input_signals[0].as_ref().unwrap().timestamp_ns();

        let mut dasp_signals: Vec<_> = input_signals.iter()
            .filter_map(|opt| opt.as_ref())
            .map(|sig| sig.create_signal())
            .collect();

        // Mix by iterating through signals and using add_amp on frames
        let mut mixed_samples = Vec::with_capacity(self.buffer_size);

        for _ in 0..self.buffer_size {
            // Get next frame from each signal and sum using add_amp
            let mut mixed_frame = [0.0f32; 1]; // Start with silence

            for signal in &mut dasp_signals {
                let frame = signal.next();
                mixed_frame = mixed_frame.add_amp(frame);
            }

            // Convert mono frame to stereo (duplicate to both channels)
            mixed_samples.push([mixed_frame[0], mixed_frame[0]]);
        }

        // Create output signal from mixed samples
        let generator = BufferGenerator::new(mixed_samples, false);
        let mixed = StereoSignal::new(Box::new(generator), self.sample_rate, timestamp_ns);

        // Write output
        self.output_ports.audio.write(mixed);
        tracing::debug!("[AudioMixer<{}>] Wrote mixed stereo signal", N);

        Ok(())
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "audio" => Some(self.output_ports.audio.port_type()),
            _ => None,
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        if let Some(idx_str) = port_name.strip_prefix("input_") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if idx < N {
                    return Some(self.input_ports[idx].port_type());
                }
            }
        }
        None
    }

    fn connect_bus_to_input(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if let Some(idx_str) = port_name.strip_prefix("input_") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if idx < N {
                    return self.input_ports[idx].connect_bus(bus);
                }
            }
        }
        false
    }

    fn create_bus_for_output(&self, port_name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match port_name {
            "audio" => Some(self.output_ports.audio.get_or_create_bus() as Arc<dyn std::any::Any + Send + Sync>),
            _ => None,
        }
    }

    fn connect_bus_to_output(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if let Some(typed_bus) = bus.downcast::<Arc<dyn crate::core::bus::Bus<StereoSignal>>>().ok() {
            if port_name == "audio" {
                self.output_ports.audio.set_bus(Arc::clone(&typed_bus));
                return true;
            }
        }
        false
    }

    fn connect_reader_to_input(&mut self, port_name: &str, reader: Box<dyn std::any::Any + Send>) -> bool {
        if let Ok(typed_reader) = reader.downcast::<Box<dyn crate::core::bus::BusReader<MonoSignal>>>() {
            if let Some(idx_str) = port_name.strip_prefix("input_") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if idx < N {
                        self.input_ports[idx].connect_reader(*typed_reader);
                        return true;
                    }
                }
            }
        }
        false
    }
}
