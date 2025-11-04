use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::{AudioFrame, Result, StreamOutput, StreamError};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements, SCHEMA_AUDIO_FRAME};
use std::f64::consts::PI;
use std::sync::Arc;
use parking_lot::Mutex;
use serde::{Serialize, Deserialize};

#[cfg(target_os = "macos")]
use cpal::Stream;
#[cfg(target_os = "macos")]
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
    pub tone_c4: Arc<StreamOutput<AudioFrame>>,
    /// E4 (329.63 Hz) - Like mic channel 1
    pub tone_e4: Arc<StreamOutput<AudioFrame>>,
    /// G4 (392.00 Hz) - Like mic channel 2
    pub tone_g4: Arc<StreamOutput<AudioFrame>>,
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

    // Phase accumulators for each tone (maintained across frames)
    phase_c4: Arc<Mutex<f64>>,  // C4 at 261.63 Hz
    phase_e4: Arc<Mutex<f64>>,  // E4 at 329.63 Hz
    phase_g4: Arc<Mutex<f64>>,  // G4 at 392.00 Hz

    frame_number: Arc<Mutex<u64>>,
    buffer_size: usize,
    output_ports: ChordGeneratorOutputPorts,

    #[cfg(target_os = "macos")]
    stream: Option<Stream>,
    #[cfg(target_os = "macos")]
    stream_setup_done: bool,
}

unsafe impl Send for ChordGeneratorProcessor {}

impl ChordGeneratorProcessor {
    const FREQ_C4: f64 = 261.63;  // C4
    const FREQ_E4: f64 = 329.63;  // E4
    const FREQ_G4: f64 = 392.00;  // G4

    pub fn new(amplitude: f64) -> Self {
        Self {
            name: "chord_generator".to_string(),
            sample_rate: 48000,
            amplitude: amplitude.clamp(0.0, 1.0),
            phase_c4: Arc::new(Mutex::new(0.0)),
            phase_e4: Arc::new(Mutex::new(0.0)),
            phase_g4: Arc::new(Mutex::new(0.0)),
            frame_number: Arc::new(Mutex::new(0)),
            buffer_size: 512,
            output_ports: ChordGeneratorOutputPorts {
                tone_c4: Arc::new(StreamOutput::new("tone_c4")),
                tone_e4: Arc::new(StreamOutput::new("tone_e4")),
                tone_g4: Arc::new(StreamOutput::new("tone_g4")),
            },

            #[cfg(target_os = "macos")]
            stream: None,
            #[cfg(target_os = "macos")]
            stream_setup_done: false,
        }
    }

    pub fn output_ports(&mut self) -> &mut ChordGeneratorOutputPorts {
        &mut self.output_ports
    }

    /// Generate samples for a single tone (mono output)
    fn generate_tone_samples(
        phase: &mut f64,
        frequency: f64,
        amplitude: f64,
        sample_rate: u32,
        buffer_size: usize,
    ) -> Vec<f32> {
        let mut samples = Vec::with_capacity(buffer_size * 2); // Stereo (duplicated mono)
        let phase_increment = 2.0 * PI * frequency / sample_rate as f64;

        for _ in 0..buffer_size {
            let sample = (phase.sin() * amplitude) as f32;

            // Duplicate to stereo (both L and R get same sample)
            samples.push(sample);
            samples.push(sample);

            *phase += phase_increment;

            if *phase >= 2.0 * PI {
                *phase -= 2.0 * PI;
            }
        }

        samples
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
        vec![
            PortDescriptor {
                name: "tone_c4".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: false,
                description: "C4 tone (261.63 Hz) - like microphone array channel 0".to_string(),
            },
            PortDescriptor {
                name: "tone_e4".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: false,
                description: "E4 tone (329.63 Hz) - like microphone array channel 1".to_string(),
            },
            PortDescriptor {
                name: "tone_g4".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: false,
                description: "G4 tone (392.00 Hz) - like microphone array channel 2".to_string(),
            },
        ]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

        tracing::info!(
            "[ChordGeneratorProcessor] start() called: C major chord at {}Hz sample rate, {} samples/buffer",
            self.sample_rate,
            self.buffer_size
        );

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.stream = None;
        }
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
        #[cfg(target_os = "macos")]
        {
            if self.stream_setup_done {
                return Ok(());
            }

            tracing::info!("[ChordGeneratorProcessor] process() called - setting up CoreAudio stream");

            let amplitude = self.amplitude;
            let sample_rate = self.sample_rate;
            let buffer_size = self.buffer_size;

            let phase_c4 = Arc::clone(&self.phase_c4);
            let phase_e4 = Arc::clone(&self.phase_e4);
            let phase_g4 = Arc::clone(&self.phase_g4);
            let frame_number = Arc::clone(&self.frame_number);

            let output_c4 = Arc::clone(&self.output_ports.tone_c4);
            let output_e4 = Arc::clone(&self.output_ports.tone_e4);
            let output_g4 = Arc::clone(&self.output_ports.tone_g4);

            let setup = crate::apple::audio_utils::setup_audio_output(
                None,
                buffer_size,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

                    // Generate all 3 tones simultaneously (like a mic array capturing 3 channels)
                    let samples_c4 = {
                        let mut phase = phase_c4.lock();
                        Self::generate_tone_samples(&mut phase, Self::FREQ_C4, amplitude, sample_rate, buffer_size)
                    };

                    let samples_e4 = {
                        let mut phase = phase_e4.lock();
                        Self::generate_tone_samples(&mut phase, Self::FREQ_E4, amplitude, sample_rate, buffer_size)
                    };

                    let samples_g4 = {
                        let mut phase = phase_g4.lock();
                        Self::generate_tone_samples(&mut phase, Self::FREQ_G4, amplitude, sample_rate, buffer_size)
                    };

                    let frame_num = {
                        let mut frame_num = frame_number.lock();
                        let num = *frame_num;
                        *frame_num += 1;
                        num
                    };

                    // Write to all 3 output ports (like a mic array splitting channels)
                    output_c4.write(AudioFrame::new(samples_c4, timestamp_ns, frame_num, 2));
                    output_e4.write(AudioFrame::new(samples_e4, timestamp_ns, frame_num, 2));
                    output_g4.write(AudioFrame::new(samples_g4, timestamp_ns, frame_num, 2));

                    data.fill(0.0);
                },
            )?;

