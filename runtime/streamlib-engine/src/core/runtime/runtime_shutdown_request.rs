// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one runtime-shutdown request funnel, plus the latch whoever owns the
//! run loop observes.
//!
//! A shutdown *request* never tears the runtime down itself. It latches the
//! request and publishes `Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown)`;
//! the loop owner ([`crate::core::runtime::Runner::wait_for_signal_with`])
//! observes it and runs the normal teardown. That separation is what keeps the
//! harness in control — a request is never a second way to kill the runtime.
//!
//! Every boundary that can ask for shutdown funnels through
//! [`request_runtime_shutdown`]: the POSIX signal handler, the
//! [`RuntimeOperations`](crate::core::runtime::RuntimeOperations) verb, the
//! api-server control-plane route, and an engine-free plugin publishing the
//! reserved plugin-ABI control topic. One funnel means the `Event` encoding
//! and the latch exist in exactly one place.

use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST;

use crate::core::error::{Error, Result};
use crate::core::plugin::host_services::HostCallbacks;
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};

/// Latched by the host arm of [`request_runtime_shutdown`], read by the run
/// loop. A latch and not a counter, so "requesting twice is not an error"
/// holds by construction. Process-global like `PUBSUB` — and, like `PUBSUB`,
/// a facade cdylib statically links its own copy of the engine and therefore
/// its own copy of this static, which is precisely why the cdylib arm of the
/// funnel publishes across the plugin ABI instead of storing here.
static RUNTIME_SHUTDOWN_REQUEST_LATCH: AtomicBool = AtomicBool::new(false);

/// Ask whoever owns the run loop to shut the runtime down (equivalent to
/// Ctrl+C / SIGTERM). Idempotent and fire-and-forget.
///
/// `reason` is a human-readable attribution logged at `info` for
/// shutdown-attribution (empty string = unspecified).
///
/// In a plugin cdylib whose `install_host_services` has run, this publishes
/// the request to the host across the plugin ABI and returns; the host's own
/// funnel does the latching. Failure is limited to that cdylib arm's msgpack
/// encode of `reason`.
#[tracing::instrument]
pub fn request_runtime_shutdown(reason: &str) -> Result<()> {
    if let Some(callbacks) = crate::core::plugin::host_services::host_callbacks() {
        return publish_runtime_shutdown_request_through_host_callbacks(callbacks, reason);
    }

    tracing::info!(reason, "runtime shutdown requested");
    // Latch BEFORE publishing: the loop owner polls the latch as well as the
    // pubsub listener, so a request issued while the shutdown subscriber is
    // still being wired up is still observed.
    RUNTIME_SHUTDOWN_REQUEST_LATCH.store(true, Ordering::SeqCst);
    let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
    Ok(())
}

/// Whether a runtime-shutdown request has been latched since the last
/// [`clear_runtime_shutdown_request_latch`].
pub fn is_runtime_shutdown_requested() -> bool {
    RUNTIME_SHUTDOWN_REQUEST_LATCH.load(Ordering::SeqCst)
}

/// Clear the latch so a subsequent run loop starts unrequested.
///
/// `Runner::start` calls this at entry: a second in-process run must not be
/// poisoned by the first run's request, and a request issued before any loop
/// exists has no owner to observe it.
pub fn clear_runtime_shutdown_request_latch() {
    RUNTIME_SHUTDOWN_REQUEST_LATCH.store(false, Ordering::SeqCst);
}

