//! Scheduler delegate trait for processor scheduling decisions.

use std::sync::Arc;

use crate::core::graph::ProcessorNode;

/// Thread priority levels for processor scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadPriority {
    /// Real-time priority for audio/timing-critical processors.
    RealTime,
    /// High priority for latency-sensitive processors.
    High,
    /// Normal priority (default).
    #[default]
    Normal,
    /// Background priority for non-time-sensitive work.
    Background,
}

impl ThreadPriority {
    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            ThreadPriority::RealTime => "real-time priority",
            ThreadPriority::High => "high priority",
            ThreadPriority::Normal => "normal priority",
            ThreadPriority::Background => "background priority",
        }
    }
}

/// How a processor should be scheduled at runtime.
///
/// This is ORTHOGONAL to ProcessExecution (Continuous/Reactive/Manual)
/// which describes how a processor fundamentally works.
///
/// SchedulingStrategy describes how we allocate runtime resources:
/// - DedicatedThread: Own OS thread (good for I/O bound, latency sensitive)
/// - WorkStealingPool: Rayon pool (good for CPU bound, many similar processors)
/// - MainThread: Required for Apple frameworks (AVFoundation, Metal)
/// - Lightweight: No dedicated resources, runs inline
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Dedicated OS thread with configurable priority.
    DedicatedThread {
        /// Thread priority level.
        priority: ThreadPriority,
        /// Optional thread name for debugging.
        name: Option<String>,
    },

    /// Run on Rayon work-stealing pool.
    WorkStealingPool,

    /// Run on main thread.
    MainThread,

    /// Lightweight - no dedicated resources.
    Lightweight,
}

impl Default for SchedulingStrategy {
    fn default() -> Self {
        SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::Normal,
            name: None,
        }
    }
}

impl SchedulingStrategy {
    /// Create a dedicated thread strategy with normal priority.
    pub fn dedicated() -> Self {
        Self::default()
    }

    /// Create a dedicated thread strategy with specified priority.
    pub fn dedicated_with_priority(priority: ThreadPriority) -> Self {
        SchedulingStrategy::DedicatedThread {
            priority,
            name: None,
        }
    }

    /// Create a dedicated thread strategy with name and priority.
    pub fn dedicated_named(name: impl Into<String>, priority: ThreadPriority) -> Self {
        SchedulingStrategy::DedicatedThread {
            priority,
            name: Some(name.into()),
        }
    }

    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                if let Some(n) = name {
                    format!("dedicated thread '{}' ({})", n, priority.description())
                } else {
                    format!("dedicated thread ({})", priority.description())
                }
            }
            SchedulingStrategy::WorkStealingPool => "work-stealing pool".to_string(),
            SchedulingStrategy::MainThread => "main thread".to_string(),
            SchedulingStrategy::Lightweight => "lightweight (inline)".to_string(),
        }
    }
}

/// Delegate for processor scheduling decisions.
///
/// Determines how each processor should be scheduled at runtime.
///
/// A blanket implementation is provided for `Arc<dyn SchedulerDelegate>`,
/// so you can pass an Arc directly where a `SchedulerDelegate` is expected.
pub trait SchedulerDelegate: Send + Sync {
    /// Determine scheduling strategy for a processor.
    fn scheduling_strategy(&self, node: &ProcessorNode) -> SchedulingStrategy;
}

// =============================================================================
// Blanket implementation for Arc wrapper
// =============================================================================

impl SchedulerDelegate for Arc<dyn SchedulerDelegate> {
    fn scheduling_strategy(&self, node: &ProcessorNode) -> SchedulingStrategy {
        (**self).scheduling_strategy(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_priority_description() {
        assert_eq!(ThreadPriority::RealTime.description(), "real-time priority");
        assert_eq!(ThreadPriority::Normal.description(), "normal priority");
    }

    #[test]
    fn test_scheduling_strategy_description() {
        assert_eq!(SchedulingStrategy::MainThread.description(), "main thread");
        assert_eq!(
            SchedulingStrategy::WorkStealingPool.description(),
            "work-stealing pool"
        );
    }
}
