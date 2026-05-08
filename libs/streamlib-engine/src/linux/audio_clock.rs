// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(dead_code)]

use crate::core::context::{AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext};
use crate::core::{Result, StreamError};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// Linux audio clock using `timerfd_create(CLOCK_MONOTONIC)` for drift-free timing.
///
/// Uses kernel-managed timers via timerfd for high-precision, absolute-time
/// scheduling without cumulative drift. A dedicated thread reads from the
/// timerfd and invokes registered callbacks on each tick.
pub struct LinuxTimerFdAudioClock {
    config: AudioClockConfig,
    callbacks: Arc<Mutex<Vec<AudioTickCallback>>>,
    running: Arc<AtomicBool>,
    tick_count: Arc<AtomicU64>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
}

impl LinuxTimerFdAudioClock {
    /// Create a new Linux timerfd audio clock with the given configuration.
    pub fn new(config: AudioClockConfig) -> Self {
        Self {
            config,
            callbacks: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            tick_count: Arc::new(AtomicU64::new(0)),
            thread_handle: Mutex::new(None),
        }
    }

    /// Create a new Linux timerfd audio clock with default configuration (48kHz, 512 samples).
    pub fn with_defaults() -> Self {
        Self::new(AudioClockConfig::default())
    }
}

impl AudioClock for LinuxTimerFdAudioClock {
    fn on_tick(&self, callback: AudioTickCallback) {
        self.callbacks.lock().push(callback);
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn buffer_size(&self) -> usize {
        self.config.buffer_size
    }

    fn start(&self) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(()); // Already running
        }

        self.running.store(true, Ordering::SeqCst);
        self.tick_count.store(0, Ordering::SeqCst);

        let config = self.config;
        let callbacks = Arc::clone(&self.callbacks);
        let running = Arc::clone(&self.running);
        let tick_count = Arc::clone(&self.tick_count);

        let handle = thread::Builder::new()
            .name("audio-clock-timerfd".to_string())
            .spawn(move || {
                if let Err(e) = run_timerfd_loop(config, &callbacks, &running, &tick_count) {
                    tracing::error!("[LinuxTimerFdAudioClock] Timer loop failed: {}", e);
                    running.store(false, Ordering::SeqCst);
                }
                tracing::info!("[LinuxTimerFdAudioClock] Stopped");
            })
            .map_err(|e| {
                StreamError::Runtime(format!(
                    "Failed to spawn audio clock timerfd thread: {}",
                    e
                ))
            })?;

        *self.thread_handle.lock() = Some(handle);

        tracing::info!(
            "[LinuxTimerFdAudioClock] Started: {}Hz, {} samples/tick, {}ns interval",
            self.config.sample_rate,
            self.config.buffer_size,
            self.config.tick_duration_nanos()
        );

