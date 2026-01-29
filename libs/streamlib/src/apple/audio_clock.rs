// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS/iOS CoreAudioClock implementation using Grand Central Dispatch timers.
//!
//! Provides high-precision audio timing using kernel-optimized GCD timers
//! instead of software timers. This offers better precision and power efficiency.

#![allow(dead_code)]

use crate::apple::time::mach_now_ns;
use crate::core::context::{AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext};
use crate::core::{Result, StreamError};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// =============================================================================
// GCD FFI Bindings
// =============================================================================

#[allow(non_camel_case_types)]
type dispatch_queue_t = *mut c_void;
#[allow(non_camel_case_types)]
type dispatch_source_t = *mut c_void;
#[allow(non_camel_case_types)]
type dispatch_time_t = u64;

/// DISPATCH_TIME_NOW constant.
const DISPATCH_TIME_NOW: dispatch_time_t = 0;

/// Quality of Service class for user-interactive work.
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

#[link(name = "System", kind = "dylib")]
extern "C" {
    // Dispatch queue creation
    fn dispatch_queue_create_with_target(
        label: *const i8,
        attr: *const c_void,
        target: dispatch_queue_t,
    ) -> dispatch_queue_t;

    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> dispatch_queue_t;

    // Dispatch source creation and management
    fn dispatch_source_create(
        type_: *const c_void,
        handle: usize,
        mask: usize,
        queue: dispatch_queue_t,
    ) -> dispatch_source_t;

    fn dispatch_source_set_timer(
        source: dispatch_source_t,
        start: dispatch_time_t,
        interval: u64,
        leeway: u64,
    );

    fn dispatch_source_set_event_handler_f(
        source: dispatch_source_t,
        handler: extern "C" fn(*mut c_void),
    );

    fn dispatch_set_context(object: *mut c_void, context: *mut c_void);

    fn dispatch_resume(object: *mut c_void);
    fn dispatch_suspend(object: *mut c_void);
    fn dispatch_source_cancel(source: dispatch_source_t);

    fn dispatch_release(object: *mut c_void);

    // Dispatch time
    fn dispatch_time(when: dispatch_time_t, delta: i64) -> dispatch_time_t;

    // Timer source type
    static _dispatch_source_type_timer: c_void;
}

/// Get the DISPATCH_SOURCE_TYPE_TIMER constant.
fn dispatch_source_type_timer() -> *const c_void {
    unsafe { &_dispatch_source_type_timer as *const _ }
}

// =============================================================================
// CoreAudioClock Implementation
// =============================================================================

/// Shared state for the GCD timer callback.
struct CoreAudioClockState {
    config: AudioClockConfig,
    callbacks: Mutex<Vec<AudioTickCallback>>,
    tick_count: AtomicU64,
    start_time_ns: AtomicU64,
}

/// macOS/iOS audio clock using GCD dispatch timers.
///
/// Uses `dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER)` for kernel-optimized
/// timing with better precision and power efficiency than software timers.
pub struct CoreAudioClock {
    state: Arc<CoreAudioClockState>,
    timer_source: Mutex<Option<dispatch_source_t>>,
    queue: dispatch_queue_t,
    running: AtomicBool,
}

// SAFETY: CoreAudioClock manages its GCD resources safely and can be shared across threads.
unsafe impl Send for CoreAudioClock {}
unsafe impl Sync for CoreAudioClock {}

impl CoreAudioClock {
    /// Create a new CoreAudioClock with the given configuration.
    pub fn new(config: AudioClockConfig) -> Self {
        // Create a high-priority serial queue for audio callbacks
        let queue = unsafe {
            let global_queue = dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE as isize, 0);
            dispatch_queue_create_with_target(
                c"com.tatolab.streamlib.audio-clock".as_ptr(),
                std::ptr::null(),
                global_queue,
            )
        };

        Self {
            state: Arc::new(CoreAudioClockState {
                config,
                callbacks: Mutex::new(Vec::new()),
                tick_count: AtomicU64::new(0),
                start_time_ns: AtomicU64::new(0),
            }),
            timer_source: Mutex::new(None),
            queue,
            running: AtomicBool::new(false),
        }
    }

    /// Create a new CoreAudioClock with default configuration (48kHz, 512 samples).
    pub fn with_defaults() -> Self {
        Self::new(AudioClockConfig::default())
    }
}

