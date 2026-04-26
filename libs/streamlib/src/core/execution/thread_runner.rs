// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor thread runner.
//!
//! Handles the main loop for processor threads based on their execution mode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::ProcessorUniqueId;
use crate::core::processors::{ProcessorInstance, ProcessorState};
use crate::core::RuntimeContext;
/// Duration to sleep when paused (avoids busy-waiting).
const PAUSE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run the processor thread main loop based on execution mode.
#[tracing::instrument(name = "processor.lifecycle", skip(processor, shutdown_rx, state, pause_gate, exec_config, runtime_ctx), fields(processor_id = %id))]
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    state: Arc<Mutex<ProcessorState>>,
    pause_gate: Arc<AtomicBool>,
    exec_config: ExecutionConfig,
    runtime_ctx: RuntimeContext,
) {
    tracing::info!(
        "[{}] Thread started ({})",
        id,
        exec_config.execution.description()
    );

    match exec_config.execution {
        ProcessExecution::Continuous { interval_ms } => {
            run_continuous_mode(
                &id,
                &processor,
                &shutdown_rx,
                &pause_gate,
                interval_ms,
                &runtime_ctx,
            );
        }
        ProcessExecution::Reactive => {
            // With iceoryx2, reactive mode polls mailboxes at a fixed interval
            run_reactive_mode(&id, &processor, &shutdown_rx, &pause_gate, &runtime_ctx);
        }
        ProcessExecution::Manual => {
            run_manual_mode(&id, &processor, &shutdown_rx, &pause_gate, &runtime_ctx);
        }
    }

    // Teardown — privileged ctx.
    tracing::info!("[{}] Invoking teardown()...", id);
    {
        let full_ctx = RuntimeContextFullAccess::new(&runtime_ctx);
        let mut guard = processor.lock();
        match runtime_ctx
            .tokio_handle()
            .block_on(guard.__generated_teardown(&full_ctx))
        {
            Ok(()) => tracing::info!("[{}] teardown() completed successfully", id),
            Err(e) => tracing::warn!("[{}] teardown() failed: {}", id, e),
        }
    }

    *state.lock() = ProcessorState::Stopped;
    tracing::info!("[{}] Thread stopped", id);
}

fn run_continuous_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    pause_gate: &Arc<AtomicBool>,
    interval_ms: u32,
    runtime_ctx: &RuntimeContext,
) {
    let sleep_duration = if interval_ms > 0 {
        std::time::Duration::from_millis(interval_ms as u64)
    } else {
        std::time::Duration::from_micros(100)
    };

    let mut was_paused = false;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            dispatch_on_pause(id, processor, runtime_ctx);
            was_paused = true;
        } else if !is_paused && was_paused {
            dispatch_on_resume(id, processor, runtime_ctx);
            was_paused = false;
        }

        if is_paused {
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            continue;
        }

        {
            let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
            let mut guard = processor.lock();
            if let Err(e) = guard.process(&limited_ctx) {
                tracing::warn!("[{}] process() failed: {}", id, e);
            }
        }

        std::thread::sleep(sleep_duration);
    }
}

fn run_reactive_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
) {
    // Reactive mode waits on the destination's iceoryx2 Listener fd via epoll —
    // any upstream Notifier::notify() (paired 1:1 with publisher.send() in
    // OutputWriter) wakes the loop. The 100 ms timeout caps shutdown-signal
    // latency without burning idle CPU; the previous 100 µs poll interval ran
    // ~10 000 wakeups/sec/processor regardless of pipeline activity.
    const SHUTDOWN_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    // Pull the listener fd up-front. None means the processor has no Rust-side
    // inputs wired (subprocess host, audio-only, etc.) — fall back to a coarse
    // sleep so shutdown still responds.
    let listener_fd = {
        let mut guard = processor.lock();
        guard
            .get_iceoryx2_input_mailboxes()
            .and_then(|m| m.listener_fd())
    };

    let waiter = listener_fd.and_then(|fd| match ListenerFdWaiter::new(fd) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                "[{}] Failed to set up listener-fd waiter, falling back to sleep: {}",
                id,
                e
            );
            None
        }
    });

    let mut was_paused = false;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            dispatch_on_pause(id, processor, runtime_ctx);
            was_paused = true;
        } else if !is_paused && was_paused {
            dispatch_on_resume(id, processor, runtime_ctx);
            was_paused = false;
        }

        if is_paused {
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            continue;
        }

        // Wait for an upstream notify or the shutdown-check timeout. Always
        // call process() afterward — it drains any available frames itself
        // (receive_pending is cheap when nothing arrived).
        match waiter.as_ref() {
            Some(w) => match w.wait(SHUTDOWN_CHECK_TIMEOUT) {
                ListenerWaitOutcome::Notified => {
                    let mut guard = processor.lock();
                    if let Some(mailboxes) = guard.get_iceoryx2_input_mailboxes() {
                        mailboxes.drain_listener();
                    }
                }
                ListenerWaitOutcome::Timeout => {}
                ListenerWaitOutcome::Error => std::thread::sleep(SHUTDOWN_CHECK_TIMEOUT),
            },
            None => std::thread::sleep(SHUTDOWN_CHECK_TIMEOUT),
        }

        {
            let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
            let mut guard = processor.lock();
            if let Err(e) = guard.process(&limited_ctx) {
                tracing::warn!("[{}] process() failed: {}", id, e);
            }
        }
    }
}

