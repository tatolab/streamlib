use crate::core::frames::{AudioChannelCount, AudioFrame};
use crate::core::{LinkOutput, Result, RuntimeContext};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChordGeneratorConfig {
    pub amplitude: f64,
    pub sample_rate: u32,
    pub buffer_size: usize,
}

impl Default for ChordGeneratorConfig {
    fn default() -> Self {
        Self {
            amplitude: 0.15,
            sample_rate: 48000,
            buffer_size: 512,
        }
    }
}

struct SineOscillator {
    phase: f64,
    phase_inc: f64,
    amplitude: f32,
}

impl Default for SineOscillator {
    fn default() -> Self {
        Self {
            phase: 0.0,
            phase_inc: 0.0,
            amplitude: 0.0,
        }
    }
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

#[crate::processor(
    execution = Manual,
    description = "Generates a C major chord (C4, E4, G4) pre-mixed into a stereo output",
    unsafe_send
)]
pub struct ChordGeneratorProcessor {
    #[crate::output(description = "Stereo C Major chord (C4 + E4 + G4 mixed to both channels)")]
    chord: LinkOutput<AudioFrame>,

    #[crate::config]
    config: ChordGeneratorConfig,

    osc_c4: Arc<Mutex<SineOscillator>>,
    osc_e4: Arc<Mutex<SineOscillator>>,
    osc_g4: Arc<Mutex<SineOscillator>>,
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: Arc<Mutex<u64>>,
    running: Arc<AtomicBool>,
    loop_handle: Option<std::thread::JoinHandle<()>>,
}

impl ChordGeneratorProcessor::Processor {
    const FREQ_C4: f64 = 261.63;
    const FREQ_E4: f64 = 329.63;
    const FREQ_G4: f64 = 392.00;

    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        self.buffer_size = self.config.buffer_size;
        self.sample_rate = self.config.sample_rate;
        *self.frame_counter.lock().unwrap() = 0;

        let amp = self.config.amplitude as f32;
        self.osc_c4 = Arc::new(Mutex::new(SineOscillator::new(
            Self::FREQ_C4,
            amp,
            self.sample_rate,
        )));
        self.osc_e4 = Arc::new(Mutex::new(SineOscillator::new(
            Self::FREQ_E4,
            amp,
            self.sample_rate,
        )));
        self.osc_g4 = Arc::new(Mutex::new(SineOscillator::new(
            Self::FREQ_G4,
            amp,
            self.sample_rate,
        )));

        tracing::info!(
            "ChordGenerator: start() called (Pull mode - {}Hz, {} samples buffer)",
            self.sample_rate,
            self.buffer_size
        );
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.loop_handle.take() {
            let _ = handle.join();
        }
        tracing::info!("ChordGenerator: Stopped");
        Ok(())
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
        let chord_output = self.chord.clone();
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
                tracing::debug!(
                    "ChordGenerator: Generation loop iteration {}",
                    iteration_count
                );

                let mut osc_c4 = osc_c4.lock().unwrap();
                let mut osc_e4 = osc_e4.lock().unwrap();
                let mut osc_g4 = osc_g4.lock().unwrap();

                let mut stereo_samples = Vec::with_capacity(buffer_size * 2);

                for _ in 0..buffer_size {
                    let sample_c4 = osc_c4.next();
                    let sample_e4 = osc_e4.next();
                    let sample_g4 = osc_g4.next();
                    let mixed = sample_c4 + sample_e4 + sample_g4;
                    stereo_samples.push(mixed);
                    stereo_samples.push(mixed);
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

                let chord_frame = AudioFrame::new(
                    stereo_samples,
                    AudioChannelCount::Two,
                    timestamp_ns,
                    counter,
                    sample_rate,
                );

                if iteration_count == 1 {
                    tracing::info!("ChordGenerator FIRST iteration: writing stereo chord frame");
                }

                if iteration_count.is_multiple_of(100) {
                    tracing::debug!(
                        "ChordGenerator iteration {}: Writing stereo chord frame",
                        iteration_count
                    );
                }

                chord_output.write(chord_frame);

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
}
