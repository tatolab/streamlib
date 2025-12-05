// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::{ProcessExecution, ThreadPriority};
use serde::{Deserialize, Serialize};

/// Configuration for how a processor executes.
///
/// Combines [`ProcessExecution`] (when `process()` is called) with
/// [`ThreadPriority`] (scheduling priority of the processor thread).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ExecutionConfig::default();
        assert_eq!(config.execution, ProcessExecution::Reactive);
        assert_eq!(config.priority, ThreadPriority::Normal);
    }

    #[test]
    fn test_new_config() {
        let config = ExecutionConfig::new(ProcessExecution::Continuous { interval_ms: 100 });
        assert_eq!(
            config.execution,
            ProcessExecution::Continuous { interval_ms: 100 }
        );
        assert_eq!(config.priority, ThreadPriority::Normal);
    }

    #[test]
    fn test_with_priority() {
        let config =
            ExecutionConfig::with_priority(ProcessExecution::Manual, ThreadPriority::RealTime);
        assert_eq!(config.execution, ProcessExecution::Manual);
        assert_eq!(config.priority, ThreadPriority::RealTime);
    }

    #[test]
    fn test_convenience_constructors() {
        assert!(ExecutionConfig::continuous().execution.is_continuous());
        assert_eq!(
            ExecutionConfig::continuous_with_interval(50).execution,
            ProcessExecution::Continuous { interval_ms: 50 }
        );
        assert!(ExecutionConfig::reactive().execution.is_reactive());
        assert!(ExecutionConfig::manual().execution.is_manual());
    }

    #[test]
    fn test_execution_config_serde() {
        let config = ExecutionConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ExecutionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.execution, deserialized.execution);
        assert_eq!(config.priority, deserialized.priority);
    }
}
