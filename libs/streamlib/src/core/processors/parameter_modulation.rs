//! Parameter Modulation Helpers
//!
//! Utilities for applying time-varying modulation to audio plugin parameters.
//! Designed for AI agents that need to dynamically control audio processing.
//!
//! # Use Cases
//!
//! - **Frequency scanning** - LFO sweep to search for specific frequencies
//! - **Adaptive filtering** - Dynamic cutoff modulation based on audio content
//! - **Gain envelopes** - Smooth fade in/out effects
//! - **Agent-driven effects** - Automated parameter animations
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{ParameterModulator, LfoWaveform};
//!
//! // Create LFO modulator for filter cutoff (1 Hz sine wave)
//! let mut lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);
//!
//! // In audio loop
//! loop {
//!     let time = get_current_time();
//!
//!     // Get LFO value (0.0 to 1.0)
//!     let lfo_value = lfo.sample(time);
//!
//!     // Map to frequency range (200 Hz to 2000 Hz)
//!     let cutoff = 200.0 + (lfo_value * 1800.0);
//!
//!     // Apply to plugin parameter
//!     filter.set_parameter(CUTOFF_PARAM, cutoff)?;
//!     filter.process_audio(&input)?;
//! }
//! ```

use std::f64::consts::PI;

/// LFO (Low Frequency Oscillator) waveform shapes
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LfoWaveform {
    /// Smooth sine wave (0.0 to 1.0)
    Sine,

    /// Triangle wave (linear rise and fall)
    Triangle,

    /// Square wave (instant switching between 0 and 1)
    Square,

    /// Sawtooth wave (linear rise, instant drop)
    Sawtooth,
}

/// ADSR envelope stage
#[derive(Debug, Clone, Copy, PartialEq)]
enum EnvelopeStage {
    Attack,
    Decay,
    Sustain,
    Release,
    Idle,
}

/// Parameter modulator for time-based parameter changes
///
/// Provides LFO, envelope, and ramp modulation for audio plugin parameters.
#[derive(Debug, Clone)]
pub struct ParameterModulator {
    kind: ModulatorKind,
}

#[derive(Debug, Clone)]
enum ModulatorKind {
    /// Low Frequency Oscillator
    Lfo {
        frequency: f64,
        waveform: LfoWaveform,
        phase_offset: f64,
    },

    /// Linear ramp from start to end value
    Ramp {
        start_value: f64,
        end_value: f64,
        start_time: f64,
        duration: f64,
    },

    /// ADSR envelope generator
    Envelope {
        attack_time: f64,
        decay_time: f64,
        sustain_level: f64,
        release_time: f64,
        stage: EnvelopeStage,
        stage_start_time: f64,
        trigger_time: Option<f64>,
        release_time_actual: Option<f64>,
    },
}

impl ParameterModulator {
    /// Create an LFO (Low Frequency Oscillator) modulator
    ///
    /// # Arguments
    ///
    /// * `frequency` - Oscillation frequency in Hz (e.g., 1.0 = one cycle per second)
    /// * `waveform` - LFO waveform shape (Sine, Triangle, Square, Sawtooth)
    ///
    /// # Returns
    ///
    /// Values from 0.0 to 1.0 when sampled
    ///
    /// # Example
    ///
    /// ```ignore
    /// // 0.5 Hz sine wave for slow filter sweep
    /// let lfo = ParameterModulator::lfo(0.5, LfoWaveform::Sine);
    /// ```
    pub fn lfo(frequency: f64, waveform: LfoWaveform) -> Self {
        Self {
            kind: ModulatorKind::Lfo {
                frequency,
                waveform,
                phase_offset: 0.0,
            },
        }
    }

    /// Create a linear ramp modulator
    ///
    /// Smoothly interpolates from start_value to end_value over duration.
    ///
    /// # Arguments
    ///
    /// * `start_value` - Starting value (0.0 to 1.0)
    /// * `end_value` - Ending value (0.0 to 1.0)
    /// * `start_time` - Time when ramp begins (seconds)
    /// * `duration` - Ramp duration (seconds)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Fade from 0 to 1 over 2 seconds, starting at t=0
    /// let ramp = ParameterModulator::ramp(0.0, 1.0, 0.0, 2.0);
    /// ```
    pub fn ramp(start_value: f64, end_value: f64, start_time: f64, duration: f64) -> Self {
        Self {
            kind: ModulatorKind::Ramp {
                start_value,
                end_value,
                start_time,
                duration,
            },
        }
    }

