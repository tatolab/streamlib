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

#[cfg(all(unix, not(target_os = "macos")))]
use crate::core::runtime::request_runtime_shutdown;
use std::sync::atomic::{AtomicBool, Ordering};

/// Only one run loop may own the shutdown signals at a time — two owners would
/// race to restore dispositions and the loser would restore a disposition the
/// winner had already replaced.
static SHUTDOWN_SIGNALS_OWNED: AtomicBool = AtomicBool::new(false);

/// Owns SIGTERM + SIGINT for as long as it is alive, funnelling both into
/// [`request_runtime_shutdown`](crate::core::runtime::request_runtime_shutdown).
///
/// Dropping it stops the forwarding thread, joins it, and restores the signal
/// dispositions captured at construction. macOS instead routes termination
/// through `NSApplication.terminate` (reaching the same teardown via
/// `applicationWillTerminate`), which cannot be uninstalled — there, drop
/// releases the ownership claim only.
pub struct ScopedShutdownSignalOwnership {
    #[cfg(all(unix, not(target_os = "macos")))]
    signal_forwarding: Option<UnixSignalForwarding>,
}

/// The Linux forwarding thread plus the dispositions it displaced.
#[cfg(all(unix, not(target_os = "macos")))]
struct UnixSignalForwarding {
    forwarding_thread: std::thread::JoinHandle<()>,
    displaced_sigint: DisplacedSignalDisposition,
    displaced_sigterm: DisplacedSignalDisposition,
}

