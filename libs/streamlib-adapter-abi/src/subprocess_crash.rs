// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generic spawn-and-SIGKILL primitive for adapter crash tests.
//!
//! The harness deliberately knows nothing about streamlib's surface-share
//! state — it owns spawn, configurable-timing kill, and the post-kill
//! "did the kernel close the fd?" observation, and exposes the rest as
//! caller-provided closures. The intended end state is that #511–#514
//! call this from their own integration tests against the real
//! surface-share service; the self-contained test in
//! `tests/subprocess_crash.rs` covers the harness contract itself by
//! observing kernel-FD-cleanup through an inherited pipe.

use std::io;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

type PostSpawnHook<'h> = Box<dyn FnOnce(&Child) -> io::Result<()> + 'h>;

/// When the harness should SIGKILL the child relative to its lifecycle.
#[derive(Debug, Clone, Copy)]
pub enum CrashTiming {
    /// Kill `delay` after spawn — the child has had time to set up.
    AfterDelay(Duration),
    /// Kill immediately on spawn — exercises the racy path.
    Immediate,
}

/// Outcome of a single harness run.
#[derive(Debug)]
pub struct SubprocessCrashOutcome {
    /// Wall-clock time between SIGKILL and the post-observation closure
    /// reporting cleanup. Useful for asserting the surface-share
    /// watchdog meets a budget (e.g. < 1 s).
    pub cleanup_latency: Duration,
    /// Exit status reported by `wait`. `None` if the child was already
    /// reaped or wait failed.
    pub exit_status: Option<std::process::ExitStatus>,
}

/// Configurable spawn+SIGKILL primitive used by adapter crash tests.
///
/// Build with [`Self::new`], add timing via [`Self::with_timing`], and
/// run with [`Self::run`] — passing in a closure that observes whether
/// host-side cleanup happened. The closure returns `Ok(())` once it has
/// confirmed cleanup; the harness times that and reports back.
pub struct SubprocessCrashHarness<'h> {
    command: Command,
    timing: CrashTiming,
    cleanup_poll_interval: Duration,
    cleanup_timeout: Duration,
    /// Hook that fires once after spawn and before the kill-timing
    /// delay. Used by callers to close their parent-side copy of any
    /// fds the child inherited (so kernel-FD-cleanup can fire on
    /// SIGKILL) or to set up host-side observers.
    post_spawn: Option<PostSpawnHook<'h>>,
}

impl<'h> SubprocessCrashHarness<'h> {
    /// Build a harness that will spawn `command` and (by default)
    /// SIGKILL it 50 ms after spawn, polling for cleanup every 10 ms
    /// for up to 5 s.
    pub fn new(command: Command) -> Self {
        Self {
            command,
            timing: CrashTiming::AfterDelay(Duration::from_millis(50)),
            cleanup_poll_interval: Duration::from_millis(10),
            cleanup_timeout: Duration::from_secs(5),
            post_spawn: None,
        }
    }

    pub fn with_timing(mut self, timing: CrashTiming) -> Self {
        self.timing = timing;
        self
    }

    pub fn with_cleanup_timeout(mut self, timeout: Duration) -> Self {
        self.cleanup_timeout = timeout;
        self
    }

    pub fn with_cleanup_poll_interval(mut self, interval: Duration) -> Self {
        self.cleanup_poll_interval = interval;
        self
    }

    /// Register a hook to run once after spawn, before the kill-timing
    /// delay. Typical use: close the parent's copy of an inherited fd
    /// so the kernel actually drops the last reference when the child
    /// is killed.
    pub fn with_post_spawn<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(&Child) -> io::Result<()> + 'h,
    {
        self.post_spawn = Some(Box::new(hook));
        self
    }

    /// Spawn the child, kill it per [`CrashTiming`], and poll
    /// `observe_cleanup` until it returns `Ok(())` or the timeout
    /// expires.
    ///
    /// `observe_cleanup` is called repeatedly each poll — return
    /// `Ok(())` to mark cleanup complete, `Err(...)` to keep waiting.
    #[tracing::instrument(level = "debug", skip(self, observe_cleanup), fields(timing = ?self.timing))]
    pub fn run<F>(
        mut self,
        mut observe_cleanup: F,
    ) -> io::Result<SubprocessCrashOutcome>
    where
        F: FnMut() -> Result<(), &'static str>,
    {
        let mut child = self.command.spawn()?;

        if let Some(hook) = self.post_spawn.take() {
            hook(&child)?;
        }

        if let CrashTiming::AfterDelay(d) = self.timing {
            std::thread::sleep(d);
        }

        let kill_at = Instant::now();
        // SIGKILL — bypass the child's signal handlers entirely.
        child.kill()?;
        let exit_status = child.wait().ok();

        let deadline = kill_at + self.cleanup_timeout;
        loop {
            if observe_cleanup().is_ok() {
                return Ok(SubprocessCrashOutcome {
                    cleanup_latency: kill_at.elapsed(),
                    exit_status,
                });
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "host did not observe cleanup within {:?} after SIGKILL",
                        self.cleanup_timeout
                    ),
                ));
            }
            std::thread::sleep(self.cleanup_poll_interval);
        }
    }

}