    /// Create an ADSR envelope modulator
    ///
    /// # Arguments
    ///
    /// * `attack_time` - Time to reach peak (seconds)
    /// * `decay_time` - Time to reach sustain level (seconds)
    /// * `sustain_level` - Sustain level (0.0 to 1.0)
    /// * `release_time` - Time to fade to zero after release (seconds)
    ///
    /// # Usage
    ///
    /// Call `trigger()` to start the envelope, `release()` to begin release phase.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Fast attack, medium decay, 70% sustain, slow release
    /// let mut env = ParameterModulator::envelope(0.01, 0.1, 0.7, 0.5);
    ///
    /// // Start envelope
    /// env.trigger(current_time);
    ///
    /// // Later... begin release
    /// env.release(current_time);
    /// ```
    pub fn envelope(
        attack_time: f64,
        decay_time: f64,
        sustain_level: f64,
        release_time: f64,
    ) -> Self {
        Self {
            kind: ModulatorKind::Envelope {
                attack_time,
                decay_time,
                sustain_level,
                release_time,
                stage: EnvelopeStage::Idle,
                stage_start_time: 0.0,
                trigger_time: None,
                release_time_actual: None,
            },
        }
    }

    /// Sample the modulator at a specific time
    ///
    /// # Arguments
    ///
    /// * `time` - Current time in seconds
    ///
    /// # Returns
    ///
    /// Modulation value (typically 0.0 to 1.0, depending on modulator type)
    pub fn sample(&mut self, time: f64) -> f64 {
        match &mut self.kind {
            ModulatorKind::Lfo {
                frequency,
                waveform,
                phase_offset,
            } => {
                let phase = (time * *frequency + *phase_offset) % 1.0;
                match waveform {
                    LfoWaveform::Sine => ((phase * 2.0 * PI).sin() + 1.0) / 2.0,
                    LfoWaveform::Triangle => {
                        if phase < 0.5 {
                            phase * 2.0
                        } else {
                            2.0 - (phase * 2.0)
                        }
                    }
                    LfoWaveform::Square => {
                        if phase < 0.5 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                    LfoWaveform::Sawtooth => phase,
                }
            }

            ModulatorKind::Ramp {
                start_value,
                end_value,
                start_time,
                duration,
            } => {
                let elapsed = time - *start_time;
                if elapsed < 0.0 {
                    *start_value
                } else if elapsed >= *duration {
                    *end_value
                } else {
                    let t = elapsed / *duration;
                    *start_value + (*end_value - *start_value) * t
                }
            }

            ModulatorKind::Envelope {
                attack_time,
                decay_time,
                sustain_level,
                release_time,
                stage,
                stage_start_time,
                trigger_time,
                release_time_actual,
            } => {
                if trigger_time.is_none() {
                    return 0.0;
                }

                let _trigger_t = trigger_time.unwrap();
                let elapsed = time - *stage_start_time;

                match stage {
                    EnvelopeStage::Attack => {
                        if elapsed >= *attack_time {
                            // Move to decay stage
                            *stage = EnvelopeStage::Decay;
                            *stage_start_time = time;
                            1.0
                        } else {
                            elapsed / *attack_time
                        }
                    }

                    EnvelopeStage::Decay => {
                        if elapsed >= *decay_time {
                            // Move to sustain stage
                            *stage = EnvelopeStage::Sustain;
                            *stage_start_time = time;
                            *sustain_level
                        } else {
                            let t = elapsed / *decay_time;
                            1.0 - (1.0 - *sustain_level) * t
                        }
                    }

                    EnvelopeStage::Sustain => *sustain_level,

                    EnvelopeStage::Release => {
                        if let Some(release_t) = release_time_actual {
                            let elapsed = time - *release_t;
                            if elapsed >= *release_time {
                                // Envelope complete
                                *stage = EnvelopeStage::Idle;
                                *trigger_time = None;
                                0.0
                            } else {
                                let t = elapsed / *release_time;
                                *sustain_level * (1.0 - t)
                            }
                        } else {
                            0.0
                        }
                    }

                    EnvelopeStage::Idle => 0.0,
                }
            }
        }
    }

    /// Trigger envelope (only for Envelope modulators)
    ///
    /// Starts the ADSR envelope from the beginning.
    ///
    /// # Arguments
    ///
    /// * `time` - Current time in seconds
    pub fn trigger(&mut self, time: f64) {
        if let ModulatorKind::Envelope {
            stage,
            stage_start_time,
            trigger_time,
            release_time_actual,
            ..
        } = &mut self.kind
        {
            *stage = EnvelopeStage::Attack;
            *stage_start_time = time;
            *trigger_time = Some(time);
            *release_time_actual = None;
        }
    }

    /// Release envelope (only for Envelope modulators)
    ///
    /// Begins the release phase of the ADSR envelope.
    ///
    /// # Arguments
    ///
    /// * `time` - Current time in seconds
    pub fn release(&mut self, time: f64) {
        if let ModulatorKind::Envelope {
            stage,
            stage_start_time,
            release_time_actual,
            ..
        } = &mut self.kind
        {
            *stage = EnvelopeStage::Release;
            *stage_start_time = time;
            *release_time_actual = Some(time);
        }
    }

    /// Set LFO phase offset (only for LFO modulators)
    ///
    /// Allows starting the LFO at a specific phase.
    ///
    /// # Arguments
    ///
    /// * `offset` - Phase offset (0.0 to 1.0, where 1.0 = full cycle)
    pub fn set_phase_offset(&mut self, offset: f64) {
        if let ModulatorKind::Lfo { phase_offset, .. } = &mut self.kind {
            *phase_offset = offset % 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lfo_sine() {
        let mut lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);

        // At t=0, sine should be at middle (0.5)
        let v0 = lfo.sample(0.0);
        assert!((v0 - 0.5).abs() < 0.01);

        // At t=0.25, sine should be at peak (1.0)
        let v1 = lfo.sample(0.25);
        assert!((v1 - 1.0).abs() < 0.01);

        // At t=0.5, sine should be at middle (0.5)
        let v2 = lfo.sample(0.5);
        assert!((v2 - 0.5).abs() < 0.01);

        // At t=0.75, sine should be at trough (0.0)
        let v3 = lfo.sample(0.75);
        assert!((v3 - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_lfo_square() {
        let mut lfo = ParameterModulator::lfo(1.0, LfoWaveform::Square);

        // First half of cycle should be 0
        assert_eq!(lfo.sample(0.0), 0.0);
        assert_eq!(lfo.sample(0.25), 0.0);

        // Second half should be 1
        assert_eq!(lfo.sample(0.5), 1.0);
        assert_eq!(lfo.sample(0.75), 1.0);
    }

    #[test]
    fn test_ramp() {
        let mut ramp = ParameterModulator::ramp(0.0, 1.0, 0.0, 2.0);

        // Before start
        assert_eq!(ramp.sample(-1.0), 0.0);

        // At start
        assert_eq!(ramp.sample(0.0), 0.0);

        // Halfway
        assert!((ramp.sample(1.0) - 0.5).abs() < 0.01);

        // At end
        assert_eq!(ramp.sample(2.0), 1.0);

        // After end
        assert_eq!(ramp.sample(3.0), 1.0);
    }

    #[test]
    fn test_envelope_adsr() {
        let mut env = ParameterModulator::envelope(
            0.1,  // attack
            0.2,  // decay
            0.7,  // sustain
            0.3,  // release
        );

        // Before trigger
        assert_eq!(env.sample(0.0), 0.0);

        // Trigger envelope
        env.trigger(0.0);

        // During attack (should rise to 1.0)
        let v1 = env.sample(0.05);
        assert!(v1 > 0.0 && v1 < 1.0);

        // At end of attack
        let v2 = env.sample(0.1);
        assert!((v2 - 1.0).abs() < 0.01);

        // During decay (should fall to sustain)
        let v3 = env.sample(0.2);
        assert!(v3 > 0.7 && v3 < 1.0);

        // At sustain
        let v4 = env.sample(0.5);
        assert!((v4 - 0.7).abs() < 0.01);

        // Release envelope
        env.release(1.0);

        // During release (should fall to 0)
        let v5 = env.sample(1.15);
        assert!(v5 > 0.0 && v5 < 0.7);

        // After release complete
        let v6 = env.sample(2.0);
        assert_eq!(v6, 0.0);
    }
}
