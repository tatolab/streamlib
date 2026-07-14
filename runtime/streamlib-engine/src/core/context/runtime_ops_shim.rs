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
/// Implements [`RuntimeOperations`] so call sites that take an
/// `Arc<dyn RuntimeOperations>` accept the shim transparently.
///
/// # Lifetime contract
///
/// The shim's `handle` field is a `*const c_void` returned from the
/// `RuntimeOpsVTable::clone_handle` callback (v2). The host's
/// implementation returns a `Box<Arc<dyn RuntimeOperations>>` with
/// an Arc refcount bump on the underlying ops implementation; the
/// inner Arc keeps the impl alive even after the originating
/// `RuntimeContext` is dropped. The shim's `Drop` releases the
/// owned handle via the paired `drop_handle` callback exactly once.
///
/// This means stashing the returned `Arc<dyn RuntimeOperations>`
/// past `Runner::stop()` is sound — the inner ops impl outlives the
/// runtime context that issued the shim.
///
/// Deliberately NOT `Clone`: the shim owns a refcount; cloning the
/// inner struct (rather than the wrapping Arc) would duplicate the
/// raw `handle` pointer without bumping the host's Arc refcount,
/// causing a double-`drop_handle` on Drop. Users clone the wrapping
/// `Arc<RuntimeOpsShim>` instead — cheap, and `Drop` only fires when
/// the last clone goes out of scope.
pub struct RuntimeOpsShim {
    handle: *const c_void,
    vtable: *const RuntimeOpsVTable,
}

// Pointer fields point at host-owned heap state (a Box<Arc<...>>) that
// the shim's Drop releases via the vtable's `drop_handle`. Lifetime is
// bounded by the shim itself, not by the RuntimeContext that minted
// the borrowed handle from which this owned one was cloned.
unsafe impl Send for RuntimeOpsShim {}
unsafe impl Sync for RuntimeOpsShim {}

impl Drop for RuntimeOpsShim {
    fn drop(&mut self) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: vtable + handle are paired at construction by
        // `from_ffi`. `drop_handle` is required by the v2 ABI and
        // releases the host-side `Box<Arc<dyn RuntimeOperations>>`
        // that `clone_handle` originally allocated. Single-fire is
        // guaranteed because Drop runs exactly once per instance.
        unsafe { ((*self.vtable).drop_handle)(self.handle) };
    }
}

impl RuntimeOpsShim {
    /// Construct a shim from a host-supplied owned handle + vtable.
    /// Crate-internal: the runtime-context shim is the only legitimate
    /// builder, and is responsible for calling
    /// [`RuntimeOpsVTable::clone_handle`] to obtain the owned handle
    /// from the borrowed one it received via
    /// [`RuntimeContextVTable::runtime_ops_handle`](streamlib_plugin_abi::RuntimeContextVTable).
    pub(crate) fn from_ffi(
        owned_handle: *const c_void,
        vtable: *const RuntimeOpsVTable,
    ) -> Arc<Self> {
        Arc::new(Self {
            handle: owned_handle,
            vtable,
        })
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
            rmp_serde::from_slice(&payload)
                .map_err(|e| Error::Runtime(format!("runtime-ops response decode failed: {e}")))
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
    crate::core::plugin::host_services::run_host_extern_c(
        "runtime_ops_completion_trampoline",
        || {
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
        },
        (),
    )
}

impl RuntimeOperations for RuntimeOpsShim {
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let bytes = match rmp_serde::to_vec_named(&spec) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::add_processor_async: spec msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(
            self.submit(move |handle, vtable, completion, user_data| unsafe {
                ((*vtable).add_processor)(
                    handle,
                    bytes.as_ptr(),
                    bytes.len(),
                    completion,
                    user_data,
                );
                // Hold bytes alive until the host has consumed them. The
                // vtable contract is "valid for the duration of the call"
                // — the spawned host task copies the bytes synchronously
                // before its first await, so it's safe to drop bytes when
                // this closure returns. `bytes` is moved into the closure,
                // so it's dropped at end-of-call here.
                drop(bytes);
            }),
        )
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        let bytes = match rmp_serde::to_vec_named(&processor_id) {
            Ok(b) => b,
            Err(e) => {
                let err = Err(Error::Config(format!(
                    "RuntimeOpsShim::remove_processor_async: id msgpack encode failed: {e}"
                )));
                return Box::pin(async move { err });
            }
        };
        Box::pin(
            self.submit_unit(move |handle, vtable, completion, user_data| unsafe {
                ((*vtable).remove_processor)(
                    handle,
                    bytes.as_ptr(),
                    bytes.len(),
                    completion,
                    user_data,
                );
                drop(bytes);
            }),
        )
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
        Box::pin(
            self.submit(move |handle, vtable, completion, user_data| unsafe {
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
            }),
        )
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
        Box::pin(
            self.submit_unit(move |handle, vtable, completion, user_data| unsafe {
                ((*vtable).disconnect)(handle, bytes.as_ptr(), bytes.len(), completion, user_data);
                drop(bytes);
            }),
        )
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
