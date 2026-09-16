// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one runtime-shutdown request funnel, and how far shutdown has escalated.
//!
//! A shutdown *request* never tears the runtime down itself: it raises the
//! escalation and publishes `Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown)`,
//! and the loop owner ([`crate::core::runtime::Runner::wait_for_signal_with`])
//! runs the normal teardown. The escalation is process-global (the signal
//! handler holds no `Runner`) and belongs to whichever run loop observes it:
//! that owner takes it once its run has ended, so a request issued while no run
//! loop is running is observed by the next one to start, and a run's interrupts
//! never escalate the next one.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: a delivered signal escalates on
//! repeat — graceful, then forced, then exit at once. A programmatic request is
//! the graceful step and only that, however often it is repeated.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::core::error::Result;
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};

/// How far the shutdown a run loop's owner observes has gone.
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

/// Raised by [`request_runtime_shutdown`] and by each delivered signal, read by
/// the run loop and the shutdown ladder. Process-global like `PUBSUB`.
static RUNTIME_SHUTDOWN_ESCALATION: AtomicU8 =
    AtomicU8::new(RuntimeShutdownEscalation::NotRequested as u8);

/// How often a loop owner re-reads the escalation. Shared so the run loop
/// ([`crate::core::runtime::Runner::wait_for_signal_with`]) and every
/// out-of-crate loop owner observe a request at the same granularity.
pub const RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Ask whoever owns the run loop to shut the runtime down gracefully (the first
/// Ctrl+C / SIGTERM). Idempotent and fire-and-forget: repeating it never forces.
///
/// `reason` is a human-readable attribution logged at `info` (empty string =
/// unspecified).
#[tracing::instrument]
pub fn request_runtime_shutdown(reason: &str) -> Result<()> {
    tracing::info!(reason, "runtime shutdown requested");
    // Raised BEFORE publishing: the loop owner polls the escalation as well as
    // the pubsub listener, so a request issued while the shutdown subscriber is
    // still being wired up is still observed.
    let _ = RUNTIME_SHUTDOWN_ESCALATION
        .fetch_max(RuntimeShutdownEscalation::Graceful as u8, Ordering::SeqCst);
    publish_the_runtime_shutdown_event();
    Ok(())
}

/// Escalate shutdown one step for a delivered signal, returning the step it
/// reached. The caller acts on [`RuntimeShutdownEscalation::ExitAtOnce`];
/// everything below it is read by the run loop and the ladder.
pub(crate) fn escalate_runtime_shutdown_for_a_delivered_signal(
    signal_name: &str,
) -> RuntimeShutdownEscalation {
    let mut reached = RuntimeShutdownEscalation::NotRequested;
    let _ =
        RUNTIME_SHUTDOWN_ESCALATION.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |stored| {
            reached = RuntimeShutdownEscalation::from_stored(stored).next_for_a_delivered_signal();
            Some(reached as u8)
        });
    match reached {
        RuntimeShutdownEscalation::NotRequested => {}
        RuntimeShutdownEscalation::Graceful => {
            tracing::info!(signal_name, "runtime shutdown requested");
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
    publish_the_runtime_shutdown_event();
    reached
}

fn publish_the_runtime_shutdown_event() {
    let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
}

/// How far shutdown has escalated.
pub fn runtime_shutdown_escalation() -> RuntimeShutdownEscalation {
    RuntimeShutdownEscalation::from_stored(RUNTIME_SHUTDOWN_ESCALATION.load(Ordering::SeqCst))
}

/// Whether any runtime shutdown has been requested.
pub fn is_runtime_shutdown_requested() -> bool {
    runtime_shutdown_escalation() >= RuntimeShutdownEscalation::Graceful
}

/// Whether shutdown has been forced by a second interrupt.
pub fn is_runtime_shutdown_forced() -> bool {
    runtime_shutdown_escalation() >= RuntimeShutdownEscalation::Forced
}

/// Clear the escalation, returning how far it had gone.
///
/// Only whoever owns a run loop may call it, once its run has ended, so the
/// requests it observed neither end nor escalate the next run in the same
/// process.
pub fn take_runtime_shutdown_escalation() -> RuntimeShutdownEscalation {
    RuntimeShutdownEscalation::from_stored(RUNTIME_SHUTDOWN_ESCALATION.swap(
        RuntimeShutdownEscalation::NotRequested as u8,
        Ordering::SeqCst,
    ))
}

/// Clears the escalation on construction and again on drop, so a `#[serial]`
/// test that touches the process-global escalation leaves it clean even when an
/// assertion unwinds past its own cleanup.
#[cfg(test)]
pub(crate) struct RuntimeShutdownEscalationClearedOnDrop;

#[cfg(test)]
impl RuntimeShutdownEscalationClearedOnDrop {
    pub(crate) fn clear_now_and_on_drop() -> Self {
        take_runtime_shutdown_escalation();
        Self
    }
}

#[cfg(test)]
impl Drop for RuntimeShutdownEscalationClearedOnDrop {
    fn drop(&mut self) {
        take_runtime_shutdown_escalation();
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
            RuntimeShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
        body();
    }

    /// A request is observed, and a loop owner that polls
    /// `is_runtime_shutdown_requested` observes it even when the pubsub
    /// listener missed the event.
    #[test]
    #[serial]
    fn a_request_is_observed_as_a_graceful_shutdown() {
        with_cleared_escalation(|| {
            assert!(
                !is_runtime_shutdown_requested(),
                "the escalation must start clear"
            );
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            assert_eq!(
                runtime_shutdown_escalation(),
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
                request_runtime_shutdown(&format!("unit test {attempt}"))
                    .expect("a repeated request is not an error");
            }
            assert_eq!(
                runtime_shutdown_escalation(),
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
                .map(|_| escalate_runtime_shutdown_for_a_delivered_signal("unit test"))
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
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            assert_eq!(
                escalate_runtime_shutdown_for_a_delivered_signal("unit test"),
                RuntimeShutdownEscalation::Forced
            );
        });
        with_cleared_escalation(|| {
            escalate_runtime_shutdown_for_a_delivered_signal("unit test");
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            assert_eq!(
                runtime_shutdown_escalation(),
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
            escalate_runtime_shutdown_for_a_delivered_signal("unit test");
            escalate_runtime_shutdown_for_a_delivered_signal("unit test");
            assert_eq!(
                take_runtime_shutdown_escalation(),
                RuntimeShutdownEscalation::Forced
            );
            assert!(!is_runtime_shutdown_requested());
            assert_eq!(
                escalate_runtime_shutdown_for_a_delivered_signal("unit test"),
                RuntimeShutdownEscalation::Graceful,
                "the next run's first interrupt must be graceful again",
            );
        });
    }
}
