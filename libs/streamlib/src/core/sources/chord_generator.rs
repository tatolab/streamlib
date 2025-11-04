use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::{Result, StreamOutput, StreamError};
use crate::core::frames::{MonoSignal, SineGenerator};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements};
use std::sync::Arc;
use parking_lot::Mutex;
use serde::{Serialize, Deserialize};

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
    pub tone_c4: Arc<StreamOutput<MonoSignal>>,
    /// E4 (329.63 Hz) - Like mic channel 1
    pub tone_e4: Arc<StreamOutput<MonoSignal>>,
    /// G4 (392.00 Hz) - Like mic channel 2
    pub tone_g4: Arc<StreamOutput<MonoSignal>>,
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

    // Continuous MonoSignals (created once, reused forever)
    signal_c4: MonoSignal,  // C4 at 261.63 Hz
    signal_e4: MonoSignal,  // E4 at 329.63 Hz
    signal_g4: MonoSignal,  // G4 at 392.00 Hz

    buffer_size: usize,
    output_ports: ChordGeneratorOutputPorts,
}

impl ChordGeneratorProcessor {
    const FREQ_C4: f64 = 261.63;  // C4
    const FREQ_E4: f64 = 329.63;  // E4
    const FREQ_G4: f64 = 392.00;  // G4

    pub fn new(amplitude: f64) -> Self {
        let sample_rate = 48000;
        let amp = amplitude.clamp(0.0, 1.0) as f32;

        let gen_c4 = SineGenerator::new(Self::FREQ_C4, amp, sample_rate);
        let signal_c4 = MonoSignal::new(gen_c4.create_signal(), sample_rate, 0);

        let gen_e4 = SineGenerator::new(Self::FREQ_E4, amp, sample_rate);
        let signal_e4 = MonoSignal::new(gen_e4.create_signal(), sample_rate, 0);

        let gen_g4 = SineGenerator::new(Self::FREQ_G4, amp, sample_rate);
        let signal_g4 = MonoSignal::new(gen_g4.create_signal(), sample_rate, 0);

        Self {
            name: "chord_generator".to_string(),
            sample_rate,
            amplitude: amplitude.clamp(0.0, 1.0),
            signal_c4,
            signal_e4,
            signal_g4,
            buffer_size: 512,
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
                schema: MonoSignal::schema(),
                required: false,
                description: "C4 tone (261.63 Hz) - like microphone array channel 0".to_string(),
            },
            PortDescriptor {
                name: "tone_e4".to_string(),
                schema: MonoSignal::schema(),
                required: false,
                description: "E4 tone (329.63 Hz) - like microphone array channel 1".to_string(),
            },
            PortDescriptor {
                name: "tone_g4".to_string(),
                schema: MonoSignal::schema(),
                required: false,
                description: "G4 tone (392.00 Hz) - like microphone array channel 2".to_string(),
            },
        ]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

        // Recreate signals with correct sample rate
        let amp = self.amplitude as f32;
        self.signal_c4 = MonoSignal::new(
            Box::new(SineGenerator::new(Self::FREQ_C4, amp, self.sample_rate)),
            self.sample_rate,
            0,
        );
        self.signal_e4 = MonoSignal::new(
            Box::new(SineGenerator::new(Self::FREQ_E4, amp, self.sample_rate)),
            self.sample_rate,
            0,
        );
        self.signal_g4 = MonoSignal::new(
            Box::new(SineGenerator::new(Self::FREQ_G4, amp, self.sample_rate)),
            self.sample_rate,
            0,
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
        // Write the continuous signals to output ports
        // These are infinite streams that were created once in new()/start()
        // Each write provides the same continuous signal to downstream processors
        self.output_ports.tone_c4.write(self.signal_c4.clone());
        self.output_ports.tone_e4.write(self.signal_e4.clone());
        self.output_ports.tone_g4.write(self.signal_g4.clone());

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
            "tone_c4" | "tone_e4" | "tone_g4" => Some(self.output_ports.tone_c4.port_type()),
            _ => None,
        }
    }

    fn create_bus_for_output(&self, port_name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match port_name {
            "tone_c4" => Some(self.output_ports.tone_c4.get_or_create_bus() as Arc<dyn std::any::Any + Send + Sync>),
            "tone_e4" => Some(self.output_ports.tone_e4.get_or_create_bus() as Arc<dyn std::any::Any + Send + Sync>),
            "tone_g4" => Some(self.output_ports.tone_g4.get_or_create_bus() as Arc<dyn std::any::Any + Send + Sync>),
            _ => None,
        }
    }

    fn connect_bus_to_output(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if let Some(typed_bus) = bus.downcast::<Arc<dyn crate::core::bus::Bus<MonoSignal>>>().ok() {
            match port_name {
                "tone_c4" => { self.output_ports.tone_c4.set_bus(Arc::clone(&typed_bus)); true }
                "tone_e4" => { self.output_ports.tone_e4.set_bus(Arc::clone(&typed_bus)); true }
                "tone_g4" => { self.output_ports.tone_g4.set_bus(Arc::clone(&typed_bus)); true }
                _ => false,
            }
        } else {
            false
        }
    }
}
