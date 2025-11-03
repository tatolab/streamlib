//! Combined scheduling configuration
//!
//! Brings together scheduling mode, thread priority, and clock source
//! into a unified configuration.

use super::{SchedulingMode, ThreadPriority, ClockSource};
use serde::{Deserialize, Serialize};

/// Combined scheduling configuration
///
/// Unifies three orthogonal concerns:
/// 1. **Scheduling Mode**: WHEN to run (loop, reactive, callback, timer)
/// 2. **Thread Priority**: HOW IMPORTANT (real-time, high, normal)
/// 3. **Clock Source**: WHAT TIMING (audio, vsync, software)
///
/// ## Design Philosophy
///
/// These concerns are **independent** and **composable**:
///
/// - A loop can run at any priority (RT, high, normal)
/// - A reactive processor can sync to any clock (audio, vsync, software)
/// - A callback can have any priority (RT for audio, high for video)
///
/// ## Examples
///
/// ```rust,ignore
/// // Audio effect processor (loop-based)
/// SchedulingConfig {
///     mode: SchedulingMode::Loop,
///     priority: ThreadPriority::RealTime,
///     clock: ClockSource::Audio,
///     provide_clock: false,
/// }
///
/// // Video effect processor (reactive)
/// SchedulingConfig {
///     mode: SchedulingMode::Reactive,
///     priority: ThreadPriority::High,
///     clock: ClockSource::Vsync,
///     provide_clock: false,
/// }
///
/// // ML inference (slow, low priority)
/// SchedulingConfig {
///     mode: SchedulingMode::Reactive,
///     priority: ThreadPriority::Normal,
///     clock: ClockSource::Software,
///     provide_clock: false,
/// }
///
/// // Camera capture (hardware-driven)
/// SchedulingConfig {
///     mode: SchedulingMode::Callback,
///     priority: ThreadPriority::High,
///     clock: ClockSource::Vsync,
///     provide_clock: false,
/// }
/// ```
///
/// ## Runtime Integration
///
/// The runtime reads this config to:
/// 1. Choose execution strategy (loop thread, reactive pool, callback)
/// 2. Set thread priority (via `audio_thread_priority` or `thread-priority`)
/// 3. Sync to appropriate clock
/// 4. Calculate timing budgets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Scheduling mode (when to execute)
    pub mode: SchedulingMode,

    /// Thread priority (how important)
    pub priority: ThreadPriority,

    /// Clock source for synchronization
    pub clock: ClockSource,

    /// Whether this processor provides the clock for the pipeline
    ///
    /// Typically true for:
    /// - Audio output (CoreAudio callback drives timing)
    /// - Display (vsync drives timing)
    ///
    /// Only one processor per pipeline should provide the clock.
    pub provide_clock: bool,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Reactive,
            priority: ThreadPriority::Normal,
            clock: ClockSource::Software,
            provide_clock: false,
        }
    }
}

impl SchedulingConfig {
    /// Create config for real-time audio processing
    ///
    /// **Preset**: Loop mode, real-time priority, audio clock
    ///
    /// **Use for**: Audio effects, test tone generators
    pub fn audio_realtime() -> Self {
        Self {
            mode: SchedulingMode::Loop,
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: false,
        }
    }

    /// Create config for high-priority video processing
    ///
    /// **Preset**: Reactive mode, high priority, vsync clock
    ///
    /// **Use for**: Video effects, real-time transformers
    pub fn video_realtime() -> Self {
        Self {
            mode: SchedulingMode::Reactive,
            priority: ThreadPriority::High,
            clock: ClockSource::Vsync,
            provide_clock: false,
        }
    }

    /// Create config for normal-priority processing
    ///
    /// **Preset**: Reactive mode, normal priority, software clock
    ///
    /// **Use for**: ML inference, file I/O, background tasks
    pub fn background() -> Self {
        Self::default()
    }

    /// Create config for hardware callback sources
    ///
    /// **Preset**: Callback mode, high priority, specified clock
    ///
    /// **Use for**: Camera, microphone, hardware I/O
    pub fn hardware_callback(clock: ClockSource) -> Self {
        Self {
            mode: SchedulingMode::Callback,
            priority: ThreadPriority::High,
            clock,
            provide_clock: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.priority == ThreadPriority::RealTime {
            match self.mode {
                SchedulingMode::Loop | SchedulingMode::Callback => {}
                _ => {
                    tracing::warn!(
                        "Real-time priority with {:?} mode is unusual - ensure RT safety",
                        self.mode
                    );
                }
            }
        }

        Ok(())
    }

    /// Get latency budget from thread priority
    ///
    /// Returns the maximum acceptable latency for this configuration.
    pub fn latency_budget_ms(&self) -> Option<f64> {
        self.priority.latency_budget_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SchedulingConfig::default();
        assert_eq!(config.mode, SchedulingMode::Reactive);
        assert_eq!(config.priority, ThreadPriority::Normal);
        assert_eq!(config.clock, ClockSource::Software);
        assert!(!config.provide_clock);
    }

    #[test]
    fn test_audio_realtime_preset() {
        let config = SchedulingConfig::audio_realtime();
        assert_eq!(config.mode, SchedulingMode::Loop);
        assert_eq!(config.priority, ThreadPriority::RealTime);
        assert_eq!(config.clock, ClockSource::Audio);
    }

    #[test]
    fn test_video_realtime_preset() {
        let config = SchedulingConfig::video_realtime();
        assert_eq!(config.mode, SchedulingMode::Reactive);
        assert_eq!(config.priority, ThreadPriority::High);
        assert_eq!(config.clock, ClockSource::Vsync);
    }

    #[test]
    fn test_background_preset() {
        let config = SchedulingConfig::background();
        assert_eq!(config.mode, SchedulingMode::Reactive);
        assert_eq!(config.priority, ThreadPriority::Normal);
    }

    #[test]
    fn test_hardware_callback_preset() {
        let config = SchedulingConfig::hardware_callback(ClockSource::Vsync);
        assert_eq!(config.mode, SchedulingMode::Callback);
        assert_eq!(config.priority, ThreadPriority::High);
        assert_eq!(config.clock, ClockSource::Vsync);
    }

    #[test]
    fn test_validate() {
        let config = SchedulingConfig::default();
        assert!(config.validate().is_ok());

        let loop_config = SchedulingConfig::audio_realtime();
        assert!(loop_config.validate().is_ok());
    }

    #[test]
    fn test_latency_budget() {
        let rt_config = SchedulingConfig::audio_realtime();
        assert_eq!(rt_config.latency_budget_ms(), Some(10.0));

        let high_config = SchedulingConfig::video_realtime();
        assert_eq!(high_config.latency_budget_ms(), Some(33.0));

        let normal_config = SchedulingConfig::background();
        assert_eq!(normal_config.latency_budget_ms(), None);
    }

    #[test]
    fn test_scheduling_config_serde() {
        let config = SchedulingConfig::audio_realtime();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SchedulingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.mode, deserialized.mode);
        assert_eq!(config.priority, deserialized.priority);
        assert_eq!(config.clock, deserialized.clock);
    }
}