/// GCD timer callback function.
extern "C" fn timer_callback(context: *mut c_void) {
    if context.is_null() {
        return;
    }

    // SAFETY: Context was created from Arc::into_raw and is valid for the timer's lifetime
    let state = unsafe { &*(context as *const CoreAudioClockState) };

    let tick_num = state.tick_count.fetch_add(1, Ordering::SeqCst);
    let start_ns = state.start_time_ns.load(Ordering::SeqCst);
    let now_ns = mach_now_ns();
    let elapsed_ns = now_ns - start_ns as i64;

    let ctx = AudioTickContext {
        timestamp_ns: elapsed_ns,
        samples_needed: state.config.buffer_size,
        sample_rate: state.config.sample_rate,
        tick_number: tick_num,
    };

    // Invoke all registered callbacks
    let callbacks = state.callbacks.lock();
    for callback in callbacks.iter() {
        callback(ctx);
    }
}

impl AudioClock for CoreAudioClock {
    fn on_tick(&self, callback: AudioTickCallback) {
        self.state.callbacks.lock().push(callback);
    }

    fn sample_rate(&self) -> u32 {
        self.state.config.sample_rate
    }

    fn buffer_size(&self) -> usize {
        self.state.config.buffer_size
    }

    fn start(&self) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(()); // Already running
        }

        // Reset tick count and record start time
        self.state.tick_count.store(0, Ordering::SeqCst);
        self.state
            .start_time_ns
            .store(mach_now_ns() as u64, Ordering::SeqCst);

        // Calculate interval in nanoseconds
        let interval_ns = self.state.config.tick_duration_nanos();

        // Create GCD timer source
        let timer_source =
            unsafe { dispatch_source_create(dispatch_source_type_timer(), 0, 0, self.queue) };

        if timer_source.is_null() {
            return Err(StreamError::Runtime(
                "Failed to create GCD timer source".into(),
            ));
        }

        // Set timer to fire at interval with minimal leeway for precision
        // Leeway of 0 requests maximum precision (audio critical)
        unsafe {
            let start_time = dispatch_time(DISPATCH_TIME_NOW, 0);
            dispatch_source_set_timer(timer_source, start_time, interval_ns, 0);

            // Set context (increment ref count for the callback)
            let state_ptr = Arc::into_raw(Arc::clone(&self.state)) as *mut c_void;
            dispatch_set_context(timer_source, state_ptr);

            // Set callback
            dispatch_source_set_event_handler_f(timer_source, timer_callback);

            // Resume (timers start suspended)
            dispatch_resume(timer_source);
        }

        *self.timer_source.lock() = Some(timer_source);
        self.running.store(true, Ordering::SeqCst);

        tracing::info!(
            "[CoreAudioClock] Started: {}Hz, {} samples/tick, {:?}ns interval",
            self.state.config.sample_rate,
            self.state.config.buffer_size,
            interval_ns
        );

        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(()); // Not running
        }

        self.running.store(false, Ordering::SeqCst);

        if let Some(timer_source) = self.timer_source.lock().take() {
            unsafe {
                // Cancel the timer source
                dispatch_source_cancel(timer_source);

                // Release the Arc reference we gave to the callback
                // SAFETY: This matches the Arc::into_raw in start()
                let state_ptr = std::ptr::null_mut::<c_void>();
                dispatch_set_context(timer_source, state_ptr);
                Arc::decrement_strong_count(Arc::as_ptr(&self.state));

                // Release the timer source
                dispatch_release(timer_source);
            }
        }

        tracing::info!("[CoreAudioClock] Stopped");
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for CoreAudioClock {
    fn drop(&mut self) {
        let _ = self.stop();
        // Release the dispatch queue
        if !self.queue.is_null() {
            unsafe {
                dispatch_release(self.queue);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn test_core_audio_clock_creation() {
        let clock = CoreAudioClock::with_defaults();
        assert_eq!(clock.sample_rate(), 48000);
        assert_eq!(clock.buffer_size(), 512);
        assert!(!clock.is_running());
    }

    #[test]
    fn test_core_audio_clock_start_stop() {
        let clock = CoreAudioClock::with_defaults();

        clock.start().expect("Failed to start clock");
        assert!(clock.is_running());

        clock.stop().expect("Failed to stop clock");
        assert!(!clock.is_running());
    }

    #[test]
    fn test_core_audio_clock_callback() {
        let clock = CoreAudioClock::with_defaults();
        let tick_count = Arc::new(AtomicUsize::new(0));
        let tick_count_clone = Arc::clone(&tick_count);

        clock.on_tick(Box::new(move |_ctx| {
            tick_count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        clock.start().expect("Failed to start clock");

        // Wait for a few ticks (~50ms at 48kHz/512 samples = ~10.67ms per tick)
        std::thread::sleep(Duration::from_millis(100));

        clock.stop().expect("Failed to stop clock");

        let ticks = tick_count.load(Ordering::SeqCst);
        // Should have received several ticks
        assert!(ticks >= 5, "Expected at least 5 ticks, got {}", ticks);
    }
}
