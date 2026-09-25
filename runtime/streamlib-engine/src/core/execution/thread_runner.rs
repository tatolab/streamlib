// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor thread runner.
//!
//! Handles the main loop for processor threads based on their execution mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use std::os::fd::OwnedFd;

use parking_lot::Mutex;

use crate::core::RuntimeContext;
use crate::core::context::{IsolationTier, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{ObservableProcessorState, ProcessorUniqueId};
use crate::core::processors::{ProcessorInstance, ProcessorState};
/// Duration to sleep when paused (avoids busy-waiting).
const PAUSE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Sleep cadence for the no-fd-waiter fallback paths (a platform with neither
/// epoll nor kqueue, or the rare case where waiter setup fails). Reactive mode
/// with a working waiter blocks in its wait up to its bound and never sleeps.
const NO_WAITER_FALLBACK_SLEEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the processor thread main loop based on execution mode.
#[tracing::instrument(name = "processor.lifecycle", skip(processor, shutdown_rx, shutdown_wake_fd, state, pause_gate, exec_config, runtime_ctx), fields(processor_id = %id, isolation_tier = isolation_tier.as_str()))]
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_wake_fd: Option<OwnedFd>,
    state: Arc<ObservableProcessorState>,
    pause_gate: Arc<AtomicBool>,
    exec_config: ExecutionConfig,
    runtime_ctx: RuntimeContext,
    isolation_tier: IsolationTier,
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
                shutdown_wake_fd,
                &pause_gate,
                &runtime_ctx,
            );
        }
        ProcessExecution::Manual => {
            run_manual_mode(
                &id,
                &processor,
                &shutdown_rx,
                &state,
                &pause_gate,
                &runtime_ctx,
                isolation_tier,
            );
        }
    }

    // Teardown — privileged ctx. Gated by the isolation trust axis: an
    // untrusted tier yields no `FullAccessGrant`, so no in-process FullAccess
    // teardown runs (privileged lifecycle belongs behind the subprocess
    // sandbox). An untrusted processor never ran its setup in-process either,
    // so there is nothing to tear down here.
    match isolation_tier.grant_full_access() {
        Some(full_access_grant) => {
            tracing::info!("[{}] Invoking teardown()...", id);
            let full_ctx = RuntimeContextFullAccess::new(&runtime_ctx, full_access_grant);
            let mut guard = processor.lock();
            // block_on is now internal to ProcessorInstance::teardown's
            // dispatch (LegacyDyn variant) or the cdylib's vtable
            // wrapper (VTable variant).
            match guard.teardown(&full_ctx) {
                Ok(()) => tracing::info!("[{}] teardown() completed successfully", id),
                Err(e) => tracing::warn!("[{}] teardown() failed: {}", id, e),
            }
        }
        None => {
            tracing::debug!(
                "[{}] Untrusted isolation tier ({}): skipping in-process teardown()",
                id,
                isolation_tier.as_str(),
            );
        }
    }

    state.transition_to_unless_already_failed(ProcessorState::Stopped);
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

        dispatch_process(id, processor, runtime_ctx);

        std::thread::sleep(sleep_duration);
    }
}

fn run_reactive_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
) {
    run_reactive_scheduling_loop(
        id,
        processor,
        shutdown_rx,
        #[cfg(unix)]
        shutdown_wake_fd,
        pause_gate,
        |callback| match callback {
            ReactiveRunnerProcessorCallback::OnPause => {
                dispatch_on_pause(id, processor, runtime_ctx)
            }
            ReactiveRunnerProcessorCallback::OnResume => {
                dispatch_on_resume(id, processor, runtime_ctx)
            }
            ReactiveRunnerProcessorCallback::Process => {
                dispatch_process(id, processor, runtime_ctx)
            }
        },
    );
}

/// One of the processor's own callbacks the reactive runner decides to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReactiveRunnerProcessorCallback {
    OnPause,
    OnResume,
    Process,
}

