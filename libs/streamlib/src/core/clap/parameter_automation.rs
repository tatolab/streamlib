//! Parameter Automation Scheduler
//!
//! Schedule parameter changes over time and apply modulators automatically.
//! Designed for AI agents that need to orchestrate complex audio processing.
//!
//! # Use Cases
//!
//! - **Timed parameter changes** - Schedule specific values at specific times
//! - **Modulation automation** - Apply LFOs/envelopes to parameters
//! - **Effect sequences** - Orchestrate multi-parameter animations
//! - **Agent-driven audio** - Automate parameter control based on sensor data
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{ParameterAutomation, ParameterModulator, LfoWaveform};
//!
//! let mut automation = ParameterAutomation::new();
//!
//! // Schedule a filter sweep starting at t=1.0
//! let cutoff_lfo = ParameterModulator::lfo(0.5, LfoWaveform::Sine);
//! automation.add_modulator(CUTOFF_PARAM, cutoff_lfo, 1.0, Some(5.0));
//!
//! // Schedule bypass disable at t=0.5
//! automation.schedule(0.5, BYPASS_PARAM, 0.0);
//!
//! // In audio loop
//! loop {
//!     let time = get_current_time();
//!     automation.update(time, &mut filter)?;
//!     let output = filter.process_audio(&input)?;
//! }
//! ```

use crate::core::Result;
use super::parameter_modulation::ParameterModulator;
use std::collections::HashMap;

/// Trait for CLAP processors that support parameter control
///
/// This is automatically implemented by ClapEffectProcessor and allows
/// ParameterAutomation to work with it.
pub trait ClapParameterControl {
    /// Set a parameter value
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;

    /// Begin parameter edit transaction
    fn begin_edit(&mut self, id: u32) -> Result<()>;

    /// End parameter edit transaction
    fn end_edit(&mut self, id: u32) -> Result<()>;
}

/// Scheduled parameter change at a specific time
#[derive(Debug, Clone)]
struct ScheduledChange {
    /// Time when change should occur (seconds)
    time: f64,

    /// Parameter ID
    param_id: u32,

    /// Target value
    value: f64,
}

/// Active parameter modulator
#[derive(Debug, Clone)]
struct ActiveModulator {
    /// Parameter ID being modulated
    param_id: u32,

    /// Modulator instance
    modulator: ParameterModulator,

    /// Start time (seconds)
    start_time: f64,

    /// Optional end time (seconds) - None means run forever
    end_time: Option<f64>,

    /// Value range for modulation (min, max)
    /// Modulator output (0.0-1.0) is mapped to this range
    range: (f64, f64),
}

/// Parameter automation scheduler
///
/// Orchestrates scheduled parameter changes and modulation over time.
/// Works with any AudioEffectProcessor implementation.
pub struct ParameterAutomation {
    /// Scheduled parameter changes (sorted by time)
    scheduled_changes: Vec<ScheduledChange>,

    /// Active modulators
    active_modulators: Vec<ActiveModulator>,

    /// Last processed time (to avoid reprocessing)
    last_time: f64,
}

impl ParameterAutomation {
    /// Create a new parameter automation scheduler
    pub fn new() -> Self {
        Self {
            scheduled_changes: Vec::new(),
            active_modulators: Vec::new(),
            last_time: 0.0,
        }
    }

