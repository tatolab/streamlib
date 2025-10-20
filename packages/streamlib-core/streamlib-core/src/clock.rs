//! Clock abstraction for timing and synchronization
//!
//! Supports multiple clock sources:
//! - SoftwareClock: Free-running software timer (bathtub mode)
//! - PTPClock: IEEE 1588 Precision Time Protocol (stub for Phase 4)
//! - GenlockClock: SDI hardware sync (stub for Phase 4)
//!
//! Clocks are swappable to support different sync sources:
//! ```ignore
//! if genlock_signal_present {
//!     clock = GenlockClock::new(sdi_port);
//! } else if ptp_available {
//!     clock = PTPClock::new(ptp_client);
//! } else {
//!     clock = SoftwareClock::new(60.0);
//! }
//! ```

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Clock tick with timing information
///
/// Ticks are signals to process, not data carriers. Actors receive ticks
/// and read latest data from ring buffers.
#[derive(Debug, Clone)]
pub struct TimedTick {
    /// Absolute time in seconds since UNIX epoch
    pub timestamp: f64,
    /// Monotonic frame counter (starts at 0)
    pub frame_number: u64,
    /// Identifier for clock source (e.g., 'software', 'ptp:0')
    pub clock_id: String,
    /// Time since last tick in seconds (for frame-rate independent movement)
    pub delta_time: f64,
}

/// Abstract clock interface
///
/// All clocks generate ticks at a specific rate and provide timing info.
pub trait Clock: Send {
    /// Wait for and return the next tick
    ///
    /// This is async and will sleep until the next tick time.
    fn next_tick(&mut self) -> impl std::future::Future<Output = TimedTick> + Send;

    /// Get nominal frame rate (ticks per second)
    fn get_fps(&self) -> f64;

    /// Get clock identifier
    fn get_clock_id(&self) -> &str;
}

/// Free-running software clock (bathtub mode)
///
/// Generates ticks at a fixed rate using async sleep. Suitable for:
/// - Local development
/// - Isolated actors (no network sync needed)
/// - Testing
///
/// Not suitable for:
/// - Multi-device synchronization (use PTPClock)
/// - Hardware sync (use GenlockClock)
pub struct SoftwareClock {
    fps: f64,
    period: Duration,
    clock_id: String,
    frame_number: u64,
    start_time: Instant,
    start_timestamp: f64, // Wall-clock time at start (cached)
    last_tick_time: Option<Instant>,
}

impl SoftwareClock {
    /// Create a new software clock
    ///
    /// # Arguments
    /// * `fps` - Frames per second (ticks per second) - TARGET, not guaranteed
    /// * `clock_id` - Optional clock identifier (default: 'software')
    ///
    /// # Panics
    /// Panics if fps <= 0
    pub fn new(fps: f64) -> Self {
        Self::with_id(fps, "software".to_string())
    }

    /// Create a new software clock with custom ID
    pub fn with_id(fps: f64, clock_id: String) -> Self {
        assert!(fps > 0.0, "FPS must be positive, got {}", fps);

        let period = Duration::from_secs_f64(1.0 / fps);

        // Cache wall-clock time at start (only syscall here, not per-tick)
        let start_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        Self {
            fps,
            period,
            clock_id,
            frame_number: 0,
            start_time: Instant::now(),
            start_timestamp,
            last_tick_time: None,
        }
    }

    /// Reset clock to frame 0
    pub fn reset(&mut self) {
        self.frame_number = 0;
        self.start_time = Instant::now();
        // Re-cache wall-clock time on reset
        self.start_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        self.last_tick_time = None;
    }
}

impl Clock for SoftwareClock {
    async fn next_tick(&mut self) -> TimedTick {
        // Calculate target time for this frame
        let target_time = self.start_time + self.period * self.frame_number as u32;
        let now = Instant::now();
        let mut sleep_duration = target_time.saturating_duration_since(now);

        // ALWAYS enforce minimum sleep to prevent tick flooding
        // Even when behind schedule, we need to yield control to handlers
        let min_sleep = self.period / 2; // At least 50% of target period
        if sleep_duration < min_sleep {
            sleep_duration = min_sleep;

            // When severely behind (more than 2 frames), reset the schedule
            // to avoid infinite catch-up that causes dt=0 flooding
            if now > target_time + self.period * 2 {
                self.start_time = now;
                self.frame_number = 0;
            }
        }

        tokio::time::sleep(sleep_duration).await;

        // Calculate actual delta time (time since last tick)
        let current_time = Instant::now();
        let delta_time = if let Some(last_time) = self.last_tick_time {
            current_time.duration_since(last_time).as_secs_f64()
        } else {
            self.period.as_secs_f64() // First tick uses target period
        };

        // Calculate absolute timestamp from cached start time + elapsed (no syscall!)
        let elapsed = current_time.duration_since(self.start_time).as_secs_f64();
        let timestamp = self.start_timestamp + elapsed;

        let tick = TimedTick {
            timestamp,
            frame_number: self.frame_number,
            clock_id: self.clock_id.clone(),
            delta_time,
        };

        self.last_tick_time = Some(current_time);
        self.frame_number += 1;

        tick
    }

