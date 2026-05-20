// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-side wrapper around the host's graph-mutation operations.
//!
//! `RuntimeOpsShim` lives behind the [`super::RuntimeContextFullAccess`] /
//! `LimitedAccess` shim's `runtime()` accessor. It carries the
//! per-RuntimeContext opaque handle plus the [`RuntimeOpsVTable`]
//! pointer pulled from `HostServices` at register time. Each method
//! issues a submit-with-completion call into the host's vtable and
//! awaits the response through a `tokio::sync::oneshot` channel —
//! plugin code keeps using its own tokio runtime to drive the
//! returned future; the host's runtime spawns the real `*_async`
//! work invisibly.
//!
//! Implements [`RuntimeOperations`] so existing call sites
//! (`ctx.runtime().add_processor_async(spec).await`) keep working
//! against a plugin-owned runtime without any change at the call
//! site.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_plugin_abi::{RuntimeOpCompletionCallback, RuntimeOpsVTable};

use crate::core::error::{Error, Result};
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::runtime::{BoxFuture, RuntimeOperations};
use crate::core::{InputLinkPortRef, OutputLinkPortRef};

/// Cdylib-side handle to the host's graph-mutation operations.
///
/// Cheap to construct (clone is just two pointer copies). Implements
/// [`RuntimeOperations`] so call sites that take an
/// `Arc<dyn RuntimeOperations>` accept the shim transparently.
///
/// # Lifetime contract
///
/// The shim's `handle` field is a `*const c_void` pointer into the
/// host's `RuntimeContext.runtime_ops` Arc. That borrow is sound for
/// the lifetime of the host's `RuntimeContext` (process-lifetime
/// today via `Runner`). The returned `Arc<RuntimeOpsShim>` does NOT
/// extend the `RuntimeContext`'s lifetime via reference-counting the
/// way the pre-Phase-B owned `Arc<dyn RuntimeOperations>` did —
/// stashing the shim past a `Runner::stop()` would dangle the
/// `handle`. Today no in-tree caller does that (lifecycle methods
/// drop the shim before the runtime can stop), but the type
/// signature no longer encodes the constraint. If a future caller
/// needs to retain runtime ops past lifecycle, switch the shim's
/// `handle` to an owning Arc clone obtained through a new vtable
/// callback (`runtime_ops_clone_arc -> *const c_void` plus a
/// matching `runtime_ops_drop_arc`).
#[derive(Clone)]
pub struct RuntimeOpsShim {
    handle: *const c_void,
    vtable: *const RuntimeOpsVTable,
}

// Pointer fields point at host-owned state with stable lifetime — the
// host pins its `RuntimeContext` and `HostServices` static for the
// cdylib's process lifetime, so the shim can outlive any per-call
// borrow within a Runner's lifetime.
unsafe impl Send for RuntimeOpsShim {}
unsafe impl Sync for RuntimeOpsShim {}

impl RuntimeOpsShim {
    /// Construct a shim from a host-supplied handle + vtable. Crate-
    /// internal: the runtime-context shim is the only legitimate
    /// builder.
    pub(crate) fn from_ffi(handle: *const c_void, vtable: *const RuntimeOpsVTable) -> Arc<Self> {
        Arc::new(Self { handle, vtable })
    }

    /// Submit a unary msgpack-encoded request and await the
    /// msgpack-encoded response decoded as `R`.
    async fn submit<R, F>(&self, request: F) -> Result<R>
    where
        R: serde::de::DeserializeOwned,
        F: FnOnce(*const c_void, *const RuntimeOpsVTable, RuntimeOpCompletionCallback, *mut c_void)
            + Send
            + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<(i32, Vec<u8>)>();
        let sender_box: Box<tokio::sync::oneshot::Sender<(i32, Vec<u8>)>> = Box::new(tx);
        let user_data = Box::into_raw(sender_box) as *mut c_void;

        // The completion callback runs from a host thread (the tokio
        // task spawned in the vtable impl). It takes ownership of the
        // boxed Sender, copies the payload bytes, and fires the
        // oneshot. The plugin-side task receives them through `rx`.
        let completion: RuntimeOpCompletionCallback = runtime_ops_completion_trampoline;

        // SAFETY: the vtable + handle pair were promised valid by the
        // engine at shim construction. The completion + user_data pair
        // satisfies the ABI's promise: completion always fires exactly
        // once and reclaims the boxed Sender.
        request(self.handle, self.vtable, completion, user_data);

        let (status, payload) = rx
            .await
            .map_err(|_| Error::Runtime("runtime-ops completion dropped".into()))?;
        if status == 0 {
            rmp_serde::from_slice(&payload).map_err(|e| {
                Error::Runtime(format!("runtime-ops response decode failed: {e}"))
            })
        } else {
            let msg = String::from_utf8_lossy(&payload).into_owned();
            Err(Error::Runtime(msg))
        }
    }

    /// Same as [`submit`] but discards the response payload.
    async fn submit_unit<F>(&self, request: F) -> Result<()>
    where
        F: FnOnce(*const c_void, *const RuntimeOpsVTable, RuntimeOpCompletionCallback, *mut c_void)
            + Send
            + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<(i32, Vec<u8>)>();
        let sender_box: Box<tokio::sync::oneshot::Sender<(i32, Vec<u8>)>> = Box::new(tx);
        let user_data = Box::into_raw(sender_box) as *mut c_void;
        let completion: RuntimeOpCompletionCallback = runtime_ops_completion_trampoline;
        request(self.handle, self.vtable, completion, user_data);
        let (status, payload) = rx
            .await
            .map_err(|_| Error::Runtime("runtime-ops completion dropped".into()))?;
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&payload).into_owned();
            Err(Error::Runtime(msg))
        }
    }
}