/// Publish a shutdown request to the host on the reserved plugin-ABI control
/// topic ([`PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`]) through the
/// cached `pubsub_publish` callback.
///
/// The payload is `rmp_serde::to_vec(reason)` — a bare msgpack UTF-8 string,
/// NOT an `Event` msgpack: the reserved control topics are matched by the host
/// before its general `Event` decode and carry the per-topic payload defined
/// next to the topic constant.
///
/// Split out with an explicit `&HostCallbacks` so the load-bearing wire
/// selection is driven by a hermetic test against a capturing `pubsub_publish`
/// without installing the process-global, set-once host-callback table.
fn publish_runtime_shutdown_request_through_host_callbacks(
    callbacks: &HostCallbacks,
    reason: &str,
) -> Result<()> {
    let reason_msgpack = rmp_serde::to_vec(reason).map_err(|e| {
        Error::Runtime(format!(
            "failed to encode runtime-shutdown reason for the plugin-ABI control topic: {e}"
        ))
    })?;

    // SAFETY: `callbacks.pubsub_publish` and `callbacks.host` were populated by
    // `install_host_services` from a host-provided `HostServices` and stay
    // valid for the plugin's process lifetime. The topic and payload slices
    // outlive the synchronous call.
    unsafe {
        (callbacks.pubsub_publish)(
            callbacks.host,
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST.as_ptr(),
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST.len(),
            reason_msgpack.as_ptr(),
            reason_msgpack.len(),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use core::ffi::c_void;
    use serial_test::serial;
    use streamlib_plugin_abi::{HostHandle, HostInterest, HostLogLevel, ProcessorVTable};

    /// The latch is process-global, so every test that touches it runs
    /// `#[serial]` and leaves it cleared for the next one.
    fn with_cleared_latch<F: FnOnce()>(body: F) {
        clear_runtime_shutdown_request_latch();
        body();
        clear_runtime_shutdown_request_latch();
    }

    /// The host arm latches, so a loop owner that polls
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

    /// `Runner::start` calls this at entry so a second in-process run is not
    /// poisoned by the first run's request.
    #[test]
    #[serial]
    fn clearing_the_latch_unrequests_shutdown() {
        with_cleared_latch(|| {
            request_runtime_shutdown("unit test").expect("the host arm never fails");
            clear_runtime_shutdown_request_latch();
            assert!(
                !is_runtime_shutdown_requested(),
                "the cleared latch must read unrequested",
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

    // The shutdown request reads only `host` + `pubsub_publish`; these stubs
    // fill the remaining fn-pointer fields so a full `HostCallbacks` can be
    // built without a live host.
    unsafe extern "C" fn unused_tracing_register_callsite(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
    ) -> HostInterest {
        HostInterest::Never
    }
    unsafe extern "C" fn unused_tracing_enabled(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
    ) -> bool {
        false
    }
    unsafe extern "C" fn unused_tracing_emit(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_schema_register(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_schema_lookup(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: extern "C" fn(*mut c_void, *const u8, usize),
        _: *mut c_void,
    ) {
    }
    unsafe extern "C" fn unused_iceoryx_log_emit(
        _: HostHandle,
        _: HostLogLevel,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_processor_register(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: *const ProcessorVTable,
    ) -> i32 {
        0
    }

    /// A `HostCallbacks` whose `pubsub_publish` records into `sink` and whose
    /// `host` points at it; every other slot is an unused stub or a null
    /// vtable pointer.
    fn host_callbacks_with_capture(
        sink: &RefCell<Vec<CapturedRuntimeShutdownPublish>>,
    ) -> HostCallbacks {
        HostCallbacks {
            host: sink as *const RefCell<Vec<CapturedRuntimeShutdownPublish>> as HostHandle,
            tracing_register_callsite: unused_tracing_register_callsite,
            tracing_enabled: unused_tracing_enabled,
            tracing_emit: unused_tracing_emit,
            pubsub_publish: capturing_pubsub_publish,
            schema_register: unused_schema_register,
            schema_lookup: unused_schema_lookup,
            iceoryx_log_emit: unused_iceoryx_log_emit,
            processor_register: unused_processor_register,
            runtime_context_vtable: core::ptr::null(),
            audio_clock_vtable: core::ptr::null(),
            runtime_ops_vtable: core::ptr::null(),
            gpu_context_limited_access_vtable: core::ptr::null(),
            surface_store_vtable: core::ptr::null(),
            gpu_context_full_access_vtable: core::ptr::null(),
            texture_ring_methods_vtable: core::ptr::null(),
            vulkan_compute_kernel_methods_vtable: core::ptr::null(),
            vulkan_graphics_kernel_methods_vtable: core::ptr::null(),
            vulkan_ray_tracing_kernel_methods_vtable: core::ptr::null(),
            vulkan_acceleration_structure_methods_vtable: core::ptr::null(),
            rhi_color_converter_methods_vtable: core::ptr::null(),
            rhi_command_recorder_methods_vtable: core::ptr::null(),
            output_writer_vtable: core::ptr::null(),
            input_mailboxes_vtable: core::ptr::null(),
            present_target_methods_vtable: core::ptr::null(),
            video_encoder_session_methods_vtable: core::ptr::null(),
            video_decoder_session_methods_vtable: core::ptr::null(),
            host_timeline_semaphore_methods_vtable: core::ptr::null(),
            vulkan_texture_readback_methods_vtable: core::ptr::null(),
        }
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

        publish_runtime_shutdown_request_through_host_callbacks(&callbacks, "x")
            .expect("the publish helper must succeed");

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
    /// The cdylib arm must publish ONLY. Mental-revert: latching before the
    /// cdylib short-circuit fails this.
    #[test]
    #[serial]
    fn cdylib_arm_does_not_set_the_engine_local_latch() {
        with_cleared_latch(|| {
            let sink: RefCell<Vec<CapturedRuntimeShutdownPublish>> = RefCell::new(Vec::new());
            let callbacks = host_callbacks_with_capture(&sink);

            publish_runtime_shutdown_request_through_host_callbacks(&callbacks, "from a cdylib")
                .expect("the publish helper must succeed");

            assert_eq!(sink.borrow().len(), 1, "the request must reach the host");
            assert!(
                !is_runtime_shutdown_requested(),
                "the cdylib arm must NOT latch in the plugin image's copy of the engine",
            );
        });
    }
}
