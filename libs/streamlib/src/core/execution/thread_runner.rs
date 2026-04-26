// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor thread runner.
//!
//! Handles the main loop for processor threads based on their execution mode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::fd::OwnedFd;

use parking_lot::Mutex;

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::ProcessorUniqueId;
use crate::core::processors::{ProcessorInstance, ProcessorState};
use crate::core::RuntimeContext;
/// Duration to sleep when paused (avoids busy-waiting).
const PAUSE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Sleep cadence for the no-fd-waiter fallback paths (non-Linux, or the
/// rare case where epoll setup fails on Linux). Reactive mode on Linux
/// with a working waiter uses `epoll_wait(-1)` and never sleeps.
const NO_WAITER_FALLBACK_SLEEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the processor thread main loop based on execution mode.
#[tracing::instrument(name = "processor.lifecycle", skip(processor, shutdown_rx, shutdown_eventfd, state, pause_gate, exec_config, runtime_ctx), fields(processor_id = %id))]
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_eventfd: Option<OwnedFd>,
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
            run_reactive_mode(
                &id,
                &processor,
                &shutdown_rx,
                #[cfg(unix)]
                shutdown_eventfd,
                &pause_gate,
                &runtime_ctx,
            );
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
    #[cfg(unix)] shutdown_eventfd: Option<OwnedFd>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
) {
    // Reactive mode waits on two fds via epoll: the destination's iceoryx2
    // Listener fd (any upstream Notifier::notify() wakes the loop) and the
    // shutdown eventfd (compiler signals teardown). epoll_wait blocks
    // indefinitely — idle CPU is truly zero until one of those fds fires.
    let listener_fd = {
        let mut guard = processor.lock();
        guard
            .get_iceoryx2_input_mailboxes()
            .and_then(|m| m.listener_fd())
    };

    #[cfg(target_os = "linux")]
    let waiter = match ReactiveLoopFdWaiter::new(listener_fd, shutdown_eventfd) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                "[{}] Reactive epoll setup failed, falling back to channel-poll loop: {}",
                id,
                e
            );
            None
        }
    };

    #[cfg(not(target_os = "linux"))]
    let waiter: Option<ReactiveLoopFdWaiter> = None;

    let mut was_paused = false;

    loop {
        // Channel-side shutdown check covers two paths:
        //   1. The fallback sleep loop (no waiter — non-Linux or epoll setup
        //      failure), which has no way to wake on shutdown otherwise.
        //   2. A race where signal_shutdown() landed between the previous
        //      epoll_wait return and reading the eventfd-side outcome.
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
            // While paused we deliberately poll: the pause_gate is an
            // AtomicBool with no fd, so on_resume can't fire from epoll.
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            continue;
        }

        // Block until an upstream notify, a shutdown signal, or (only in
        // the no-waiter fallback) the next channel-poll tick.
        match waiter.as_ref() {
            Some(w) => match w.wait() {
                ReactiveLoopWakeOutcome::Notified => {
                    let mut guard = processor.lock();
                    if let Some(mailboxes) = guard.get_iceoryx2_input_mailboxes() {
                        mailboxes.drain_listener();
                    }
                }
                ReactiveLoopWakeOutcome::Shutdown => {
                    tracing::info!("[{}] Received shutdown via eventfd", id);
                    break;
                }
                ReactiveLoopWakeOutcome::Interrupted => continue,
                ReactiveLoopWakeOutcome::Error => {
                    std::thread::sleep(NO_WAITER_FALLBACK_SLEEP);
                }
            },
            None => std::thread::sleep(NO_WAITER_FALLBACK_SLEEP),
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

/// Outcome of one [`ReactiveLoopFdWaiter::wait`] call.
#[derive(Debug, Clone, Copy)]
enum ReactiveLoopWakeOutcome {
    /// Listener fd became readable — at least one upstream notify arrived.
    Notified,
    /// Shutdown eventfd became readable — runner should exit.
    Shutdown,
    /// `epoll_wait` was interrupted by a signal (`EINTR`); caller should retry.
    Interrupted,
    /// `epoll_wait` returned an unrecoverable error.
    Error,
}

/// Tag stored in `epoll_event.u64` for the shutdown eventfd; chosen so it
/// can never collide with a listener-fd tag (which we set to 0).
#[cfg(target_os = "linux")]
const SHUTDOWN_EVENTFD_TAG: u64 = u64::MAX;

/// Linux-only: epoll fd watching the iceoryx2 listener fd plus the shutdown
/// eventfd, used by the reactive runner. Non-Linux constructor returns
/// `Unsupported` so the runner falls back to channel-poll sleep.
#[cfg(target_os = "linux")]
struct ReactiveLoopFdWaiter {
    epoll_fd: i32,
    /// Stored to keep the kernel-side eventfd alive for the lifetime of the
    /// epoll registration. Closing the fd before the epoll fd would leave a
    /// dangling registration that never fires.
    _shutdown_eventfd: Option<OwnedFd>,
}

#[cfg(target_os = "linux")]
impl ReactiveLoopFdWaiter {
    fn new(
        listener_fd: Option<i32>,
        shutdown_eventfd: Option<OwnedFd>,
    ) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;

        // SAFETY: epoll_create1 returns -1 on failure; checked below.
        let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let register = |fd: i32, tag: u64| -> std::io::Result<()> {
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: tag,
            };
            // SAFETY: epoll_ctl with EPOLL_CTL_ADD takes a pointer to a
            // valid epoll_event for the duration of the call.
            let r =
                unsafe { libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut event) };
            if r < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        if let Some(fd) = listener_fd {
            if let Err(e) = register(fd, 0) {
                // SAFETY: epoll_fd is owned and unused after this point.
                unsafe { libc::close(epoll_fd) };
                return Err(e);
            }
        }
        if let Some(ref efd) = shutdown_eventfd {
            if let Err(e) = register(efd.as_raw_fd(), SHUTDOWN_EVENTFD_TAG) {
                unsafe { libc::close(epoll_fd) };
                return Err(e);
            }
        }

        Ok(Self {
            epoll_fd,
            _shutdown_eventfd: shutdown_eventfd,
        })
    }

    fn wait(&self) -> ReactiveLoopWakeOutcome {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];
        // -1 = block forever. Wakes only when one of the registered fds is
        // actually readable, or a signal interrupts the call.
        // SAFETY: epoll_wait writes up to events.len() events into the buffer.
        let n = unsafe { libc::epoll_wait(self.epoll_fd, events.as_mut_ptr(), 2, -1) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                return ReactiveLoopWakeOutcome::Interrupted;
            }
            tracing::warn!("epoll_wait failed in reactive runner: {}", err);
            return ReactiveLoopWakeOutcome::Error;
        }

        // Shutdown takes priority over notify when both fired in the same
        // wait — let the runner exit instead of draining one more frame.
        let mut notified = false;
        for ev in &events[..n as usize] {
            if ev.u64 == SHUTDOWN_EVENTFD_TAG {
                return ReactiveLoopWakeOutcome::Shutdown;
            }
            notified = true;
        }
        if notified {
            ReactiveLoopWakeOutcome::Notified
        } else {
            // n > 0 but no events matched — shouldn't happen.
            ReactiveLoopWakeOutcome::Error
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ReactiveLoopFdWaiter {
    fn drop(&mut self) {
        // SAFETY: epoll_fd is owned by Self and closed at most once. The
        // OwnedFd field drops after this; epoll_ctl(EPOLL_CTL_DEL) isn't
        // required because closing the epoll fd releases its registrations.
        unsafe { libc::close(self.epoll_fd) };
    }
}

#[cfg(not(target_os = "linux"))]
struct ReactiveLoopFdWaiter;

#[cfg(not(target_os = "linux"))]
impl ReactiveLoopFdWaiter {
    fn wait(&self) -> ReactiveLoopWakeOutcome {
        ReactiveLoopWakeOutcome::Error
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
    use std::os::fd::{AsRawFd, FromRawFd};

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

    fn make_eventfd() -> OwnedFd {
        // SAFETY: eventfd returns -1 on failure; checked below. Initial
        // counter is 0; EFD_CLOEXEC matches production.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(raw >= 0, "eventfd failed: {}", std::io::Error::last_os_error());
        // SAFETY: raw is a fresh, owned fd from a successful eventfd() call.
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    fn write_eventfd(fd: i32) {
        let buf = 1u64.to_ne_bytes();
        // SAFETY: fd is a valid eventfd; eventfd accepts 8-byte writes.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        assert!(
            n == buf.len() as isize,
            "eventfd write failed: n={n}, err={}",
            std::io::Error::last_os_error()
        );
    }

    /// The reactive runner's wake primitive: a notify() from another thread
    /// must transition `ReactiveLoopFdWaiter::wait` to Notified well within
    /// the runner's wake-latency budget. iceoryx2's `ipc::Service` Notifier
    /// is `!Send` (Rc-backed SingleThreaded threadsafety policy), so the
    /// test keeps notifier on the main thread and ships the waiter to the
    /// waiter thread.
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
        let listener_fd = unsafe { listener.file_descriptor().native_handle() };
        let waiter =
            ReactiveLoopFdWaiter::new(Some(listener_fd), Some(make_eventfd())).expect("epoll setup");

        // Move the waiter to a worker thread, then fire notify() from this
        // thread. The worker reports the outcome and elapsed time back via
        // a channel.
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome = waiter.wait();
            tx.send((outcome, started.elapsed())).unwrap();
            waiter
        });

        std::thread::sleep(std::time::Duration::from_millis(5));
        notifier.notify().unwrap();

        let (outcome, elapsed) = rx
            .recv_timeout(std::time::Duration::from_millis(800))
            .expect("worker did not respond — wait did not wake");
        let _waiter = worker.join().expect("worker panicked");

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "expected Notified, got {:?}",
            outcome
        );
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "wake latency too high: {:?} (notify was scheduled 5 ms in)",
            elapsed
        );

        // Drain so the next wait would block again — done implicitly by
        // dropping references; not asserted because there's no second wait
        // here (a second wait without re-notify would block until shutdown).
        listener.try_wait_all(|_| {}).unwrap();
    }

    /// Writing to the shutdown eventfd must transition `wait` to Shutdown
    /// within milliseconds, even when no listener-fd activity occurs. This
    /// is the runner's exit primitive — the runner breaks its loop the
    /// moment `wait` returns Shutdown, so wake latency here is exit latency.
    #[test]
    fn reactive_loop_exits_on_shutdown_signal() {
        // Build a real iceoryx2 listener fd so the waiter exercises the
        // production two-fd shape (listener + shutdown eventfd). The
        // listener never sees a notify in this test — only the shutdown
        // eventfd should fire.
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let name = unique_suffix("shutdown");
        let svc = node
            .service_builder(&ServiceName::new(&name).unwrap())
            .event()
            .max_notifiers(1)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let listener = svc.listener_builder().create().unwrap();
        // SAFETY: listener outlives the worker thread (joined below).
        let listener_fd = unsafe { listener.file_descriptor().native_handle() };

        let shutdown_eventfd = make_eventfd();
        let shutdown_raw = shutdown_eventfd.as_raw_fd();

        let waiter = ReactiveLoopFdWaiter::new(Some(listener_fd), Some(shutdown_eventfd))
            .expect("epoll setup");

        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome = waiter.wait();
            tx.send((outcome, started.elapsed())).unwrap();
            waiter
        });

        // Give the worker a moment to enter epoll_wait, then fire shutdown.
        std::thread::sleep(std::time::Duration::from_millis(5));
        write_eventfd(shutdown_raw);

        let (outcome, elapsed) = rx
            .recv_timeout(std::time::Duration::from_millis(800))
            .expect("worker did not respond — shutdown did not wake the waiter");
        let _waiter = worker.join().expect("worker panicked");

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Shutdown),
            "expected Shutdown, got {:?}",
            outcome
        );
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "shutdown wake latency too high: {:?} (eventfd write scheduled 5 ms in)",
            elapsed
        );
    }
}
