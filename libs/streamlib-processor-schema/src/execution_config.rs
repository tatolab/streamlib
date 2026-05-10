// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::ProcessExecution;

/// Execution configuration for a processor.
///
/// Thread priority is **not** part of this type — it's a per-processor
/// scheduling decision sourced from the manifest's `scheduling:` block at
/// registration time and stored on `ProcessorDescriptor`. See `compiler/
/// scheduling.rs` for how the runtime resolves it.
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
