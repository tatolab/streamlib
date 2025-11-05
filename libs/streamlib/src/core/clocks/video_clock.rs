//! Video hardware clock (vsync-accurate)
//!
//! Driven by CVDisplayLink (macOS) or DRM vsync (Linux).

use super::Clock;
use crate::core::scheduling::ClockType;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Video hardware clock (vsync-accurate)
///
/// Driven by CVDisplayLink (macOS) or DRM vsync (Linux).
/// Provides the most accurate timing for video pipelines.
///
/// ## How It Works
///
/// 1. DisplayProcessor registers vsync callback
/// 2. Callback fires at display refresh rate (e.g., 60 Hz)
/// 3. Callback increments `frames_rendered` counter
/// 4. Clock converts frames → nanoseconds
///
/// ## Accuracy
///
/// - **Frame-accurate**: Tracks exact display refresh
/// - **No drift**: Hardware vsync is ground truth
/// - **Typical jitter**: < 1 ms
///
/// ## Usage
///
/// ```rust,ignore
/// // In DisplayProcessor:
/// let clock = Arc::new(VideoClock::new(60.0, "CVDisplayLink Clock"));
///
/// // In vsync callback:
/// fn display_callback(&self) {
///     self.clock.increment_frames(1);
/// }
/// ```
pub struct VideoClock {
    /// Display refresh rate (e.g., 60.0)
    refresh_rate: f64,

    /// Total frames rendered since start
    frames_rendered: AtomicU64,

    /// Timestamp when clock started (nanoseconds)
    base_time_ns: i64,

    /// Human-readable description
    description: String,
}

impl VideoClock {
    /// Create a new video clock
    ///
    /// # Arguments
    ///
    /// * `refresh_rate` - Display refresh rate in Hz (e.g., 60.0)
    /// * `description` - Human-readable name (e.g., "CVDisplayLink Clock")
    pub fn new(refresh_rate: f64, description: String) -> Self {
        let base_time_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        Self {
            refresh_rate,
            frames_rendered: AtomicU64::new(0),
            base_time_ns,
            description,
        }
    }

    /// Increment frame counter (called by vsync callback)
    ///
    /// # Thread Safety
    ///
    /// Safe to call from display callback thread.
    pub fn increment_frames(&self, num_frames: u64) {
        self.frames_rendered.fetch_add(num_frames, Ordering::Relaxed);
    }

    /// Reset frame counter to zero
    pub fn reset(&self) {
        self.frames_rendered.store(0, Ordering::Relaxed);
    }

    /// Get total frames rendered
    pub fn frames(&self) -> u64 {
        self.frames_rendered.load(Ordering::Relaxed)
    }
}

impl Clock for VideoClock {
    fn now_ns(&self) -> i64 {
        let frames = self.frames_rendered.load(Ordering::Relaxed);
        let elapsed_ns = (frames as f64 / self.refresh_rate * 1e9) as i64;
        self.base_time_ns + elapsed_ns
    }

    fn rate_hz(&self) -> Option<f64> {
        Some(self.refresh_rate)
    }

    fn clock_type(&self) -> ClockType {
        ClockType::Video
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_clock_frame_counting() {
        let clock = VideoClock::new(60.0, "Test Video Clock".to_string());

        assert_eq!(clock.frames(), 0);

        clock.increment_frames(1);
        assert_eq!(clock.frames(), 1);

        clock.increment_frames(59);
        assert_eq!(clock.frames(), 60);
    }

    #[test]
    fn test_video_clock_time_calculation() {
        let clock = VideoClock::new(60.0, "Test Video Clock".to_string());
        let base_time = clock.now_ns();

        // Render 60 frames (1 second at 60 Hz)
        clock.increment_frames(60);

        let elapsed_ns = clock.now_ns() - base_time;
        let expected_ns = 1_000_000_000; // 1 second in nanoseconds

        // Allow 1ms tolerance
        assert!((elapsed_ns - expected_ns).abs() < 1_000_000);
    }

    #[test]
    fn test_video_clock_reset() {
        let clock = VideoClock::new(60.0, "Test Video Clock".to_string());

        clock.increment_frames(100);
        assert_eq!(clock.frames(), 100);

        clock.reset();
        assert_eq!(clock.frames(), 0);
    }

    #[test]
    fn test_video_clock_rate_hz() {
        let clock = VideoClock::new(60.0, "Test Video Clock".to_string());
        assert_eq!(clock.rate_hz(), Some(60.0));
    }

    #[test]
    fn test_video_clock_description() {
        let clock = VideoClock::new(60.0, "CVDisplayLink".to_string());
        assert_eq!(clock.description(), "CVDisplayLink");
    }
}
