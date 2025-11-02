//! Audio hardware clock (sample-accurate)
//!
//! Driven by CoreAudio (macOS), ALSA (Linux), or WASAPI (Windows) callbacks.

use super::Clock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Audio hardware clock (sample-accurate)
///
/// Driven by CoreAudio (macOS), ALSA (Linux), or WASAPI (Windows) callbacks.
/// Provides the most accurate timing for audio pipelines.
///
/// ## How It Works
///
/// 1. AudioOutputProcessor registers hardware callback
/// 2. Callback fills audio buffer (e.g., 2048 samples)
/// 3. Callback increments `samples_played` counter
/// 4. Clock converts samples → nanoseconds
///
/// ## Accuracy
///
/// - **Sample-accurate**: Tracks exact hardware playback position
/// - **No drift**: Hardware oscillator is ground truth
/// - **Typical jitter**: < 1 sample (~20 μs at 48 kHz)
///
/// ## Usage
///
/// ```rust,ignore
/// // In AudioOutputProcessor:
/// let clock = Arc::new(AudioClock::new(48000, "CoreAudio Clock"));
///
/// // In hardware callback:
/// fn audio_callback(&self, buffer: &mut [f32]) {
///     // Fill buffer...
///     self.clock.increment_samples(buffer.len() / 2); // stereo
/// }
///
/// // In sources:
/// let now = clock.now_ns();
/// ```
pub struct AudioClock {
    /// Sample rate (e.g., 48000)
    sample_rate: u32,

    /// Total samples played since start
    samples_played: AtomicU64,

    /// Timestamp when clock started (nanoseconds)
    base_time_ns: i64,

    /// Human-readable description
    description: String,
}

impl AudioClock {
    /// Create a new audio clock
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Hardware sample rate (e.g., 48000)
    /// * `description` - Human-readable name (e.g., "CoreAudio Clock")
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
        }
    }

    /// Increment sample counter (called by hardware callback)
    ///
    /// # Thread Safety
    ///
    /// Safe to call from audio callback thread.
    ///
    /// # Arguments
    ///
    /// * `num_samples` - Number of samples rendered (mono, not frames)
    pub fn increment_samples(&self, num_samples: u64) {
        self.samples_played.fetch_add(num_samples, Ordering::Relaxed);
    }

    /// Reset sample counter to zero
    ///
    /// Used when restarting playback.
    pub fn reset(&self) {
        self.samples_played.store(0, Ordering::Relaxed);
    }

    /// Get total samples played
    pub fn samples(&self) -> u64 {
        self.samples_played.load(Ordering::Relaxed)
    }
}

impl Clock for AudioClock {
    fn now_ns(&self) -> i64 {
        let samples = self.samples_played.load(Ordering::Relaxed);
        let elapsed_ns = (samples as f64 / self.sample_rate as f64 * 1e9) as i64;
        self.base_time_ns + elapsed_ns
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
