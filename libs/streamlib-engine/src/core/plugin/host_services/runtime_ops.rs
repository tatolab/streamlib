// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `RuntimeOpsVTable` callbacks + static vtable + accessor.
//!
//! The cdylib-side `RuntimeOpsShim` wraps each submit-with-completion
//! callback in a `tokio::sync::oneshot` whose Sender is boxed and
//! shipped across the FFI as the `user_data` pointer. The host's
//! callback impl spawns on the host's tokio runtime (held in
//! [`HOST_RUNTIME_TOKIO_HANDLE`]), awaits the real
//! `RuntimeOperations::*_async` method, encodes the response payload,
//! and fires the completion callback.

use std::ffi::c_void;
use std::sync::{Arc, OnceLock};

use streamlib_plugin_abi::{RuntimeOpsVTable, RUNTIME_OPS_VTABLE_LAYOUT_VERSION};

use crate::core::runtime::RuntimeOperations;

use super::host_callbacks;
use super::run_host_extern_c;

/// Set by the host once at startup before any cdylib registers. The
/// runtime-ops vtable's callbacks block on this handle to run the
/// real `*_async` methods on the host's tokio runtime, completely
/// invisible to the cdylib (which sees only a `oneshot` it polls on
/// its own runtime).
static HOST_RUNTIME_TOKIO_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Install the host's tokio handle so the [`HOST_RUNTIME_OPS_VTABLE`]
/// callbacks can spawn `*_async` futures against it. The host's
/// `Runner::start` calls this once before any cdylib is loaded.
/// Idempotent: subsequent calls with a different handle are silently
/// ignored.
pub fn install_host_runtime_tokio_handle(handle: tokio::runtime::Handle) {
    let _ = HOST_RUNTIME_TOKIO_HANDLE.set(handle);
}

fn host_tokio_handle() -> Option<&'static tokio::runtime::Handle> {
    HOST_RUNTIME_TOKIO_HANDLE.get()
}

unsafe fn invoke_completion(
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
    status: i32,
    bytes: &[u8],
) {
    // SAFETY: cdylib promises completion is safe to invoke with the
    // user_data pointer; payload bytes are valid for the call.
    unsafe { completion(user_data, status, bytes.as_ptr(), bytes.len()) };
}

/// RAII guard around the cdylib's submit-with-completion contract.
/// The ABI promises the host fires `completion(user_data, ...)`
/// exactly once. Without this guard a panic inside the spawned
/// `async` body (or a runtime shutdown that drops the future before
/// it awaits) would leak the cdylib's boxed `oneshot::Sender` and
/// hang the cdylib's `rx.await` forever. With the guard, the Drop
/// impl fires an aborted-task error completion if the explicit fire
/// path didn't run.
///
/// Holds `user_data` as a `usize` so the guard is `Send + Sync` (raw
/// pointers aren't). The completion fn pointer is naturally Send.
struct CompletionGuard {
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data_addr: usize,
    fired: bool,
}

impl CompletionGuard {
    fn new(
        completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ) -> Self {
        Self {
            completion,
            user_data_addr: user_data as usize,
            fired: false,
        }
    }

    fn fire_with_result<T: serde::Serialize>(mut self, result: crate::core::Result<T>) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        match result {
            Ok(value) => match rmp_serde::to_vec_named(&value) {
                Ok(bytes) => unsafe {
                    invoke_completion(self.completion, user_data_ptr, 0, &bytes)
                },
                Err(e) => {
                    let msg = format!("response msgpack encode failed: {e}");
                    unsafe {
                        invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes())
                    };
                }
            },
            Err(e) => {
                let msg = e.to_string();
                unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes()) };
            }
        }
    }

    fn fire_err_msg(mut self, msg: &[u8]) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg) };
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.fired {
            // SAFETY: contract promise — completion is always fired
            // exactly once. A drop without a fire signals the host's
            // tokio task aborted (panic or runtime shutdown before
            // the future completed). The cdylib's completion
            // trampoline reclaims its boxed `Sender` either way.
            let user_data_ptr = self.user_data_addr as *mut c_void;
            let msg = b"runtime-ops host task aborted before completion";
            unsafe {
                invoke_completion(self.completion, user_data_ptr, -1, msg);
            }
        }
    }
}

