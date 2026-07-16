// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free runtime-control requests a plugin can make of its host.
//!
//! A plugin authored against this SDK links no engine, so it holds no
//! engine `Event` / `RuntimeEvent` type and cannot publish a shutdown
//! event directly. Instead it publishes a msgpack reason string to the
//! reserved plugin-ABI control topic
//! ([`streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`])
//! through the cached `pubsub_publish` callback; the host — the only
//! party that owns `Event` — maps the request onto its internal
//! runtime-shutdown event. Only the plugin-ABI topic constant and a
//! reason string cross the boundary, never an engine type.

use crate::plugin::HostCallbacks;
use streamlib_error::{Error, Result};
use streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST;

/// Ask the host runtime to shut down (equivalent to Ctrl+C / SIGTERM).
///
/// `reason` is a human-readable attribution the host logs (empty string
/// = unspecified). Idempotent and fire-and-forget: the host maps this
/// onto its internal runtime-shutdown event, repeated calls are
/// harmless, and a call during teardown is a no-op on the host side.
///
/// Returns [`Error::PluginHostUnavailable`] when called from a cdylib
/// whose host services were never installed — i.e. code not loaded by a
/// streamlib host — which is the only real failure class.
#[tracing::instrument]
pub fn request_runtime_shutdown(reason: &str) -> Result<()> {
    let Some(callbacks) = crate::plugin::host_callbacks() else {
        return Err(Error::PluginHostUnavailable(
            "request_runtime_shutdown called in a cdylib whose host services \
             were never installed (not loaded by a streamlib host)"
                .into(),
        ));
    };

    publish_runtime_shutdown_request(callbacks, reason)
}

/// Encode `reason` and publish a runtime-shutdown request on the reserved
/// plugin-ABI control topic through the host's `pubsub_publish` callback.
///
/// Split out of [`request_runtime_shutdown`] so the load-bearing wire
/// selection — the reserved topic constant and the `rmp_serde::to_vec`
/// reason encoding the host decodes with `rmp_serde::from_slice` — is
/// driven by a hermetic test against a capturing `pubsub_publish`, without
/// installing the process-global host-services table (which is a set-once
/// `OnceLock` shared with the no-host negative test in this module).
fn publish_runtime_shutdown_request(callbacks: &HostCallbacks, reason: &str) -> Result<()> {
    let reason_msgpack = rmp_serde::to_vec(reason)
        .map_err(|e| Error::Runtime(format!("failed to encode shutdown reason: {e}")))?;

    // SAFETY: `callbacks.pubsub_publish` and `callbacks.host` were
    // populated by `install_host_services` from a host-provided
    // `HostServices` and stay valid for the plugin's process lifetime.
    // The topic and payload slices outlive the synchronous call.
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
    use streamlib_plugin_abi::{HostHandle, HostInterest, HostLogLevel, ProcessorVTable};

    /// With no host services installed (the SDK lib test binary never
    /// calls `install_host_services`), the request has no transport to
    /// reach and must surface the named `PluginHostUnavailable` error
    /// rather than silently succeeding.
    #[test]
    fn request_runtime_shutdown_without_host_returns_plugin_host_unavailable() {
        let result = request_runtime_shutdown("no host installed");
        assert!(
            matches!(result, Err(Error::PluginHostUnavailable(_))),
            "expected PluginHostUnavailable, got {result:?}",
        );
    }

    /// One captured `pubsub_publish` call: the exact topic + payload byte
    /// slices `request_runtime_shutdown` handed the host callback.
    struct CapturedRuntimeShutdownPublish {
        topic: Vec<u8>,
        payload: Vec<u8>,
    }

    // Capturing `pubsub_publish`: `host` is a `*const RefCell<Vec<...>>`
    // the test owns; copy the topic + payload bytes out and record them.
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

    // The shutdown request reads only `host` + `pubsub_publish`; these
    // stubs fill the remaining `#[repr(C)]` fn-pointer fields so a full
    // `HostCallbacks` can be built without a live host. Mirrors the
    // `host_services_with_capture` pattern in
    // `plugin/layout_skew_diagnostic.rs`.
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

    // A `HostCallbacks` whose `pubsub_publish` records into `sink` and
    // whose `host` points at it; every other slot is an unused stub or a
    // null vtable pointer.
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

    /// The SDK's wire selection is load-bearing: the host decodes the
    /// reason with `rmp_serde::from_slice(..).unwrap_or_default()` and
    /// STILL shuts down, so a drifted SDK encoding silently loses reason
    /// attribution with no failure. This pins the exact `(topic, payload)`
    /// the SDK hands the host `pubsub_publish` callback. Mental-revert:
    /// swapping the topic constant, or `to_vec` → `to_vec_named` / raw
    /// bytes / omitted encode, fails one of the asserts below.
    #[test]
    fn request_runtime_shutdown_publishes_reserved_control_topic_with_msgpack_reason() {
        let sink: RefCell<Vec<CapturedRuntimeShutdownPublish>> = RefCell::new(Vec::new());
        let callbacks = host_callbacks_with_capture(&sink);

        let result = publish_runtime_shutdown_request(&callbacks, "x");
        assert!(result.is_ok(), "publish helper must succeed, got {result:?}");

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
}
