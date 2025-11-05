
use super::Clock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct VideoClock {
    refresh_rate: f64,

    frames_rendered: AtomicU64,

    base_time_ns: i64,

    description: String,
}

impl VideoClock {
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

    pub fn increment_frames(&self, num_frames: u64) {
        self.frames_rendered.fetch_add(num_frames, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        self.frames_rendered.store(0, Ordering::Relaxed);
    }

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

        clock.increment_frames(60);

        let elapsed_ns = clock.now_ns() - base_time;
        let expected_ns = 1_000_000_000; // 1 second in nanoseconds

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