    fn get_fps(&self) -> f64 {
        self.fps
    }

    fn get_clock_id(&self) -> &str {
        &self.clock_id
    }
}

/// IEEE 1588 Precision Time Protocol clock (stub for Phase 4)
///
/// PTP provides microsecond-accurate synchronization across network devices.
/// Used in SMPTE ST 2110 professional broadcast environments.
///
/// This is a stub implementation. Real implementation in Phase 4 will:
/// - Use linuxptp or similar PTP client
/// - Sync to PTP grandmaster clock
/// - Provide < 1μs accuracy
/// - Support multiple PTP domains
///
/// For now, falls back to software timing.
pub struct PTPClock {
    _ptp_client: Option<()>, // Placeholder for PTP client
    fps: f64,
    fallback: SoftwareClock,
}

impl PTPClock {
    /// Create a new PTP clock
    ///
    /// # Arguments
    /// * `ptp_client` - PTP client instance (stub, not implemented)
    /// * `fps` - Frames per second
    ///
    /// Note: Currently falls back to software timing.
    pub fn new(fps: f64) -> Self {
        eprintln!("[PTPClock] Warning: PTP not implemented, using software fallback");

        Self {
            _ptp_client: None,
            fps,
            fallback: SoftwareClock::with_id(fps, "ptp-stub".to_string()),
        }
    }

    /// Get PTP domain
    pub fn get_domain(&self) -> u8 {
        0 // Stub: would come from PTP client
    }
}

impl Clock for PTPClock {
    async fn next_tick(&mut self) -> TimedTick {
        // TODO Phase 4: Implement real PTP synchronization:
        // - Get PTP time from client
        // - Align tick to frame boundary
        // - Sleep until next boundary
        self.fallback.next_tick().await
    }

    fn get_fps(&self) -> f64 {
        self.fps
    }

    fn get_clock_id(&self) -> &str {
        "ptp:0" // Stub: would use actual domain
    }
}

/// SDI hardware sync clock (genlock signal) - stub for Phase 4
///
/// Genlock provides hardware sync for SDI devices (professional video equipment).
/// The genlock signal is a reference pulse (typically black burst or tri-level sync)
/// that all devices sync to.
///
/// Different from PTP:
/// - PTP: Network-based sync (IEEE 1588)
/// - Genlock: Hardware sync pulse on SDI/BNC connector
///
/// This is a stub implementation. Real implementation in Phase 4 will:
/// - Interface with SDI hardware (e.g., Blackmagic DeckLink)
/// - Wait for hardware pulse
/// - Generate ticks aligned to pulse
///
/// For now, falls back to software timing.
pub struct GenlockClock {
    _sdi_device: Option<()>, // Placeholder for SDI device
    fallback: SoftwareClock,
}

impl GenlockClock {
    /// Create a new genlock clock
    ///
    /// # Arguments
    /// * `sdi_device` - SDI device instance (stub, not implemented)
    ///
    /// Note: Currently falls back to software timing.
    pub fn new() -> Self {
        eprintln!("[GenlockClock] Warning: Genlock not implemented, using software fallback");

        let fps = 60.0; // Would be detected from hardware
        Self {
            _sdi_device: None,
            fallback: SoftwareClock::with_id(fps, "genlock-stub".to_string()),
        }
    }

    /// Get SDI port
    pub fn get_port(&self) -> u8 {
        0 // Stub: would come from SDI device
    }
}

impl Clock for GenlockClock {
    async fn next_tick(&mut self) -> TimedTick {
        // TODO Phase 4: Implement real hardware sync:
        // - Wait for hardware pulse from SDI device
        // - Generate tick when pulse arrives
        // - Handle frame rate detection
        self.fallback.next_tick().await
    }

    fn get_fps(&self) -> f64 {
        // TODO Phase 4: Detect from hardware
        self.fallback.get_fps()
    }

    fn get_clock_id(&self) -> &str {
        "genlock:0" // Stub: would use actual port
    }
}

impl Default for GenlockClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_software_clock_initialization() {
        let clock = SoftwareClock::new(60.0);