/// The reactive runner's scheduling: when to wait, when to drain the listener,
/// and when to call the processor, which `call_processor` does.
fn run_reactive_scheduling_loop(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &AtomicBool,
    mut call_processor: impl FnMut(ReactiveRunnerProcessorCallback),
) {
    // Reactive mode waits on two fds through epoll on Linux and kqueue on
    // macOS: the destination's iceoryx2 Listener fd (any upstream
    // Notifier::notify() wakes the loop) and the shutdown wake fd (compiler
    // signals teardown). The wait blocks until one of those fds fires or its
    // bound elapses — idle CPU is a wake every REACTIVE_WAIT_BOUND and nothing
    // more.
    //
    // Processors with no Rust-side listener fd (subprocess host, audio-only,
    // etc.) fall through to the channel-poll sleep loop, waking at
    // NO_WAITER_FALLBACK_SLEEP cadence. Waking is not dispatching: the loop
    // below gates every `process()` on a read having something to return, so a
    // processor whose ports are empty — or not wired yet — wakes on that
    // cadence and goes back to sleep. That is the same rule the helper loop
    // has always applied to every Python processor.
    //
    // The listener is followed on every pass, not registered once before the
    // loop: a processor added to a running graph gets its first inbound link
    // — and with it the listener — only when a later connect wires it, and a
    // destination whose last link went away gets a new listener with the next.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut shutdown_wake_fd = shutdown_wake_fd;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut waiter: Option<ReactiveLoopFdWaiter> = None;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut waiter_setup_failed = false;

    let mut was_paused = false;

    loop {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (listener_fd, listener_generation) = {
                let guard = processor.lock();
                match guard.iceoryx2_input_mailboxes_inner() {
                    Some(inner) => (inner.listener_fd(), inner.listener_generation()),
                    None => (None, 0),
                }
            };
            refresh_reactive_loop_waiter(
                id,
                &mut waiter,
                &mut shutdown_wake_fd,
                &mut waiter_setup_failed,
                listener_fd,
                listener_generation,
            );
        }

        // Channel-side shutdown check covers two paths:
        //   1. The fallback sleep loop (no waiter, or waiter setup failure),
        //      which has no way to wake on shutdown otherwise.
        //   2. A race where signal_shutdown() landed between the previous
        //      wait's return and reading the wake-fd-side outcome.
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            call_processor(ReactiveRunnerProcessorCallback::OnPause);
            was_paused = true;
        } else if !is_paused && was_paused {
            call_processor(ReactiveRunnerProcessorCallback::OnResume);
            was_paused = false;
        }

        // Every path that is not waking on the listener fd still owns that
        // listener, and upstream still notifies it on every frame — so each
        // one drains. Skip a drain here and the listener's queue fills, after
        // which iceoryx2 warns per frame for the rest of the run (#1764).
        if is_paused {
            // While paused we deliberately poll: the pause_gate is an
            // AtomicBool with no fd, so on_resume can't fire from the wait.
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            drain_input_listener(processor);
            continue;
        }

        // A bag can be waiting with no notification left to wake on it: one
        // that arrived during a pause, whose notifications the paused ticks
        // drained, or one published after a first link's subscriber existed
        // and before its listener did, which notified nobody. So the runner
        // asks before it waits. A bag that lands after the check notifies, and
        // the wait below ends for it — at the latest at its bound, when the
        // listener was replaced since the waiter was built.
        let a_read_is_already_waiting = ports_would_return_something(processor).unwrap_or(false);

        if !a_read_is_already_waiting {
            // Block until an upstream notify, a shutdown signal, or (in the
            // no-waiter and wait-error fallbacks) the next channel-poll tick.
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            match waiter.as_ref() {
                Some(w) => match w.wait(REACTIVE_WAIT_BOUND) {
                    ReactiveLoopWakeOutcome::Notified => drain_input_listener(processor),
                    ReactiveLoopWakeOutcome::Shutdown => {
                        tracing::info!("[{}] Received shutdown via the wake fd", id);
                        break;
                    }
                    ReactiveLoopWakeOutcome::Interrupted | ReactiveLoopWakeOutcome::TimedOut => {
                        continue;
                    }
                    ReactiveLoopWakeOutcome::Error => {
                        std::thread::sleep(NO_WAITER_FALLBACK_SLEEP);
                        drain_input_listener(processor);
                    }
                },
                None => {
                    std::thread::sleep(NO_WAITER_FALLBACK_SLEEP);
                    drain_input_listener(processor);
                }
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                std::thread::sleep(NO_WAITER_FALLBACK_SLEEP);
                drain_input_listener(processor);
            }

            // The first dispatch after a wait is gated on readiness like every
            // later one. A wake is not evidence that a read would return
            // anything: an audio input port declaring a window contract reports
            // data only when a full window can be emitted, so a bag that does
            // not complete one wakes this loop and must not dispatch. The helper
            // loop already gates every dispatch this way; this is the
            // app-process half of the same rule. A processor with no mailboxes
            // to ask has nothing to gate on, and gating on an absent answer
            // would stop it running at all.
            if !ports_would_return_something(processor).unwrap_or(true) {
                continue;
            }
        }

        // Drain-loop dispatch: iceoryx2's Event service coalesces
        // multiple notify()s on the same EventId into one fd-readable
        // transition (the underlying IdTracker is a bit-set, not a
        // counter). After `drain_listener` clears that bit, the
        // listener fd is not-readable again, and the next wait
        // would block — even though the subscriber's shared-memory
        // ring and the per-port mailboxes may still hold unread
        // samples from the same burst. Call `process()` until every
        // input port reports empty, then go back to sleep. This is
        // the standard level-triggered drain pattern (libuv, tokio
        // reactor, GStreamer base-src loop). Skips entirely for
        // processors that have no input mailboxes (manual sources),
        // which fall back to the single-process() shape.
        //
        // A shutdown_rx check is interleaved with each drain iteration
        // so a producer that publishes faster than the consumer can
        // drain (sustained back-pressure) doesn't starve the runner's
        // shutdown signaling — without it, the outer loop's
        // shutdown_rx.try_recv at the top never fires.
        loop {
            call_processor(ReactiveRunnerProcessorCallback::Process);

            if shutdown_rx.try_recv().is_ok() {
                tracing::info!("[{}] Received shutdown signal mid-drain", id);
                return;
            }

            // Drained on every dispatch, not only on a wake: a processor slower
            // than its upstream stays in this loop for as long as bags keep
            // arriving, each one notifies, and a listener left undrained here
            // fills within a few hundred of them. Drained before the readiness
            // check below, so a bag whose notify this clears is one that check
            // sees.
            drain_input_listener(processor);

            // A processor with no mailboxes drains in one dispatch, which is
            // the single-`process()` shape this loop's doc describes.
            if !ports_would_return_something(processor).unwrap_or(false) {
                break;
            }
        }
    }
}

/// Whether any of this processor's input ports would hand a reader something.
///
/// `None` when there are no mailboxes to ask — a processor that declared no
/// input ports, or whose handle is not wired yet. Callers want different
/// defaults for that case and each says which, rather than this picking one:
/// the gate after a wait must not silence a processor it cannot ask, while the
/// check before a wait and the drain loop must not spin on one.
fn ports_would_return_something(processor: &Arc<Mutex<ProcessorInstance>>) -> Option<bool> {
    let guard = processor.lock();
    guard
        .iceoryx2_input_mailboxes_inner()
        .map(|inner| inner.any_port_has_data())
}

/// Clear the pending events on this processor's listener, so its fd goes
/// not-readable and the queue upstream keeps filling has room again.
///
/// No-op for a processor with no input mailboxes (a manual source).
fn drain_input_listener(processor: &Arc<Mutex<ProcessorInstance>>) {
    let guard = processor.lock();
    if let Some(inner) = guard.iceoryx2_input_mailboxes_inner() {
        inner.drain_listener();
    }
}

/// Outcome of one [`ReactiveLoopFdWaiter::wait`] call.
#[derive(Debug, Clone, Copy)]
enum ReactiveLoopWakeOutcome {
    /// Listener fd became readable — at least one upstream notify arrived.
    Notified,
    /// Shutdown wake fd became readable — runner should exit.
    Shutdown,
    /// The wait was interrupted by a signal (`EINTR`); caller should retry.
    Interrupted,
    /// Nothing fired within the bounded wait; the caller comes round to check
    /// whether its listener is still the one the mailboxes hold.
    TimedOut,
    /// The wait returned an unrecoverable error.
    Error,
}

