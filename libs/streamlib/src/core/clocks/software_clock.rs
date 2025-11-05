
use super::Clock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct SoftwareClock {
    start_time: Instant,
    start_timestamp: i64, // nanoseconds since epoch
    description: String,
}

impl SoftwareClock {
    pub fn new() -> Self {
        Self::with_description("Software Clock".to_string())
    }

    pub fn with_description(description: String) -> Self {
        let start_time = Instant::now();
        let start_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        Self {
            start_time,
            start_timestamp,
            description,
        }
    }

    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.start_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
    }
}

impl Default for SoftwareClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SoftwareClock {
    fn now_ns(&self) -> i64 {
        let elapsed = self.start_time.elapsed().as_nanos() as i64;
        self.start_timestamp + elapsed
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_software_clock_now() {
        let clock = SoftwareClock::new();
        let t1 = clock.now_ns();

        thread::sleep(Duration::from_millis(10));

        let t2 = clock.now_ns();
        assert!(t2 > t1, "Time should increase");
        assert!(t2 - t1 >= 10_000_000, "Should be at least 10ms");
    }

    #[test]
    fn test_software_clock_monotonic() {
        let clock = SoftwareClock::new();
        let mut last_time = clock.now_ns();

        for _ in 0..100 {
            let current_time = clock.now_ns();
            assert!(current_time >= last_time, "Time must be monotonic");
            last_time = current_time;
        }
    }

    #[test]
    fn test_software_clock_reset() {
        let mut clock = SoftwareClock::new();
        let base = clock.now_ns();

        thread::sleep(Duration::from_millis(10));
        let t1 = clock.now_ns();
        let elapsed_before_reset = t1 - base;

        clock.reset();
        let t2 = clock.now_ns();

        assert!(elapsed_before_reset >= 10_000_000, "Should have at least 10ms elapsed before reset");

        thread::sleep(Duration::from_millis(5));
        let t3 = clock.now_ns();
        let elapsed_after_reset = t3 - t2;
        assert!(elapsed_after_reset >= 5_000_000, "Should have ~5ms elapsed after reset");
        assert!(elapsed_after_reset < elapsed_before_reset, "Elapsed after reset should be less than before reset");
    }

    #[test]
    fn test_clock_descriptions() {
        let sw_clock = SoftwareClock::new();
        assert_eq!(sw_clock.description(), "Software Clock");

        let custom_clock = SoftwareClock::with_description("Custom Clock".to_string());
        assert_eq!(custom_clock.description(), "Custom Clock");
    }
}
