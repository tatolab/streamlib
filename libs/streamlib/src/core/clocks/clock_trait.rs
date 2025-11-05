//! Clock trait - Passive time reference for processor synchronization
//!
//! Following GStreamer's GstClock pattern, clocks provide **passive time references**
//! that processors query, but do NOT actively schedule or wake processors.

use std::time::Duration;

/// Passive clock interface for processor synchronization
///
/// Following GStreamer's GstClock pattern, this provides a time reference
/// that processors can query, but does NOT actively schedule or wake processors.
///
/// ## Design
///
/// - **Passive**: Clock provides `now()`, processors decide when to wait
/// - **No callbacks**: Clock doesn't call into processors
/// - **Thread-safe**: All methods can be called from any thread
///
/// ## Implementations
///
/// - `SoftwareClock`: System timestamps (fallback)
/// - `AudioClock`: Hardware sample counter (most accurate for audio)
/// - `VideoClock`: Display vsync counter (most accurate for video)
///
/// ## Usage in Sources
///
/// ```rust,ignore
/// // Runtime spawns source loop with clock reference
/// loop {
///     let frame = source.generate()?;
///     let sync_point = source.clock_sync_point();
///
///     // Wait for clock to reach target time
///     let now = clock.now_ns();
///     if now < next_sync_time_ns {
///         sleep(next_sync_time_ns - now);
///     }
///
///     source.write_output(frame);
/// }
/// ```
///
/// ## Usage in Sinks
///
/// ```rust,ignore
/// fn render(&mut self, frame: VideoFrame) {
///     // Wait until presentation time
///     let now = self.clock.now_ns();
///     if now < frame.timestamp_ns {
///         sleep(frame.timestamp_ns - now);
///     }
///
///     // Render to hardware
///     self.display.render(frame);
/// }
/// ```
pub trait Clock: Send + Sync {
    /// Current time in nanoseconds (monotonic)
    ///
    /// Returns time since some arbitrary epoch (e.g., pipeline start).
    /// Guaranteed to be monotonically increasing.
    ///
    /// # Thread Safety
    ///
    /// Safe to call from any thread concurrently.
    fn now_ns(&self) -> i64;

    /// Current time as Duration (convenience)
    ///
    /// Wraps `now_ns()` for easier arithmetic.
    fn now(&self) -> Duration {
        Duration::from_nanos(self.now_ns() as u64)
    }

    /// Clock rate in Hz (for variable-rate clocks)
    ///
    /// - AudioClock: sample rate (e.g., 48000.0)
    /// - VideoClock: refresh rate (e.g., 60.0)
    /// - SoftwareClock: None (not hardware-driven)
    ///
    /// Returns None if clock rate is not fixed.
    fn rate_hz(&self) -> Option<f64> {
        None
    }

    /// Human-readable clock description
    ///
    /// Used for debugging and logging.
    fn description(&self) -> &str;
}