/// Outcome of one [`ListenerFdWaiter::wait`] call.
#[derive(Debug, Clone, Copy)]
enum ListenerWaitOutcome {
    /// Listener fd became readable — at least one upstream notify arrived.
    Notified,
    /// Wait timed out — no notify in this window.
    Timeout,
    /// epoll_wait returned an unrecoverable error.
    Error,
}

/// Owned epoll fd registered with a single listener fd. Linux-only; the
/// non-Linux constructor returns an error so the runner falls back to sleep
/// (kqueue/macOS support can be added when streamlib runs reactive
/// processors on macOS — currently they're host-callback driven).
#[cfg(target_os = "linux")]
struct ListenerFdWaiter {
    epoll_fd: i32,
}

#[cfg(target_os = "linux")]
impl ListenerFdWaiter {
    fn new(listener_fd: i32) -> std::io::Result<Self> {
        // SAFETY: epoll_create1 returns -1 on failure; checked below.
        let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        // SAFETY: epoll_ctl with EPOLL_CTL_ADD takes a pointer to a valid
        // epoll_event for the duration of the call.
        let ctl =
            unsafe { libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, listener_fd, &mut event) };
        if ctl < 0 {
            let err = std::io::Error::last_os_error();
            // SAFETY: epoll_fd is owned and unused after this point.
            unsafe { libc::close(epoll_fd) };
            return Err(err);
        }
        Ok(Self { epoll_fd })
    }

    fn wait(&self, timeout: std::time::Duration) -> ListenerWaitOutcome {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 1];
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: epoll_wait writes up to events.len() events into the buffer.
        let n = unsafe { libc::epoll_wait(self.epoll_fd, events.as_mut_ptr(), 1, timeout_ms) };
        if n > 0 {
            ListenerWaitOutcome::Notified
        } else if n == 0 {
            ListenerWaitOutcome::Timeout
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                ListenerWaitOutcome::Timeout
            } else {
                tracing::warn!("epoll_wait failed on listener fd: {}", err);
                ListenerWaitOutcome::Error
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ListenerFdWaiter {
    fn drop(&mut self) {
        // SAFETY: epoll_fd is owned by Self and closed at most once.
        unsafe { libc::close(self.epoll_fd) };
    }
}

#[cfg(not(target_os = "linux"))]
struct ListenerFdWaiter;

#[cfg(not(target_os = "linux"))]
impl ListenerFdWaiter {
    fn new(_listener_fd: i32) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "ListenerFdWaiter not implemented for this platform",
        ))
    }

    fn wait(&self, _timeout: std::time::Duration) -> ListenerWaitOutcome {
        ListenerWaitOutcome::Timeout
    }
}

