// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The machine's shutdown request funnel, how far it has escalated, and each
//! loaded stream's own escalation.
//!
//! A machine shutdown *request* never tears anything down itself: it raises the
//! machine's escalation, and whoever owns the machine's shutdown signals walks
//! every loaded stream to it. The machine's escalation is process-global (the
//! signal handler holds no `Runner`) and belongs to whichever owner observes it:
//! that owner takes it once its run has ended, so a request issued while no
//! owner is running is observed by the next one to start, and a run's
//! interrupts never escalate the next one.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: a delivered signal escalates on
//! repeat — graceful, then forced, then exit at once. A programmatic request is
//! the graceful step and only that, however often it is repeated.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use std::sync::Arc;

use crate::core::error::Result;

/// How far a shutdown has gone — the machine's, or one loaded stream's, which
/// never reaches `ExitAtOnce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RuntimeShutdownEscalation {
    /// No shutdown has been asked for.
    NotRequested = 0,
    /// The graph stops gracefully: every helper walks its whole ladder.
    Graceful = 1,
    /// Every helper's ladder skips to terminating its process group, and a
    /// native processor thread still inside its callback is abandoned.
    Forced = 2,
    /// Every helper's process group is killed and the process exits with
    /// status 130.
    ExitAtOnce = 3,
}

impl RuntimeShutdownEscalation {
    fn from_stored(stored: u8) -> Self {
        match stored {
            0 => Self::NotRequested,
            1 => Self::Graceful,
            2 => Self::Forced,
            _ => Self::ExitAtOnce,
        }
    }

    fn next_for_a_delivered_signal(self) -> Self {
        match self {
            Self::NotRequested => Self::Graceful,
            Self::Graceful => Self::Forced,
            Self::Forced | Self::ExitAtOnce => Self::ExitAtOnce,
        }
    }
}

/// The machine's shutdown level: raised by
/// [`request_the_shutdown_of_every_loaded_stream`] and by each delivered
/// signal, and read by the owner of the machine's shutdown signals, which walks
/// every loaded stream to it. Process-global like `PUBSUB`.
static THE_MACHINES_SHUTDOWN_ESCALATION: AtomicU8 =
    AtomicU8::new(RuntimeShutdownEscalation::NotRequested as u8);

/// How often a waiter re-reads the escalation. Shared so the engine's waits
/// ([`crate::core::runtime::Runner::wait_until_the_stream_ends`]) and every
/// out-of-crate waiter observe a request at the same granularity.
pub const RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Ask whoever owns the machine's shutdown signals to shut every loaded stream
/// down gracefully, exactly as a first Ctrl+C / SIGTERM does. Idempotent and
/// fire-and-forget: repeating it never forces.
///
/// `reason` is a human-readable attribution logged at `info` (empty string =
/// unspecified).
#[tracing::instrument]
pub fn request_the_shutdown_of_every_loaded_stream(reason: &str) -> Result<()> {
    tracing::info!(reason, "the shutdown of every loaded stream was requested");
    let _ = THE_MACHINES_SHUTDOWN_ESCALATION
        .fetch_max(RuntimeShutdownEscalation::Graceful as u8, Ordering::SeqCst);
    Ok(())
}

/// Escalate shutdown one step for a delivered signal, returning the step it
/// reached. The caller acts on [`RuntimeShutdownEscalation::ExitAtOnce`];
/// everything below it is read by the run loop and the ladder.
///
/// Nothing here publishes or takes a lock: it runs on the signal-forwarding
/// thread, and the owner of the signals walks every stream to the step reached.
pub(crate) fn escalate_the_machines_shutdown_for_a_delivered_signal(
    signal_name: &str,
) -> RuntimeShutdownEscalation {
    let (Ok(previous) | Err(previous)) = THE_MACHINES_SHUTDOWN_ESCALATION.fetch_update(
        Ordering::SeqCst,
        Ordering::SeqCst,
        |stored| {
            Some(RuntimeShutdownEscalation::from_stored(stored).next_for_a_delivered_signal() as u8)
        },
    );
    let reached = RuntimeShutdownEscalation::from_stored(previous).next_for_a_delivered_signal();
    match reached {
        RuntimeShutdownEscalation::NotRequested | RuntimeShutdownEscalation::Graceful => {
            tracing::info!(
                signal_name,
                "the shutdown of every loaded stream was requested"
            );
        }
        RuntimeShutdownEscalation::Forced => tracing::warn!(
            signal_name,
            "a second interrupt forces the shutdown: every helper's process group is \
             terminated without its teardown, and a native processor thread still inside its \
             callback is abandoned. A third interrupt exits at once."
        ),
        RuntimeShutdownEscalation::ExitAtOnce => tracing::error!(
            signal_name,
            "a third interrupt: killing every helper's process group and exiting with status 130"
        ),
    }
    reached
}

