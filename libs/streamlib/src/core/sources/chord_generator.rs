use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
use crate::core::{Result, StreamOutput};
use crate::core::frames::AudioFrame;
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChordGeneratorConfig {
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
    pub tone_c4: Arc<StreamOutput<AudioFrame<1>>>,
    pub tone_e4: Arc<StreamOutput<AudioFrame<1>>>,
    pub tone_g4: Arc<StreamOutput<AudioFrame<1>>>,
}

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

pub struct ChordGeneratorProcessor {
    name: String,
    amplitude: f64,

    output_ports: ChordGeneratorOutputPorts,

    osc_c4: Arc<Mutex<SineOscillator>>,
    osc_e4: Arc<Mutex<SineOscillator>>,
    osc_g4: Arc<Mutex<SineOscillator>>,

    sample_rate: u32,
    buffer_size: usize,
    frame_counter: Arc<Mutex<u64>>,

    running: Arc<AtomicBool>,
    loop_handle: Option<std::thread::JoinHandle<()>>,
}

impl ChordGeneratorProcessor {
    const FREQ_C4: f64 = 261.63;  // C4
    const FREQ_E4: f64 = 329.63;  // E4
    const FREQ_G4: f64 = 392.00;  // G4

    pub fn new(amplitude: f64) -> Self {
        let sample_rate = 48000;
        let amp = amplitude.clamp(0.0, 1.0) as f32;

        Self {
            name: "chord_generator".to_string(),
            amplitude: amplitude.clamp(0.0, 1.0),
            output_ports: ChordGeneratorOutputPorts {
                tone_c4: Arc::new(StreamOutput::new("tone_c4")),
                tone_e4: Arc::new(StreamOutput::new("tone_e4")),
                tone_g4: Arc::new(StreamOutput::new("tone_g4")),
            },
            osc_c4: Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_C4, amp, sample_rate))),
            osc_e4: Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_E4, amp, sample_rate))),
            osc_g4: Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_G4, amp, sample_rate))),
            sample_rate,
            buffer_size: 128,
            frame_counter: Arc::new(Mutex::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            loop_handle: None,
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
        *self.frame_counter.lock().unwrap() = 0;

        let amp = self.amplitude as f32;
        self.osc_c4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_C4, amp, self.sample_rate)));
        self.osc_e4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_E4, amp, self.sample_rate)));
        self.osc_g4 = Arc::new(Mutex::new(SineOscillator::new(Self::FREQ_G4, amp, self.sample_rate)));

        tracing::info!(
            "ChordGenerator: start() called (Pull mode - {}Hz, {} samples buffer)",
            self.sample_rate,
            self.buffer_size
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.loop_handle.take() {
            let _ = handle.join();
        }
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

        if self.running.load(Ordering::Relaxed) {
            return Ok(());
        }

        tracing::info!("ChordGenerator: process() called - spawning audio generation thread");

        self.running.store(true, Ordering::Relaxed);

        let osc_c4 = Arc::clone(&self.osc_c4);
        let osc_e4 = Arc::clone(&self.osc_e4);
        let osc_g4 = Arc::clone(&self.osc_g4);
        let tone_c4_output = Arc::clone(&self.output_ports.tone_c4);
        let tone_e4_output = Arc::clone(&self.output_ports.tone_e4);
        let tone_g4_output = Arc::clone(&self.output_ports.tone_g4);
        let frame_counter = Arc::clone(&self.frame_counter);
        let running = Arc::clone(&self.running);
        let buffer_size = self.buffer_size;
        let sample_rate = self.sample_rate;

        let buffer_duration_us = (buffer_size as f64 / sample_rate as f64 * 1_000_000.0) as u64;

        tracing::info!(
            "ChordGenerator: Starting loop at {}Hz rate ({} us per buffer, buffer_size={}, sample_rate={})",
            sample_rate as f64 / buffer_size as f64,
            buffer_duration_us,
            buffer_size,
            sample_rate
        );

        let handle = std::thread::spawn(move || {
            use std::time::{Duration, Instant};

            let buffer_duration = Duration::from_micros(buffer_duration_us);
            let mut next_tick = Instant::now() + buffer_duration;
            let mut iteration_count = 0u64;

            while running.load(Ordering::Relaxed) {
                iteration_count += 1;
                tracing::debug!("ChordGenerator: Generation loop iteration {}", iteration_count);

                let mut osc_c4 = osc_c4.lock().unwrap();
                let mut osc_e4 = osc_e4.lock().unwrap();
                let mut osc_g4 = osc_g4.lock().unwrap();
                

                let mut samples_c4 = Vec::with_capacity(buffer_size);
                let mut samples_e4 = Vec::with_capacity(buffer_size);
                let mut samples_g4 = Vec::with_capacity(buffer_size);

                for _ in 0..buffer_size {
                    samples_c4.push(osc_c4.next());
                    samples_e4.push(osc_e4.next());
                    samples_g4.push(osc_g4.next());
                }

                drop(osc_c4);
                drop(osc_e4);
                drop(osc_g4);

                let timestamp_ns = crate::MediaClock::now().as_nanos() as i64;
                let counter = {
                    let mut c = frame_counter.lock().unwrap();
                    let val = *c;
                    *c += 1;
                    val
                };

                let frame_c4 = AudioFrame::<1>::new(samples_c4, timestamp_ns, counter);
                let frame_e4 = AudioFrame::<1>::new(samples_e4, timestamp_ns, counter);
                let frame_g4 = AudioFrame::<1>::new(samples_g4, timestamp_ns, counter);

                if iteration_count == 1 {
                    tracing::info!(
                        "ChordGenerator FIRST iteration: writing frames"
                    );
                }

                if iteration_count % 100 == 0 {
                    tracing::debug!(
                        "ChordGenerator iteration {}: Writing frames",
                        iteration_count
                    );
                }

                tone_c4_output.write(frame_c4);
                tone_e4_output.write(frame_e4);
                tone_g4_output.write(frame_g4);

                let now = Instant::now();
                if now < next_tick {
                    std::thread::sleep(next_tick - now);
                }
                next_tick += buffer_duration;
            }

            tracing::info!("ChordGenerator: Generation loop ended");
        });

        self.loop_handle = Some(handle);
        tracing::info!("ChordGenerator: Thread spawned successfully");
        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // Manages own loop
            priority: ThreadPriority::RealTime,
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
