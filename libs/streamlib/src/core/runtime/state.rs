//! Runtime state machine types
//!
//! This module defines the core state enums that drive the runtime's behavior:
//! - `RuntimeState` - The main state machine for runtime lifecycle
//! - `ProcessorStatus` - Per-processor lifecycle state
//! - `WakeupEvent` - Signals for processor thread wakeup

/// Events that can wake up a processor thread
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupEvent {
    /// New data available on an input port
    DataAvailable,
    /// Timer tick for periodic processing
    TimerTick,
    /// Shutdown signal - processor should exit
    Shutdown,
}

/// Per-processor lifecycle state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    /// Processor created but not yet running
    Pending,
    /// Processor thread is running
    Running,
    /// Processor is shutting down
    Stopping,
    /// Processor thread has stopped
    Stopped,
}

/// Comprehensive runtime state tracking
///
/// This enum replaces the simple `bool running` with a full state machine that enables:
/// - Proper lifecycle management (pause, resume, restart)
/// - State-aware API behavior (connect works in all states)
/// - Auto-recompilation decisions based on current state
///
/// # State Transitions
///
/// ```text
/// ┌─────────┐
/// │ Stopped │◄──────────────────────────┐
/// └────┬────┘                           │
///      │ start()                        │
///      ▼                                │
/// ┌──────────┐                          │
/// │ Starting │                          │
/// └────┬─────┘                          │
///      │ initialization complete        │
///      ▼                                │
/// ┌─────────┐  pause()   ┌────────┐     │
/// │ Running │───────────►│ Paused │     │
/// └────┬────┘◄───────────┴────────┘     │
///      │      resume()                  │
///      │                                │
///      │ stop()                         │
///      ▼                                │
/// ┌──────────┐                          │
/// │ Stopping │──────────────────────────┘
/// └──────────┘
///
/// Special states:
/// - Restarting: stop() + start() sequence
/// - PurgeRebuild: clear execution plan, full reoptimization
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    /// Runtime not started - graph can be mutated, no threads running
    Stopped,

    /// Runtime is starting (initializing GPU, spawning threads)
    Starting,

    /// Runtime is fully running - all processor threads active
    Running,

    /// Runtime is paused (threads suspended, can resume quickly)
    Paused,

    /// Runtime is stopping (threads shutting down gracefully)
    Stopping,

    /// Runtime is restarting (stop → start with same graph)
    Restarting,

    /// Complete purge and rebuild (clear execution plan, reoptimize from scratch)
    PurgeRebuild,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::Stopped
    }
}

impl RuntimeState {
    /// Check if runtime is in an active state (threads running or paused)
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }

    /// Check if runtime is in a transitional state
    pub fn is_transitional(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Stopping | Self::Restarting | Self::PurgeRebuild
        )
    }

    /// Check if graph mutations should trigger immediate recompilation
    pub fn should_auto_recompile(&self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_state_default_is_stopped() {
        assert_eq!(RuntimeState::default(), RuntimeState::Stopped);
    }

    #[test]
    fn test_runtime_state_is_active() {
        assert!(!RuntimeState::Stopped.is_active());
        assert!(!RuntimeState::Starting.is_active());
        assert!(RuntimeState::Running.is_active());
        assert!(RuntimeState::Paused.is_active());
        assert!(!RuntimeState::Stopping.is_active());
        assert!(!RuntimeState::Restarting.is_active());
        assert!(!RuntimeState::PurgeRebuild.is_active());
    }

    #[test]
    fn test_runtime_state_is_transitional() {
        assert!(!RuntimeState::Stopped.is_transitional());
        assert!(RuntimeState::Starting.is_transitional());
        assert!(!RuntimeState::Running.is_transitional());
        assert!(!RuntimeState::Paused.is_transitional());
        assert!(RuntimeState::Stopping.is_transitional());
        assert!(RuntimeState::Restarting.is_transitional());
        assert!(RuntimeState::PurgeRebuild.is_transitional());
    }

    #[test]
    fn test_runtime_state_should_auto_recompile() {
        assert!(!RuntimeState::Stopped.should_auto_recompile());
        assert!(!RuntimeState::Starting.should_auto_recompile());
        assert!(RuntimeState::Running.should_auto_recompile());
        assert!(RuntimeState::Paused.should_auto_recompile());
        assert!(!RuntimeState::Stopping.should_auto_recompile());
        assert!(!RuntimeState::Restarting.should_auto_recompile());
        assert!(!RuntimeState::PurgeRebuild.should_auto_recompile());
    }

    #[test]
    fn test_state_copy_and_clone() {
        let state1 = RuntimeState::Running;
        let state2 = state1; // Copy
        let state3 = state1.clone(); // Clone

        assert_eq!(state1, state2);
        assert_eq!(state2, state3);
    }

    #[test]
    fn test_state_equality() {
        assert_eq!(RuntimeState::Stopped, RuntimeState::Stopped);
        assert_eq!(RuntimeState::Running, RuntimeState::Running);
        assert_ne!(RuntimeState::Stopped, RuntimeState::Running);
    }

    #[test]
    fn test_state_debug_format() {
        let states = [
            RuntimeState::Stopped,
            RuntimeState::Starting,
            RuntimeState::Running,
            RuntimeState::Paused,
            RuntimeState::Stopping,
            RuntimeState::Restarting,
            RuntimeState::PurgeRebuild,
        ];

        for state in states {
            let debug_str = format!("{:?}", state);
            assert!(!debug_str.is_empty());
        }
    }
}
