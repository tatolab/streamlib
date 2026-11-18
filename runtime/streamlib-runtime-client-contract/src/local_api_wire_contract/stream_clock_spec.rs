// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The clock a stream's spec names, as its graph document writes it and `graph` answers it.

use serde::{Deserialize, Serialize};

/// Stream nanoseconds per step a scaled clock takes when its spec names none: one frame at
/// 60 Hz.
pub const DEFAULT_SCALED_STREAM_CLOCK_STEP_NS: u64 = 16_666_667;

fn default_scaled_stream_clock_step_ns() -> u64 {
    DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
}

/// The clock a stream's spec names: the graph document's `clock` key, tagged by its `mode`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamClockSpec {
    /// The machine's monotonic clock.
    #[default]
    Real,
    /// `base + scale × elapsed` on the machine's monotonic clock.
    Scaled {
        /// Stream seconds per machine second; greater than zero.
        #[cfg_attr(feature = "json-schema", schemars(extend("exclusiveMinimum" = 0)))]
        scale: f64,
        /// Stream nanoseconds per step when the scale runs behind the step barrier; at least one.
        #[serde(default = "default_scaled_stream_clock_step_ns")]
        #[cfg_attr(feature = "json-schema", schemars(range(min = 1)))]
        step_ns: u64,
    },
    /// Advances `step_ns` at a time, once every node finished the step.
    Stepped {
        /// Stream nanoseconds per step; at least one.
        #[cfg_attr(feature = "json-schema", schemars(range(min = 1)))]
        step_ns: u64,
        /// The multiple of real time steps are paced to, greater than zero; unpaced when absent
        /// or null.
        #[serde(default)]
        #[cfg_attr(feature = "json-schema", schemars(extend("exclusiveMinimum" = 0)))]
        pace: Option<f64>,
        /// The step after which the stream stops itself.
        #[serde(default)]
        stop_after_steps: Option<u64>,
    },
}

impl StreamClockSpec {
    /// Whether this is the machine's own clock.
    pub fn is_real(&self) -> bool {
        matches!(self, Self::Real)
    }

    /// The mode's name, as the graph document spells it.
    pub fn mode_name(&self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Scaled { .. } => "scaled",
            Self::Stepped { .. } => "stepped",
        }
    }
}
