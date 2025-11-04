use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::{Result, StreamOutput};
use crate::core::frames::AudioFrame;
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use dasp::Signal;
use dasp::signal;

/// Configuration for ChordGeneratorProcessor
///
/// Generates a C major chord (C4, E4, G4) from a single hardware-like source,
/// emulating a 3-channel microphone array where each "mic" produces a different tone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChordGeneratorConfig {
    /// Amplitude for all tones (0.0 to 1.0)
    pub amplitude: f64,
}

impl Default for ChordGeneratorConfig {
    fn default() -> Self {
        Self {
            amplitude: 0.15, // 15% to avoid clipping when mixed
        }
    }
}

pub struct ChordGeneratorOutputPorts {
    /// C4 (261.63 Hz) - Like mic channel 0
    pub tone_c4: Arc<StreamOutput<AudioFrame<1>>>,
    /// E4 (329.63 Hz) - Like mic channel 1
    pub tone_e4: Arc<StreamOutput<AudioFrame<1>>>,
    /// G4 (392.00 Hz) - Like mic channel 2
    pub tone_g4: Arc<StreamOutput<AudioFrame<1>>>,
}

/// ChordGeneratorProcessor - Emulates a 3-channel microphone array
///
/// Unlike TestToneGenerator (which creates independent sources), this processor
/// generates all 3 tones of a C major chord from a single synchronized source,
/// mimicking how a real microphone array captures multiple channels simultaneously.
///
/// This demonstrates the microphone array pattern:
/// - Single hardware device (one clock, one callback)
/// - Multiple synchronized outputs (3 tones generated together)
/// - Each output can be routed independently downstream
pub struct ChordGeneratorProcessor {
    name: String,
    sample_rate: u32,
    amplitude: f64,

    // dasp signal generators for each tone
    signal_c4: Box<dyn Signal<Frame = f32> + Send>,  // C4 at 261.63 Hz
    signal_e4: Box<dyn Signal<Frame = f32> + Send>,  // E4 at 329.63 Hz
    signal_g4: Box<dyn Signal<Frame = f32> + Send>,  // G4 at 392.00 Hz

    buffer_size: usize,
    frame_counter: u64,
    output_ports: ChordGeneratorOutputPorts,
}

impl ChordGeneratorProcessor {
    const FREQ_C4: f64 = 261.63;  // C4
    const FREQ_E4: f64 = 329.63;  // E4
    const FREQ_G4: f64 = 392.00;  // G4

