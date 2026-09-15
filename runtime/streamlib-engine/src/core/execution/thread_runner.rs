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

/// Sleep cadence for the no-fd-waiter fallback paths (non-Linux, or the
/// rare case where epoll setup fails on Linux). Reactive mode on Linux
/// with a working waiter blocks in `epoll_wait` up to `REACTIVE_WAIT_BOUND_MS`
/// and never sleeps.
const NO_WAITER_FALLBACK_SLEEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the processor thread main loop based on execution mode.
#[tracing::instrument(name = "processor.lifecycle", skip(processor, shutdown_rx, shutdown_eventfd, state, pause_gate, exec_config, runtime_ctx), fields(processor_id = %id, isolation_tier = isolation_tier.as_str()))]
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_eventfd: Option<OwnedFd>,
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
                shutdown_eventfd,
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
    // shutdown eventfd (compiler signals teardown). epoll_wait blocks until
    // one of those fds fires or its bound elapses — idle CPU is a wake every
    // REACTIVE_WAIT_BOUND_MS and nothing more.
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
    #[cfg(target_os = "linux")]
    let mut shutdown_eventfd = shutdown_eventfd;
    #[cfg(target_os = "linux")]
    let mut waiter: Option<ReactiveLoopFdWaiter> = None;
    #[cfg(target_os = "linux")]
    let mut epoll_setup_failed = false;

    let mut was_paused = false;

    loop {
        #[cfg(target_os = "linux")]
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
                &mut shutdown_eventfd,
                &mut epoll_setup_failed,
                listener_fd,
                listener_generation,
            );
        }

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

        // Every path that is not waking on the listener fd still owns that
        // listener, and upstream still notifies it on every frame — so each
        // one drains. Skip a drain here and the listener's queue fills, after
        // which iceoryx2 warns per frame for the rest of the run (#1764).
        if is_paused {
            // While paused we deliberately poll: the pause_gate is an
            // AtomicBool with no fd, so on_resume can't fire from epoll.
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            drain_input_listener(processor);
            continue;
        }

        // Block until an upstream notify, a shutdown signal, or (in the
        // no-waiter and epoll-error fallbacks) the next channel-poll tick.
        #[cfg(target_os = "linux")]
        match waiter.as_ref() {
            Some(w) => match w.wait() {
                ReactiveLoopWakeOutcome::Notified => drain_input_listener(processor),
                ReactiveLoopWakeOutcome::Shutdown => {
                    tracing::info!("[{}] Received shutdown via eventfd", id);
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
        #[cfg(not(target_os = "linux"))]
        {
            std::thread::sleep(NO_WAITER_FALLBACK_SLEEP);
            drain_input_listener(processor);
        }

        // Drain-loop dispatch: iceoryx2's Event service coalesces
        // multiple notify()s on the same EventId into one fd-readable
        // transition (the underlying IdTracker is a bit-set, not a
        // counter). After `drain_listener` clears that bit, the
        // listener fd is not-readable again, and the next `epoll_wait`
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
        //
        // The first dispatch is gated on readiness like every later one. A
        // wake is not evidence that a read would return anything: an audio
        // input port declaring a window contract reports data only when a full
        // window can be emitted, so a bag that does not complete one wakes this
        // loop and must not dispatch. The helper loop already gates every
        // dispatch this way; this is the app-process half of the same rule.
        // A processor with no mailboxes to ask has nothing to gate on, and
        // gating on an absent answer would stop it running at all.
        if !ports_would_return_something(processor).unwrap_or(true) {
            continue;
        }

        loop {
            {
                let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
                let mut guard = processor.lock();
                if let Err(e) = guard.process(&limited_ctx) {
                    tracing::warn!("[{}] process() failed: {}", id, e);
                }
            }

            if shutdown_rx.try_recv().is_ok() {
                tracing::info!("[{}] Received shutdown signal mid-drain", id);
                return;
            }

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
/// input ports, or whose handle is not wired yet. The two callers want opposite
/// defaults for that case and each says which, rather than this picking one:
/// the pre-dispatch gate must not silence a processor it cannot ask, and the
/// drain loop must not spin on one.
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
    /// Shutdown eventfd became readable — runner should exit.
    Shutdown,
    /// `epoll_wait` was interrupted by a signal (`EINTR`); caller should retry.
    Interrupted,
    /// Nothing fired within the bounded wait; the caller comes round to check
    /// whether its listener is still the one the mailboxes hold.
    TimedOut,
    /// `epoll_wait` returned an unrecoverable error.
    Error,
}

/// Keep the waiter on the listener the mailboxes hold now.
///
/// A listener goes with a destination's last inbound link and comes back with
/// the next one, and an epoll set never learns of the new fd on its own — a
/// closed fd simply leaves it. So a waiter built for an earlier generation, or
/// for a listener that is gone, is released with its shutdown eventfd
/// recovered, and one is built for the current listener when there is one.
#[cfg(target_os = "linux")]
fn refresh_reactive_loop_waiter(
    id: &ProcessorUniqueId,
    waiter: &mut Option<ReactiveLoopFdWaiter>,
    shutdown_eventfd: &mut Option<OwnedFd>,
    epoll_setup_failed: &mut bool,
    listener_fd: Option<i32>,
    listener_generation: u64,
) {
    let registered_listener_is_gone = waiter.as_ref().is_some_and(|registered| {
        listener_fd.is_none() || registered.listener_generation != listener_generation
    });
    if registered_listener_is_gone {
        *shutdown_eventfd = waiter
            .take()
            .and_then(ReactiveLoopFdWaiter::into_shutdown_eventfd);
    }
    if waiter.is_some() || *epoll_setup_failed {
        return;
    }
    let Some(fd) = listener_fd else {
        return;
    };
    match ReactiveLoopFdWaiter::new(fd, listener_generation, shutdown_eventfd.take()) {
        Ok(built) => *waiter = Some(built),
        Err(e) => {
            tracing::warn!(
                "[{}] Reactive epoll setup failed, falling back to channel-poll loop: {}",
                id,
                e
            );
            *epoll_setup_failed = true;
        }
    }
}

/// Tag stored in `epoll_event.u64` for the shutdown eventfd; chosen so it
/// can never collide with a listener-fd tag (which we set to 0).
#[cfg(target_os = "linux")]
const SHUTDOWN_EVENTFD_TAG: u64 = u64::MAX;

/// Linux-only: epoll fd watching the iceoryx2 listener fd plus an optional
/// shutdown eventfd, used by the reactive runner.
#[cfg(target_os = "linux")]
struct ReactiveLoopFdWaiter {
    epoll_fd: i32,
    /// Which of the destination's listeners this waiter registered, by the
    /// generation the mailboxes assigned it. A listener created after the last
    /// inbound link went away is a new fd the epoll set never saw.
    listener_generation: u64,
    /// Stored to keep the kernel-side eventfd alive for the lifetime of the
    /// epoll registration. Closing the fd before the epoll fd would leave a
    /// dangling registration that never fires.
    shutdown_eventfd: Option<OwnedFd>,
}

/// How long a reactive wait blocks before returning empty-handed, so a runner
/// whose listener was replaced while it slept comes round to rebuild its
/// waiter rather than sleeping on a dead fd until shutdown.
#[cfg(target_os = "linux")]
const REACTIVE_WAIT_BOUND_MS: i32 = 500;

#[cfg(target_os = "linux")]
impl ReactiveLoopFdWaiter {
    fn new(
        listener_fd: i32,
        listener_generation: u64,
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
            let r = unsafe { libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut event) };
            if r < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        if let Err(e) = register(listener_fd, 0) {
            // SAFETY: epoll_fd is owned and unused after this point.
            unsafe { libc::close(epoll_fd) };
            return Err(e);
        }
        if let Some(ref efd) = shutdown_eventfd {
            if let Err(e) = register(efd.as_raw_fd(), SHUTDOWN_EVENTFD_TAG) {
                unsafe { libc::close(epoll_fd) };
                return Err(e);
            }
        }

        Ok(Self {
            epoll_fd,
            listener_generation,
            shutdown_eventfd,
        })
    }

    /// Release the epoll registration and hand back the shutdown eventfd, so
    /// the next waiter can register it.
    fn into_shutdown_eventfd(mut self) -> Option<OwnedFd> {
        self.shutdown_eventfd.take()
    }

    fn wait(&self) -> ReactiveLoopWakeOutcome {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];
        // Wakes when one of the registered fds is readable, a signal interrupts
        // the call, or the bound elapses with nothing to report.
        // SAFETY: epoll_wait writes up to events.len() events into the buffer.
        let n = unsafe {
            libc::epoll_wait(
                self.epoll_fd,
                events.as_mut_ptr(),
                2,
                REACTIVE_WAIT_BOUND_MS,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                return ReactiveLoopWakeOutcome::Interrupted;
            }
            tracing::warn!("epoll_wait failed in reactive runner: {}", err);
            return ReactiveLoopWakeOutcome::Error;
        }
        if n == 0 {
            return ReactiveLoopWakeOutcome::TimedOut;
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
        if !already_reported_failure && processor.lock().has_failed_unrecoverably() {
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
    // block_on is internal to ProcessorInstance::on_pause's dispatch.
    match guard.on_pause(&limited_ctx) {
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
    // block_on is internal to ProcessorInstance::on_resume's dispatch.
    match guard.on_resume(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use iceoryx2::prelude::*;
    use std::os::fd::{AsRawFd, FromRawFd};

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/runner/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    fn make_eventfd() -> OwnedFd {
        // SAFETY: eventfd returns -1 on failure; checked below. Initial
        // counter is 0; EFD_CLOEXEC matches production.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(
            raw >= 0,
            "eventfd failed: {}",
            std::io::Error::last_os_error()
        );
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
            "there is nothing to gate on, and the two callers each say what that means"
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
        use crate::iceoryx2::{FRAME_HEADER_SIZE, FrameHeader};

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

        let mut frame = vec![0u8; FRAME_HEADER_SIZE + body.len()];
        FrameHeader::new(port, first_sample_timestamp_ns, body.len() as u32)
            .expect("port fits PortKey")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame[FRAME_HEADER_SIZE..].copy_from_slice(&body);
        frame
    }

    /// Every reactive tick that is not an fd wake still has to drain, because
    /// the listener stays subscribed and upstream keeps notifying it. This is
    /// the primitive those ticks call: a listener saturated to the point of
    /// undeliverable notifications takes them again straight after.
    ///
    /// Fail-without-fix: drop the `drain_input_listener` call from the paused
    /// branch, the no-waiter arm, or the epoll-error arm and that path is back
    /// to #1764 — an fd nobody clears, warned about once per frame. Those arms
    /// are unreachable from the two waiter-backed tests in this module (the
    /// no-waiter arm is every tick of every reactive processor off Linux), so
    /// this is what covers them.
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

    /// The reactive runner's wake primitive: a notify() from another thread
    /// must transition `ReactiveLoopFdWaiter::wait` to Notified well within
    /// the runner's wake-latency budget. iceoryx2's `ipc::Service` Notifier
    /// is `!Send` (Rc-backed SingleThreaded threadsafety policy), so the
    /// test keeps notifier on the main thread and ships the waiter to the
    /// waiter thread.
    #[test]
    fn reactive_loop_wakes_on_notify() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
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
            ReactiveLoopFdWaiter::new(listener_fd, 1, Some(make_eventfd())).expect("epoll setup");

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
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
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

        let waiter =
            ReactiveLoopFdWaiter::new(listener_fd, 1, Some(shutdown_eventfd)).expect("epoll setup");

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

    /// A destination that loses its last inbound link drops its listener, and
    /// the next connect creates a new one the epoll set never saw. The runner
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
        let mut shutdown_eventfd = Some(make_eventfd());
        let mut epoll_setup_failed = false;

        let first = open_event_service("replaced-first");
        let first_listener = first.listener_builder().create().unwrap();
        // SAFETY: the fd is used only while `first_listener` is alive.
        let first_fd = unsafe { first_listener.file_descriptor().native_handle() };
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_eventfd,
            &mut epoll_setup_failed,
            Some(first_fd),
            1,
        );
        let first_epoll_fd = waiter
            .as_ref()
            .expect("a waiter for the first listener")
            .epoll_fd;
        assert!(
            shutdown_eventfd.is_none(),
            "the waiter took the shutdown eventfd"
        );

        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_eventfd,
            &mut epoll_setup_failed,
            Some(first_fd),
            1,
        );
        assert_eq!(
            waiter.as_ref().map(|registered| registered.epoll_fd),
            Some(first_epoll_fd),
            "the same listener keeps the same waiter"
        );

        // The last link goes, and the listener with it.
        drop(first_listener);
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_eventfd,
            &mut epoll_setup_failed,
            None,
            1,
        );
        assert!(
            waiter.is_none(),
            "a waiter on a dropped listener is released"
        );
        assert!(
            shutdown_eventfd.is_some(),
            "the shutdown eventfd comes back for the next one"
        );

        // A reconnect creates a new listener, on a fresh notify service.
        let second = open_event_service("replaced-second");
        let second_listener = second.listener_builder().create().unwrap();
        // SAFETY: the fd is used only while `second_listener` is alive; the
        // worker below is joined before this function returns.
        let second_fd = unsafe { second_listener.file_descriptor().native_handle() };
        refresh_reactive_loop_waiter(
            &id,
            &mut waiter,
            &mut shutdown_eventfd,
            &mut epoll_setup_failed,
            Some(second_fd),
            2,
        );
        let waiter = waiter.expect("a waiter for the new listener");
        let notifier = second.notifier_builder().create().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome = waiter.wait();
            tx.send(outcome).unwrap();
            waiter
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        notifier.notify().unwrap();
        let outcome = rx
            .recv_timeout(std::time::Duration::from_millis(800))
            .expect("the worker did not respond — the new listener never woke the waiter");
        let _waiter = worker.join().expect("worker panicked");
        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "a notify on the reconnected link must wake the runner; got {outcome:?}"
        );
        second_listener.try_wait_all(|_| {}).unwrap();
    }

    /// A wait with nothing to report returns on its own, which is what lets a
    /// runner whose listener was replaced while it slept come round to rebuild
    /// rather than sleeping on a dead fd until shutdown.
    #[test]
    fn a_wait_with_nothing_to_report_returns_at_its_bound() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let svc = node
            .service_builder(&ServiceName::new(&unique_suffix("bounded-wait")).unwrap())
            .event()
            .max_notifiers(1)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let listener = svc.listener_builder().create().unwrap();
        // SAFETY: the fd is used only while `listener` is alive.
        let listener_fd = unsafe { listener.file_descriptor().native_handle() };
        let waiter =
            ReactiveLoopFdWaiter::new(listener_fd, 1, Some(make_eventfd())).expect("epoll setup");

        let started = std::time::Instant::now();
        let outcome = waiter.wait();
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::TimedOut),
            "an idle wait must time out rather than block; got {outcome:?}"
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(REACTIVE_WAIT_BOUND_MS as u64 - 50)
                && elapsed < std::time::Duration::from_secs(3),
            "the wait must return at its bound; took {elapsed:?}"
        );
    }
}
