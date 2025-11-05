//! Audio hardware clock (sample-accurate)
//!
//! Driven by CoreAudio (macOS), ALSA (Linux), or WASAPI (Windows) callbacks.

use super::Clock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AudioClock {
    sample_rate: u32,

    samples_played: AtomicU64,

    base_time_ns: i64,

    description: String,

    last_hardware_timestamp_ns: AtomicI64,
}

impl AudioClock {
    pub fn new(sample_rate: u32, description: String) -> Self {
        let base_time_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        Self {
            sample_rate,
            samples_played: AtomicU64::new(0),
            base_time_ns,
            description,
            last_hardware_timestamp_ns: AtomicI64::new(0),
        }
    }

    pub fn increment_samples(&self, num_samples: u64) {
        self.samples_played.fetch_add(num_samples, Ordering::Relaxed);
    }

    pub fn update_hardware_timestamp(&self, timestamp_ns: i64) {
        self.last_hardware_timestamp_ns.store(timestamp_ns, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        self.samples_played.store(0, Ordering::Relaxed);
        self.last_hardware_timestamp_ns.store(0, Ordering::Relaxed);
    }

    pub fn samples(&self) -> u64 {
        self.samples_played.load(Ordering::Relaxed)
    }
}

impl Clock for AudioClock {
    fn now_ns(&self) -> i64 {
        let hw_timestamp = self.last_hardware_timestamp_ns.load(Ordering::Relaxed);
        if hw_timestamp > 0 {
            hw_timestamp
        } else {
            let samples = self.samples_played.load(Ordering::Relaxed);
            let elapsed_ns = (samples as f64 / self.sample_rate as f64 * 1e9) as i64;
            self.base_time_ns + elapsed_ns
        }
    }

    fn rate_hz(&self) -> Option<f64> {
        Some(self.sample_rate as f64)
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_clock_sample_counting() {
        let clock = AudioClock::new(48000, "Test Audio Clock".to_string());

        assert_eq!(clock.samples(), 0);

        clock.increment_samples(2048);
        assert_eq!(clock.samples(), 2048);

        clock.increment_samples(2048);
        assert_eq!(clock.samples(), 4096);
    }

    #[test]
    fn test_audio_clock_time_calculation() {
        let clock = AudioClock::new(48000, "Test Audio Clock".to_string());
        let base_time = clock.now_ns();

        // Render 48000 samples (1 second worth)
        clock.increment_samples(48000);

        let elapsed_ns = clock.now_ns() - base_time;
        let expected_ns = 1_000_000_000; // 1 second in nanoseconds

        // Allow 1ms tolerance for calculation precision
        assert!((elapsed_ns - expected_ns).abs() < 1_000_000);
    }

    #[test]
    fn test_audio_clock_reset() {
        let clock = AudioClock::new(48000, "Test Audio Clock".to_string());

        clock.increment_samples(10000);
        assert_eq!(clock.samples(), 10000);

        clock.reset();
        assert_eq!(clock.samples(), 0);
    }

    #[test]
    fn test_audio_clock_rate_hz() {
        let clock = AudioClock::new(48000, "Test Audio Clock".to_string());
        assert_eq!(clock.rate_hz(), Some(48000.0));
    }

    #[test]
    fn test_audio_clock_description() {
        let clock = AudioClock::new(48000, "CoreAudio".to_string());
        assert_eq!(clock.description(), "CoreAudio");
    }
}
