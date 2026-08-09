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
    use crate::core::plugin::host_services::host_services_test_support::host_callbacks_with_capturing_pubsub_publish;
    use core::cell::RefCell;
    use serial_test::serial;
    use streamlib_plugin_abi::HostHandle;

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

    /// One captured `pubsub_publish` call: the exact topic + payload byte
    /// slices the cdylib arm handed the host callback.
    struct CapturedRuntimeShutdownPublish {
        topic: Vec<u8>,
        payload: Vec<u8>,
    }

    // Capturing `pubsub_publish`: `host` is a `*const RefCell<Vec<...>>` the
    // test owns; copy the topic + payload bytes out and record them.
    unsafe extern "C" fn capturing_pubsub_publish(
        host: HostHandle,
        topic_ptr: *const u8,
        topic_len: usize,
        payload_ptr: *const u8,
        payload_len: usize,
    ) {
        let sink = unsafe { &*(host as *const RefCell<Vec<CapturedRuntimeShutdownPublish>>) };
        let topic = unsafe { core::slice::from_raw_parts(topic_ptr, topic_len) }.to_vec();
        let payload = unsafe { core::slice::from_raw_parts(payload_ptr, payload_len) }.to_vec();
        sink.borrow_mut()
            .push(CapturedRuntimeShutdownPublish { topic, payload });
    }

    /// A `HostCallbacks` whose `pubsub_publish` records into `sink` and whose
    /// `host` points at it; every other slot is an unused stub or a null
    /// vtable pointer.
    fn host_callbacks_with_capture(
        sink: &RefCell<Vec<CapturedRuntimeShutdownPublish>>,
    ) -> HostCallbacks {
        host_callbacks_with_capturing_pubsub_publish(
            sink as *const RefCell<Vec<CapturedRuntimeShutdownPublish>> as HostHandle,
            capturing_pubsub_publish,
        )
    }

    /// The cdylib arm's wire selection is load-bearing: the host decodes the
    /// reason with `rmp_serde::from_slice(..).unwrap_or_default()` and STILL
    /// shuts down, so a drifted encoding silently loses reason attribution
    /// with no failure. This pins the exact `(topic, payload)` handed to the
    /// host's `pubsub_publish`. Mental-revert: swapping the topic constant, or
    /// `to_vec` → `to_vec_named` / the general `Event` msgpack encode, fails
    /// one of the asserts below.
    #[test]
    fn cdylib_arm_publishes_the_reserved_control_topic_with_a_msgpack_reason() {
        let sink: RefCell<Vec<CapturedRuntimeShutdownPublish>> = RefCell::new(Vec::new());
        let callbacks = host_callbacks_with_capture(&sink);

        publish_runtime_shutdown_request(&callbacks, "x").expect("the publish helper must succeed");

        let captured = sink.borrow();
        assert_eq!(captured.len(), 1, "exactly one pubsub_publish call");
        let call = &captured[0];
        assert_eq!(
            call.topic,
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST.as_bytes(),
            "topic must be EXACTLY the reserved runtime-shutdown control topic",
        );
        assert_eq!(
            call.payload,
            rmp_serde::to_vec("x").expect("encode reason"),
            "payload must be the msgpack reason the host decodes with rmp_serde::from_slice",
        );
    }

    /// A facade cdylib statically links the engine, so this latch exists twice
    /// (host image + plugin image). Latching in the plugin's copy would be
    /// invisible to the host's run loop: nothing stops, no error, no panic.
    /// The cdylib arm must publish ONLY.
    ///
    /// This drives the funnel's arm SELECTION, not the publish helper, because
    /// the ordering between the latch store and the cdylib short-circuit is
    /// the thing that breaks. Mental-revert: hoisting
    /// `RUNTIME_SHUTDOWN_REQUEST_LATCH.store(true, ..)` above the `if let Some`
    /// fails this.
    #[test]
    #[serial]
    fn cdylib_arm_does_not_set_the_engine_local_latch() {
        with_cleared_latch(|| {
            let sink: RefCell<Vec<CapturedRuntimeShutdownPublish>> = RefCell::new(Vec::new());
            let callbacks = host_callbacks_with_capture(&sink);

            request_runtime_shutdown_with_installed_host_callbacks(
                Some(&callbacks),
                "from a cdylib",
            )
            .expect("the cdylib arm must succeed");

            assert_eq!(sink.borrow().len(), 1, "the request must reach the host");
            assert!(
                !is_runtime_shutdown_requested(),
                "the cdylib arm must NOT latch in the plugin image's copy of the engine",
            );
        });
    }

    /// The documented host-only semantic of the read side, composed with the
    /// cdylib arm: a plugin image that requests shutdown can never see its own
    /// request, because the request went to the host and the latch it reads is
    /// its own. Mental-revert: making the cdylib arm latch as well as publish
    /// fails this.
    #[test]
    #[serial]
    fn a_plugin_image_never_observes_its_own_shutdown_request() {
        with_cleared_latch(|| {
            let sink: RefCell<Vec<CapturedRuntimeShutdownPublish>> = RefCell::new(Vec::new());
            let callbacks = host_callbacks_with_capture(&sink);

            request_runtime_shutdown_with_installed_host_callbacks(
                Some(&callbacks),
                "from a cdylib",
            )
            .expect("the cdylib arm must succeed");

            assert!(
                !is_runtime_shutdown_requested_with_installed_host_callbacks(Some(&callbacks)),
                "a plugin image's own latch stays clear — the request is the host's to observe",
            );
        });
    }
}