/// Keep the waiter on the listener the mailboxes hold now.
///
/// A listener goes with a destination's last inbound link and comes back with
/// the next one, and a readiness queue never learns of the new fd on its own —
/// a closed fd simply leaves it. So a waiter built for an earlier generation,
/// or for a listener that is gone, is released with its shutdown wake fd
/// recovered, and one is built for the current listener when there is one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn refresh_reactive_loop_waiter(
    id: &ProcessorUniqueId,
    waiter: &mut Option<ReactiveLoopFdWaiter>,
    shutdown_wake_fd: &mut Option<OwnedFd>,
    waiter_setup_failed: &mut bool,
    listener_fd: Option<i32>,
    listener_generation: u64,
) {
    let registered_listener_is_gone = waiter.as_ref().is_some_and(|registered| {
        listener_fd.is_none() || registered.listener_generation != listener_generation
    });
    if registered_listener_is_gone {
        *shutdown_wake_fd = waiter
            .take()
            .and_then(ReactiveLoopFdWaiter::into_shutdown_wake_fd);
    }
    if waiter.is_some() || *waiter_setup_failed {
        return;
    }
    let Some(fd) = listener_fd else {
        return;
    };
    match ReactiveLoopFdWaiter::new(fd, listener_generation, shutdown_wake_fd.take()) {
        Ok(built) => *waiter = Some(built),
        Err(e) => {
            tracing::warn!(
                "[{}] Reactive waiter setup failed, falling back to channel-poll loop: {}",
                id,
                e
            );
            *waiter_setup_failed = true;
        }
    }
}

/// Which of the waiter's two fds a readiness-queue registration watches.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReactiveLoopFdRegistration {
    InputListener,
    ShutdownWake,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopFdRegistration {
    /// The value carried in the kernel's per-registration user data.
    fn user_data(self) -> u64 {
        match self {
            Self::InputListener => 0,
            Self::ShutdownWake => 1,
        }
    }

    fn from_user_data(user_data: u64) -> Self {
        if user_data == Self::ShutdownWake.user_data() {
            Self::ShutdownWake
        } else {
            Self::InputListener
        }
    }
}

/// Which registrations one wait found readable; neither means it timed out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Default, Clone, Copy)]
struct ReactiveLoopReadableRegistrations {
    input_listener: bool,
    shutdown_wake: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopReadableRegistrations {
    fn mark_readable(&mut self, registration: ReactiveLoopFdRegistration) {
        match registration {
            ReactiveLoopFdRegistration::InputListener => self.input_listener = true,
            ReactiveLoopFdRegistration::ShutdownWake => self.shutdown_wake = true,
        }
    }
}

/// The reactive runner's readiness queue — an epoll set on Linux, a kqueue on
/// macOS. Level-triggered on both, so an undrained fd wakes every wait.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ReactiveLoopReadinessQueue {
    readiness_queue_fd: OwnedFd,
}

/// The waiter the reactive runner blocks in: its readiness queue watching the
/// iceoryx2 listener fd plus an optional shutdown wake fd.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ReactiveLoopFdWaiter {
    readiness_queue: ReactiveLoopReadinessQueue,
    /// Which of the destination's listeners this waiter registered, by the
    /// generation the mailboxes assigned it. A listener created after the last
    /// inbound link went away is a new fd the readiness queue never saw.
    listener_generation: u64,
    /// Declared after the readiness queue so it closes after it: closing a
    /// registered fd first would leave a registration that never fires.
    shutdown_wake_fd: Option<OwnedFd>,
}

/// How long a reactive wait blocks before returning empty-handed, so a runner
/// whose listener was replaced while it slept comes round to rebuild its
/// waiter rather than sleeping on a dead fd until shutdown.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const REACTIVE_WAIT_BOUND: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopFdWaiter {
    fn new(
        listener_fd: std::os::fd::RawFd,
        listener_generation: u64,
        shutdown_wake_fd: Option<OwnedFd>,
    ) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;

        let readiness_queue = ReactiveLoopReadinessQueue::create()?;
        readiness_queue
            .register_readable_fd(listener_fd, ReactiveLoopFdRegistration::InputListener)?;
        if let Some(ref wake_fd) = shutdown_wake_fd {
            readiness_queue.register_readable_fd(
                wake_fd.as_raw_fd(),
                ReactiveLoopFdRegistration::ShutdownWake,
            )?;
        }

        Ok(Self {
            readiness_queue,
            listener_generation,
            shutdown_wake_fd,
        })
    }

    /// Release the registration and hand back the shutdown wake fd, so the
    /// next waiter can register it.
    fn into_shutdown_wake_fd(mut self) -> Option<OwnedFd> {
        self.shutdown_wake_fd.take()
    }

    /// Block until a registered fd is readable, a signal interrupts the call,
    /// or `wait_bound` elapses with nothing to report.
    fn wait(&self, wait_bound: std::time::Duration) -> ReactiveLoopWakeOutcome {
        match self.readiness_queue.wait_for_readable(wait_bound) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                ReactiveLoopWakeOutcome::Interrupted
            }
            Err(e) => {
                tracing::warn!("reactive runner wait failed: {}", e);
                ReactiveLoopWakeOutcome::Error
            }
            // Shutdown takes priority over notify when both fired in the same
            // wait — let the runner exit instead of draining one more frame.
            Ok(readable) if readable.shutdown_wake => ReactiveLoopWakeOutcome::Shutdown,
            Ok(readable) if readable.input_listener => ReactiveLoopWakeOutcome::Notified,
            Ok(_) => ReactiveLoopWakeOutcome::TimedOut,
        }
    }
}

