
use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LfoWaveform {
    Sine,

    Triangle,

    Square,

    Sawtooth,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EnvelopeStage {
    Attack,
    Decay,
    Sustain,
    Release,
    Idle,
}

#[derive(Debug, Clone)]
pub struct ParameterModulator {
    kind: ModulatorKind,
}

#[derive(Debug, Clone)]
enum ModulatorKind {
    Lfo {
        frequency: f64,
        waveform: LfoWaveform,
        phase_offset: f64,
    },

    Ramp {
        start_value: f64,
        end_value: f64,
        start_time: f64,
        duration: f64,
    },

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
    pub fn lfo(frequency: f64, waveform: LfoWaveform) -> Self {
        Self {
            kind: ModulatorKind::Lfo {
                frequency,
                waveform,
                phase_offset: 0.0,
            },
        }
    }

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
                            *stage = EnvelopeStage::Decay;
                            *stage_start_time = time;
                            1.0
                        } else {
                            elapsed / *attack_time
                        }
                    }

                    EnvelopeStage::Decay => {
                        if elapsed >= *decay_time {
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

        let v0 = lfo.sample(0.0);
        assert!((v0 - 0.5).abs() < 0.01);

        let v1 = lfo.sample(0.25);
        assert!((v1 - 1.0).abs() < 0.01);

        let v2 = lfo.sample(0.5);
        assert!((v2 - 0.5).abs() < 0.01);

        let v3 = lfo.sample(0.75);
        assert!((v3 - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_lfo_square() {
        let mut lfo = ParameterModulator::lfo(1.0, LfoWaveform::Square);

        assert_eq!(lfo.sample(0.0), 0.0);
        assert_eq!(lfo.sample(0.25), 0.0);

        assert_eq!(lfo.sample(0.5), 1.0);
        assert_eq!(lfo.sample(0.75), 1.0);
    }

    #[test]
    fn test_ramp() {
        let mut ramp = ParameterModulator::ramp(0.0, 1.0, 0.0, 2.0);

        assert_eq!(ramp.sample(-1.0), 0.0);

        assert_eq!(ramp.sample(0.0), 0.0);

        assert!((ramp.sample(1.0) - 0.5).abs() < 0.01);

        assert_eq!(ramp.sample(2.0), 1.0);

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

        assert_eq!(env.sample(0.0), 0.0);

        env.trigger(0.0);

        let v1 = env.sample(0.05);
        assert!(v1 > 0.0 && v1 < 1.0);

        let v2 = env.sample(0.1);
        assert!((v2 - 1.0).abs() < 0.01);

        let v3 = env.sample(0.2);
        assert!(v3 > 0.7 && v3 < 1.0);

        let v4 = env.sample(0.5);
        assert!((v4 - 0.7).abs() < 0.01);

        env.release(1.0);

        let v5 = env.sample(1.15);
        assert!(v5 > 0.0 && v5 < 0.7);

        let v6 = env.sample(2.0);
        assert_eq!(v6, 0.0);
    }
}
