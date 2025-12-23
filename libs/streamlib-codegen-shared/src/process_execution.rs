// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

/// Determines how and when the runtime invokes your `process()` function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum ProcessExecution {
    /// Runtime calls `process()` continuously in a loop.
    Continuous {
        /// Minimum interval between `process()` calls in milliseconds.
        #[serde(default)]
        interval_ms: u32,
    },

    /// Runtime calls `process()` when upstream writes to any input port.
    #[default]
    Reactive,

    /// Runtime calls `process()` once, then you control timing.
    Manual,
}

impl ProcessExecution {
    /// Create a Continuous execution with default interval (as fast as possible).
    pub const fn continuous() -> Self {
        ProcessExecution::Continuous { interval_ms: 0 }
    }

    /// Create a Continuous execution with a specific interval.
    pub const fn continuous_with_interval(interval_ms: u32) -> Self {
        ProcessExecution::Continuous { interval_ms }
    }

    /// Create a Reactive execution (wake on input).
    pub const fn reactive() -> Self {
        ProcessExecution::Reactive
    }

    /// Create a Manual execution (you control timing).
    pub const fn manual() -> Self {
        ProcessExecution::Manual
    }

    /// Returns true if this is Continuous execution mode.
    pub fn is_continuous(&self) -> bool {
        matches!(self, ProcessExecution::Continuous { .. })
    }

    /// Returns true if this is Reactive execution mode.
    pub fn is_reactive(&self) -> bool {
        matches!(self, ProcessExecution::Reactive)
    }

    /// Returns true if this is Manual execution mode.
    pub fn is_manual(&self) -> bool {
        matches!(self, ProcessExecution::Manual)
    }

    /// Returns the interval in milliseconds for Continuous mode, or None for other modes.
    pub fn interval_ms(&self) -> Option<u32> {
        match self {
            ProcessExecution::Continuous { interval_ms } => Some(*interval_ms),
            _ => None,
        }
    }

    /// Returns a human-readable description of this execution mode.
    pub fn description(&self) -> String {
        match self {
            ProcessExecution::Continuous { interval_ms: 0 } => {
                "Continuous (runtime loops as fast as possible)".to_string()
            }
            ProcessExecution::Continuous { interval_ms } => {
                format!("Continuous (runtime loops every {}ms minimum)", interval_ms)
            }
            ProcessExecution::Reactive => {
                "Reactive (runtime calls process() when input data arrives)".to_string()
            }
            ProcessExecution::Manual => self.to_string(),
        }
    }
}

impl std::fmt::Display for ProcessExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessExecution::Continuous { interval_ms: 0 } => write!(f, "Continuous"),
            ProcessExecution::Continuous { interval_ms } => {
                write!(f, "Continuous({}ms)", interval_ms)
            }
            ProcessExecution::Reactive => write!(f, "Reactive"),
            ProcessExecution::Manual => write!(f, "Manual"),
        }
    }
}
