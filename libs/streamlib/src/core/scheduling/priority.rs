//! Thread priority management
//!
//! Provides thread priority levels and integration with platform-specific
//! threading libraries for real-time and high-priority processing.
//!
//! ## Platform Integration
//!
//! On macOS/iOS:
//! - **RealTime**: Uses `audio_thread_priority` → THREAD_TIME_CONSTRAINT_POLICY
//! - **High**: Uses `thread-priority` → Precedence policy
//! - **Normal**: Standard thread priority
//!
//! On Linux (future):
//! - **RealTime**: SCHED_FIFO or SCHED_RR
//! - **High**: SCHED_RR with lower priority
//! - **Normal**: SCHED_OTHER
//!
//! ## Usage Guidelines
//!
//! From `threading.md`:
//!
//! ### RealTime (< 10ms latency)
//! - Audio I/O processing
//! - Critical AR/HUD video (combat scenarios)
//! - Must complete within time constraint or system will demote
//!
//! ### High (< 33ms latency)
//! - Video effects processing
//! - ML inference for real-time use
//! - Standard video capture/display
//!
//! ### Normal (no strict latency)
//! - Background tasks
//! - File I/O
//! - Network streams
//! - Logging, metrics

use serde::{Deserialize, Serialize};

/// Thread priority level
///
/// Determines HOW IMPORTANT the thread is for scheduling.
/// Orthogonal to scheduling mode (priority = IMPORTANCE, mode = WHEN).
///
/// ## Platform Mapping
///
/// ### macOS/iOS
/// - **RealTime**: `THREAD_TIME_CONSTRAINT_POLICY` (< 10ms, non-preemptible)
/// - **High**: `THREAD_PRECEDENCE_POLICY` (< 33ms, elevated priority)
/// - **Normal**: Standard priority
///
/// ### Linux (future)
/// - **RealTime**: `SCHED_FIFO` or `SCHED_DEADLINE`
/// - **High**: `SCHED_RR` with elevated priority
/// - **Normal**: `SCHED_OTHER`
///
/// ### Windows (future)
/// - **RealTime**: `THREAD_PRIORITY_TIME_CRITICAL`
/// - **High**: `THREAD_PRIORITY_HIGHEST`
/// - **Normal**: `THREAD_PRIORITY_NORMAL`
///
/// ## Real-Time Safety
///
/// **CRITICAL**: Real-time threads MUST:
/// - No allocations (pre-allocate everything)
/// - No locks (use lock-free data structures)
/// - No blocking I/O
/// - Complete within time budget
///
/// Violating these rules causes priority inversion and audio glitches.
///
/// ## Examples
///
/// ```rust,ignore
/// // Audio processing - must be real-time
/// ThreadPriority::RealTime
///
/// // Video effects - high priority but not RT
/// ThreadPriority::High
///
/// // File writing - background task
/// ThreadPriority::Normal
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadPriority {
    /// Real-time priority (< 10ms latency)
    ///
    /// **Use for**:
    /// - Audio I/O processing (< 5ms)
    /// - Combat-critical AR/HUD video
    /// - Time-sensitive control systems
    ///
    /// **Platform**:
    /// - macOS: THREAD_TIME_CONSTRAINT_POLICY
    /// - Linux: SCHED_FIFO / SCHED_DEADLINE
    /// - Windows: THREAD_PRIORITY_TIME_CRITICAL
    ///
    /// **Requirements**:
    /// - No allocations
    /// - No locks
    /// - No blocking I/O
    /// - Guaranteed completion < 10ms
    ///
    /// **Consequences of violation**:
    /// - System demotes thread priority
    /// - Audio glitches
    /// - Video stutter
    RealTime,

    /// High priority (< 33ms latency)
    ///
    /// **Use for**:
    /// - Video effects processing
    /// - ML inference (real-time)
    /// - Standard video capture/display
    /// - Audio effects (non-I/O)
    ///
    /// **Platform**:
    /// - macOS: THREAD_PRECEDENCE_POLICY
    /// - Linux: SCHED_RR (elevated)
    /// - Windows: THREAD_PRIORITY_HIGHEST
    ///
    /// **Guidelines**:
    /// - Minimize allocations
    /// - Avoid locks in hot paths
    /// - Use lock-free queues
    /// - Target < 33ms processing time
    High,

    /// Normal priority (no strict latency)
    ///
    /// **Use for**:
    /// - Background tasks
    /// - File I/O
    /// - Network streams
    /// - Logging
    /// - Metrics collection
    ///
    /// **Platform**:
    /// - macOS: Default priority
    /// - Linux: SCHED_OTHER
    /// - Windows: THREAD_PRIORITY_NORMAL
    Normal,
}

impl Default for ThreadPriority {
    fn default() -> Self {
        ThreadPriority::Normal
    }
}

impl ThreadPriority {
    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            ThreadPriority::RealTime => "Real-time (< 10ms latency, time-constrained)",
            ThreadPriority::High => "High priority (< 33ms latency, elevated)",
            ThreadPriority::Normal => "Normal priority (no strict latency)",
        }
    }

    /// Get expected latency budget in milliseconds
    pub fn latency_budget_ms(&self) -> Option<f64> {
        match self {
            ThreadPriority::RealTime => Some(10.0),
            ThreadPriority::High => Some(33.0),
            ThreadPriority::Normal => None,  // No strict budget
        }
    }

    /// Check if this priority requires real-time safety
    ///
    /// Real-time threads MUST:
    /// - Pre-allocate all memory
    /// - Use lock-free data structures
    /// - No blocking I/O
    /// - No system calls in hot path
    pub fn requires_realtime_safety(&self) -> bool {
        matches!(self, ThreadPriority::RealTime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_priority_equality() {
        assert_eq!(ThreadPriority::RealTime, ThreadPriority::RealTime);
        assert_ne!(ThreadPriority::RealTime, ThreadPriority::High);
        assert_ne!(ThreadPriority::High, ThreadPriority::Normal);
    }

    #[test]
    fn test_thread_priority_default() {
        assert_eq!(ThreadPriority::default(), ThreadPriority::Normal);
    }

    #[test]
    fn test_thread_priority_description() {
        assert_eq!(
            ThreadPriority::RealTime.description(),
            "Real-time (< 10ms latency, time-constrained)"
        );
        assert_eq!(
            ThreadPriority::High.description(),
            "High priority (< 33ms latency, elevated)"
        );
        assert_eq!(
            ThreadPriority::Normal.description(),
            "Normal priority (no strict latency)"
        );
    }

    #[test]
    fn test_latency_budget() {
        assert_eq!(ThreadPriority::RealTime.latency_budget_ms(), Some(10.0));
        assert_eq!(ThreadPriority::High.latency_budget_ms(), Some(33.0));
        assert_eq!(ThreadPriority::Normal.latency_budget_ms(), None);
    }

    #[test]
    fn test_requires_realtime_safety() {
        assert!(ThreadPriority::RealTime.requires_realtime_safety());
        assert!(!ThreadPriority::High.requires_realtime_safety());
        assert!(!ThreadPriority::Normal.requires_realtime_safety());
    }

    #[test]
    fn test_thread_priority_serde() {
        let priority = ThreadPriority::High;
        let json = serde_json::to_string(&priority).unwrap();
        let deserialized: ThreadPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(priority, deserialized);
    }

    #[test]
    fn test_thread_priority_debug() {
        let priority = ThreadPriority::RealTime;
        let debug_str = format!("{:?}", priority);
        assert_eq!(debug_str, "RealTime");
    }
}
