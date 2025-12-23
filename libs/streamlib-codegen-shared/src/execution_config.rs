// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::{ProcessExecution, ThreadPriority};

/// Execution configuration for a processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExecutionConfig {
    /// How and when `process()` is called.
    pub execution: ProcessExecution,

    /// Thread scheduling priority.
    pub priority: ThreadPriority,
}

impl ExecutionConfig {
    /// Create a new execution config with the given execution mode and default priority.
    pub fn new(execution: ProcessExecution) -> Self {
        Self {
            execution,
            priority: ThreadPriority::default(),
        }
    }

    /// Create a new execution config with both execution mode and priority.
    pub fn with_priority(execution: ProcessExecution, priority: ThreadPriority) -> Self {
        Self {
            execution,
            priority,
        }
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