// SAFETY: completion fn pointer is naturally Send; user_data is held
// as a `usize` so the guard can cross `.await` boundaries inside
// tokio task bodies.
unsafe impl Send for CompletionGuard {}
unsafe impl Sync for CompletionGuard {}

unsafe extern "C" fn host_rov_add_processor(
    handle: *const c_void,
    spec_msgpack_ptr: *const u8,
    spec_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_add_processor",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"add_processor: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let spec_bytes = if spec_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(spec_msgpack_ptr, spec_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::processors::ProcessorSpec>(
                    &spec_bytes,
                ) {
                    Ok(spec) => ops.add_processor_async(spec).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "add_processor: spec msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        // Sync-body panic: CompletionGuard's Drop fires the abort
        // completion if `guard` was constructed before the panic;
        // otherwise the cdylib's `rx.await` hangs. The cdylib's
        // RAII-on-Drop trampoline reclaims its boxed Sender either
        // way.
        (),
    )
}

unsafe extern "C" fn host_rov_remove_processor(
    handle: *const c_void,
    processor_id_msgpack_ptr: *const u8,
    processor_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_remove_processor",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"remove_processor: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let id_bytes = if processor_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe {
                    std::slice::from_raw_parts(processor_id_msgpack_ptr, processor_id_msgpack_len)
                }
                .to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::graph::ProcessorUniqueId>(
                    &id_bytes,
                ) {
                    Ok(pid) => ops.remove_processor_async(pid).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "remove_processor: processor_id msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_connect(
    handle: *const c_void,
    from_msgpack_ptr: *const u8,
    from_msgpack_len: usize,
    to_msgpack_ptr: *const u8,
    to_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_connect",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"connect: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let from_bytes = if from_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(from_msgpack_ptr, from_msgpack_len) }.to_vec()
            };
            let to_bytes = if to_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(to_msgpack_ptr, to_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let from: crate::core::OutputLinkPortRef =
                    match rmp_serde::from_slice(&from_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                                Err(crate::core::Error::Config(format!(
                                    "connect: from-port msgpack decode failed: {e}"
                                )));
                            guard.fire_with_result(result);
                            return;
                        }
                    };
                let to: crate::core::InputLinkPortRef = match rmp_serde::from_slice(&to_bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                            Err(crate::core::Error::Config(format!(
                                "connect: to-port msgpack decode failed: {e}"
                            )));
                        guard.fire_with_result(result);
                        return;
                    }
                };
                let result = ops.connect_async(from, to).await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_disconnect(
    handle: *const c_void,
    link_id_msgpack_ptr: *const u8,
    link_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_disconnect",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"disconnect: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let bytes = if link_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(link_id_msgpack_ptr, link_id_msgpack_len) }
                    .to_vec()
            };
            rt.spawn(async move {
                let result =
                    match rmp_serde::from_slice::<crate::core::graph::LinkUniqueId>(&bytes) {
                        Ok(link_id) => ops.disconnect_async(link_id).await,
                        Err(e) => Err(crate::core::Error::Config(format!(
                            "disconnect: link_id msgpack decode failed: {e}"
                        ))),
                    };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_to_json(
    handle: *const c_void,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_to_json",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"to_json: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            rt.spawn(async move {
                let result = ops.to_json_async().await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

/// Take a (borrowed) handle returned from
/// `RuntimeContextVTable::runtime_ops_handle` (a `*const Arc<dyn
/// RuntimeOperations>` pointing into `RuntimeContext`-owned storage)
/// and return a new owned handle: a `Box<Arc<dyn RuntimeOperations>>`
/// with an Arc refcount bump. The owned handle stays alive even if
/// the originating `RuntimeContext` is dropped, because the inner Arc
/// keeps the underlying `dyn RuntimeOperations` impl alive
/// independently. Cdylib drops it via [`host_rov_drop_handle`].
unsafe extern "C" fn host_rov_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rov_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` came from `host_rcv_runtime_ops_handle`
            // which cast `&RuntimeContext.runtime_ops` to `*const c_void`.
            let original = unsafe { &*(borrowed_handle as *const Arc<dyn RuntimeOperations>) };
            let cloned: Arc<dyn RuntimeOperations> = Arc::clone(original);
            Box::into_raw(Box::new(cloned)) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rov_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_rov_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `host_rov_clone_handle`'s `Box::into_raw`.
            unsafe {
                let _ = Box::from_raw(owned_handle as *mut Arc<dyn RuntimeOperations>);
            }
        },
        (),
    )
}

/// Static [`RuntimeOpsVTable`] installed once per process. Paired
/// with the per-RuntimeContext runtime-ops handle returned by
/// `HOST_RUNTIME_CONTEXT_VTABLE::runtime_ops_handle`.
pub static HOST_RUNTIME_OPS_VTABLE: RuntimeOpsVTable = RuntimeOpsVTable {
    layout_version: RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    add_processor: host_rov_add_processor,
    remove_processor: host_rov_remove_processor,
    connect: host_rov_connect,
    disconnect: host_rov_disconnect,
    to_json: host_rov_to_json,
    clone_handle: host_rov_clone_handle,
    drop_handle: host_rov_drop_handle,
};

/// Pointer to the [`RuntimeOpsVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`super::host_runtime_context_vtable`].
pub fn host_runtime_ops_vtable() -> *const RuntimeOpsVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_ops_vtable.is_null() => c.runtime_ops_vtable,
        _ => &HOST_RUNTIME_OPS_VTABLE,
    }
}

#[cfg(test)]
mod runtime_ops_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `RuntimeOpsVTable` callbacks. Each callback is
    //! submit-with-completion (void return + completion callback):
    //! the contract is that completion fires exactly once. Null
    //! handle must fire the completion with `status = -1` and an
    //! error message identifying the offending op — mental-revert
    //! removes the guard, the wrapper SIGSEGVs through
    //! `&*(null as *const Arc<dyn RuntimeOperations>)`.
    //!
    //! Each test installs a tiny completion that pushes
    //! `(status, message)` into a shared queue; the assertion
    //! confirms a single error completion fired with the expected
    //! per-op marker.

    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    struct CompletionSink {
        events: Mutex<Vec<(i32, Vec<u8>)>>,
    }

    impl CompletionSink {
        fn new() -> StdArc<Self> {
            StdArc::new(Self { events: Mutex::new(Vec::new()) })
        }
    }

    unsafe extern "C" fn record_completion(
        user_data: *mut c_void,
        status: i32,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let sink_arc = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
        let payload = if result_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result_ptr, result_len) }.to_vec()
        };
        sink_arc.events.lock().expect("poisoned").push((status, payload));
        // Re-leak so the host's CompletionGuard's Drop (if it fires
        // again — it shouldn't, but defensive) can still find it.
        // In practice the guard's `fire_err_msg` consumes via `mut`,
        // so this re-leak is just paranoia matching the cdylib's
        // RAII-trampoline shape.
        let _ = StdArc::into_raw(sink_arc);
    }

    fn install_sink_user_data() -> (*mut c_void, StdArc<CompletionSink>) {
        let sink = CompletionSink::new();
        let user_data = StdArc::into_raw(StdArc::clone(&sink)) as *mut c_void;
        (user_data, sink)
    }

    fn assert_single_err_completion(sink: &CompletionSink, expected_marker: &str) {
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "expected exactly one completion fire");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1, "null-handle must produce err status");
        let msg = std::str::from_utf8(payload).expect("UTF-8");
        assert!(
            msg.contains(expected_marker),
            "expected marker `{expected_marker}` in msg: {msg}"
        );
    }

    /// After each test the test's CompletionSink Arc still holds one
    /// extra refcount (the original `StdArc::into_raw` we passed as
    /// user_data, never reclaimed by the host on the null-handle
    /// path). Reclaim it explicitly so the sink doesn't leak.
    unsafe fn reclaim_sink(user_data: *mut c_void) {
        let _ = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
    }

    #[test]
    fn add_processor_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.add_processor)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "add_processor: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn remove_processor_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.remove_processor)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "remove_processor: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn connect_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.connect)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "connect: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn disconnect_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.disconnect)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "disconnect: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn to_json_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.to_json)(
                std::ptr::null(),
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "to_json: null handle");
        unsafe { reclaim_sink(user_data) };
    }
}

