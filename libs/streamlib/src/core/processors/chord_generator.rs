// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, RuntimeContext, StreamError};
use crate::schemas::Audioframe2ch;

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

#[crate::processor("src/core/processors/chord_generator.yaml")]
pub struct ChordGeneratorProcessor {
    osc_c4: SineOscillator,
    osc_e4: SineOscillator,
    osc_g4: SineOscillator,
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: u64,
}

impl ChordGeneratorProcessor::Processor {
    const FREQ_C4: f64 = 261.63;
    const FREQ_E4: f64 = 329.63;
    const FREQ_G4: f64 = 392.00;
}

impl crate::core::ContinuousProcessor for ChordGeneratorProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.buffer_size = self.config.buffer_size;
        self.sample_rate = self.config.sample_rate;
        self.frame_counter = 0;

        let amp = self.config.amplitude as f32;
        self.osc_c4 = SineOscillator::new(Self::FREQ_C4, amp, self.sample_rate);
        self.osc_e4 = SineOscillator::new(Self::FREQ_E4, amp, self.sample_rate);
        self.osc_g4 = SineOscillator::new(Self::FREQ_G4, amp, self.sample_rate);

        tracing::info!(
            "ChordGenerator: setup() called (Continuous mode - {}Hz, {} samples buffer)",
            self.sample_rate,
            self.buffer_size
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("ChordGenerator: teardown complete");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        // Generate one buffer of audio samples
        let mut stereo_samples = Vec::with_capacity(self.buffer_size * 2);

        for _ in 0..self.buffer_size {
            let sample_c4 = self.osc_c4.next();
            let sample_e4 = self.osc_e4.next();
            let sample_g4 = self.osc_g4.next();
            let mixed = sample_c4 + sample_e4 + sample_g4;
            stereo_samples.push(mixed);
            stereo_samples.push(mixed);
        }

        let timestamp_ns = crate::MediaClock::now().as_nanos() as i64;
        let counter = self.frame_counter;
        self.frame_counter += 1;

        let chord_frame = Audioframe2ch {
            samples: stereo_samples,
            sample_rate: self.sample_rate,
            timestamp_ns,
            frame_index: counter,
        };

        if counter == 0 {
            tracing::info!("ChordGenerator FIRST iteration: writing stereo chord frame");
        }

        if counter.is_multiple_of(100) && counter > 0 {
            tracing::debug!(
                "ChordGenerator iteration {}: Writing stereo chord frame",
                counter
            );
        }

        let bytes = chord_frame
            .to_msgpack()
            .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
        self.outputs.write("chord", &bytes)?;

        Ok(())
    }
}