#[cfg(target_os = "linux")]
impl ReactiveLoopReadinessQueue {
    fn create() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: epoll_create1 returns -1 on failure; checked below.
        let raw_epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if raw_epoll_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw_epoll_fd was just opened here and nothing else owns it.
        let readiness_queue_fd = unsafe { OwnedFd::from_raw_fd(raw_epoll_fd) };
        Ok(Self { readiness_queue_fd })
    }

    fn register_readable_fd(
        &self,
        watched_fd: std::os::fd::RawFd,
        registration: ReactiveLoopFdRegistration,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: registration.user_data(),
        };
        // SAFETY: epoll_ctl with EPOLL_CTL_ADD takes a pointer to a valid
        // epoll_event for the duration of the call.
        let result = unsafe {
            libc::epoll_ctl(
                self.readiness_queue_fd.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                watched_fd,
                &mut event,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait_for_readable(
        &self,
        wait_bound: std::time::Duration,
    ) -> std::io::Result<ReactiveLoopReadableRegistrations> {
        use std::os::fd::AsRawFd;
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];
        let timeout_ms = wait_bound.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: epoll_wait writes up to events.len() events into the buffer.
        let ready_count = unsafe {
            libc::epoll_wait(
                self.readiness_queue_fd.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
            )
        };
        if ready_count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut readable = ReactiveLoopReadableRegistrations::default();
        for event in &events[..ready_count as usize] {
            readable.mark_readable(ReactiveLoopFdRegistration::from_user_data(event.u64));
        }
        Ok(readable)
    }
}