/// Bridge completion callback. The host fires it once per submit; we
/// reclaim the boxed Sender, copy the payload, and forward through
/// the oneshot.
unsafe extern "C" fn runtime_ops_completion_trampoline(
    user_data: *mut c_void,
    status: i32,
    result_ptr: *const u8,
    result_len: usize,
) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: paired with `Box::into_raw` in `submit`/`submit_unit`.
    let sender_box = unsafe {
        Box::from_raw(user_data as *mut tokio::sync::oneshot::Sender<(i32, Vec<u8>)>)
    };
    let payload = if result_len == 0 || result_ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: payload bytes are valid for the duration of the
        // callback per the ABI contract; we copy out before returning.
        let slice = unsafe { std::slice::from_raw_parts(result_ptr, result_len) };
        slice.to_vec()
    };
    let _ = sender_box.send((status, payload));
}

impl RuntimeOperations for RuntimeOpsShim {
    fn add_processor_async(
        &self,
        spec: ProcessorSpec,
    ) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let bytes = match rmp_serde::to_vec_named(&spec) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::add_processor_async: spec msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(self.submit(move |handle, vtable, completion, user_data| unsafe {
            ((*vtable).add_processor)(handle, bytes.as_ptr(), bytes.len(), completion, user_data);
            // Hold bytes alive until the host has consumed them. The
            // vtable contract is "valid for the duration of the call"
            // — the spawned host task copies the bytes synchronously
            // before its first await, so it's safe to drop bytes when
            // this closure returns. `bytes` is moved into the closure,
            // so it's dropped at end-of-call here.
            drop(bytes);
        }))
    }

    fn remove_processor_async(
        &self,
        processor_id: ProcessorUniqueId,
    ) -> BoxFuture<'_, Result<()>> {
        let bytes = match rmp_serde::to_vec_named(&processor_id) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::remove_processor_async: id msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(self.submit_unit(move |handle, vtable, completion, user_data| unsafe {
            ((*vtable).remove_processor)(
                handle,
                bytes.as_ptr(),
                bytes.len(),
                completion,
                user_data,
            );
            drop(bytes);
        }))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        let from_bytes = match rmp_serde::to_vec_named(&from) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::connect_async: from-port msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        let to_bytes = match rmp_serde::to_vec_named(&to) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::connect_async: to-port msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(self.submit(move |handle, vtable, completion, user_data| unsafe {
            ((*vtable).connect)(
                handle,
                from_bytes.as_ptr(),
                from_bytes.len(),
                to_bytes.as_ptr(),
                to_bytes.len(),
                completion,
                user_data,
            );
            drop(from_bytes);
            drop(to_bytes);
        }))
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        let bytes = match rmp_serde::to_vec_named(&link_id) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::disconnect_async: link_id msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(self.submit_unit(move |handle, vtable, completion, user_data| unsafe {
            ((*vtable).disconnect)(
                handle,
                bytes.as_ptr(),
                bytes.len(),
                completion,
                user_data,
            );
            drop(bytes);
        }))
    }

    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(self.submit(|handle, vtable, completion, user_data| unsafe {
            ((*vtable).to_json)(handle, completion, user_data);
        }))
    }

    // -------------------------------------------------------------------------
    // Sync convenience wrappers — `block_on` against the caller's
    // ambient tokio context. Plugins driving these from non-async
    // code must hold a live runtime handle (typical pattern: stash
    // `runtime.handle()` in `setup` and use `handle.block_on(fut)`).
    // -------------------------------------------------------------------------

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        block_on_current_runtime(self.add_processor_async(spec))
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        block_on_current_runtime(self.remove_processor_async(processor_id.clone()))
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        block_on_current_runtime(self.connect_async(from, to))
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        block_on_current_runtime(self.disconnect_async(link_id.clone()))
    }

    fn to_json(&self) -> Result<serde_json::Value> {
        block_on_current_runtime(self.to_json_async())
    }
}

/// Sync convenience wrapper that drives the async submit on a tokio
/// runtime. Mirrors the existing `RuntimeOperations` sync-method
/// contract: must not be called from within a tokio task on the same
/// runtime (would deadlock); callers are expected to hold their own
/// tokio handle when they need sync access.
fn block_on_current_runtime<F: std::future::Future>(fut: F) -> F::Output {
    // Try to use the ambient handle when one exists (plugin code that
    // already has a tokio runtime active); fall back to a temporary
    // current-thread runtime otherwise (sync-only callers).
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("transient tokio runtime build failed");
            rt.block_on(fut)
        }
    }
}
