use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::{Result, StreamOutput, StreamError};
use crate::core::frames::AudioFrame;
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements};
use std::sync::{Arc, Mutex};
use serde::{Serialize, Deserialize};
use cpal::Stream;
use cpal::traits::StreamTrait;

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

/// Sine wave oscillator state
struct SineOscillator {
    phase: f64,
    phase_inc: f64,
    amplitude: f32,
}

impl SineOscillator {
    fn new(frequency: f64, amplitude: f32, sample_rate: u32) -> Self {
        use std::f64::consts::PI;
        let phase_inc = 2.0 * PI * frequency / sample_rate as f64;
        Self {
            phase: 0.0,
            phase_inc,
            amplitude,
        }
    }

    fn next(&mut self) -> f32 {
        use std::f64::consts::PI;
        let sample = (self.phase.sin() * self.amplitude as f64) as f32;
        self.phase += self.phase_inc;
        if self.phase >= 2.0 * PI {
            self.phase -= 2.0 * PI;
        }
        sample
    }
}

/// ChordGeneratorProcessor - Emulates a 3-channel microphone array
///
/// Uses CoreAudio callback loop to generate tones hardware-synchronized,
/// mimicking how a real microphone array captures multiple channels simultaneously.
///
/// This demonstrates the microphone array pattern:
/// - Single hardware device (one clock, one callback)
/// - Multiple synchronized outputs (3 tones generated together)
/// - Each output can be routed independently downstream
pub struct ChordGeneratorProcessor {
    name: String,
    amplitude: f64,

    output_ports: ChordGeneratorOutputPorts,

    stream: Option<Stream>,
    stream_setup_done: bool,

    sample_rate: u32,
    buffer_size: usize,
}

impl ChordGeneratorProcessor {
    const FREQ_C4: f64 = 261.63;  // C4
    const FREQ_E4: f64 = 329.63;  // E4
    const FREQ_G4: f64 = 392.00;  // G4

    pub fn new(amplitude: f64) -> Self {
        Self {
            name: "chord_generator".to_string(),
            amplitude: amplitude.clamp(0.0, 1.0),
            output_ports: ChordGeneratorOutputPorts {
                tone_c4: Arc::new(StreamOutput::new("tone_c4")),
                tone_e4: Arc::new(StreamOutput::new("tone_e4")),
                tone_g4: Arc::new(StreamOutput::new("tone_g4")),
            },
            stream: None,
            stream_setup_done: false,
            sample_rate: 48000,
            buffer_size: 512,
        }
    }

    pub fn output_ports(&mut self) -> &mut ChordGeneratorOutputPorts {
        &mut self.output_ports
    }
}

unsafe impl Send for ChordGeneratorProcessor {}

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
        self.buffer_size = ctx.audio.buffer_size;
        self.sample_rate = ctx.audio.sample_rate;
        tracing::info!("ChordGenerator: start() called (Pull mode - buffer_size: {})", self.buffer_size);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Drop stream to stop generation
        self.stream = None;
        tracing::info!("ChordGenerator: Stopped");
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
        // In Pull mode, process() is called once by runtime after connections are wired
        // This is where we set up the stream and callback

        if self.stream_setup_done {
            // Already set up, nothing to do
            return Ok(());
        }

        tracing::info!("ChordGenerator: process() called - setting up stream now that connections are wired");

        // Clone output port connections for the callback to use
        let tone_c4_conn = self.output_ports.tone_c4.connections();
        let tone_e4_conn = self.output_ports.tone_e4.connections();
        let tone_g4_conn = self.output_ports.tone_g4.connections();

        let sample_rate = self.sample_rate;
        let amplitude = self.amplitude as f32;

        // Create oscillators (wrapped in Mutex for callback access)
        let osc_c4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_C4, amplitude, sample_rate)));
        let osc_e4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_E4, amplitude, sample_rate)));
        let osc_g4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_G4, amplitude, sample_rate)));

        let frame_counter = Arc::new(Mutex::new(0u64));

        tracing::info!("ChordGenerator: Setting up audio output with cpal");

        let setup = crate::apple::audio_utils::setup_audio_output(
            None, // Use default output device
            self.buffer_size,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                use crate::MediaClock;

                // Generate samples for each tone
                let mut osc_c4 = osc_c4.lock().unwrap();
                let mut osc_e4 = osc_e4.lock().unwrap();
                let mut osc_g4 = osc_g4.lock().unwrap();

                let buffer_frames = data.len() / 2; // stereo output
                let mut samples_c4 = Vec::with_capacity(buffer_frames);
                let mut samples_e4 = Vec::with_capacity(buffer_frames);
                let mut samples_g4 = Vec::with_capacity(buffer_frames);

                for _ in 0..buffer_frames {
                    samples_c4.push(osc_c4.next());
                    samples_e4.push(osc_e4.next());
                    samples_g4.push(osc_g4.next());
                }

                // Write samples to output device (stereo, so duplicate mono for L/R)
                for i in 0..buffer_frames {
                    let mixed = (samples_c4[i] + samples_e4[i] + samples_g4[i]) / 3.0;
                    data[i * 2] = mixed;     // L
                    data[i * 2 + 1] = mixed; // R
                }

                // Create AudioFrames and write to output ports
                let timestamp_ns = MediaClock::now().as_nanos() as i64;
                let mut counter = frame_counter.lock().unwrap();

                let frame_c4 = AudioFrame::<1>::new(samples_c4, timestamp_ns, *counter);
                let frame_e4 = AudioFrame::<1>::new(samples_e4, timestamp_ns, *counter);
                let frame_g4 = AudioFrame::<1>::new(samples_g4, timestamp_ns, *counter);

                // Write to all connections
                for conn in &tone_c4_conn {
                    conn.write(frame_c4.clone());
                }
                for conn in &tone_e4_conn {
                    conn.write(frame_e4.clone());
                }
                for conn in &tone_g4_conn {
                    conn.write(frame_g4.clone());
                }

                *counter += 1;
            }
        )?;

        // Start playback (this starts the callback loop)
        tracing::info!("ChordGenerator: Starting cpal stream");
        setup.stream.play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start stream: {}", e)))?;

        tracing::info!("ChordGenerator: cpal stream.play() succeeded");

        // Store stream
        self.stream = Some(setup.stream);
        self.sample_rate = setup.sample_rate;
        self.stream_setup_done = true;

        tracing::info!(
            "ChordGenerator: Stream setup complete ({}Hz, Pull mode)",
            self.sample_rate
        );

        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // Hardware callback drives execution
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,
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
            .with_tags(vec!["audio", "source", "generator", "multi-output", "chord", "test", "pull-mode", "real-time"])
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