#[cfg(test)]
mod runtime_ops_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_RUNTIME_OPS_VTABLE`].
    //!
    //! Per-callback null-handle coverage for the 5
    //! submit-with-completion ops (`add_processor`,
    //! `remove_processor`, `connect`, `disconnect`, `to_json`)
    //! lives in [`runtime_ops_vtable_null_handle_guards`] above.
    //! This module adds:
    //!
    //! - `layout_version_matches_constant` — locks the v2 layout
    //!   version against the cdylib-visible constant.
    //! - `clone_handle` / `drop_handle` null-handle coverage — the
    //!   v2 Arc-lifecycle pair already had explicit guards
    //!   (`if owned_handle.is_null() { return; }`); we test that the
    //!   contract holds.
    //! - `CompletionGuard` fire-exactly-once contract — the host-
    //!   side RAII guard around the cdylib's "completion fires
    //!   exactly once" promise. Two cases:
    //!     - Drop without fire → abort completion fires with
    //!       `status = -1` and the documented aborted-task message.
    //!     - `fire_err_msg` then drop → completion fires once, Drop
    //!       does NOT fire a second time.

    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RUNTIME_OPS_VTABLE.layout_version,
            streamlib_plugin_abi::RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn clone_handle_returns_null_on_null_borrowed() {
        let out = unsafe {
            (HOST_RUNTIME_OPS_VTABLE.clone_handle)(std::ptr::null())
        };
        assert!(out.is_null());
    }

    #[test]
    fn drop_handle_handles_null_owned_no_crash() {
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    // ------------------------------------------------------------------
    // CompletionGuard fire-exactly-once contract
    // ------------------------------------------------------------------

    struct CompletionSink {
        events: Mutex<Vec<(i32, Vec<u8>)>>,
    }

    impl CompletionSink {
        fn new() -> StdArc<Self> {
            StdArc::new(Self { events: Mutex::new(Vec::new()) })
        }
    }

    unsafe extern "C" fn record_completion(
        user_data: *mut c_void,
        status: i32,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let sink_arc = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
        let payload = if result_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result_ptr, result_len) }.to_vec()
        };
        sink_arc.events.lock().expect("poisoned").push((status, payload));
        let _ = StdArc::into_raw(sink_arc);
    }

    fn install_sink_user_data() -> (*mut c_void, StdArc<CompletionSink>) {
        let sink = CompletionSink::new();
        let user_data = StdArc::into_raw(StdArc::clone(&sink)) as *mut c_void;
        (user_data, sink)
    }

    unsafe fn reclaim_sink(user_data: *mut c_void) {
        let _ = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
    }

    #[test]
    fn completion_guard_drop_without_fire_fires_aborted_completion() {
        // Mental-revert: removing the `if !self.fired` branch in
        // CompletionGuard::Drop reverts to silent drop on un-fired
        // guards. The cdylib's `rx.await` then hangs forever instead
        // of returning the aborted-task error. This test would fail
        // because `events` would be empty.
        let (user_data, sink) = install_sink_user_data();
        {
            let _guard = CompletionGuard::new(record_completion, user_data);
            // Drop without firing.
        }
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "Drop must fire exactly one completion");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1, "aborted completion uses status -1");
        let msg = std::str::from_utf8(payload).expect("UTF-8");
        assert!(
            msg.contains("runtime-ops host task aborted before completion"),
            "got: {msg}"
        );
        drop(events);
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn completion_guard_fire_then_drop_does_not_double_fire() {
        // Mental-revert: removing `self.fired = true;` from
        // fire_err_msg reverts to Drop firing again, this test
        // observes `events.len() == 2` and fails.
        let (user_data, sink) = install_sink_user_data();
        let guard = CompletionGuard::new(record_completion, user_data);
        guard.fire_err_msg(b"deliberate-test-msg");
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "fire_err_msg must fire exactly once");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1);
        assert_eq!(payload, b"deliberate-test-msg");
        drop(events);
        unsafe { reclaim_sink(user_data) };
    }
}
