// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shutdown-signal ownership, scoped to the lifetime of a run loop.
//!
//! Ownership is a value, not a process-wide side effect: whoever owns the run
//! loop takes [`ScopedShutdownSignalOwnership`] for as long as it blocks and
//! drops it afterwards, which restores the signal dispositions that were
//! installed before. The wheel's `rt.run()` depends on that restoration — it
//! hands SIGINT back to CPython, so a Ctrl-C after `run()` returns raises
//! `KeyboardInterrupt` instead of being swallowed by a handler whose run loop
//! is gone.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: each delivered signal escalates
//! the shutdown one step — graceful, forced, then every helper's process group
//! killed and the process gone with status 130.

#[cfg(unix)]
use crate::core::runtime::{
    RuntimeShutdownEscalation, escalate_runtime_shutdown_for_a_delivered_signal,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Only one run loop may own the shutdown signals at a time — two owners would
/// race to restore dispositions and the loser would restore a disposition the
/// winner had already replaced.
static SHUTDOWN_SIGNALS_OWNED: AtomicBool = AtomicBool::new(false);

/// Owns SIGINT, SIGTERM and SIGHUP for as long as it is alive, escalating the
/// runtime shutdown one step for each one delivered.
///
/// A SIGHUP already ignored when ownership is taken stays ignored: that is how
/// `nohup` and a supervisor tell a process to outlive its terminal.
///
/// Dropping it stops the forwarding thread, joins it, and restores the signal
/// dispositions captured at construction. macOS instead installs its handlers
/// once for the process's life, because they cannot be uninstalled, and feeds
/// the same escalation from them — there, drop releases the ownership claim
/// only.
pub struct ScopedShutdownSignalOwnership {
    #[cfg(all(unix, not(target_os = "macos")))]
    signal_forwarding: Option<UnixSignalForwarding>,
}

/// The signals a run loop owns, in the order their dispositions are displaced.
#[cfg(all(unix, not(target_os = "macos")))]
const SHUTDOWN_SIGNALS_OWNED_BY_THE_RUN_LOOP: [libc::c_int; 3] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The Linux forwarding thread plus the dispositions it displaced.
#[cfg(all(unix, not(target_os = "macos")))]
struct UnixSignalForwarding {
    forwarding_thread: std::thread::JoinHandle<()>,
    displaced_shutdown_signal_dispositions: Vec<DisplacedSignalDisposition>,
}

/// One signal's pre-existing disposition, restored on drop unless already
/// restored explicitly.
#[cfg(all(unix, not(target_os = "macos")))]
struct DisplacedSignalDisposition {
    signal: libc::c_int,
    previous_action: libc::sigaction,
    already_restored: bool,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl DisplacedSignalDisposition {
    /// Point `signal` at the self-pipe handler, capturing what it displaced.
    fn displace_with_self_pipe_handler(signal: libc::c_int) -> std::io::Result<Self> {
        // SAFETY: `requested` is fully initialized before use, `displaced` is
        // owned here and written by the kernel, and the handler installed is a
        // plain `extern "C"` function with no Rust-level invariants to uphold.
        unsafe {
            let mut requested: libc::sigaction = std::mem::zeroed();
            let handler: extern "C" fn(libc::c_int) = write_delivered_signal_to_self_pipe;
            requested.sa_sigaction = handler as usize;
            // SA_RESTART so a shutdown signal does not surface as EINTR in
            // engine threads blocked on a syscall — the request funnel is the
            // only path that reacts to it.
            requested.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut requested.sa_mask);

            let mut previous_action: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, &requested, &mut previous_action) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                signal,
                previous_action,
                already_restored: false,
            })
        }
    }

    /// Hand the signal back to whoever held it. Idempotent.
    fn restore_now(&mut self) {
        if self.already_restored {
            return;
        }
        self.already_restored = true;
        // SAFETY: `previous_action` was captured by this same value's
        // constructor for this same signal, and a NULL `oldact` discards the
        // displaced action.
        let restored =
            unsafe { libc::sigaction(self.signal, &self.previous_action, std::ptr::null_mut()) };
        if restored != 0 {
            tracing::error!(
                signal = self.signal,
                error = %std::io::Error::last_os_error(),
                "failed to restore the previous signal disposition"
            );
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl Drop for DisplacedSignalDisposition {
    fn drop(&mut self) {
        self.restore_now();
    }
}

/// The process-lifetime self-pipe the signal handler writes to.
///
/// Created once and never torn down, because a handler that is already
/// executing when ownership drops must still write somewhere valid — closing
/// these would leave a window where an in-flight handler writes to a reused
/// descriptor. The reader end is drained and re-read by each new owner instead.
#[cfg(all(unix, not(target_os = "macos")))]
struct ShutdownSignalSelfPipe {
    read_end: std::os::fd::RawFd,
    write_end: std::os::fd::RawFd,
}

#[cfg(all(unix, not(target_os = "macos")))]
static SHUTDOWN_SIGNAL_SELF_PIPE: std::sync::OnceLock<ShutdownSignalSelfPipe> =
    std::sync::OnceLock::new();

/// The status a third interrupt exits with: 128 plus SIGINT, the shell's own
/// convention for a process ended by Ctrl-C.
#[cfg(unix)]
const EXIT_STATUS_OF_A_THIRD_INTERRUPT: libc::c_int = 130;

/// Wakes the forwarding thread for shutdown rather than for a signal. Real
/// signal numbers start at 1, so zero can never collide with one.
#[cfg(all(unix, not(target_os = "macos")))]
const FORWARDING_THREAD_STOP_BYTE: u8 = 0;

/// What actually bounds the join in `Drop` — the stop byte only shortens the
/// wait, so losing it must not be able to hang teardown.
#[cfg(all(unix, not(target_os = "macos")))]
static FORWARDING_THREAD_SHOULD_STOP: AtomicBool = AtomicBool::new(false);

#[cfg(all(unix, not(target_os = "macos")))]
const FORWARDING_THREAD_STOP_POLL_INTERVAL_MILLISECONDS: libc::c_int = 250;

/// Writes the delivered signal number to the self-pipe.
///
/// Three calls, each async-signal-safe: the `OnceLock` read is a plain atomic
/// load with no locking, `__errno_location` is a pure TLS address computation,
/// and `write(2)` is on the POSIX AS-safe list. All interpretation — logging,
/// attribution, the shutdown request itself — happens on the forwarding thread.
/// Re-entry is safe too: `sa_mask` is empty, so a SIGTERM may preempt this
/// mid-SIGINT, and the nested errno save/restore still leaves the outer value
/// intact.
#[cfg(all(unix, not(target_os = "macos")))]
extern "C" fn write_delivered_signal_to_self_pipe(delivered_signal: libc::c_int) {
    let Some(self_pipe) = SHUTDOWN_SIGNAL_SELF_PIPE.get() else {
        return;
    };
    let delivered_signal_byte = delivered_signal as u8;
    // SAFETY: `write_end` stays open for the process lifetime, and the source
    // is one byte of stack this frame owns. A failure is either a full pipe (a
    // request is already queued) or a descriptor the process has lost; neither
    // is actionable from signal context. `errno` is saved and restored around
    // it because the handler preempted a syscall that may be about to read its
    // own errno.
    unsafe {
        let errno_slot = libc::__errno_location();
        let interrupted_errno = *errno_slot;
        libc::write(
            self_pipe.write_end,
            std::ptr::from_ref(&delivered_signal_byte).cast(),
            1,
        );
        *errno_slot = interrupted_errno;
    }
}

impl ScopedShutdownSignalOwnership {
    /// Take ownership of the shutdown signals until the returned value drops.
    ///
    /// Fails if another run loop already owns them.
    pub fn take_until_dropped() -> std::io::Result<Self> {
        if SHUTDOWN_SIGNALS_OWNED.swap(true, Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "shutdown signals are already owned by a running run loop",
            ));
        }

        match Self::install() {
            Ok(owned) => Ok(owned),
            Err(installation_failure) => {
                SHUTDOWN_SIGNALS_OWNED.store(false, Ordering::SeqCst);
                Err(installation_failure)
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn install() -> std::io::Result<Self> {
        let self_pipe = shutdown_signal_self_pipe()?;
        // A signal delivered during the previous owner's teardown would
        // otherwise be read by this owner and shut it down on the spot.
        drain_pending_bytes(self_pipe.read_end)?;

        let displaced_shutdown_signal_dispositions = SHUTDOWN_SIGNALS_OWNED_BY_THE_RUN_LOOP
            .into_iter()
            .filter(|signal| !is_a_hangup_the_process_was_told_to_ignore(*signal))
            .map(DisplacedSignalDisposition::displace_with_self_pipe_handler)
            .collect::<std::io::Result<Vec<_>>>()?;

        FORWARDING_THREAD_SHOULD_STOP.store(false, Ordering::SeqCst);
        let read_end = self_pipe.read_end;
        let forwarding_thread = std::thread::Builder::new()
            .name("shutdown-signal-forwarding".to_string())
            .spawn(move || forward_signals_until_stopped(read_end))?;

        let owned_signal_names: Vec<&str> = displaced_shutdown_signal_dispositions
            .iter()
            .map(|displaced| {
                signal_hook::low_level::signal_name(displaced.signal).unwrap_or("unnamed signal")
            })
            .collect();
        tracing::info!(
            "Shutdown signals owned by this run loop ({})",
            owned_signal_names.join(", ")
        );
        Ok(Self {
            signal_forwarding: Some(UnixSignalForwarding {
                forwarding_thread,
                displaced_shutdown_signal_dispositions,
            }),
        })
    }

    #[cfg(target_os = "macos")]
    fn install() -> std::io::Result<Self> {
        // Installed once per process, not once per owner: `ctrlc::set_handler`
        // refuses a second call outright, and each SIGTERM registration would
        // leak another polling thread. Ownership after the first take is
        // therefore the claim alone — which is also why `Drop` restores nothing
        // here.
        if MACOS_TERMINATION_HANDLERS_INSTALLED.get().is_none() {
            ctrlc::set_handler(|| {
                escalate_the_runtime_shutdown_one_step_for_a_delivered_signal("SIGINT");
            })
            .map_err(std::io::Error::other)?;
            // Claimed here rather than after the SIGTERM install below: the
            // ctrlc handler is already irreversible, so a later failure that
            // left this unset would re-enter this branch forever and every
            // subsequent take would fail on `MultipleHandlers`.
            let _ = MACOS_TERMINATION_HANDLERS_INSTALLED.set(());

            install_sigterm_handler_macos()?;
            tracing::info!(
                "macOS shutdown signals owned (Ctrl+C via ctrlc, SIGTERM via signal-hook)"
            );
        }

        Ok(Self {})
    }

    #[cfg(windows)]
    fn install() -> std::io::Result<Self> {
        // Would use SetConsoleCtrlHandler; the platform floor is Linux.
        tracing::warn!("Windows signal handling not yet implemented");
        Ok(Self {})
    }
}

impl Drop for ScopedShutdownSignalOwnership {
    fn drop(&mut self) {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(mut forwarding) = self.signal_forwarding.take() {
            // Restore before winding the thread down, so a signal arriving
            // during teardown reaches whoever held the disposition rather than
            // a handler whose reader is going away.
            for displaced in &mut forwarding.displaced_shutdown_signal_dispositions {
                displaced.restore_now();
            }

            stop_forwarding_thread();
            if forwarding.forwarding_thread.join().is_err() {
                tracing::error!("Shutdown-signal forwarding thread panicked");
            }
            tracing::debug!("Shutdown-signal dispositions restored");
        }

        SHUTDOWN_SIGNALS_OWNED.store(false, Ordering::SeqCst);
    }
}

/// The process-lifetime self-pipe, created on first use.
#[cfg(all(unix, not(target_os = "macos")))]
fn shutdown_signal_self_pipe() -> std::io::Result<&'static ShutdownSignalSelfPipe> {
    if let Some(existing) = SHUTDOWN_SIGNAL_SELF_PIPE.get() {
        return Ok(existing);
    }

    let mut pipe_ends: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `pipe2` writes exactly two descriptors into the array we own.
    // `O_CLOEXEC` keeps the descriptors out of helper processes the runtime
    // spawns, which must not be able to request our shutdown by accident.
    let created = unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) };
    if created != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // A full pipe must never block the handler, so the write end is
    // non-blocking; a dropped byte only ever means a request is already queued.
    // SAFETY: `pipe_ends[1]` was just returned by `pipe2`.
    if unsafe { libc::fcntl(pipe_ends[1], libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        let flag_failure = std::io::Error::last_os_error();
        // SAFETY: both ends were just created here and are not published yet.
        unsafe {
            libc::close(pipe_ends[0]);
            libc::close(pipe_ends[1]);
        }
        return Err(flag_failure);
    }

    Ok(
        SHUTDOWN_SIGNAL_SELF_PIPE.get_or_init(|| ShutdownSignalSelfPipe {
            read_end: pipe_ends[0],
            write_end: pipe_ends[1],
        }),
    )
}

/// Read the self-pipe until stopped, escalating the runtime shutdown one step for
/// each delivered signal.
#[cfg(all(unix, not(target_os = "macos")))]
fn forward_signals_until_stopped(read_end: std::os::fd::RawFd) {
    tracing::debug!("Shutdown-signal forwarding thread started");
    while !FORWARDING_THREAD_SHOULD_STOP.load(Ordering::SeqCst) {
        let mut awaited = libc::pollfd {
            fd: read_end,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one owned `pollfd` describing a descriptor that stays open
        // for the process lifetime.
        let ready = unsafe {
            libc::poll(
                std::ptr::from_mut(&mut awaited),
                1,
                FORWARDING_THREAD_STOP_POLL_INTERVAL_MILLISECONDS,
            )
        };
        if ready < 0 {
            let poll_failure = std::io::Error::last_os_error();
            if poll_failure.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            tracing::error!(error = %poll_failure, "Shutdown-signal forwarding: poll failed");
            break;
        }
        if ready == 0 {
            continue;
        }

        let mut delivered = 0u8;
        // SAFETY: `read_end` stays open for the process lifetime and the
        // destination is one byte of stack this frame owns.
        let bytes_read =
            unsafe { libc::read(read_end, std::ptr::from_mut(&mut delivered).cast(), 1) };

        if bytes_read < 0 {
            let read_failure = std::io::Error::last_os_error();
            if read_failure.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            tracing::error!(error = %read_failure, "Shutdown-signal forwarding: read failed");
            break;
        }
        // EOF is unreachable while the write end is held for the process
        // lifetime, but treating it as a stop keeps the loop bounded.
        if bytes_read == 0 || delivered == FORWARDING_THREAD_STOP_BYTE {
            break;
        }

        let signal_name = signal_hook::low_level::signal_name(libc::c_int::from(delivered))
            .unwrap_or("unrecognized signal");
        escalate_the_runtime_shutdown_one_step_for_a_delivered_signal(signal_name);
    }
    tracing::debug!("Shutdown-signal forwarding thread exiting");
}

/// Escalate the runtime shutdown one step for a delivered signal, ending the
/// process at once on the step that says so.
#[cfg(unix)]
fn escalate_the_runtime_shutdown_one_step_for_a_delivered_signal(signal_name: &str) {
    if escalate_runtime_shutdown_for_a_delivered_signal(&format!("posix signal {signal_name}"))
        == RuntimeShutdownEscalation::ExitAtOnce
    {
        crate::core::runtime::kill_every_helper_process_group_and_end_the_process_at_once(
            EXIT_STATUS_OF_A_THIRD_INTERRUPT,
        );
    }
}

/// Whether `signal` is a SIGHUP this process was already set to ignore.
#[cfg(all(unix, not(target_os = "macos")))]
fn is_a_hangup_the_process_was_told_to_ignore(signal: libc::c_int) -> bool {
    signal == libc::SIGHUP && current_disposition_of(signal).sa_sigaction == libc::SIG_IGN
}

/// Ask the forwarding thread to stop, and nudge it out of its poll.
#[cfg(all(unix, not(target_os = "macos")))]
fn stop_forwarding_thread() {
    FORWARDING_THREAD_SHOULD_STOP.store(true, Ordering::SeqCst);

    let Some(self_pipe) = SHUTDOWN_SIGNAL_SELF_PIPE.get() else {
        return;
    };
    // SAFETY: `write_end` stays open for the process lifetime; the source is
    // one byte of stack this frame owns.
    let written = unsafe {
        libc::write(
            self_pipe.write_end,
            std::ptr::from_ref(&FORWARDING_THREAD_STOP_BYTE).cast(),
            1,
        )
    };
    if written != 1 {
        tracing::debug!(
            error = %std::io::Error::last_os_error(),
            "Shutdown-signal forwarding: stop byte not delivered; the thread will stop on its next poll"
        );
    }
}

/// Discard bytes left in the pipe by a previous owner. Terminates only while
/// the read end is non-blocking, so both `fcntl`s are checked.
#[cfg(all(unix, not(target_os = "macos")))]
fn drain_pending_bytes(read_end: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: `read_end` stays open for the process lifetime; the destination
    // buffer is owned by this frame.
    unsafe {
        let original_flags = libc::fcntl(read_end, libc::F_GETFL);
        if original_flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(read_end, libc::F_SETFL, original_flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut discarded = [0u8; 32];
        while libc::read(read_end, discarded.as_mut_ptr().cast(), discarded.len()) > 0 {}
        if libc::fcntl(read_end, libc::F_SETFL, original_flags) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Read a signal's current disposition without changing it.
#[cfg(all(unix, any(not(target_os = "macos"), test)))]
fn current_disposition_of(signal: libc::c_int) -> libc::sigaction {
    // SAFETY: a NULL `act` is POSIX's read-only query. `previous` is a fully
    // owned, zeroed `sigaction` the kernel writes into.
    unsafe {
        let mut previous: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut previous);
        previous
    }
}

/// Set once the process-lifetime macOS handlers are in place.
#[cfg(target_os = "macos")]
static MACOS_TERMINATION_HANDLERS_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
fn install_sigterm_handler_macos() -> std::io::Result<()> {
    use signal_hook::consts::signal::SIGTERM;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    // Counted rather than flagged, so SIGTERMs landing within one poll still
    // escalate one step each.
    let sigterm_deliveries_not_yet_escalated = Arc::new(AtomicUsize::new(0));
    let sigterm_deliveries_counted_by_the_handler =
        Arc::clone(&sigterm_deliveries_not_yet_escalated);
    // SAFETY: the handler only increments an atomic, which is async-signal-safe.
    unsafe {
        signal_hook::low_level::register(SIGTERM, move || {
            sigterm_deliveries_counted_by_the_handler.fetch_add(1, Ordering::SeqCst);
        })
    }?;

    std::thread::spawn(move || {
        loop {
            for _ in 0..sigterm_deliveries_not_yet_escalated.swap(0, Ordering::SeqCst) {
                escalate_the_runtime_shutdown_one_step_for_a_delivered_signal("SIGTERM");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// The restoration contract, asserted at the only layer that can witness it:
    /// the kernel's own record of the handler. `rt.run()` returning with SIGINT
    /// still pointed at a dead run loop's forwarding thread is exactly the
    /// "Ctrl-C stops working after run()" failure the wheel must not ship.
    ///
    /// Mental-revert: dropping the `restore_now` calls from `Drop` leaves
    /// `sa_sigaction` pointing at the self-pipe handler and fails.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn dropping_ownership_restores_the_previous_dispositions() {
        let _hangup_not_ignored = SignalDispositionSetForOneTest::set(libc::SIGHUP, libc::SIG_DFL);
        let dispositions_before: Vec<(libc::c_int, libc::sigaction)> =
            SHUTDOWN_SIGNALS_OWNED_BY_THE_RUN_LOOP
                .into_iter()
                .map(|signal| (signal, current_disposition_of(signal)))
                .collect();

        {
            let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("no other run loop owns the shutdown signals");
            for (signal, before) in &dispositions_before {
                assert_ne!(
                    current_disposition_of(*signal).sa_sigaction,
                    before.sa_sigaction,
                    "taking ownership must actually displace signal {signal}'s handler",
                );
            }
        }

        for (signal, before) in &dispositions_before {
            assert_eq!(
                current_disposition_of(*signal).sa_sigaction,
                before.sa_sigaction,
                "signal {signal} must be handed back to whoever held it",
            );
        }
    }

    /// Ownership is exclusive, and a refused take must not disturb the owner's
    /// handlers — nor leak the claim, or every later run loop in the process
    /// would be refused.
    #[test]
    #[serial]
    fn a_second_owner_is_refused_while_the_first_is_alive() {
        let first = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        assert!(
            ScopedShutdownSignalOwnership::take_until_dropped().is_err(),
            "a second run loop must not be able to take the shutdown signals",
        );
        drop(first);

        drop(
            ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("ownership must be retakeable once the first owner drops"),
        );
    }

    /// Raise `signal` and wait for the forwarding thread to escalate the
    /// shutdown to `awaited`. Panics rather than hanging if it never does.
    #[cfg(unix)]
    fn raise_and_await_escalation_to(
        signal: libc::c_int,
        awaited: crate::core::runtime::RuntimeShutdownEscalation,
        context: &str,
    ) {
        use crate::core::runtime::runtime_shutdown_escalation;

        // SAFETY: the signal is owned by the caller's live
        // `ScopedShutdownSignalOwnership`, so this reaches the self-pipe
        // handler rather than the default terminate action.
        assert_eq!(
            unsafe { libc::raise(signal) },
            0,
            "raising signal {signal} must succeed ({context})",
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if runtime_shutdown_escalation() == awaited {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "signal {signal} never escalated the shutdown to {awaited:?}; it reads {:?} \
             ({context})",
            runtime_shutdown_escalation()
        );
    }

    /// The whole point of owning the signals: a delivered SIGINT must reach the
    /// request the run loop polls.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn a_delivered_sigint_becomes_a_runtime_shutdown_request() {
        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();

        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        raise_and_await_escalation_to(
            signal_hook::consts::signal::SIGINT,
            crate::core::runtime::RuntimeShutdownEscalation::Graceful,
            "first owner",
        );
    }

    /// A closed terminal or a supervisor's reload sends SIGHUP, which must tear
    /// the graph down gracefully rather than kill the app where it stands.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_delivered_sighup_becomes_a_graceful_shutdown() {
        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
        let _hangup_not_ignored = SignalDispositionSetForOneTest::set(libc::SIGHUP, libc::SIG_DFL);

        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        raise_and_await_escalation_to(
            signal_hook::consts::signal::SIGHUP,
            crate::core::runtime::RuntimeShutdownEscalation::Graceful,
            "SIGHUP",
        );
    }

    /// A second interrupt forces the run it lands in, and the next run's first
    /// interrupt is graceful again — the escalation is one run's, never the
    /// process's.
    ///
    /// Fail-without-fix: a latch reads the second SIGINT as the first, so the
    /// escalation never reaches `Forced`.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn repeated_interrupts_escalate_one_run_and_the_next_run_starts_graceful() {
        use crate::core::runtime::{RuntimeShutdownEscalation, take_runtime_shutdown_escalation};
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();

        let first_run = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        raise_and_await_escalation_to(SIGINT, RuntimeShutdownEscalation::Graceful, "first");
        raise_and_await_escalation_to(SIGTERM, RuntimeShutdownEscalation::Forced, "second");
        drop(first_run);
        assert_eq!(
            take_runtime_shutdown_escalation(),
            RuntimeShutdownEscalation::Forced
        );

        let _next_run = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("ownership must be retakeable once the first owner drops");
        raise_and_await_escalation_to(SIGINT, RuntimeShutdownEscalation::Graceful, "next run");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            crate::core::runtime::runtime_shutdown_escalation(),
            RuntimeShutdownEscalation::Graceful,
            "the next run's first interrupt must not force it"
        );
    }

    /// Re-taking after a drop is the wheel's second-`Runtime()`-in-one-process
    /// case, and it must still *work* — asserting the handler pointer alone
    /// would not catch it.
    ///
    /// This is a regression lock on a real defect: a registry-based
    /// implementation (signal-hook's `Signals`) installs its dispatcher once
    /// per signal and skips reinstallation on later registrations, so restoring
    /// the previous disposition on drop desynchronizes the registry from the
    /// kernel and leaves the SECOND run loop silently unable to catch Ctrl-C.
    /// Mental-revert: reverting to `Signals::new` + `handle().close()` passes
    /// the disposition asserts above and fails here on the second iteration.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn every_retaken_ownership_still_catches_sigint() {
        for ownership_generation in 1..=3 {
            let _escalation_cleared_even_on_unwind =
                crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();

            let owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("each run loop in turn may own the shutdown signals");
            raise_and_await_escalation_to(
                signal_hook::consts::signal::SIGINT,
                crate::core::runtime::RuntimeShutdownEscalation::Graceful,
                &format!("ownership generation {ownership_generation}"),
            );
            drop(owned);
        }
    }

    /// A signal delivered while the previous owner was tearing down must not
    /// shut the next run loop down the instant it starts.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_stale_signal_does_not_shut_down_the_next_run_loop() {
        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();

        {
            let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("no other run loop owns the shutdown signals");
            // Written straight into the pipe so it is still unread when the
            // owner below starts — racing a real signal against teardown would
            // make this test flaky rather than deterministic.
            let self_pipe = shutdown_signal_self_pipe().expect("the self-pipe exists");
            let stale = signal_hook::consts::signal::SIGINT as u8;
            // SAFETY: the write end stays open for the process lifetime; the
            // source is one byte of stack this frame owns.
            unsafe {
                libc::write(self_pipe.write_end, std::ptr::from_ref(&stale).cast(), 1);
            }
        }

        crate::core::runtime::take_runtime_shutdown_escalation();
        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("ownership must be retakeable once the first owner drops");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !crate::core::runtime::is_runtime_shutdown_requested(),
            "a byte left over from the previous owner must not shut this run loop down",
        );
    }

    /// macOS installs its handlers once for the process's life, so a run that
    /// ends hands nothing back and the next one still catches SIGINT.
    #[test]
    #[serial]
    #[cfg(target_os = "macos")]
    fn dropping_ownership_on_macos_leaves_the_process_lifetime_handlers_installed() {
        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();

        drop(
            ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("no other run loop owns the shutdown signals"),
        );
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let after_the_run = current_disposition_of(signal).sa_sigaction;
            assert!(
                after_the_run != libc::SIG_DFL && after_the_run != libc::SIG_IGN,
                "signal {signal}'s handler must stay installed once its run ends",
            );
        }
    }

    /// SIGHUP is not owned on macOS: taking the shutdown signals leaves its
    /// disposition exactly as it found it, ignored or not.
    ///
    /// Fail-without-fix: a handler installed for SIGHUP displaces the default,
    /// and a supervisor's `nohup` loses its ignored disposition.
    #[test]
    #[serial]
    #[cfg(target_os = "macos")]
    fn sighup_is_not_owned_on_macos() {
        for hangup_disposition in [libc::SIG_DFL, libc::SIG_IGN] {
            let _hangup_set = SignalDispositionSetForOneTest::set(libc::SIGHUP, hangup_disposition);
            let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("no other run loop owns the shutdown signals");
            assert_eq!(
                current_disposition_of(libc::SIGHUP).sa_sigaction,
                hangup_disposition,
                "taking the shutdown signals must leave SIGHUP alone",
            );
        }
    }

    /// Sets one signal's disposition for the length of a test and puts back what
    /// was there — so a suite run under `nohup` does not read as a failure.
    #[cfg(unix)]
    struct SignalDispositionSetForOneTest {
        signal: libc::c_int,
        previous: libc::sigaction,
    }

    #[cfg(unix)]
    impl SignalDispositionSetForOneTest {
        fn set(signal: libc::c_int, handler: libc::sighandler_t) -> Self {
            let previous = current_disposition_of(signal);
            // SAFETY: a zeroed `sigaction` with only its handler set is a valid
            // disposition, and the previous one is restored on drop.
            unsafe {
                let mut requested: libc::sigaction = std::mem::zeroed();
                requested.sa_sigaction = handler;
                libc::sigemptyset(&mut requested.sa_mask);
                libc::sigaction(signal, &requested, std::ptr::null_mut());
            }
            Self { signal, previous }
        }
    }

    #[cfg(unix)]
    impl Drop for SignalDispositionSetForOneTest {
        fn drop(&mut self) {
            // SAFETY: restores the disposition this value captured.
            unsafe { libc::sigaction(self.signal, &self.previous, std::ptr::null_mut()) };
        }
    }

    /// `nohup` tells a process to outlive its terminal by ignoring SIGHUP, and
    /// taking the shutdown signals must not undo that.
    ///
    /// Fail-without-fix: displacing SIGHUP whatever it was makes closing the
    /// terminal shut down an app its user started under `nohup`.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_hangup_the_process_was_told_to_ignore_stays_ignored() {
        let _escalation_cleared_even_on_unwind =
            crate::core::RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
        let _hangup_ignored = SignalDispositionSetForOneTest::set(libc::SIGHUP, libc::SIG_IGN);

        let owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        assert_eq!(
            current_disposition_of(libc::SIGHUP).sa_sigaction,
            libc::SIG_IGN
        );
        // SAFETY: SIGHUP is ignored, so raising it delivers nothing.
        unsafe { libc::raise(libc::SIGHUP) };
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !crate::core::runtime::is_runtime_shutdown_requested(),
            "an ignored SIGHUP still shut the run loop down"
        );
        drop(owned);
        assert_eq!(
            current_disposition_of(libc::SIGHUP).sa_sigaction,
            libc::SIG_IGN
        );
    }

    /// Set in the child process the third-interrupt test re-runs itself in,
    /// naming the file it records its stand-in helper's process group in.
    #[cfg(unix)]
    const THIRD_INTERRUPT_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_THIRD_INTERRUPT_CHILD_RECORD_PATH";

    /// The third interrupt ends the process at once with status 130, and takes
    /// every registered helper process group with it — a helper's descendants
    /// included, which the kernel's parent-death signal never reaches.
    ///
    /// Run in a child process, because passing is exiting.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn a_third_interrupt_kills_every_helper_process_group_and_exits_with_130() {
        if let Some(record_path) =
            std::env::var_os(THIRD_INTERRUPT_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE)
        {
            interrupt_this_process_three_times_holding_a_helper_process_group(record_path.into());
        }

        let record = tempfile::tempdir().expect("a temporary directory");
        let record_path = record.path().join("helper-process-group");
        let child = crate::core::test_support::rerun_this_test_in_a_child_process(
            "core::signals::tests::a_third_interrupt_kills_every_helper_process_group_and_exits_with_130",
            THIRD_INTERRUPT_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE,
            record_path.as_os_str(),
        );

        assert_eq!(
            child.status.code(),
            Some(EXIT_STATUS_OF_A_THIRD_INTERRUPT),
            "the child did not exit on its third interrupt with status 130: {}\n{}\n{}",
            child.status,
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );

        let helper_process_group: libc::pid_t = std::fs::read_to_string(&record_path)
            .expect("the child recorded its helper's process group")
            .trim()
            .parse()
            .expect("the record is a process group id");
        assert!(
            crate::core::test_support::a_process_group_is_gone_within(
                helper_process_group,
                std::time::Duration::from_secs(5)
            ),
            "the helper's process group outlived the app's third interrupt"
        );
    }

    /// The child's half: a stand-in helper in a group of its own, registered,
    /// and three SIGINTs. Never returns — the third one exits the process.
    #[cfg(unix)]
    fn interrupt_this_process_three_times_holding_a_helper_process_group(
        record_path: std::path::PathBuf,
    ) -> ! {
        let stand_in_helper =
            crate::core::test_support::a_process_parked_in_a_process_group_of_its_own();
        let helper_process_group = stand_in_helper.id() as i32;
        std::fs::write(&record_path, helper_process_group.to_string())
            .expect("the record is written");
        assert!(crate::core::runtime::register_a_helper_process_group(
            helper_process_group
        ));

        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        for _ in 0..3 {
            // SAFETY: SIGINT is owned above, so it reaches the self-pipe handler.
            unsafe { libc::raise(signal_hook::consts::signal::SIGINT) };
        }
        std::thread::sleep(std::time::Duration::from_secs(30));
        panic!("three interrupts did not end the process");
    }
}