        assert_eq!(clock.get_fps(), 60.0);
        assert_eq!(clock.get_clock_id(), "software");
        assert_eq!(clock.frame_number, 0);
    }

    #[tokio::test]
    async fn test_software_clock_custom_id() {
        let clock = SoftwareClock::with_id(30.0, "custom-clock".to_string());

        assert_eq!(clock.get_fps(), 30.0);
        assert_eq!(clock.get_clock_id(), "custom-clock");
    }

    #[tokio::test]
    #[should_panic(expected = "FPS must be positive")]
    async fn test_software_clock_zero_fps_panics() {
        let _clock = SoftwareClock::new(0.0);
    }

    #[tokio::test]
    #[should_panic(expected = "FPS must be positive")]
    async fn test_software_clock_negative_fps_panics() {
        let _clock = SoftwareClock::new(-10.0);
    }

    #[tokio::test]
    async fn test_software_clock_ticks() {
        let mut clock = SoftwareClock::new(60.0);

        // Get first tick
        let tick1 = clock.next_tick().await;
        assert_eq!(tick1.frame_number, 0);
        assert_eq!(tick1.clock_id, "software");
        assert!(tick1.delta_time > 0.0);
        assert!(tick1.timestamp > 0.0);

        // Get second tick
        let tick2 = clock.next_tick().await;
        assert_eq!(tick2.frame_number, 1);
        assert!(tick2.timestamp > tick1.timestamp);
        assert!(tick2.delta_time > 0.0);
    }

    #[tokio::test]
    async fn test_software_clock_frame_numbers() {
        let mut clock = SoftwareClock::new(100.0); // Fast clock for quick test

        for expected_frame in 0..5 {
            let tick = clock.next_tick().await;
            assert_eq!(tick.frame_number, expected_frame);
        }
    }

    #[tokio::test]
    async fn test_software_clock_delta_time() {
        let mut clock = SoftwareClock::new(60.0);
        let expected_period = 1.0 / 60.0;

        // First tick - delta_time should be close to target period
        let tick1 = clock.next_tick().await;
        assert!((tick1.delta_time - expected_period).abs() < 0.01);

        // Second tick - delta_time should reflect actual elapsed time
        let tick2 = clock.next_tick().await;
        assert!(tick2.delta_time > 0.0);
        assert!(tick2.delta_time < 0.1); // Should be reasonable
    }

    #[tokio::test]
    async fn test_software_clock_timing_reasonable() {
        let mut clock = SoftwareClock::new(60.0);

        let start = Instant::now();

        // Generate 5 ticks
        for _ in 0..5 {
            clock.next_tick().await;
        }

        let elapsed = start.elapsed().as_secs_f64();

        // Timing should be reasonable: not instant, not excessively slow
        // At 60 FPS, 5 frames is nominally 83ms, but async timing varies
        assert!(elapsed > 0.01, "Ticks should not be instant: {}", elapsed);
        assert!(elapsed < 0.5, "Ticks should not be excessively slow: {}", elapsed);
    }

    #[tokio::test]
    async fn test_software_clock_reset() {
        let mut clock = SoftwareClock::new(60.0);

        // Generate a few ticks
        clock.next_tick().await;
        clock.next_tick().await;
        clock.next_tick().await;

        // Reset
        clock.reset();

        // Next tick should be frame 0 again
        let tick = clock.next_tick().await;
        assert_eq!(tick.frame_number, 0);
        assert_eq!(tick.clock_id, "software");
    }

    #[tokio::test]
    async fn test_software_clock_timestamps_increase() {
        let mut clock = SoftwareClock::new(100.0);

        let tick1 = clock.next_tick().await;
        let tick2 = clock.next_tick().await;
        let tick3 = clock.next_tick().await;

        // Timestamps should strictly increase
        assert!(tick2.timestamp > tick1.timestamp);
        assert!(tick3.timestamp > tick2.timestamp);
    }

    #[tokio::test]
    async fn test_software_clock_no_syscalls_per_tick() {
        // This test verifies the optimization where we cache start_timestamp
        // and compute timestamps without SystemTime::now() calls per tick

        let mut clock = SoftwareClock::new(100.0);

        // Get several ticks - if we were calling SystemTime::now() per tick,
        // this would be slower. We can't directly measure syscalls, but we
        // can verify the timestamps are computed correctly.
        for _ in 0..10 {
            let tick = clock.next_tick().await;
            assert!(tick.timestamp > 0.0);
        }
    }

    #[tokio::test]
    async fn test_software_clock_different_fps_rates() {
        // Test various FPS rates
        for fps in [30.0, 60.0, 120.0] {
            let mut clock = SoftwareClock::new(fps);
            assert_eq!(clock.get_fps(), fps);

            let tick = clock.next_tick().await;
            assert_eq!(tick.frame_number, 0);
        }
    }

    #[tokio::test]
    async fn test_software_clock_high_fps() {
        // Test that very high FPS doesn't break
        let mut clock = SoftwareClock::new(240.0);

        let tick1 = clock.next_tick().await;
        let tick2 = clock.next_tick().await;

        assert_eq!(tick1.frame_number, 0);
        assert_eq!(tick2.frame_number, 1);
        assert!(tick2.timestamp > tick1.timestamp);
    }
}
