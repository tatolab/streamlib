// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::ProcessExecution;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExecutionConfig {
    /// How and when `process()` is called.
    pub execution: ProcessExecution,
}

impl ExecutionConfig {
    /// Create a new execution config with the given execution mode.
    pub fn new(execution: ProcessExecution) -> Self {
        Self { execution }
    }

    /// Create a Continuous execution config (runtime loops, calling process() repeatedly).
    pub fn continuous() -> Self {
        Self::new(ProcessExecution::continuous())
    }

    /// Create a Continuous execution config with a specific interval.
    pub fn continuous_with_interval(interval_ms: u32) -> Self {
        Self::new(ProcessExecution::continuous_with_interval(interval_ms))
    }

    /// Create a Reactive execution config (process() called when input arrives).
    pub fn reactive() -> Self {
        Self::new(ProcessExecution::reactive())
    }

    /// Create a Manual execution config (you control when process() is called).
    pub fn manual() -> Self {
        Self::new(ProcessExecution::manual())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        for cfg in [
            ExecutionConfig::default(),
            ExecutionConfig::continuous(),
            ExecutionConfig::continuous_with_interval(33),
            ExecutionConfig::reactive(),
            ExecutionConfig::manual(),
        ] {
            let bytes = rmp_serde::to_vec_named(&cfg).expect("encode");
            let back: ExecutionConfig = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(cfg, back, "round-trip mismatch for {:?}", cfg);
        }
    }
}
