// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::Audioframe;
use crate::core::context::AudioTickContext;
use crate::core::{Result, RuntimeContext};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

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

/// Shared oscillator state for audio clock callback.
struct OscillatorState {
    osc_c4: SineOscillator,
    osc_e4: SineOscillator,
    osc_g4: SineOscillator,
}

#[crate::processor("com.tatolab.chord_generator")]
pub struct ChordGeneratorProcessor {
    /// Shared oscillator state for the audio clock callback.
    oscillators: Arc<Mutex<Option<OscillatorState>>>,
    /// Frame counter for output frames.
    frame_counter: Arc<AtomicU64>,
    /// Sample rate from the audio clock.
    sample_rate: u32,
    /// Flag to indicate if audio generation is active.
    is_active: Arc<AtomicBool>,
    /// Runtime context stored from setup for use in start.
    runtime_ctx: Option<RuntimeContext>,
}

impl ChordGeneratorProcessor::Processor {
    const FREQ_C4: f64 = 261.63;
    const FREQ_E4: f64 = 329.63;
    const FREQ_G4: f64 = 392.00;
}

impl crate::core::ManualProcessor for ChordGeneratorProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        // Get sample rate from the audio clock
        let audio_clock = ctx.audio_clock();
        self.sample_rate = audio_clock.sample_rate();

        // Initialize oscillators with amplitude from config, sample rate from clock
        let amp = self.config.amplitude as f32;
        let oscillators = OscillatorState {
            osc_c4: SineOscillator::new(Self::FREQ_C4, amp, self.sample_rate),
            osc_e4: SineOscillator::new(Self::FREQ_E4, amp, self.sample_rate),
            osc_g4: SineOscillator::new(Self::FREQ_G4, amp, self.sample_rate),
        };
        *self.oscillators.lock() = Some(oscillators);
        self.frame_counter.store(0, Ordering::SeqCst);

        // Store context for use in start()
        self.runtime_ctx = Some(ctx.clone());

        tracing::info!(
            "ChordGenerator: setup() called (Manual mode with AudioClock - {}Hz, {} samples/tick)",
            self.sample_rate,
            audio_clock.buffer_size()
        );

        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        // Mark as inactive to stop generating
        self.is_active.store(false, Ordering::SeqCst);
        self.runtime_ctx = None;
        tracing::info!("ChordGenerator: teardown complete");
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        self.is_active.store(true, Ordering::SeqCst);

        // Get the audio clock from stored context
        let ctx = self.runtime_ctx.as_ref().ok_or_else(|| {
            crate::core::StreamError::Runtime("RuntimeContext not available in start()".into())
        })?;
        let audio_clock = ctx.audio_clock();
        let sample_rate = self.sample_rate;

        // Clone Arc references for the callback
        let oscillators = Arc::clone(&self.oscillators);
        let frame_counter = Arc::clone(&self.frame_counter);
        let is_active = Arc::clone(&self.is_active);

        // Clone the outputs Arc for use in the callback
        let outputs = Arc::clone(&self.outputs);

        // Register callback with the audio clock
        audio_clock.on_tick(Box::new(move |tick: AudioTickContext| {
            // Check if we're still active
            if !is_active.load(Ordering::SeqCst) {
                return;
            }

            // Lock oscillators and generate samples
            let mut osc_guard = oscillators.lock();
            if let Some(ref mut osc) = *osc_guard {
                let samples_needed = tick.samples_needed;
                let mut stereo_samples = Vec::with_capacity(samples_needed * 2);

                for _ in 0..samples_needed {
                    let sample_c4 = osc.osc_c4.next();
                    let sample_e4 = osc.osc_e4.next();
                    let sample_g4 = osc.osc_g4.next();
                    let mixed = sample_c4 + sample_e4 + sample_g4;
                    stereo_samples.push(mixed);
                    stereo_samples.push(mixed);
                }

                let counter = frame_counter.fetch_add(1, Ordering::SeqCst);

                let chord_frame = Audioframe {
                    samples: stereo_samples,
                    channels: 2,
                    sample_rate,
                    timestamp_ns: tick.timestamp_ns.to_string(),
                    frame_index: counter.to_string(),
                };

                if counter == 0 {
                    tracing::info!(
                        "ChordGenerator: First audio clock tick - generating {} samples",
                        samples_needed
                    );
                }

                if let Err(e) = outputs.write("chord", &chord_frame) {
                    tracing::error!("ChordGenerator: Failed to write frame: {}", e);
                }
            }
        }));

        tracing::info!("ChordGenerator: Registered with audio clock");
        Ok(())
    }
}
