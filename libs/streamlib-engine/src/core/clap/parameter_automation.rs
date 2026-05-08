// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::parameter_modulation::ParameterModulator;
use crate::core::Result;
use std::collections::HashMap;

pub trait ClapParameterControl {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;

    fn begin_edit(&mut self, id: u32) -> Result<()>;

    fn end_edit(&mut self, id: u32) -> Result<()>;
}

#[derive(Debug, Clone)]
struct ScheduledChange {
    time: f64,

    param_id: u32,

    value: f64,
}

#[derive(Debug, Clone)]
struct ActiveModulator {
    param_id: u32,

    modulator: ParameterModulator,

    start_time: f64,

    end_time: Option<f64>,

    range: (f64, f64),
}

pub struct ParameterAutomation {
    scheduled_changes: Vec<ScheduledChange>,

    active_modulators: Vec<ActiveModulator>,

    last_time: f64,
}

impl ParameterAutomation {
    pub fn new() -> Self {
        Self {
            scheduled_changes: Vec::new(),
            active_modulators: Vec::new(),
            last_time: 0.0,
        }
    }

    pub fn schedule(&mut self, time: f64, param_id: u32, value: f64) {
        self.scheduled_changes.push(ScheduledChange {
            time,
            param_id,
            value,
        });

        self.scheduled_changes.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    pub fn add_modulator(
        &mut self,
        param_id: u32,
        modulator: ParameterModulator,
        start_time: f64,
        end_time: Option<f64>,
        min_value: f64,
        max_value: f64,
    ) {
        self.active_modulators.push(ActiveModulator {
            param_id,
            modulator,
            start_time,
            end_time,
            range: (min_value, max_value),
        });
    }

    pub fn update<P>(&mut self, time: f64, processor: &mut P) -> Result<usize>
    where
        P: ClapParameterControl,
    {
        let mut updates = 0;

        while !self.scheduled_changes.is_empty() {
            if self.scheduled_changes[0].time <= time {
                let change = self.scheduled_changes.remove(0);
                processor.set_parameter(change.param_id, change.value)?;
                updates += 1;
            } else {
                break;
            }
        }

        let mut param_updates: HashMap<u32, f64> = HashMap::new();

        for modulator_state in &mut self.active_modulators {
            if time < modulator_state.start_time {
                continue;
            }

            if let Some(end_time) = modulator_state.end_time {
                if time >= end_time {
                    continue;
                }
            }

            let mod_value = modulator_state.modulator.sample(time);

            let (min, max) = modulator_state.range;
            let param_value = min + (mod_value * (max - min));

            param_updates.insert(modulator_state.param_id, param_value);
        }

        for (param_id, value) in param_updates {
            processor.begin_edit(param_id)?;
            processor.set_parameter(param_id, value)?;
            processor.end_edit(param_id)?;
            updates += 1;
        }

        self.active_modulators.retain(|m| {
            if let Some(end_time) = m.end_time {
                time < end_time
            } else {
                true
            }
        });

        self.last_time = time;

        Ok(updates)
    }

    pub fn clear(&mut self) {
        self.scheduled_changes.clear();
        self.active_modulators.clear();
    }

    pub fn pending_changes(&self) -> usize {
        self.scheduled_changes.len()
    }

    pub fn active_modulators(&self) -> usize {
        self.active_modulators.len()
    }

    pub fn clear_parameter(&mut self, param_id: u32) {
        self.scheduled_changes.retain(|c| c.param_id != param_id);
        self.active_modulators.retain(|m| m.param_id != param_id);
    }
}

impl Default for ParameterAutomation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::parameter_modulation::LfoWaveform;
    use super::*;

    #[allow(dead_code)]
    struct MockProcessor {
        parameters: HashMap<u32, f64>,
    }

    #[allow(dead_code)]
    impl MockProcessor {
        fn new() -> Self {
            Self {
                parameters: HashMap::new(),
            }
        }

        fn get(&self, id: u32) -> Option<f64> {
            self.parameters.get(&id).copied()
        }
    }

    #[test]
    fn test_schedule_ordering() {
        let mut automation = ParameterAutomation::new();

        automation.schedule(2.0, 1, 0.5);
        automation.schedule(0.5, 1, 0.1);
        automation.schedule(1.0, 1, 0.3);

        assert_eq!(automation.scheduled_changes[0].time, 0.5);
        assert_eq!(automation.scheduled_changes[1].time, 1.0);
        assert_eq!(automation.scheduled_changes[2].time, 2.0);
    }

    #[test]
    fn test_add_modulator() {
        let mut automation = ParameterAutomation::new();

        let lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);
        automation.add_modulator(1, lfo, 0.0, Some(5.0), 0.0, 1.0);

        assert_eq!(automation.active_modulators(), 1);
    }

    #[test]
    fn test_clear() {
        let mut automation = ParameterAutomation::new();

        automation.schedule(1.0, 1, 0.5);
        let lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);
        automation.add_modulator(1, lfo, 0.0, None, 0.0, 1.0);

        automation.clear();

        assert_eq!(automation.pending_changes(), 0);
        assert_eq!(automation.active_modulators(), 0);
    }

    #[test]
    fn test_clear_parameter() {
        let mut automation = ParameterAutomation::new();

        automation.schedule(1.0, 1, 0.5);
        automation.schedule(2.0, 2, 0.7);

        let lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);
        automation.add_modulator(1, lfo, 0.0, None, 0.0, 1.0);

        automation.clear_parameter(1);

        assert_eq!(automation.pending_changes(), 1);
        assert_eq!(automation.scheduled_changes[0].param_id, 2);
    }
}