#[cfg(target_os = "macos")]
impl ReactiveLoopReadinessQueue {
    fn create() -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: kqueue returns -1 on failure; checked below.
        let raw_kqueue_fd = unsafe { libc::kqueue() };
        if raw_kqueue_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw_kqueue_fd was just opened here and nothing else owns it.
        let readiness_queue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };
        // Darwin has no `kqueue1`, so close-on-exec is set before the fd is used.
        // SAFETY: fcntl on a live fd this function owns.
        if unsafe {
            libc::fcntl(
                readiness_queue_fd.as_raw_fd(),
                libc::F_SETFD,
                libc::FD_CLOEXEC,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { readiness_queue_fd })
    }

    /// No `EV_CLEAR`, which is what keeps the registration level-triggered.
    fn register_readable_fd(
        &self,
        watched_fd: std::os::fd::RawFd,
        registration: ReactiveLoopFdRegistration,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let change = libc::kevent {
            ident: watched_fd as libc::uintptr_t,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: std::ptr::without_provenance_mut(registration.user_data() as usize),
        };
        // SAFETY: one valid change in, no event slots out; a failed
        // registration returns -1 when the event list is empty.
        let result = unsafe {
            libc::kevent(
                self.readiness_queue_fd.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait_for_readable(
        &self,
        wait_bound: std::time::Duration,
    ) -> std::io::Result<ReactiveLoopReadableRegistrations> {
        use std::os::fd::AsRawFd;
        let timeout = libc::timespec {
            tv_sec: wait_bound.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
            tv_nsec: wait_bound.subsec_nanos() as libc::c_long,
        };
        // SAFETY: an all-zero `kevent` is a valid out-slot.
        let mut events: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
        // SAFETY: kevent writes up to events.len() events into the buffer; the
        // timeout is a valid stack slot.
        let ready_count = unsafe {
            libc::kevent(
                self.readiness_queue_fd.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                &timeout,
            )
        };
        if ready_count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut readable = ReactiveLoopReadableRegistrations::default();
        for event in &events[..ready_count as usize] {
            if event.flags & libc::EV_ERROR != 0 {
                return Err(std::io::Error::from_raw_os_error(event.data as i32));
            }
            readable.mark_readable(ReactiveLoopFdRegistration::from_user_data(
                event.udata.addr() as u64,
            ));
        }
        Ok(readable)
    }
}

fn run_manual_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    state: &Arc<ObservableProcessorState>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
    isolation_tier: IsolationTier,
) {
    // Call start() - for callback-driven processors this returns immediately
    // after registering callbacks with OS (AVFoundation, CoreAudio, CVDisplayLink).
    // start() is resource-lifecycle, so it receives full-access ctx. Gated by
    // the isolation trust axis: an untrusted tier yields no `FullAccessGrant`,
    // so an in-process FullAccess start() is unrepresentable — the untrusted
    // processor's privileged lifecycle belongs behind the subprocess sandbox.
    let Some(start_grant) = isolation_tier.grant_full_access() else {
        tracing::warn!(
            "[{}] Untrusted isolation tier ({}): in-process FullAccess denied by \
             construction — refusing privileged start() (belongs behind the \
             subprocess sandbox)",
            id,
            isolation_tier.as_str(),
        );
        state.transition_to(ProcessorState::Error);
        return;
    };
    tracing::info!("[{}] Invoking start()...", id);
    {
        let full_ctx = RuntimeContextFullAccess::new(runtime_ctx, start_grant);
        let mut guard = processor.lock();
        match guard.start(&full_ctx) {
            Ok(()) => tracing::info!("[{}] start() completed successfully", id),
            Err(e) => {
                // Marked here or nowhere: this thread goes straight to
                // teardown, and a processor whose `start()` failed is not
                // running — a reader that saw `Running` between `setup` and
                // here must be able to find out it was wrong.
                tracing::error!("[{}] start() failed: {}", id, e);
                state.transition_to(ProcessorState::Error);
                return;
            }
        }
    }

    // Wait for shutdown signal - this thread is just a lifecycle manager
    // Real work happens on OS-managed callback threads
    let mut was_paused = false;
    let mut already_reported_failure = false;

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

        // A processor whose work happens elsewhere reports a failure here or
        // nowhere. The loop keeps running rather than breaking: the rest of
        // the pipeline is unaffected, and breaking would run teardown and
        // then overwrite the state with `Stopped`, hiding what happened.
        //
        // Asked before it is read, under one lock: a helper process that died
        // on its own is noticed here, by the process rather than by its
        // escalate socket, whose EOF a surviving descendant defers
        // indefinitely.
        let has_failed_unrecoverably = {
            let mut processor = processor.lock();
            processor.detect_and_clean_up_after_an_out_of_process_helper_that_died();
            processor.has_failed_unrecoverably()
        };
        if !already_reported_failure && has_failed_unrecoverably {
            tracing::error!("[{}] Processor failed unrecoverably", id);
            state.transition_to(ProcessorState::Error);
            already_reported_failure = true;
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Call stop() - stops callbacks and waits for in-flight work. Privileged
    // ctx. Reuse the same trust-axis gate as start(): reaching here means
    // start() ran under a trusted grant, so a fresh grant is available for the
    // symmetric stop().
    match isolation_tier.grant_full_access() {
        Some(stop_grant) => {
            tracing::info!("[{}] Invoking stop()...", id);
            let full_ctx = RuntimeContextFullAccess::new(runtime_ctx, stop_grant);
            let mut guard = processor.lock();
            match guard.stop(&full_ctx) {
                Ok(()) => tracing::info!("[{}] stop() completed successfully", id),
                Err(e) => tracing::warn!("[{}] stop() failed: {}", id, e),
            }
        }
        None => {
            tracing::debug!(
                "[{}] Untrusted isolation tier ({}): skipping in-process stop()",
                id,
                isolation_tier.as_str(),
            );
        }
    }
}

fn dispatch_on_pause(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    tracing::info!("[{}] Invoking on_pause()...", id);
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    // block_on is internal to ProcessorInstance::on_pause's dispatch.
    match guard.on_pause(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
    }
}

fn dispatch_process(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    if let Err(e) = guard.process(&limited_ctx) {
        tracing::warn!("[{}] process() failed: {}", id, e);
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
    // block_on is internal to ProcessorInstance::on_resume's dispatch.
    match guard.on_resume(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::core::graph::ShutdownChannelComponent;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use iceoryx2::prelude::*;
    use std::os::fd::AsRawFd;

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/runner/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// A production shutdown channel and a duplicate of its wake fd, as the
    /// spawn op hands one to a reactive runner.
    fn shutdown_channel_and_its_wake_fd() -> (ShutdownChannelComponent, OwnedFd) {
        let shutdown_channel = ShutdownChannelComponent::new();
        let shutdown_wake_fd = shutdown_channel
            .try_clone_shutdown_wake_fd()
            .expect("the shutdown wake fd duplicates");
        (shutdown_channel, shutdown_wake_fd)
    }

    fn listener_fd_of(listener: &iceoryx2::port::listener::Listener<ipc::Service>) -> i32 {
        // SAFETY: every caller uses the fd only while `listener` is alive.
        unsafe { listener.file_descriptor().native_handle() }
    }

    /// The rule the window contract turns on: a reactive processor is never
    /// dispatched with nothing to read. A bag that does not complete a window
    /// wakes the runner and must not make it call `process()`.
    ///
    /// Fail-without-fix: drop the `if !ports_would_return_something(...)` guard
    /// above the drain loop and this goes green while the contract is broken —
    /// which is why the gate is asserted here rather than only through
    /// `has_data`.
    #[test]
    fn a_bag_that_does_not_complete_a_window_does_not_dispatch_the_reactive_runner() {
        use crate::core::test_support::MockWindowedAudioConsumerProcessor;

        let mut instance = ProcessorInstance::new(Box::new(
            <MockWindowedAudioConsumerProcessor::Processor as crate::core::GeneratedProcessor>
                ::from_config(Default::default())
                .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let mailboxes = instance
            .iceoryx2_input_mailboxes_inner()
            .expect("a windowed mock holds input mailboxes");
        mailboxes.add_windowed_port(
            "audio",
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            windowed_mock_contract(),
        );
        let processor = Arc::new(Mutex::new(instance));

        assert_eq!(
            ports_would_return_something(&processor),
            Some(false),
            "an empty windowed port must not dispatch"
        );

        // A third of the declared 512-sample window.
        mailboxes.route(one_mono_audio_frame_for("audio", 160, 0));
        assert_eq!(
            ports_would_return_something(&processor),
            Some(false),
            "160 of 512 samples is not a window, and dispatching here would hand \
             `process()` an empty read"
        );

        for block in 1..4i64 {
            mailboxes.route(one_mono_audio_frame_for(
                "audio",
                160,
                block * 160 * 1_000_000_000 / 16_000,
            ));
        }
        assert_eq!(
            ports_would_return_something(&processor),
            Some(true),
            "640 samples completes a window, and a ready window must not sit latent"
        );
    }

    /// A processor the runner cannot ask is dispatched rather than silenced —
    /// the pre-dispatch gate's default, which is the opposite of the drain
    /// loop's and is why the helper hands back an `Option` instead of picking.
    #[test]
    fn a_processor_with_no_input_mailboxes_is_not_gated_at_all() {
        use crate::core::test_support::MockOutputOnlyProcessor;

        let instance = ProcessorInstance::new(Box::new(
            <MockOutputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        let processor = Arc::new(Mutex::new(instance));

        assert_eq!(
            ports_would_return_something(&processor),
            None,
            "there is nothing to gate on, and each caller says what that means"
        );
    }

    fn windowed_mock_contract() -> crate::iceoryx2::ResolvedAudioWindowContract {
        crate::iceoryx2::ResolvedAudioWindowContract::from_declared_values(
            &crate::core::descriptors::AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                channels: Some(1),
                dtype: "f32".to_string(),
                window_size: 512,
                hop: 512,
            },
        )
        .expect("the mock's own declaration resolves")
    }

    /// One wire frame stamped for `port`, carrying `frames` mono 16 kHz samples
    /// — the shape `InputMailboxesInner::route` injects.
    fn one_mono_audio_frame_for(
        port: &str,
        frames: usize,
        first_sample_timestamp_ns: i64,
    ) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct AudioBlockBag<'a> {
            #[serde(rename = "samples", with = "serde_bytes")]
            interleaved_sample_bytes: &'a [u8],
            sample_rate: u32,
            channels: u32,
            sample_count: u32,
            dtype: &'a str,
            first_sample_timestamp_ns: i64,
        }
        let payload: Vec<u8> = (0..frames)
            .flat_map(|index| (index as f32 / frames as f32).to_le_bytes())
            .collect();
        let body = rmp_serde::to_vec_named(&AudioBlockBag {
            interleaved_sample_bytes: &payload,
            sample_rate: 16_000,
            channels: 1,
            sample_count: frames as u32,
            dtype: "f32",
            first_sample_timestamp_ns,
        })
        .expect("an audio block bag encodes");

        one_wire_frame_stamped_for(port, first_sample_timestamp_ns, &body)
    }

    /// `body` behind a frame header stamped for `port` — the shape
    /// `InputMailboxesInner::route` injects.
    fn one_wire_frame_stamped_for(port: &str, timestamp_ns: i64, body: &[u8]) -> Vec<u8> {
        use crate::iceoryx2::{FRAME_HEADER_SIZE, FrameHeader};

        let mut frame = vec![0u8; FRAME_HEADER_SIZE + body.len()];
        FrameHeader::new(port, timestamp_ns, body.len() as u32)
            .expect("port fits PortKey")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame[FRAME_HEADER_SIZE..].copy_from_slice(body);
        frame
    }

    /// A reactive processor's scheduling loop running on its own thread, as the
    /// runner runs it, with its processor callbacks recorded and each dispatch
    /// reading one bag off `in1`.
    struct ReactiveSchedulingLoopOnItsOwnThread {
        processor_callbacks: Arc<Mutex<Vec<ReactiveRunnerProcessorCallback>>>,
        shutdown_channel: ShutdownChannelComponent,
        pause_gate: Arc<AtomicBool>,
        loop_thread: std::thread::JoinHandle<()>,
    }

    /// Whether the pause gate is already closed when the loop's thread starts.
    #[derive(Clone, Copy)]
    enum PauseGateAtLoopStart {
        Open,
        Closed,
    }

    impl ReactiveSchedulingLoopOnItsOwnThread {
        fn start(
            processor: Arc<Mutex<ProcessorInstance>>,
            mailboxes: Arc<crate::iceoryx2::InputMailboxesInner>,
            each_dispatch_takes: std::time::Duration,
            pause_gate_at_loop_start: PauseGateAtLoopStart,
        ) -> Self {
            let processor_callbacks: Arc<Mutex<Vec<ReactiveRunnerProcessorCallback>>> =
                Arc::default();
            let (mut shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
            let shutdown_receiver = shutdown_channel
                .take_receiver()
                .expect("a fresh shutdown channel holds its receiver");
            let pause_gate = Arc::new(AtomicBool::new(matches!(
                pause_gate_at_loop_start,
                PauseGateAtLoopStart::Closed
            )));
            let loop_thread = std::thread::spawn({
                let processor_callbacks = Arc::clone(&processor_callbacks);
                let pause_gate = Arc::clone(&pause_gate);
                move || {
                    run_reactive_scheduling_loop(
                        &"Preactive-scheduling".into(),
                        &processor,
                        &shutdown_receiver,
                        Some(shutdown_wake_fd),
                        &pause_gate,
                        |callback| {
                            if callback == ReactiveRunnerProcessorCallback::Process {
                                let _ = mailboxes.read_raw("in1");
                                std::thread::sleep(each_dispatch_takes);
                            }
                            processor_callbacks.lock().push(callback);
                        },
                    )
                }
            });
            Self {
                processor_callbacks,
                shutdown_channel,
                pause_gate,
                loop_thread,
            }
        }

        fn count_of(&self, callback: ReactiveRunnerProcessorCallback) -> usize {
            self.processor_callbacks
                .lock()
                .iter()
                .filter(|made| **made == callback)
                .count()
        }

        /// Whether `callback` has been made `times` times within `deadline`.
        fn made_within(
            &self,
            callback: ReactiveRunnerProcessorCallback,
            times: usize,
            deadline: std::time::Duration,
        ) -> bool {
            let started = std::time::Instant::now();
            while started.elapsed() < deadline {
                if self.count_of(callback) >= times {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.count_of(callback) >= times
        }

        fn stop(self) {
            self.shutdown_channel.signal_shutdown();
            self.loop_thread
                .join()
                .expect("the scheduling loop must not panic");
        }
    }

    /// An input-only processor with a plain `in1` mailbox and, when given one,
    /// the listener its runner waits on.
    fn input_only_processor_with_plain_port(
        listener: Option<iceoryx2::port::listener::Listener<ipc::Service>>,
    ) -> (
        Arc<Mutex<ProcessorInstance>>,
        Arc<crate::iceoryx2::InputMailboxesInner>,
    ) {
        use crate::core::test_support::MockInputOnlyProcessor;

        let mut instance = ProcessorInstance::new(Box::new(
            <MockInputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let mailboxes = instance
            .iceoryx2_input_mailboxes_inner()
            .expect("an input-only mock holds input mailboxes");
        mailboxes.add_port("in1", 16, crate::iceoryx2::ReadMode::ReadNextInOrder);
        if let Some(listener) = listener {
            mailboxes.set_listener(listener);
        }
        (Arc::new(Mutex::new(instance)), mailboxes)
    }

    /// One wire frame stamped for `in1`, carrying four opaque bytes.
    fn one_frame_for_in1() -> Vec<u8> {
        one_wire_frame_stamped_for("in1", 0, &[0; 4])
    }

    fn open_one_listener_event_service(
        node: &iceoryx2::node::Node<ipc::Service>,
        tag: &str,
    ) -> iceoryx2::service::port_factory::event::PortFactory<ipc::Service> {
        node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
            .event()
            .max_notifiers(1)
            .max_listeners(1)
            .open_or_create()
            .unwrap()
    }

    /// A reactive processor slower than its upstream never leaves its dispatch
    /// loop while bags keep arriving, and every bag notifies — so the loop
    /// drains the listener on every dispatch, or the listener's queue fills and
    /// every later notify reaches nobody, one iceoryx2 warning per frame.
    ///
    /// The ticket's repro: a 2 ms `process()` against a 1 kHz source.
    /// Fail-without-fix: drop the drain from the dispatch loop and about 280
    /// frames in, every notify comes back undelivered.
    #[test]
    fn a_processor_slower_than_its_upstream_keeps_every_notify_deliverable() {
        const FRAMES_AT_ONE_KILOHERTZ: usize = 1000;

        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "slow-consumer");
        let notifier = service.notifier_builder().create().unwrap();
        let (processor, mailboxes) = input_only_processor_with_plain_port(Some(
            service.listener_builder().create().unwrap(),
        ));
        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            Arc::clone(&mailboxes),
            std::time::Duration::from_millis(2),
            PauseGateAtLoopStart::Open,
        );

        let mut undelivered_notifies = 0;
        for _ in 0..FRAMES_AT_ONE_KILOHERTZ {
            mailboxes.route(one_frame_for_in1());
            if notifier.notify().unwrap() == 0 {
                undelivered_notifies += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let dispatches = running.count_of(ReactiveRunnerProcessorCallback::Process);
        running.stop();

        assert!(
            dispatches >= 100,
            "the consumer must have been kept busy for the burst; it dispatched {dispatches} times"
        );
        assert_eq!(
            undelivered_notifies, 0,
            "{undelivered_notifies} of {FRAMES_AT_ONE_KILOHERTZ} notifies reached nobody"
        );
    }

    /// Bags that arrive while a reactive processor is paused are notified, and
    /// the paused ticks drain those notifications — so after the resume there
    /// is nothing left to wake on, and the runner has to ask before it waits.
    ///
    /// The ticket's repro: pause, publish three bags, resume, publish nothing
    /// more. Fail-without-fix: drop the check before the wait and `process()`
    /// never runs.
    #[test]
    fn bags_that_arrived_during_a_pause_are_dispatched_after_the_resume_with_nothing_more_published()
     {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "paused-consumer");
        let notifier = service.notifier_builder().create().unwrap();
        let (processor, mailboxes) = input_only_processor_with_plain_port(Some(
            service.listener_builder().create().unwrap(),
        ));
        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            Arc::clone(&mailboxes),
            std::time::Duration::ZERO,
            PauseGateAtLoopStart::Closed,
        );
        assert!(
            running.made_within(
                ReactiveRunnerProcessorCallback::OnPause,
                1,
                std::time::Duration::from_secs(5)
            ),
            "the loop must see the pause"
        );

        for _ in 0..3 {
            mailboxes.route(one_frame_for_in1());
            notifier.notify().unwrap();
        }
        // Several paused ticks, each of which drains the listener.
        std::thread::sleep(PAUSE_CHECK_INTERVAL * 5);
        running.pause_gate.store(false, Ordering::Release);

        let dispatched = running.made_within(
            ReactiveRunnerProcessorCallback::Process,
            3,
            std::time::Duration::from_secs(2),
        );
        let dispatches = running.count_of(ReactiveRunnerProcessorCallback::Process);
        running.stop();
        assert!(
            dispatched,
            "three bags were waiting at the resume and {dispatches} were dispatched"
        );
    }

    /// A first live link creates its subscriber before its listener, so a bag
    /// published in between notifies nobody and waits in the subscriber. The
    /// runner reaches its next pass with that bag queued and a listener holding
    /// nothing, which is the state built here before the loop starts.
    ///
    /// Fail-without-fix: drop the check before the wait and the runner sleeps
    /// on the empty listener until the upstream publishes again.
    #[test]
    fn a_bag_queued_before_the_listener_existed_is_dispatched_with_no_notify() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "first-link-gap");
        let (processor, mailboxes) = input_only_processor_with_plain_port(None);
        mailboxes.route(one_frame_for_in1());
        mailboxes.set_listener(service.listener_builder().create().unwrap());

        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            mailboxes,
            std::time::Duration::ZERO,
            PauseGateAtLoopStart::Open,
        );
        let dispatched = running.made_within(
            ReactiveRunnerProcessorCallback::Process,
            1,
            std::time::Duration::from_secs(2),
        );
        running.stop();
        assert!(
            dispatched,
            "the queued bag must be dispatched without waiting for a notify"
        );
    }

    /// Every reactive tick that is not an fd wake still has to drain, because
    /// the listener stays subscribed and upstream keeps notifying it. This is
    /// the primitive those ticks call: a listener saturated to the point of
    /// undeliverable notifications takes them again straight after.
    ///
    /// Fail-without-fix: drop the `drain_input_listener` call from the paused
    /// branch, the no-waiter arm, or the wait-error arm and that path is back
    /// to #1764 — an fd nobody clears, warned about once per frame. Those arms
    /// are unreachable from the waiter-backed tests in this module, so this is
    /// what covers them.
    #[test]
    fn draining_a_saturated_listener_lets_it_be_notified_again() {
        use crate::core::test_support::MockInputOnlyProcessor;

        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = node
            .service_builder(&ServiceName::new(&unique_suffix("saturated-drain")).unwrap())
            .event()
            .max_notifiers(1)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = service.notifier_builder().create().unwrap();

        let mut instance = ProcessorInstance::new(Box::new(
            <MockInputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        instance
            .iceoryx2_input_mailboxes_inner()
            .expect("an input-only mock holds input mailboxes")
            .set_listener(service.listener_builder().create().unwrap());
        let processor = Arc::new(Mutex::new(instance));

        // Well past any plausible queue depth; the ticket measured the onset at
        // ~280 notifications against the default socket buffer.
        const SENDS: usize = 8192;
        let saturated = (0..SENDS).any(|_| notifier.notify().unwrap() == 0);
        assert!(
            saturated,
            "an undrained listener absorbed {SENDS} notifications and still took more"
        );

        drain_input_listener(&processor);

        assert_eq!(
            notifier.notify().unwrap(),
            1,
            "the listener must take notifications again once the runner drains it"
        );
    }

    /// A waiter on a fresh listener, and the notifier that feeds it.
    struct ListenerWaiterFixture {
        _node: iceoryx2::node::Node<ipc::Service>,
        notifier: iceoryx2::port::notifier::Notifier<ipc::Service>,
        listener: iceoryx2::port::listener::Listener<ipc::Service>,
        shutdown_channel: ShutdownChannelComponent,
        waiter: ReactiveLoopFdWaiter,
    }

    impl ListenerWaiterFixture {
        fn new(tag: &str) -> Self {
            let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
            let service = open_one_listener_event_service(&node, tag);
            let notifier = service.notifier_builder().create().unwrap();
            let listener = service.listener_builder().create().unwrap();
            let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
            let waiter =
                ReactiveLoopFdWaiter::new(listener_fd_of(&listener), 1, Some(shutdown_wake_fd))
                    .expect("the readiness queue registers both fds");
            Self {
                _node: node,
                notifier,
                listener,
                shutdown_channel,
                waiter,
            }
        }
    }

    /// The waiter reports the listener's readiness. Level-triggered, so the
    /// notify lands first and the wait is a zero-bound poll.
    #[test]
    fn a_delivered_notify_makes_the_wait_report_notified() {
        let fixture = ListenerWaiterFixture::new("wait-notified");
        fixture.notifier.notify().unwrap();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "expected Notified, got {outcome:?}"
        );
        fixture.listener.try_wait_all(|_| {}).unwrap();
    }

    /// The production shutdown signal makes the waiter report shutdown, with
    /// no listener activity at all.
    #[test]
    fn a_signalled_shutdown_makes_the_wait_report_shutdown() {
        let fixture = ListenerWaiterFixture::new("wait-shutdown");
        fixture.shutdown_channel.signal_shutdown();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Shutdown),
            "expected Shutdown, got {outcome:?}"
        );
    }

    /// A runner woken by both exits rather than draining one more frame.
    #[test]
    fn shutdown_outranks_a_notify_reported_by_the_same_wait() {
        let fixture = ListenerWaiterFixture::new("wait-both");
        fixture.notifier.notify().unwrap();
        fixture.shutdown_channel.signal_shutdown();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Shutdown),
            "expected Shutdown, got {outcome:?}"
        );
        fixture.listener.try_wait_all(|_| {}).unwrap();
    }

    /// A wait with nothing to report returns on its own, which is what lets a
    /// runner whose listener was replaced while it slept come round to rebuild
    /// rather than sleeping on a dead fd until shutdown.
    #[test]
    fn a_wait_with_nothing_to_report_times_out() {
        let fixture = ListenerWaiterFixture::new("wait-idle");

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::TimedOut),
            "an idle wait must time out rather than block; got {outcome:?}"
        );
    }

    /// The one cross-thread check: a shutdown made visible only on the wake
    /// fd ends a reactive loop blocked in its wait. The crossbeam half of the
    /// shutdown is withheld, so the channel-poll fallback — the loop every
    /// macOS reactive processor ran before its kqueue arm — never exits here.
    /// The deadline is generous and nothing is asserted about latency.
    #[test]
    fn a_shutdown_seen_only_on_the_wake_fd_ends_a_loop_blocked_in_its_wait() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "loop-wake-fd-shutdown");
        let (processor, _mailboxes) = input_only_processor_with_plain_port(Some(
            service.listener_builder().create().unwrap(),
        ));
        let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
        let (withheld_shutdown_sender, withheld_shutdown_receiver) = crossbeam_channel::bounded(1);
        let (loop_exited_sender, loop_exited_receiver) = crossbeam_channel::bounded(1);
        let loop_thread = std::thread::spawn(move || {
            run_reactive_scheduling_loop(
                &"Preactive-wake-fd-shutdown".into(),
                &processor,
                &withheld_shutdown_receiver,
                Some(shutdown_wake_fd),
                &AtomicBool::new(false),
                |_| {},
            );
            let _ = loop_exited_sender.send(());
        });

        shutdown_channel.signal_shutdown();
        let exited_on_the_wake_fd = loop_exited_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = withheld_shutdown_sender.send(());
        loop_thread
            .join()
            .expect("the scheduling loop must not panic");

        assert!(
            exited_on_the_wake_fd,
            "the loop never saw the wake fd's shutdown, so it is not waiting on it"
        );
    }

    /// A destination that loses its last inbound link drops its listener, and
    /// the next connect creates a new one the readiness queue never saw. The runner
    /// keeps its waiter on the listener the mailboxes hold now, so a frame on
    /// the reconnected link wakes it. Revert lock: register the listener once
    /// before the loop and the second notify below wakes nothing.
    #[test]
    fn the_waiter_follows_a_listener_replaced_after_the_last_link_went_away() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let open_event_service = |suffix: &str| {
            node.service_builder(&ServiceName::new(&unique_suffix(suffix)).unwrap())
                .event()
                .max_notifiers(1)
                .max_listeners(1)
                .open_or_create()
                .unwrap()
        };
        let id: ProcessorUniqueId = "Preactive".into();
        let mut waiter = None;
        let (_shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
        let mut shutdown_wake_fd = Some(shutdown_wake_fd);
        let mut waiter_setup_failed = false;

        let first = open_event_service("replaced-first");
        let first_listener = first.listener_builder().create().unwrap();
        let first_fd = listener_fd_of(&first_listener);
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_wake_fd,
            &mut waiter_setup_failed,
            Some(first_fd),
            1,
        );
        let first_readiness_queue_fd = waiter
            .as_ref()
            .expect("a waiter for the first listener")
            .readiness_queue
            .readiness_queue_fd
            .as_raw_fd();
        assert!(
            shutdown_wake_fd.is_none(),
            "the waiter took the shutdown wake fd"
        );

        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_wake_fd,
            &mut waiter_setup_failed,
            Some(first_fd),
            1,
        );
        assert_eq!(
            waiter
                .as_ref()
                .map(|registered| registered.readiness_queue.readiness_queue_fd.as_raw_fd()),
            Some(first_readiness_queue_fd),
            "the same listener keeps the same waiter"
        );

        // The last link goes, and the listener with it.
        drop(first_listener);
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_wake_fd,
            &mut waiter_setup_failed,
            None,
            1,
        );
        assert!(
            waiter.is_none(),
            "a waiter on a dropped listener is released"
        );
        assert!(
            shutdown_wake_fd.is_some(),
            "the shutdown wake fd comes back for the next one"
        );

        // A reconnect creates a new listener, on a fresh notify service.
        let second = open_event_service("replaced-second");
        let second_listener = second.listener_builder().create().unwrap();
        let second_fd = listener_fd_of(&second_listener);
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_wake_fd,
            &mut waiter_setup_failed,
            Some(second_fd),
            2,
        );
        let waiter = waiter.expect("a waiter for the new listener");
        let notifier = second.notifier_builder().create().unwrap();

        notifier.notify().unwrap();
        let outcome = waiter.wait(std::time::Duration::ZERO);
        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "a notify on the reconnected link must wake the runner; got {outcome:?}"
        );
        second_listener.try_wait_all(|_| {}).unwrap();
    }
}