            setup.stream.play()
                .map_err(|e| StreamError::Configuration(format!("Failed to start stream: {}", e)))?;

            self.stream = Some(setup.stream);
            self.stream_setup_done = true;

            tracing::info!("[ChordGeneratorProcessor] CoreAudio stream started - hardware callback active");
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

            // Generate all 3 tones simultaneously (like a mic array capturing 3 channels)
            let samples_c4 = {
                let mut phase = self.phase_c4.lock();
                Self::generate_tone_samples(&mut phase, Self::FREQ_C4, self.amplitude, self.sample_rate, self.buffer_size)
            };

            let samples_e4 = {
                let mut phase = self.phase_e4.lock();
                Self::generate_tone_samples(&mut phase, Self::FREQ_E4, self.amplitude, self.sample_rate, self.buffer_size)
            };

            let samples_g4 = {
                let mut phase = self.phase_g4.lock();
                Self::generate_tone_samples(&mut phase, Self::FREQ_G4, self.amplitude, self.sample_rate, self.buffer_size)
            };

            let frame_num = {
                let mut frame_num = self.frame_number.lock();
                let num = *frame_num;
                *frame_num += 1;
                num
            };

            // Write to all 3 output ports (like a mic array splitting channels)
            self.output_ports.tone_c4.write(AudioFrame::new(samples_c4, timestamp_ns, frame_num, 2));
            self.output_ports.tone_e4.write(AudioFrame::new(samples_e4, timestamp_ns, frame_num, 2));
            self.output_ports.tone_g4.write(AudioFrame::new(samples_g4, timestamp_ns, frame_num, 2));

            Ok(())
        }
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        #[cfg(target_os = "macos")]
        {
            SchedulingConfig {
                mode: SchedulingMode::Pull,
                priority: ThreadPriority::RealTime,
                clock: ClockSource::Audio,
                provide_clock: true,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            SchedulingConfig {
                mode: SchedulingMode::Loop,
                priority: ThreadPriority::RealTime,
                clock: ClockSource::Audio,
                provide_clock: false,
            }
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

    fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::traits::PortConsumer> {
        let port = match port_name {
            "tone_c4" => &self.output_ports.tone_c4,
            "tone_e4" => &self.output_ports.tone_e4,
            "tone_g4" => &self.output_ports.tone_g4,
            _ => return None,
        };

        port.consumer_holder()
            .lock()
            .take()
            .map(|consumer| crate::core::traits::PortConsumer::Audio(consumer))
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        match port_name {
            "tone_c4" => self.output_ports.tone_c4.set_downstream_wakeup(wakeup_tx),
            "tone_e4" => self.output_ports.tone_e4.set_downstream_wakeup(wakeup_tx),
            "tone_g4" => self.output_ports.tone_g4.set_downstream_wakeup(wakeup_tx),
            _ => {},
        }
    }
}