/// One signal's pre-existing disposition, restored on drop unless already
/// restored explicitly.
///
/// The restore is RAII because it must survive every failure path in
/// `install()` — a handler left pointing at the self-pipe with no forwarding
/// thread behind it swallows Ctrl-C and SIGTERM for the rest of the process.
/// Carrying the signal number alongside its action also makes restoring one
/// signal's disposition onto another unrepresentable.
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
    // is one byte of stack this frame owns. The write is non-blocking, so a
    // failure here means the pipe is backed up and a shutdown request is
    // already queued — dropping this one changes nothing. `errno` is saved and
    // restored around it because the handler preempted a syscall that may be
    // about to read its own errno.
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
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let self_pipe = shutdown_signal_self_pipe()?;
        // A signal delivered during the previous owner's teardown would
        // otherwise be read by this owner and shut it down on the spot.
        drain_pending_bytes(self_pipe.read_end)?;

        // Both displacements restore themselves if anything below fails —
        // including the thread spawn, which would otherwise leave the handlers
        // installed with nothing reading the pipe.
        let displaced_sigint = DisplacedSignalDisposition::displace_with_self_pipe_handler(SIGINT)?;
        let displaced_sigterm =
            DisplacedSignalDisposition::displace_with_self_pipe_handler(SIGTERM)?;

        FORWARDING_THREAD_SHOULD_STOP.store(false, Ordering::SeqCst);
        let read_end = self_pipe.read_end;
        let forwarding_thread = std::thread::Builder::new()
            .name("shutdown-signal-forwarding".to_string())
            .spawn(move || forward_signals_until_stopped(read_end))?;

        tracing::info!("Shutdown signals owned by this run loop (SIGTERM, SIGINT)");
        Ok(Self {
            signal_forwarding: Some(UnixSignalForwarding {
                forwarding_thread,
                displaced_sigint,
                displaced_sigterm,
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
            // Ctrl+C reaches teardown through `NSApplication.terminate` rather
            // than the request funnel, because an AppKit app's shutdown must
            // run `applicationWillTerminate` on the main thread.
            ctrlc::set_handler(move || {
                tracing::info!("Ctrl+C received, triggering graceful shutdown");
                trigger_macos_termination();
            })
            .map_err(std::io::Error::other)?;

            install_sigterm_handler_macos()?;

            let _ = MACOS_TERMINATION_HANDLERS_INSTALLED.set(());
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
            // a handler whose reader is going away. This is the call that hands
            // SIGINT back to CPython in the wheel. Both are already-restored
            // after this, so their `Drop` is a no-op.
            forwarding.displaced_sigint.restore_now();
            forwarding.displaced_sigterm.restore_now();

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
    // Checked rather than assumed: leaving the write end blocking would let the
    // handler stall inside signal context, the one thing this design must not
    // do.
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

/// Read the self-pipe until stopped, funnelling each delivered signal into
/// [`request_runtime_shutdown`].
///
/// The wait is a bounded `poll` rather than a blocking `read` so the stop flag
/// is observed even if the stop byte never lands — a lost wakeup here would
/// hang `join()` inside a `Drop`.
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
        if let Err(error) = request_runtime_shutdown(&format!("posix signal {signal_name}")) {
            tracing::error!(%error, "Shutdown-signal forwarding: request failed");
        }
    }
    tracing::debug!("Shutdown-signal forwarding thread exiting");
}

/// Ask the forwarding thread to stop, and nudge it out of its poll.
#[cfg(all(unix, not(target_os = "macos")))]
fn stop_forwarding_thread() {
    // The flag is what actually bounds the join; the byte only shortens the
    // wait from the poll interval to immediately, so a failed write is
    // recoverable and not worth failing teardown over.
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

/// Discard bytes left in the pipe by a previous owner.
///
/// Both `fcntl`s are checked because the drain below only terminates while the
/// read end is non-blocking — on an unchecked failure it would block forever
/// inside `install()`, which is inside `rt.run()`.
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

/// Read a signal's current disposition without changing it. The restoration
/// contract is only witnessable against the kernel's own record, so this exists
/// for the tests that assert it.
#[cfg(all(test, unix, not(target_os = "macos")))]
fn read_current_disposition(signal: libc::c_int) -> libc::sigaction {
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
    use signal_hook::flag;
    use std::sync::Arc;

    let term_flag = Arc::new(AtomicBool::new(false));
    flag::register(SIGTERM, Arc::clone(&term_flag))?;

    std::thread::spawn(move || {
        loop {
            if term_flag.load(Ordering::Relaxed) {
                tracing::info!("SIGTERM received, triggering graceful shutdown");
                trigger_macos_termination();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    Ok(())
}

#[cfg(target_os = "macos")]
fn trigger_macos_termination() {
    use dispatch2::DispatchQueue;

    DispatchQueue::main().exec_async(move || {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            tracing::info!("Signal handler: Calling NSApplication.terminate()");
            app.terminate(None);
        } else {
            tracing::error!(
                "Signal handler: Not on main thread, cannot call NSApplication.terminate()"
            );
        }
    });
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
    /// Mental-revert: dropping the `restore_disposition` calls from `Drop`
    /// leaves `sa_sigaction` pointing at the self-pipe handler and fails.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn dropping_ownership_restores_the_previous_dispositions() {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let sigint_before = read_current_disposition(SIGINT);
        let sigterm_before = read_current_disposition(SIGTERM);

        {
            let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("no other run loop owns the shutdown signals");
            assert_ne!(
                read_current_disposition(SIGINT).sa_sigaction,
                sigint_before.sa_sigaction,
                "taking ownership must actually displace the SIGINT handler",
            );
        }

        assert_eq!(
            read_current_disposition(SIGINT).sa_sigaction,
            sigint_before.sa_sigaction,
            "SIGINT must be handed back to whoever held it",
        );
        assert_eq!(
            read_current_disposition(SIGTERM).sa_sigaction,
            sigterm_before.sa_sigaction,
            "SIGTERM must be handed back to whoever held it",
        );
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

    /// Raise SIGINT and wait for the forwarding thread to latch a request.
    /// Panics rather than hanging if the signal never arrives.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn raise_sigint_and_await_latched_request(context: &str) {
        use crate::core::runtime::is_runtime_shutdown_requested;

        // SAFETY: SIGINT is owned by the caller's live
        // `ScopedShutdownSignalOwnership`, so this reaches the self-pipe
        // handler rather than the default terminate action.
        assert_eq!(
            unsafe { libc::raise(signal_hook::consts::signal::SIGINT) },
            0,
            "raising SIGINT must succeed ({context})",
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if is_runtime_shutdown_requested() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("SIGINT was never funnelled into a runtime-shutdown request ({context})");
    }

    /// The whole point of owning the signals: a delivered SIGINT must reach the
    /// request funnel the run loop polls.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_delivered_sigint_becomes_a_runtime_shutdown_request() {
        let _latch_cleared_even_on_unwind =
            crate::core::RuntimeShutdownRequestLatchClearedOnDrop::clear_now_and_on_drop();

        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("no other run loop owns the shutdown signals");
        raise_sigint_and_await_latched_request("first owner");
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
    #[cfg(all(unix, not(target_os = "macos")))]
    fn every_retaken_ownership_still_catches_sigint() {
        for ownership_generation in 1..=3 {
            let _latch_cleared_even_on_unwind =
                crate::core::RuntimeShutdownRequestLatchClearedOnDrop::clear_now_and_on_drop();

            let owned = ScopedShutdownSignalOwnership::take_until_dropped()
                .expect("each run loop in turn may own the shutdown signals");
            raise_sigint_and_await_latched_request(&format!(
                "ownership generation {ownership_generation}"
            ));
            drop(owned);
        }
    }

    /// A signal delivered while the previous owner was tearing down must not
    /// shut the next run loop down the instant it starts.
    #[test]
    #[serial]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_stale_signal_does_not_shut_down_the_next_run_loop() {
        let _latch_cleared_even_on_unwind =
            crate::core::RuntimeShutdownRequestLatchClearedOnDrop::clear_now_and_on_drop();

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

        crate::core::runtime::take_runtime_shutdown_request_latch();
        let _owned = ScopedShutdownSignalOwnership::take_until_dropped()
            .expect("ownership must be retakeable once the first owner drops");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !crate::core::runtime::is_runtime_shutdown_requested(),
            "a byte left over from the previous owner must not shut this run loop down",
        );
    }
}