/// How far the machine's shutdown has escalated.
pub fn the_machines_shutdown_escalation() -> RuntimeShutdownEscalation {
    RuntimeShutdownEscalation::from_stored(THE_MACHINES_SHUTDOWN_ESCALATION.load(Ordering::SeqCst))
}

/// Whether the machine's shutdown has been requested.
pub fn is_the_machines_shutdown_requested() -> bool {
    the_machines_shutdown_escalation() >= RuntimeShutdownEscalation::Graceful
}

/// Clear the machine's escalation, returning how far it had gone.
///
/// Only the owner of the machine's shutdown signals may call it, once its run has ended, so the
/// requests it observed neither end nor escalate the next run in the same
/// process. Never returns while a third interrupt or the teardown watchdog is
/// ending the process, so the status that ends it is theirs.
pub fn take_the_machines_shutdown_escalation() -> RuntimeShutdownEscalation {
    crate::core::runtime::park_forever_if_the_process_is_ending_at_once();
    let taken = swap_the_machines_shutdown_escalation_clear();
    // A third interrupt that escalated before the swap is on its way to
    // `_exit`, whether or not it has raised the ending-at-once flag yet.
    if taken == RuntimeShutdownEscalation::ExitAtOnce {
        crate::core::runtime::park_forever();
    }
    taken
}

fn swap_the_machines_shutdown_escalation_clear() -> RuntimeShutdownEscalation {
    RuntimeShutdownEscalation::from_stored(THE_MACHINES_SHUTDOWN_ESCALATION.swap(
        RuntimeShutdownEscalation::NotRequested as u8,
        Ordering::SeqCst,
    ))
}

/// How far one loaded stream's own shutdown has gone: `NotRequested`,
/// `Graceful` or `Forced`. Shared between the stream and its runtime context.
#[derive(Debug, Clone, Default)]
pub struct ShutdownEscalationOfOneStream {
    stored_escalation: Arc<AtomicU8>,
}

impl ShutdownEscalationOfOneStream {
    /// Raise this stream's escalation to `Graceful`, saying whether this call
    /// was the one that raised it.
    pub(crate) fn raise_to_graceful(&self) -> bool {
        self.raise_to(RuntimeShutdownEscalation::Graceful)
    }

    /// Raise this stream's escalation to `Forced`, saying whether this call was
    /// the one that raised it.
    pub(crate) fn raise_to_forced(&self) -> bool {
        self.raise_to(RuntimeShutdownEscalation::Forced)
    }

    fn raise_to(&self, escalation: RuntimeShutdownEscalation) -> bool {
        self.stored_escalation
            .fetch_max(escalation as u8, Ordering::SeqCst)
            < escalation as u8
    }

    /// How far this stream's shutdown has gone.
    pub fn escalation(&self) -> RuntimeShutdownEscalation {
        RuntimeShutdownEscalation::from_stored(self.stored_escalation.load(Ordering::SeqCst))
    }

    /// Whether this stream's shutdown has been requested.
    pub fn is_requested(&self) -> bool {
        self.escalation() >= RuntimeShutdownEscalation::Graceful
    }

    /// Whether this stream's shutdown has been forced.
    pub fn is_forced(&self) -> bool {
        self.escalation() >= RuntimeShutdownEscalation::Forced
    }
}

/// Clears the escalation on construction and again on drop, so a `#[serial]`
/// test that touches the process-global escalation leaves it clean even when an
/// assertion unwinds past its own cleanup.
#[cfg(test)]
pub(crate) struct TheMachinesShutdownEscalationClearedOnDrop;

