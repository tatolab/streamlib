// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Audio clock for synchronized audio production.
//!
//! Provides a timing source for audio processors to produce samples at the correct rate.
//! The clock is device-independent - sinks handle resampling to their specific devices.

use std::sync::Arc;

/// Configuration for an audio clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioClockConfig {
    /// Sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Number of samples per tick/callback (e.g., 512).
    pub buffer_size: usize,
}

impl Default for AudioClockConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            buffer_size: 512, // ~10.67ms per tick at 48kHz
        }
    }
}

impl AudioClockConfig {
    /// Create a new audio clock configuration.
    pub fn new(sample_rate: u32, buffer_size: usize) -> Self {
        Self {
            sample_rate,
            buffer_size,
        }
    }

    /// Duration of one tick in seconds.
    pub fn tick_duration_secs(&self) -> f64 {
        self.buffer_size as f64 / self.sample_rate as f64
    }

    /// Duration of one tick in nanoseconds.
    pub fn tick_duration_nanos(&self) -> u64 {
        ((self.buffer_size as f64 / self.sample_rate as f64) * 1_000_000_000.0) as u64
    }
}

/// Context passed to audio clock tick callbacks.
#[derive(Debug, Clone, Copy)]
pub struct AudioTickContext {
    /// Monotonic timestamp in nanoseconds.
    pub timestamp_ns: i64,
    /// Number of samples to produce this tick (per channel).
    pub samples_needed: usize,
    /// Sample rate of the clock in Hz.
    pub sample_rate: u32,
    /// Tick number (starts at 0, increments each tick).
    pub tick_number: u64,
}

/// Callback type for audio clock ticks.
pub type AudioTickCallback = Box<dyn Fn(AudioTickContext) + Send + Sync>;

/// Audio clock providing synchronized timing for audio production.
///
/// The clock fires callbacks at a regular interval determined by the configured
/// sample rate and buffer size. Audio producers subscribe to these callbacks
/// and produce exactly the requested number of samples each tick.
///
/// Implementations may be backed by:
/// - Software timer (cross-platform fallback)
/// - Platform-specific APIs (GCD on macOS, etc.)
pub trait AudioClock: Send + Sync {
    /// Register a callback to be invoked each tick.
    ///
    /// The callback receives an [`AudioTickContext`] with timing information
    /// and the number of samples to produce.
    ///
    /// Multiple callbacks can be registered; they are invoked in registration order.
    fn on_tick(&self, callback: AudioTickCallback);

    /// Get the clock's sample rate in Hz.
    fn sample_rate(&self) -> u32;

    /// Get the number of samples per tick (buffer size).
    fn buffer_size(&self) -> usize;

    /// Get the clock's configuration.
    fn config(&self) -> AudioClockConfig {
        AudioClockConfig {
            sample_rate: self.sample_rate(),
            buffer_size: self.buffer_size(),
        }
    }

    /// Start the clock. Callbacks begin firing after this is called.
    fn start(&self) -> crate::core::Result<()>;

    /// Stop the clock. No more callbacks will fire after this returns.
    fn stop(&self) -> crate::core::Result<()>;

    /// Check if the clock is currently running.
    fn is_running(&self) -> bool;
}

/// Type alias for a shared audio clock reference.
pub type SharedAudioClock = Arc<dyn AudioClock>;

// ============================================================================
// SoftwareAudioClock - Cross-platform fallback implementation
// ============================================================================

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Software-based audio clock using high-resolution timers.
///
/// This is the cross-platform fallback implementation. It uses a dedicated
/// thread with precise sleep timing to fire callbacks at the audio rate.
///
/// For better precision on specific platforms, use platform-specific
/// implementations (e.g., CoreAudioClock on macOS).
pub struct SoftwareAudioClock {
    config: AudioClockConfig,
    callbacks: Arc<Mutex<Vec<AudioTickCallback>>>,
    running: Arc<AtomicBool>,
    tick_count: Arc<AtomicU64>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SoftwareAudioClock {
    /// Create a new software audio clock with the given configuration.
    pub fn new(config: AudioClockConfig) -> Self {
        Self {
            config,
            callbacks: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            tick_count: Arc::new(AtomicU64::new(0)),
            thread_handle: Mutex::new(None),
        }
    }

    /// Create a new software audio clock with default configuration (48kHz, 512 samples).
    pub fn with_defaults() -> Self {
        Self::new(AudioClockConfig::default())
    }
}

impl AudioClock for SoftwareAudioClock {
    fn on_tick(&self, callback: AudioTickCallback) {
        self.callbacks.lock().push(callback);
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn buffer_size(&self) -> usize {
        self.config.buffer_size
    }

    fn start(&self) -> crate::core::Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(()); // Already running
        }

        self.running.store(true, Ordering::SeqCst);
        self.tick_count.store(0, Ordering::SeqCst);

        let config = self.config;
        let callbacks = Arc::clone(&self.callbacks);
        let running = Arc::clone(&self.running);
        let tick_count = Arc::clone(&self.tick_count);

        let tick_duration = Duration::from_nanos(config.tick_duration_nanos());

        let handle = thread::Builder::new()
            .name("audio-clock".to_string())
            .spawn(move || {
                tracing::info!(
                    "[SoftwareAudioClock] Started: {}Hz, {} samples/tick, {:?} interval",
                    config.sample_rate,
                    config.buffer_size,
                    tick_duration
                );

                let start_time = Instant::now();
                let mut next_tick = start_time + tick_duration;

                while running.load(Ordering::SeqCst) {
                    let now = Instant::now();

                    if now >= next_tick {
                        let tick_num = tick_count.fetch_add(1, Ordering::SeqCst);
                        let elapsed = start_time.elapsed();
                        let timestamp_ns = elapsed.as_nanos() as i64;

                        let ctx = AudioTickContext {
                            timestamp_ns,
                            samples_needed: config.buffer_size,
                            sample_rate: config.sample_rate,
                            tick_number: tick_num,
                        };

                        // Invoke all registered callbacks
                        let cbs = callbacks.lock();
                        for callback in cbs.iter() {
                            callback(ctx);
                        }

                        // Schedule next tick relative to ideal time to prevent drift
                        next_tick += tick_duration;

                        // If we've fallen behind, catch up to now + one tick
                        if next_tick < now {
                            let missed = ((now - next_tick).as_nanos() as u64
                                / tick_duration.as_nanos() as u64)
                                + 1;
                            next_tick = now + tick_duration;
                            if missed > 1 {
                                tracing::warn!(
                                    "[SoftwareAudioClock] Missed {} ticks, catching up",
                                    missed
                                );
                            }
                        }
                    }

                    // Sleep until next tick (with some margin for wakeup latency)
                    let sleep_time = next_tick.saturating_duration_since(Instant::now());
                    if !sleep_time.is_zero() {
                        // Use spin_sleep for sub-millisecond precision if available
                        // Otherwise fall back to std::thread::sleep
                        thread::sleep(sleep_time);
                    }
                }

                tracing::info!("[SoftwareAudioClock] Stopped");
            })
            .map_err(|e| {
                crate::core::StreamError::Runtime(format!(
                    "Failed to spawn audio clock thread: {}",
                    e
                ))
            })?;

        *self.thread_handle.lock() = Some(handle);

        Ok(())
    }

    fn stop(&self) -> crate::core::Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(()); // Not running
        }

        self.running.store(false, Ordering::SeqCst);

        // Wait for thread to finish
        if let Some(handle) = self.thread_handle.lock().take() {
            let _ = handle.join();
        }

        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for SoftwareAudioClock {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
