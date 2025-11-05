//! Combined scheduling configuration
//!
//! Brings together scheduling mode and thread priority.

use super::{SchedulingMode, ThreadPriority};
use serde::{Deserialize, Serialize};

/// Combined scheduling configuration
///
/// Specifies two orthogonal concerns:
/// 1. **Scheduling Mode**: WHEN to run (Loop, Pull, Push)
/// 2. **Thread Priority**: HOW IMPORTANT (real-time, high, normal)
///
/// ## Design Philosophy
///
/// These concerns are **independent** and **composable**:
///
/// - Loop mode runs continuously in its own thread
/// - Pull mode is driven by hardware callbacks (audio, video)
/// - Push mode is event-driven (woken by upstream data)
/// - Any mode can have any priority (RT, high, normal)
///
/// ## Examples
///
/// ```rust,ignore
/// // Audio output (hardware-driven callback)
/// SchedulingConfig {
///     mode: SchedulingMode::Pull,
///     priority: ThreadPriority::RealTime,
/// }
///
/// // Video effect (event-driven)
/// SchedulingConfig {
///     mode: SchedulingMode::Push,
///     priority: ThreadPriority::High,
/// }
///
/// // ML inference (continuous loop, low priority)
/// SchedulingConfig {
///     mode: SchedulingMode::Loop,
///     priority: ThreadPriority::Normal,
/// }
/// ```
///
/// ## Runtime Integration
///
/// The runtime reads this config to:
/// 1. Choose execution strategy (spawn loop thread, wait for wakeup, etc.)
/// 2. Set thread priority (via `audio_thread_priority` crate or similar)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Scheduling mode (when to execute)
    pub mode: SchedulingMode,

    /// Thread priority (how important)
    pub priority: ThreadPriority,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Push,
            priority: ThreadPriority::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SchedulingConfig::default();
        assert_eq!(config.mode, SchedulingMode::Push);
        assert_eq!(config.priority, ThreadPriority::Normal);
    }

    #[test]
    fn test_scheduling_config_serde() {
        let config = SchedulingConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SchedulingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.mode, deserialized.mode);
        assert_eq!(config.priority, deserialized.priority);
    }
}