    pub fn new(amplitude: f64) -> Self {
        let sample_rate = 48000;
        let amp = amplitude.clamp(0.0, 1.0) as f32;

        // Create infinite sine wave signals using dasp
        // Note: dasp signals work in f64, so we map to f32 for mono f32 output
        let signal_c4 = Box::new(
            signal::rate(sample_rate as f64)
                .const_hz(Self::FREQ_C4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );
        let signal_e4 = Box::new(
            signal::rate(sample_rate as f64)
                .const_hz(Self::FREQ_E4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );
        let signal_g4 = Box::new(
            signal::rate(sample_rate as f64)
                .const_hz(Self::FREQ_G4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );

        Self {
            name: "chord_generator".to_string(),
            sample_rate,
            amplitude: amplitude.clamp(0.0, 1.0),
            signal_c4,
            signal_e4,
            signal_g4,
            buffer_size: 512,
            frame_counter: 0,
            output_ports: ChordGeneratorOutputPorts {
                tone_c4: Arc::new(StreamOutput::new("tone_c4")),
                tone_e4: Arc::new(StreamOutput::new("tone_e4")),
                tone_g4: Arc::new(StreamOutput::new("tone_g4")),
            },
        }
    }

    pub fn output_ports(&mut self) -> &mut ChordGeneratorOutputPorts {
        &mut self.output_ports
    }
}

impl StreamElement for ChordGeneratorProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <ChordGeneratorProcessor as StreamProcessor>::descriptor()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        use crate::core::ports::PortMessage;

        vec![
            PortDescriptor {
                name: "tone_c4".to_string(),
                schema: AudioFrame::<1>::schema(),
                required: false,
                description: "C4 tone (261.63 Hz) - like microphone array channel 0".to_string(),
            },
            PortDescriptor {
                name: "tone_e4".to_string(),
                schema: AudioFrame::<1>::schema(),
                required: false,
                description: "E4 tone (329.63 Hz) - like microphone array channel 1".to_string(),
            },
            PortDescriptor {
                name: "tone_g4".to_string(),
                schema: AudioFrame::<1>::schema(),
                required: false,
                description: "G4 tone (392.00 Hz) - like microphone array channel 2".to_string(),
            },
        ]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;
        self.frame_counter = 0;

        // Recreate signals with correct sample rate
        let amp = self.amplitude as f32;
        self.signal_c4 = Box::new(
            signal::rate(self.sample_rate as f64)
                .const_hz(Self::FREQ_C4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );
        self.signal_e4 = Box::new(
            signal::rate(self.sample_rate as f64)
                .const_hz(Self::FREQ_E4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );
        self.signal_g4 = Box::new(
            signal::rate(self.sample_rate as f64)
                .const_hz(Self::FREQ_G4)
                .sine()
                .scale_amp(amp as f64)
                .map(|x| x as f32)
        );

        tracing::info!(
            "[ChordGeneratorProcessor] start() called: C major chord at {}Hz sample rate, {} samples/buffer",
            self.sample_rate,
            self.buffer_size
        );

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("[ChordGeneratorProcessor] Stopped");
        Ok(())
    }

    fn as_source(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl StreamProcessor for ChordGeneratorProcessor {
    type Config = ChordGeneratorConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self::new(config.amplitude))
    }

    fn process(&mut self) -> Result<()> {
        let timestamp_ns = (self.frame_counter as f64 * self.buffer_size as f64 / self.sample_rate as f64 * 1_000_000_000.0) as i64;

        // Generate buffer_size samples for each tone
        let mut samples_c4 = Vec::with_capacity(self.buffer_size);
        let mut samples_e4 = Vec::with_capacity(self.buffer_size);
        let mut samples_g4 = Vec::with_capacity(self.buffer_size);

        for _ in 0..self.buffer_size {
            samples_c4.push(self.signal_c4.next());
            samples_e4.push(self.signal_e4.next());
            samples_g4.push(self.signal_g4.next());
        }

        // Create AudioFrames and write to outputs
        let frame_c4 = AudioFrame::<1>::new(samples_c4, timestamp_ns, self.frame_counter);
        let frame_e4 = AudioFrame::<1>::new(samples_e4, timestamp_ns, self.frame_counter);
        let frame_g4 = AudioFrame::<1>::new(samples_g4, timestamp_ns, self.frame_counter);

        self.output_ports.tone_c4.write(frame_c4);
        self.output_ports.tone_e4.write(frame_e4);
        self.output_ports.tone_g4.write(frame_g4);

        self.frame_counter += 1;

        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Loop,
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,  // This is a hardware-like source
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "ChordGeneratorProcessor",
                "Generates a C major chord (C4, E4, G4) as separate synchronized outputs, emulating a 3-channel microphone array"
            )
            .with_usage_context(
                "Demonstrates the microphone array pattern: single hardware source, multiple synchronized outputs. \
                 All three tones are generated together from one clock source (like a mic array capturing 3 channels simultaneously). \
                 Each tone can be routed independently to different downstream processors. \
                 Perfect for testing mixer processors and understanding multi-output source patterns."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: None,
                required_buffer_size: None,
                supported_sample_rates: vec![],
                required_channels: None,
            })
            .with_tags(vec!["audio", "source", "generator", "multi-output", "chord", "test"])
        )
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        match port_name {
            "tone_c4" => self.output_ports.tone_c4.set_downstream_wakeup(wakeup_tx),
            "tone_e4" => self.output_ports.tone_e4.set_downstream_wakeup(wakeup_tx),
            "tone_g4" => self.output_ports.tone_g4.set_downstream_wakeup(wakeup_tx),
            _ => {},
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "tone_c4" | "tone_e4" | "tone_g4" => Some(crate::core::ports::PortType::Audio1),
            _ => None,
        }
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::connection::ProcessorConnection;
        use crate::core::AudioFrame;

        // Downcast to the correct connection type (AudioFrame<1> for mono)
        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<1>>>>() {
            match port_name {
                "tone_c4" => {
                    self.output_ports.tone_c4.add_connection(std::sync::Arc::clone(&typed_conn));
                    true
                },
                "tone_e4" => {
                    self.output_ports.tone_e4.add_connection(std::sync::Arc::clone(&typed_conn));
                    true
                },
                "tone_g4" => {
                    self.output_ports.tone_g4.add_connection(std::sync::Arc::clone(&typed_conn));
                    true
                },
                _ => false,
            }
        } else {
            false
        }
    }
}
