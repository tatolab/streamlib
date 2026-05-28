// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `RuntimeOpsVTable` — extern "C" dispatch for `RuntimeOperations` plus
//! the paired `RuntimeOpCompletionCallback` callback type.

use core::ffi::c_void;

/// Layout version of [`crate::RuntimeOpsVTable`].
///
/// - v1: 5 submit-with-completion ops (`add_processor` /
///   `remove_processor` / `connect` / `disconnect` / `to_json`). Handle
///   lifetime was a borrow into RuntimeContext-owned storage; a shim
///   stashed past `Runner::stop()` would dangle (sound today because
///   nothing stashes; type signature didn't encode it).
/// - v2: added `clone_handle` / `drop_handle` for owning-Arc semantics.
///   The cdylib-side `RuntimeOpsShim` now holds an Arc-bumped owned
///   handle and releases it via `drop_handle` in its Drop impl,
///   keeping the host's `Arc<dyn RuntimeOperations>` alive for the
///   shim's lifetime independently of `RuntimeContext`'s lifetime.
pub const RUNTIME_OPS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Completion callback signature for async runtime ops.
///
/// `status` is `0` on success, non-zero on error. On success,
/// `result_ptr` points at a msgpack-encoded result payload of length
/// `result_len`. On error, `result_ptr` points at a UTF-8 error
/// message of length `result_len`.
///
/// The pointed-at bytes are valid only for the duration of the
/// callback invocation; the cdylib must copy any data it needs to
/// retain.
pub type RuntimeOpCompletionCallback = unsafe extern "C" fn(
    user_data: *mut c_void,
    status: i32,
    result_ptr: *const u8,
    result_len: usize,
);

/// Dispatch table for the host's graph-mutation operations
/// (`add_processor`, `connect`, etc.). The cdylib obtains a handle
/// via [`crate::RuntimeContextVTable::runtime_ops_handle`] and reads the
/// static vtable from [`crate::HostServices::runtime_ops_vtable`].
///
/// All methods are submit-with-completion: the host fires
/// `completion(user_data, status, result_ptr, result_len)` once
/// when the operation finishes. The completion may fire synchronously
/// (op was instantly ready) or asynchronously (on a host thread).
/// The cdylib's wrapper bridges back to its own runtime via a
/// `tokio::sync::oneshot` or equivalent.
///
/// Request payloads are msgpack-encoded; the host decodes against
/// the same types the in-process trait surface accepts
/// (`ProcessorSpec`, `OutputLinkPortRef`, `InputLinkPortRef`,
/// `ProcessorUniqueId`, `LinkUniqueId`).
#[repr(C)]
pub struct RuntimeOpsVTable {
    pub layout_version: u32,
    pub _reserved_padding: u32,

    /// Submit an `add_processor` operation. `spec_msgpack` carries a
    /// msgpack-encoded `ProcessorSpec`. On success the result payload
    /// is the msgpack-encoded `ProcessorUniqueId`.
    pub add_processor: unsafe extern "C" fn(
        handle: *const c_void,
        spec_msgpack_ptr: *const u8,
        spec_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `remove_processor` operation. `processor_id_msgpack`
    /// carries a msgpack-encoded `ProcessorUniqueId`. Empty success
    /// payload.
    pub remove_processor: unsafe extern "C" fn(
        handle: *const c_void,
        processor_id_msgpack_ptr: *const u8,
        processor_id_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `connect` operation. `from_msgpack` and `to_msgpack`
    /// carry msgpack-encoded `OutputLinkPortRef` / `InputLinkPortRef`.
    /// Success payload is the msgpack-encoded `LinkUniqueId`.
    pub connect: unsafe extern "C" fn(
        handle: *const c_void,
        from_msgpack_ptr: *const u8,
        from_msgpack_len: usize,
        to_msgpack_ptr: *const u8,
        to_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `disconnect` operation. `link_id_msgpack` carries a
    /// msgpack-encoded `LinkUniqueId`. Empty success payload.
    pub disconnect: unsafe extern "C" fn(
        handle: *const c_void,
        link_id_msgpack_ptr: *const u8,
        link_id_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `to_json` operation. Success payload is the msgpack-
    /// encoded `serde_json::Value`.
    pub to_json: unsafe extern "C" fn(
        handle: *const c_void,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    // v2 additions: owning-Arc handle lifetime management.

    /// Take a (borrowed) handle returned from
    /// [`crate::RuntimeContextVTable::runtime_ops_handle`] and return a new
    /// owned handle with an Arc refcount bump on the underlying
    /// `Arc<dyn RuntimeOperations>`. The owned handle remains valid
    /// even after the originating `RuntimeContext` is dropped, and
    /// MUST be released exactly once via [`Self::drop_handle`].
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a no-op.
    /// Calling on the same owned handle twice is undefined behaviour
    /// (it would double-free the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),
}

unsafe impl Send for RuntimeOpsVTable {}
unsafe impl Sync for RuntimeOpsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn runtime_ops_vtable_layout() {
        // 4 + 4 + 7 fn pointers (v2: 5 submit ops + clone_handle + drop_handle) = 64 bytes
        assert_eq!(size_of::<RuntimeOpsVTable>(), 64);
        assert_eq!(align_of::<RuntimeOpsVTable>(), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, layout_version), 0);
        assert_eq!(offset_of!(RuntimeOpsVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(RuntimeOpsVTable, add_processor), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, remove_processor), 16);
        assert_eq!(offset_of!(RuntimeOpsVTable, connect), 24);
        assert_eq!(offset_of!(RuntimeOpsVTable, disconnect), 32);
        assert_eq!(offset_of!(RuntimeOpsVTable, to_json), 40);
        assert_eq!(offset_of!(RuntimeOpsVTable, clone_handle), 48);
        assert_eq!(offset_of!(RuntimeOpsVTable, drop_handle), 56);
    }
}
