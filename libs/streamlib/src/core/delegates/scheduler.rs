//! Scheduler delegate for processor scheduling decisions.

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
    /// Good for: CPU-bound parallel work, many similar processors.
    /// Scales to 50k+ processors like a game engine.
    WorkStealingPool,

    /// Run on main thread.
    /// Required for: Apple frameworks (AVFoundation, Metal, AppKit).
    MainThread,

    /// Lightweight - no dedicated resources.
    /// Runs inline in caller's context.
    /// Good for: Simple, fast transformations.
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
/// The default implementation uses heuristics based on processor type.
pub trait SchedulerDelegate: Send + Sync {
    /// Determine scheduling strategy for a processor.
    fn scheduling_strategy(&self, node: &ProcessorNode) -> SchedulingStrategy;
}

/// Default scheduler implementation using type-based heuristics.
pub struct DefaultScheduler;

impl Default for DefaultScheduler {
    fn default() -> Self {
        Self
    }
}

impl SchedulerDelegate for DefaultScheduler {
    fn scheduling_strategy(&self, node: &ProcessorNode) -> SchedulingStrategy {
        let processor_type = &node.processor_type;

        // Apple framework processors require main thread
        if processor_type == "CameraProcessor"
            || processor_type == "DisplayProcessor"
            || processor_type.contains("Display")
        {
            return SchedulingStrategy::MainThread;
        }

        // Audio processors get real-time priority
        if processor_type.contains("Audio")
            || processor_type.contains("Microphone")
            || processor_type.contains("Speaker")
        {
            return SchedulingStrategy::DedicatedThread {
                priority: ThreadPriority::RealTime,
                name: Some(format!("audio-{}", node.id)),
            };
        }

        // Video encoding/decoding gets high priority
        if processor_type.contains("Encoder")
            || processor_type.contains("Decoder")
            || processor_type.contains("H264")
            || processor_type.contains("H265")
        {
            return SchedulingStrategy::DedicatedThread {
                priority: ThreadPriority::High,
                name: Some(format!("video-{}", node.id)),
            };
        }

        // Default: normal dedicated thread
        SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::Normal,
            name: None,
        }
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

    #[test]
    fn test_default_scheduler_camera() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("cam".into(), "CameraProcessor".into(), None, vec![], vec![]);

        assert_eq!(
            scheduler.scheduling_strategy(&node),
            SchedulingStrategy::MainThread
        );
    }

    #[test]
    fn test_default_scheduler_audio() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new(
            "mic".into(),
            "AudioCaptureProcessor".into(),
            None,
            vec![],
            vec![],
        );

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
            _ => panic!("Expected DedicatedThread for audio processor"),
        }
    }

    #[test]
    fn test_default_scheduler_encoder() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("enc".into(), "H264Encoder".into(), None, vec![], vec![]);

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
            _ => panic!("Expected DedicatedThread for encoder"),
        }
    }

    #[test]
    fn test_default_scheduler_generic() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("proc".into(), "SomeProcessor".into(), None, vec![], vec![]);

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
            _ => panic!("Expected DedicatedThread for generic processor"),
        }
    }
}
