//! Clock sources and synchronization
//!
//! Provides types for clock synchronization and timing in the processing pipeline.

use serde::{Deserialize, Serialize};

/// Clock source for synchronization
///
/// Determines WHAT TIMING the processor uses for synchronization.
/// Orthogonal to scheduling mode and thread priority.
///
/// ## Clock Types
///
/// - **Audio**: Sample-accurate hardware clock (CoreAudio, ALSA)
/// - **Vsync**: Frame-accurate display clock (CVDisplayLink, DRM vsync)
/// - **Software**: CPU timestamps (std::time::Instant)
/// - **Custom**: User-provided clock (genlock, PTP, network time)
///
/// ## Usage
///
/// ```rust,ignore
/// // Audio processor synced to hardware clock
/// ClockSource::Audio
///
/// // Video display synced to vsync
/// ClockSource::Vsync
///
/// // Test tone generator using software clock
/// ClockSource::Software
///
/// // Genlock sync for broadcast
/// ClockSource::Custom("genlock".to_string())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockSource {
    /// Audio hardware clock (sample-accurate)
    ///
    /// Provided by CoreAudio, ALSA, WASAPI.
    /// Most accurate for audio processing (sub-millisecond precision).
    ///
    /// **Example**: 48kHz audio = 20.83μs per sample
    Audio,

    /// Video vsync clock (frame-accurate)
    ///
    /// Provided by CVDisplayLink (macOS), DRM vsync (Linux).
    /// Accurate for video display (frame-level precision).
    ///
    /// **Example**: 60Hz display = 16.67ms per frame
    Vsync,

    /// Software clock (CPU timestamps)
    ///
    /// Uses std::time::Instant for timing.
    /// Less accurate but always available.
    ///
    /// **Precision**: ~1ms on most systems
    Software,

    /// Custom clock (user-provided)
    ///
    /// For specialized timing scenarios:
    /// - Genlock (broadcast sync)
    /// - PTP (precision time protocol)
    /// - Network time sync
    /// - External hardware clock
    ///
    /// **Example**: `ClockSource::Custom("ptp".to_string())`
    Custom(String),
}

impl Default for ClockSource {
    fn default() -> Self {
        ClockSource::Software
    }
}

/// Clock configuration for sinks
///
/// Determines whether this sink provides a clock to the pipeline.
/// Used by audio output and display sinks that drive timing.
///
/// ## Clock Providers
///
/// Some sinks provide the master clock:
/// - **Audio output**: CoreAudio callback is sample-accurate master
/// - **Display**: CVDisplayLink is frame-accurate master
///
/// ## Usage
///
/// ```rust,ignore
/// // Audio output provides clock
/// ClockConfig {
///     provides_clock: true,
///     clock_type: Some(ClockType::Audio),
///     clock_name: Some("coreaudio".to_string()),
/// }
///
/// // File writer doesn't provide clock
/// ClockConfig::default()
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockConfig {
    /// Whether this sink provides a clock
    ///
    /// True for audio output (CoreAudio callback is master clock)
    /// True for display with vsync (CVDisplayLink is master clock)
    pub provides_clock: bool,

    /// Clock type provided (if any)
    pub clock_type: Option<ClockType>,

    /// Clock name for debugging
    pub clock_name: Option<String>,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            provides_clock: false,
            clock_type: None,
            clock_name: None,
        }
    }
}

/// Type of clock provided by a sink
///
/// Categorizes the clock for runtime scheduling decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockType {
    /// Audio hardware clock (sample-accurate)
    ///
    /// Highest precision - used as master when available.
    /// **Example**: CoreAudio clock at 48kHz (20.83μs resolution)
    Audio,

    /// Video vsync clock (frame-accurate)
    ///
    /// Frame-level precision - good for video.
    /// **Example**: 60Hz vsync (16.67ms resolution)
    Vsync,

    /// Network clock (RTP, PTP)
    ///
    /// For distributed systems and network sync.
    /// **Example**: PTP grand master clock
    Network,

    /// System clock
    ///
    /// Fallback - uses CPU timestamps.
    /// **Precision**: ~1ms
    System,
}

/// Synchronization mode for sinks
///
/// Determines how runtime schedules render() calls relative to timestamps.
///
/// ## Modes
///
/// - **Timestamp**: Sync to buffer timestamps (most sinks)
/// - **None**: Render immediately (file writers)
/// - **External**: Sync to external clock (genlock, network)
///
/// ## Usage
///
/// ```rust,ignore
/// // Display syncs to timestamps
/// SyncMode::Timestamp
///
/// // File writer has no sync
/// SyncMode::None
///
/// // Broadcast uses genlock
/// SyncMode::External
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    /// Sync to buffer timestamps (default)
    ///
    /// Compare buffer timestamp to clock, render at correct time.
    /// Used by most sinks (display, audio, file with timestamps).
    ///
    /// **Behavior**: Wait until `buffer.timestamp <= clock.now()`
    Timestamp,

    /// No sync - render immediately
    ///
    /// Used for file sinks without timing requirements.
    ///
    /// **Behavior**: Process as fast as possible
    None,

    /// Sync to external clock
    ///
    /// Used for genlock, network sync, PTP.
    ///
    /// **Behavior**: Wait for external sync signal
    External,
}

impl Default for SyncMode {
    fn default() -> Self {
        SyncMode::Timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_source_equality() {
        assert_eq!(ClockSource::Audio, ClockSource::Audio);
        assert_ne!(ClockSource::Audio, ClockSource::Vsync);
        assert_eq!(
            ClockSource::Custom("ptp".to_string()),
            ClockSource::Custom("ptp".to_string())
        );
        assert_ne!(
            ClockSource::Custom("ptp".to_string()),
            ClockSource::Custom("genlock".to_string())
        );
    }

    #[test]
    fn test_clock_source_default() {
        assert_eq!(ClockSource::default(), ClockSource::Software);
    }

    #[test]
    fn test_clock_type_equality() {
        assert_eq!(ClockType::Audio, ClockType::Audio);
        assert_ne!(ClockType::Audio, ClockType::Vsync);
    }

    #[test]
    fn test_sync_mode_equality() {
        assert_eq!(SyncMode::Timestamp, SyncMode::Timestamp);
        assert_ne!(SyncMode::Timestamp, SyncMode::None);
    }

    #[test]
    fn test_sync_mode_default() {
        assert_eq!(SyncMode::default(), SyncMode::Timestamp);
    }

    #[test]
    fn test_clock_config_default() {
        let config = ClockConfig::default();
        assert!(!config.provides_clock);
        assert_eq!(config.clock_type, None);
        assert_eq!(config.clock_name, None);
    }

    #[test]
    fn test_clock_source_serde() {
        let source = ClockSource::Audio;
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ClockSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn test_clock_source_custom_serde() {
        let source = ClockSource::Custom("ptp_master".to_string());
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ClockSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn test_clock_type_serde() {
        let clock_type = ClockType::Vsync;
        let json = serde_json::to_string(&clock_type).unwrap();
        let deserialized: ClockType = serde_json::from_str(&json).unwrap();
        assert_eq!(clock_type, deserialized);
    }

    #[test]
    fn test_sync_mode_serde() {
        let mode = SyncMode::External;
        let json = serde_json::to_string(&mode).unwrap();
        let deserialized: SyncMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, deserialized);
    }
}