    /// Schedule a parameter change at a specific time
    ///
    /// # Arguments
    ///
    /// * `time` - When to apply the change (seconds)
    /// * `param_id` - Parameter ID to change
    /// * `value` - Target value
    ///
    /// # Example
    ///
    /// ```ignore
    /// // At 2.5 seconds, set bypass parameter to 0 (disable bypass)
    /// automation.schedule(2.5, BYPASS_PARAM, 0.0);
    /// ```
    pub fn schedule(&mut self, time: f64, param_id: u32, value: f64) {
        self.scheduled_changes.push(ScheduledChange {
            time,
            param_id,
            value,
        });

        // Keep sorted by time for efficient processing
        self.scheduled_changes.sort_by(|a, b| {
            a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Add a modulator to a parameter
    ///
    /// The modulator will run from `start_time` to optional `end_time`.
    /// Modulator output (0.0-1.0) is mapped to the specified value range.
    ///
    /// # Arguments
    ///
    /// * `param_id` - Parameter ID to modulate
    /// * `modulator` - Modulator instance (LFO, envelope, etc.)
    /// * `start_time` - When to start modulation (seconds)
    /// * `end_time` - When to stop modulation (None = run forever)
    /// * `min_value` - Minimum parameter value
    /// * `max_value` - Maximum parameter value
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Apply 1 Hz sine wave to filter cutoff (200-2000 Hz) starting at t=1.0
    /// let lfo = ParameterModulator::lfo(1.0, LfoWaveform::Sine);
    /// automation.add_modulator(CUTOFF_PARAM, lfo, 1.0, None, 200.0, 2000.0);
    /// ```
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

    /// Update parameters based on current time
    ///
    /// Processes all scheduled changes and applies active modulators.
    /// Call this in your audio loop with the current time.
    ///
    /// Works with any CLAP plugin processor that has:
    /// - `set_parameter(id, value)` method
    /// - `begin_edit(id)` method
    /// - `end_edit(id)` method
    ///
    /// # Arguments
    ///
    /// * `time` - Current time (seconds)
    /// * `processor` - CLAP effect processor to update
    ///
    /// # Returns
    ///
    /// Number of parameters updated
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib::ClapEffectProcessor;
    ///
    /// let mut plugin = ClapEffectProcessor::load("plugin.clap")?;
    /// let updates = automation.update(current_time, &mut plugin)?;
    /// if updates > 0 {
    ///     tracing::debug!("Updated {} parameters", updates);
    /// }
    /// ```
    pub fn update<P>(
        &mut self,
        time: f64,
        processor: &mut P,
    ) -> Result<usize>
    where
        P: ClapParameterControl,
    {
        let mut updates = 0;

        // Process scheduled changes that are due
        while !self.scheduled_changes.is_empty() {
            if self.scheduled_changes[0].time <= time {
                let change = self.scheduled_changes.remove(0);
                processor.set_parameter(change.param_id, change.value)?;
                updates += 1;
            } else {
                break;
            }
        }

        // Apply active modulators
        // Group updates by parameter ID to support transactions
        let mut param_updates: HashMap<u32, f64> = HashMap::new();

        for modulator_state in &mut self.active_modulators {
            // Check if modulator is active at this time
            if time < modulator_state.start_time {
                continue;
            }

            if let Some(end_time) = modulator_state.end_time {
                if time >= end_time {
                    continue;
                }
            }

            // Sample modulator
            let mod_value = modulator_state.modulator.sample(time);

            // Map to parameter range
            let (min, max) = modulator_state.range;
            let param_value = min + (mod_value * (max - min));

            // Store update (will be applied with transactions)
            param_updates.insert(modulator_state.param_id, param_value);
        }

        // Apply modulator updates with transactions
        for (param_id, value) in param_updates {
            processor.begin_edit(param_id)?;
            processor.set_parameter(param_id, value)?;
            processor.end_edit(param_id)?;
            updates += 1;
        }

        // Remove expired modulators
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

    /// Clear all scheduled changes and modulators
    pub fn clear(&mut self) {
        self.scheduled_changes.clear();
        self.active_modulators.clear();
    }

    /// Get number of pending scheduled changes
    pub fn pending_changes(&self) -> usize {
        self.scheduled_changes.len()
    }

    /// Get number of active modulators
    pub fn active_modulators(&self) -> usize {
        self.active_modulators.len()
    }

    /// Remove all scheduled changes for a specific parameter
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
    use super::*;
    use super::super::parameter_modulation::LfoWaveform;

    // Mock processor for testing
    struct MockProcessor {
        parameters: HashMap<u32, f64>,
    }

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

    // Note: This is a minimal mock - real tests would use an actual processor
    // For now, we'll test the scheduling logic

    #[test]
    fn test_schedule_ordering() {
        let mut automation = ParameterAutomation::new();

        // Add changes out of order
        automation.schedule(2.0, 1, 0.5);
        automation.schedule(0.5, 1, 0.1);
        automation.schedule(1.0, 1, 0.3);

        // Check they're sorted
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

        // Clear parameter 1
        automation.clear_parameter(1);

        // Parameter 2 should still be there
        assert_eq!(automation.pending_changes(), 1);
        assert_eq!(automation.scheduled_changes[0].param_id, 2);
    }
}