        Ok(())
    }

    fn stop(&self) -> Result<()> {
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

impl Drop for LinuxTimerFdAudioClock {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Run the timerfd-based timing loop.
///
/// Creates a timerfd with `CLOCK_MONOTONIC`, sets it to fire at absolute intervals
/// using `TFD_TIMER_ABSTIME` to prevent drift, then blocks on reads until shutdown.
fn run_timerfd_loop(
    config: AudioClockConfig,
    callbacks: &Arc<Mutex<Vec<AudioTickCallback>>>,
    running: &Arc<AtomicBool>,
    tick_count: &Arc<AtomicU64>,
) -> Result<()> {
    let interval_ns = config.tick_duration_nanos();
    let interval_sec = (interval_ns / 1_000_000_000) as libc::time_t;
    let interval_nsec = (interval_ns % 1_000_000_000) as libc::c_long;

    // Create timerfd with CLOCK_MONOTONIC and TFD_NONBLOCK so we can check shutdown
    let timer_fd = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_NONBLOCK) };

    if timer_fd < 0 {
        return Err(StreamError::Runtime(format!(
            "timerfd_create failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    // Get current time to set the first absolute expiration
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let clock_ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    if clock_ret < 0 {
        unsafe { libc::close(timer_fd) };
        return Err(StreamError::Runtime(format!(
            "clock_gettime failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    // Calculate first expiration: now + one interval
    let mut start_sec = now.tv_sec;
    let mut start_nsec = now.tv_nsec + interval_nsec;
    if start_nsec >= 1_000_000_000 {
        start_sec += start_nsec as libc::time_t / 1_000_000_000;
        start_nsec %= 1_000_000_000;
    }
    start_sec += interval_sec;

    // Record the start time in nanoseconds for timestamp calculation
    let start_time_ns =
        now.tv_sec as i64 * 1_000_000_000 + now.tv_nsec as i64;

    // Set timerfd with TFD_TIMER_ABSTIME for drift-free repeats
    let timer_spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: interval_sec,
            tv_nsec: interval_nsec,
        },
        it_value: libc::timespec {
            tv_sec: start_sec,
            tv_nsec: start_nsec,
        },
    };

    let set_ret = unsafe {
        libc::timerfd_settime(
            timer_fd,
            libc::TFD_TIMER_ABSTIME,
            &timer_spec,
            std::ptr::null_mut(),
        )
    };
    if set_ret < 0 {
        unsafe { libc::close(timer_fd) };
        return Err(StreamError::Runtime(format!(
            "timerfd_settime failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    // Use epoll to wait on the timerfd with a timeout so we can check shutdown
    let epoll_fd = unsafe { libc::epoll_create1(0) };
    if epoll_fd < 0 {
        unsafe { libc::close(timer_fd) };
        return Err(StreamError::Runtime(format!(
            "epoll_create1 failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0,
    };
    let ctl_ret = unsafe {
        libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, timer_fd, &mut event)
    };
    if ctl_ret < 0 {
        unsafe {
            libc::close(epoll_fd);
            libc::close(timer_fd);
        }
        return Err(StreamError::Runtime(format!(
            "epoll_ctl failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    tracing::info!(
        "[LinuxTimerFdAudioClock] timerfd loop running (interval: {}ns)",
        interval_ns
    );

    // Main loop: epoll_wait with short timeout, read timerfd on ready
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 1];

    while running.load(Ordering::SeqCst) {
        // Wait up to 10ms so we can check shutdown flag periodically
        let nfds = unsafe { libc::epoll_wait(epoll_fd, events.as_mut_ptr(), 1, 10) };

        if nfds < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue; // EINTR — retry
            }
            unsafe {
                libc::close(epoll_fd);
                libc::close(timer_fd);
            }
            return Err(StreamError::Runtime(format!(
                "epoll_wait failed: {}",
                err
            )));
        }

        if nfds == 0 {
            // Timeout — no timer event, loop to check shutdown
            continue;
        }

        // Timer fired — read the expiration count
        let mut expirations: u64 = 0;
        let read_ret = unsafe {
            libc::read(
                timer_fd,
                &mut expirations as *mut u64 as *mut libc::c_void,
                std::mem::size_of::<u64>(),
            )
        };

        if read_ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                continue; // Spurious wakeup
            }
            unsafe {
                libc::close(epoll_fd);
                libc::close(timer_fd);
            }
            return Err(StreamError::Runtime(format!(
                "timerfd read failed: {}",
                err
            )));
        }

        // Get current time for timestamp
        let mut current_time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut current_time) };
        let current_ns =
            current_time.tv_sec as i64 * 1_000_000_000 + current_time.tv_nsec as i64;
        let elapsed_ns = current_ns - start_time_ns;

        if expirations > 1 {
            tracing::warn!(
                "[LinuxTimerFdAudioClock] Missed {} ticks, catching up",
                expirations - 1
            );
        }

        // Fire callback for each expiration
        for _ in 0..expirations {
            let tick_num = tick_count.fetch_add(1, Ordering::SeqCst);

            let ctx = AudioTickContext {
                timestamp_ns: elapsed_ns,
                samples_needed: config.buffer_size,
                sample_rate: config.sample_rate,
                tick_number: tick_num,
            };

            let cbs = callbacks.lock();
            for callback in cbs.iter() {
                callback(ctx);
            }
        }
    }

    // Cleanup
    unsafe {
        libc::close(epoll_fd);
        libc::close(timer_fd);
    }

    Ok(())
}