#[cfg(test)]
impl TheMachinesShutdownEscalationClearedOnDrop {
    pub(crate) fn clear_now_and_on_drop() -> Self {
        swap_the_machines_shutdown_escalation_clear();
        Self
    }
}

#[cfg(test)]
impl Drop for TheMachinesShutdownEscalationClearedOnDrop {
    fn drop(&mut self) {
        swap_the_machines_shutdown_escalation_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// The escalation is process-global, so every test that touches it runs
    /// `#[serial]` and leaves it cleared for the next one — including when an
    /// assertion inside `body` unwinds.
    fn with_cleared_escalation<F: FnOnce()>(body: F) {
        let _escalation_cleared_even_on_unwind =
            TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
        body();
    }

    /// A request is observed, and a loop owner that polls
    /// `is_the_machines_shutdown_requested` observes it even when the pubsub
    /// listener missed the event.
    #[test]
    #[serial]
    fn a_request_is_observed_as_a_graceful_shutdown() {
        with_cleared_escalation(|| {
            assert!(
                !is_the_machines_shutdown_requested(),
                "the escalation must start clear"
            );
            request_the_shutdown_of_every_loaded_stream("unit test")
                .expect("the host arm never fails");
            assert_eq!(
                the_machines_shutdown_escalation(),
                RuntimeShutdownEscalation::Graceful
            );
        });
    }

    /// "Requesting shutdown twice is not an error", and never a force: a
    /// programmatic request is the graceful step however often it is issued.
    #[test]
    #[serial]
    fn repeated_requests_stay_graceful() {
        with_cleared_escalation(|| {
            for attempt in 0..3 {
                request_the_shutdown_of_every_loaded_stream(&format!("unit test {attempt}"))
                    .expect("a repeated request is not an error");
            }
            assert_eq!(
                the_machines_shutdown_escalation(),
                RuntimeShutdownEscalation::Graceful,
                "a repeated request must not force the shutdown",
            );
        });
    }

    #[test]
    #[serial]
    fn each_delivered_signal_escalates_one_step_and_the_third_is_exit_at_once() {
        with_cleared_escalation(|| {
            let reached: Vec<RuntimeShutdownEscalation> = (0..4)
                .map(|_| escalate_the_machines_shutdown_for_a_delivered_signal("unit test"))
                .collect();
            assert_eq!(
                reached,
                vec![
                    RuntimeShutdownEscalation::Graceful,
                    RuntimeShutdownEscalation::Forced,
                    RuntimeShutdownEscalation::ExitAtOnce,
                    RuntimeShutdownEscalation::ExitAtOnce,
                ]
            );
        });
    }

    /// A Ctrl-C after `shutdown()` is the user's second attempt to stop, so it
    /// forces; a `shutdown()` after a Ctrl-C adds nothing.
    #[test]
    #[serial]
    fn a_signal_after_a_request_forces_and_a_request_after_a_signal_does_not() {
        with_cleared_escalation(|| {
            request_the_shutdown_of_every_loaded_stream("unit test")
                .expect("the host arm never fails");
            assert_eq!(
                escalate_the_machines_shutdown_for_a_delivered_signal("unit test"),
                RuntimeShutdownEscalation::Forced
            );
        });
        with_cleared_escalation(|| {
            escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
            request_the_shutdown_of_every_loaded_stream("unit test")
                .expect("the host arm never fails");
            assert_eq!(
                the_machines_shutdown_escalation(),
                RuntimeShutdownEscalation::Graceful
            );
        });
    }

    /// Taking the escalation is what scopes it to one run: the owner that
    /// observed it clears it, so the next run in the same process neither ends
    /// on a request already served nor starts one step from forced.
    #[test]
    #[serial]
    fn taking_the_escalation_reports_how_far_it_went_and_resets_it() {
        with_cleared_escalation(|| {
            escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
            escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
            assert_eq!(
                take_the_machines_shutdown_escalation(),
                RuntimeShutdownEscalation::Forced
            );
            assert!(!is_the_machines_shutdown_requested());
            assert_eq!(
                escalate_the_machines_shutdown_for_a_delivered_signal("unit test"),
                RuntimeShutdownEscalation::Graceful,
                "the next run's first interrupt must be graceful again",
            );
        });
    }
}