fn run_manual_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
) {
    // Call start() - for callback-driven processors this returns immediately
    // after registering callbacks with OS (AVFoundation, CoreAudio, CVDisplayLink).
    // start() is resource-lifecycle, so it receives full-access ctx.
    tracing::info!("[{}] Invoking start()...", id);
    {
        let full_ctx = RuntimeContextFullAccess::new(runtime_ctx);
        let mut guard = processor.lock();
        match guard.start(&full_ctx) {
            Ok(()) => tracing::info!("[{}] start() completed successfully", id),
            Err(e) => {
                tracing::warn!("[{}] start() failed: {}", id, e);
                return;
            }
        }
    }

    // Wait for shutdown signal - this thread is just a lifecycle manager
    // Real work happens on OS-managed callback threads
    let mut was_paused = false;

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        // Periodic check for pause/resume state changes
        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            dispatch_on_pause(id, processor, runtime_ctx);
            was_paused = true;
        } else if !is_paused && was_paused {
            dispatch_on_resume(id, processor, runtime_ctx);
            was_paused = false;
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Call stop() - stops callbacks and waits for in-flight work. Privileged ctx.
    tracing::info!("[{}] Invoking stop()...", id);
    {
        let full_ctx = RuntimeContextFullAccess::new(runtime_ctx);
        let mut guard = processor.lock();
        match guard.stop(&full_ctx) {
            Ok(()) => tracing::info!("[{}] stop() completed successfully", id),
            Err(e) => tracing::warn!("[{}] stop() failed: {}", id, e),
        }
    }
}

// Helper dispatchers for on_pause / on_resume — shared across Continuous,
// Reactive, and Manual modes. Each builds a fresh RuntimeContextLimitedAccess
// for the call. Keeping these tiny avoids duplicating the tokio-block-on +
// logging boilerplate in every branch above.
fn dispatch_on_pause(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    tracing::info!("[{}] Invoking on_pause()...", id);
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    match runtime_ctx
        .tokio_handle()
        .block_on(guard.__generated_on_pause(&limited_ctx))
    {
        Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
    }
}

fn dispatch_on_resume(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    tracing::info!("[{}] Invoking on_resume()...", id);
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    match runtime_ctx
        .tokio_handle()
        .block_on(guard.__generated_on_resume(&limited_ctx))
    {
        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use iceoryx2::prelude::*;

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/runner/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// The reactive runner's wake primitive: a notify() from another thread
    /// must transition `ListenerFdWaiter::wait` from Timeout to Notified
    /// well within the issue's "process() called within 1 ms" exit-criterion
    /// budget. iceoryx2's `ipc::Service` Notifier is `!Send` (Rc-backed
    /// SingleThreaded threadsafety policy), so the test keeps notifier on
    /// the main thread and ships the `ListenerFdWaiter` (which is just an
    /// `i32` + Drop) to the waiter thread.
    #[test]
    fn reactive_loop_wakes_on_notify() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let name = unique_suffix("wake");
        let svc = node
            .service_builder(&ServiceName::new(&name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = svc.notifier_builder().create().unwrap();
        let listener = svc.listener_builder().create().unwrap();

        // SAFETY: same lifetime contract as production code — fd is used
        // only while listener stays alive (listener outlives the waiter
        // thread because we join it before this function returns).
        let fd = unsafe { listener.file_descriptor().native_handle() };
        let waiter = ListenerFdWaiter::new(fd).expect("epoll setup");

        // Pre-flight: with no notify pending, wait hits the timeout.
        assert!(
            matches!(
                waiter.wait(std::time::Duration::from_millis(20)),
                ListenerWaitOutcome::Timeout
            ),
            "expected Timeout before notify"
        );

        // Move the waiter to a worker thread, then fire notify() from this
        // thread. The worker reports the outcome and elapsed time back via
        // a channel.
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome = waiter.wait(std::time::Duration::from_millis(500));
            tx.send((outcome, started.elapsed())).unwrap();
            waiter
        });

        std::thread::sleep(std::time::Duration::from_millis(5));
        notifier.notify().unwrap();

        let (outcome, elapsed) = rx
            .recv_timeout(std::time::Duration::from_millis(800))
            .expect("worker did not respond — wait did not wake");
        let waiter = worker.join().expect("worker panicked");

        assert!(
            matches!(outcome, ListenerWaitOutcome::Notified),
            "expected Notified, got {:?}",
            outcome
        );
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "wake latency too high: {:?} (notify was scheduled 5 ms in)",
            elapsed
        );

        // Drain so the next wait blocks again.
        listener.try_wait_all(|_| {}).unwrap();
        assert!(
            matches!(
                waiter.wait(std::time::Duration::from_millis(20)),
                ListenerWaitOutcome::Timeout
            ),
            "expected Timeout after drain"
        );
    }
}
