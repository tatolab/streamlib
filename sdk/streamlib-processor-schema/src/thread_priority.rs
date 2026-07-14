// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Thread scheduling priority.
///
/// Serializes as `"realtime"` / `"high"` / `"normal"` so YAML manifests
/// can declare `scheduling: { priority: high }` without PascalCase noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThreadPriority {
    /// Real-time priority (SCHED_FIFO on Linux, time-constraint policy on Apple).
    #[serde(alias = "real_time", alias = "real-time", alias = "RealTime")]
    RealTime,
    /// Elevated priority (SCHED_RR on Linux/Apple).
    High,
    /// Default OS scheduling.
    #[default]
    Normal,
}

impl ThreadPriority {
    pub fn description(&self) -> &'static str {
        match self {
            ThreadPriority::RealTime => "Real-time (< 10ms latency, time-constrained)",
            ThreadPriority::High => "High priority (< 33ms latency, elevated)",
            ThreadPriority::Normal => "Normal priority (no strict latency)",
        }
    }

    pub fn latency_budget_ms(&self) -> Option<f64> {
        match self {
            ThreadPriority::RealTime => Some(10.0),
            ThreadPriority::High => Some(33.0),
            ThreadPriority::Normal => None,
        }
    }

    pub fn requires_realtime_safety(&self) -> bool {
        matches!(self, ThreadPriority::RealTime)
    }
}
