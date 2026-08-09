// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one runtime-shutdown request funnel, plus the latch whoever owns the
//! run loop observes.
//!
//! A shutdown *request* never tears the runtime down itself: it latches and
//! publishes `Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown)`, and the
//! loop owner ([`crate::core::runtime::Runner::wait_for_signal_with`]) runs the
//! normal teardown. The latch is process-global (the signal handler and the
//! plugin ABI hold no `Runner`) and first-observer-wins: whichever loop owner
//! reads it takes it, so a request issued while no run loop is running is
//! observed by the next one to start.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;


use crate::core::error::{Error, Result};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};

/// Latched by [`request_runtime_shutdown`], read by the run
/// loop. A latch and not a counter, so "requesting twice is not an error"
/// holds by construction. Process-global like `PUBSUB`.
static RUNTIME_SHUTDOWN_REQUEST_LATCH: AtomicBool = AtomicBool::new(false);

/// Fired at most once per image, so the wrong-image diagnostic below stays
/// fail-loud without becoming a log firehose: the predicate it guards is meant
/// to be polled, and inside a facade cdylib every emit is a plugin-ABI hop.

/// How often a loop owner re-reads the latch. Shared so the run loop
/// ([`crate::core::runtime::Runner::wait_for_signal_with`]) and every
/// out-of-crate loop owner observe a request at the same granularity.
pub const RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Ask whoever owns the run loop to shut the runtime down (equivalent to
/// Ctrl+C / SIGTERM). Idempotent and fire-and-forget.
///
/// `reason` is a human-readable attribution logged at `info` (empty string =
/// unspecified).
#[tracing::instrument]
pub fn request_runtime_shutdown(reason: &str) -> Result<()> {
    tracing::info!(reason, "runtime shutdown requested");
    // Latch BEFORE publishing: the loop owner polls the latch as well as the
    // pubsub listener, so a request issued while the shutdown subscriber is
    // still being wired up is still observed.
    RUNTIME_SHUTDOWN_REQUEST_LATCH.store(true, Ordering::SeqCst);
    let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
    Ok(())
}

/// Whether a runtime-shutdown request is latched.
pub fn is_runtime_shutdown_requested() -> bool {
    RUNTIME_SHUTDOWN_REQUEST_LATCH.load(Ordering::SeqCst)
}

/// Clear the latch, returning whether a request was pending. Host-only, for the
/// same reason as [`is_runtime_shutdown_requested`].
///
/// Taking the latch is what makes it first-observer-wins, so only whoever owns
/// a run loop may call it — once its loop has ended, so the request it just
/// observed does not end the next loop in the same process too.
pub fn take_runtime_shutdown_request_latch() -> bool {
    RUNTIME_SHUTDOWN_REQUEST_LATCH.swap(false, Ordering::SeqCst)
}

/// Clears the latch on construction and again on drop, so a `#[serial]` test
/// that touches the process-global latch leaves it clean even when an assertion
/// unwinds past its own cleanup.
#[cfg(test)]
pub(crate) struct RuntimeShutdownRequestLatchClearedOnDrop;

#[cfg(test)]
impl RuntimeShutdownRequestLatchClearedOnDrop {
    pub(crate) fn clear_now_and_on_drop() -> Self {
        take_runtime_shutdown_request_latch();
        Self
    }
}

#[cfg(test)]
impl Drop for RuntimeShutdownRequestLatchClearedOnDrop {
    fn drop(&mut self) {
        take_runtime_shutdown_request_latch();
    }
}

/// Publish a shutdown request to the host on the reserved plugin-ABI control
/// topic ([`PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`]) through the
/// cached `pubsub_publish` callback.
///
/// The payload is `rmp_serde::to_vec(reason)` — a bare msgpack UTF-8 string,
/// NOT an `Event` msgpack: the reserved control topics are matched by the host
/// before its general `Event` decode and carry the per-topic payload defined
/// next to the topic constant.
// twin-guard(runtime-shutdown-publish): BEGIN
// twin-guard(runtime-shutdown-publish): END

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// The latch is process-global, so every test that touches it runs
    /// `#[serial]` and leaves it cleared for the next one — including when an
    /// assertion inside `body` unwinds.
    fn with_cleared_latch<F: FnOnce()>(body: F) {
        let _latch_cleared_even_on_unwind =
            RuntimeShutdownRequestLatchClearedOnDrop::clear_now_and_on_drop();
        body();
    }

    /// The host binary's `host_callbacks()` is `None`, so this drives the host
    /// arm: it latches, and a loop owner that polls
    /// `is_runtime_shutdown_requested` observes the request even when the
    /// pubsub listener missed the event.
    #[test]
    #[serial]
    fn host_arm_latches_the_request() {
        with_cleared_latch(|| {
            assert!(
                !is_runtime_shutdown_requested(),
                "the latch must start clear"
            );
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            assert!(
                is_runtime_shutdown_requested(),
                "the host arm must latch the request",
            );
        });
    }

    /// "Requesting shutdown twice is not an error" — a latch, never a counter.
    #[test]
    #[serial]
    fn repeated_requests_are_idempotent() {
        with_cleared_latch(|| {
            for attempt in 0..3 {
                request_runtime_shutdown(&format!("unit test {attempt}"))
                    .expect("a repeated request is not an error");
                assert!(
                    is_runtime_shutdown_requested(),
                    "the latch stays set across repeats",
                );
            }
        });
    }

    /// Taking the latch is what makes it first-observer-wins: the loop owner
    /// that observed the request clears it, so the next loop in the same
    /// process does not exit on a request that was already served.
    #[test]
    #[serial]
    fn taking_the_latch_reports_the_pending_request_and_unrequests_shutdown() {
        with_cleared_latch(|| {
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            assert!(
                take_runtime_shutdown_request_latch(),
                "taking a set latch must report the pending request",
            );
            assert!(
                !is_runtime_shutdown_requested(),
                "the taken latch must read unrequested",
            );
            assert!(
                !take_runtime_shutdown_request_latch(),
                "taking a clear latch must report no pending request",
            );
        });
    }

}
