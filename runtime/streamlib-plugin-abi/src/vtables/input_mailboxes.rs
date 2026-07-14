// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `InputMailboxesVTable` — extern "C" dispatch for the cdylib's `InputMailboxes` PluginAbiObject.

use core::ffi::c_void;

/// Layout version of [`crate::InputMailboxesVTable`].
///
/// - v1: ships four slots — the two cdylib processor's
///   `InputMailboxes` PluginAbiObject needs from inside `process()`
///   (`read_raw` returns the next raw frame for a port with
///   `has_data` distinguishing "no data" from "error"; `has_data`
///   queries without consuming), plus the Arc lifecycle pair
///   (`clone_arc` / `drop_arc`) every Arc-handle PluginAbiObject on this
///   ABI carries for refcount accounting in host-compiled code.
/// - v2: appends `max_payload_for_port` so the cdylib can allocate
///   exactly the schema-declared `metadata.max_payload_bytes` for
///   each port up-front. The publisher side already honors this
///   value (the iceoryx2 service is sized by it; the publisher
///   can't loan a bigger slot), so the cdylib's read buffer is
///   guaranteed sufficient by service-creation invariant — no
///   truncation, no retry loop. Replaces the v1 4 KiB scratch +
///   "resize and retry on truncation" dance that silently dropped
///   every >4 KiB frame for ~4 days post-#894 (audio-mixer-demo
///   silent-output bug).
pub const INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION: u32 = 2;

/// `extern "C" fn` dispatch table for the cdylib's `InputMailboxes`
/// PluginAbiObject. Replaces the shared-Rust-type `&mut InputMailboxes`
/// crossing the cdylib used to expose to the host via
/// `ProcessorVTable::get_iceoryx2_input_mailboxes_mut`.
///
/// The cdylib's `process()` body reaches input data through this
/// vtable: `read_raw` consumes the next queued frame for a port
/// according to its read mode, `has_data` queries without consuming.
/// All other `InputMailboxes` methods (`add_port`, `set_subscriber`,
/// `set_listener`, `listener_fd`, `drain_listener`,
/// `receive_pending`, `route`, `any_port_has_data`) are host-side
/// only and do not appear on this vtable.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded
/// into newer hosts are rejected cleanly. New fields append after
/// `has_data` and bump [`INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// `read_raw` returns `0` on success, non-zero on host-side
/// failure (a malformed inbound frame, allocator failure, etc.).
/// On success, `*has_data` distinguishes "consumed a frame"
/// (`true`) from "no frames queued" (`false`). When `*has_data ==
/// true`, the callee writes the raw msgpack-encoded frame body to
/// `out_buf` and the timestamp to `*out_timestamp`. The cdylib is
/// expected to size `out_buf` to the value returned by
/// `max_payload_for_port` for the port — the iceoryx2 service is
/// sized by the same schema-declared `metadata.max_payload_bytes`
/// on the publisher side, so any actual frame body is guaranteed
/// ≤ that bound by service-creation invariant. Truncation
/// (`required > out_cap`) is now a protocol violation and surfaces
/// as a non-zero return.
///
/// `has_data` and `max_payload_for_port` are infallible.
#[repr(C)]
pub struct InputMailboxesVTable {
    /// Vtable layout version. Must equal
    /// [`INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned; zero today, never read).
    pub _reserved_padding: u32,

    /// Consume the next queued raw frame for the named port. The
    /// host runs `receive_pending` first (draining iceoryx2 into
    /// the per-port mailbox) then pops according to the port's
    /// `ReadMode` (skip-to-latest for video, FIFO for audio).
    ///
    /// On entry `*out_len = 0`. On success the callee writes:
    /// - `*has_data = true` if a frame was consumed; the frame's
    ///   msgpack-encoded body is copied to `out_buf[..*out_len]`
    ///   and the frame's monotonic timestamp to `*out_timestamp`.
    /// - `*has_data = false` if the mailbox was empty.
    ///
    /// The cdylib must size `out_buf` to the value returned by
    /// `max_payload_for_port` for the port. If a frame body
    /// exceeds `out_cap`, the call returns non-zero with an error
    /// in `err_buf` — this indicates either a protocol violation
    /// or a stale cached max from before a schema change.
    pub read_raw: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        out_timestamp: *mut i64,
        has_data: *mut bool,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check whether the named port has at least one queued frame
    /// after draining iceoryx2's per-publisher buffer into the
    /// per-port mailbox. Returns `false` for unknown ports.
    pub has_data:
        unsafe extern "C" fn(handle: *const c_void, port_ptr: *const u8, port_len: usize) -> bool,

    /// Bump the host-side `Arc<InputMailboxesInner>` strong count.
    /// Returns the same opaque handle (the underlying inner is the
    /// same object). Pairs 1:1 with `drop_arc`.
    pub clone_arc: unsafe extern "C" fn(handle: *const c_void) -> *const c_void,

    /// Decrement the host-side `Arc<InputMailboxesInner>` strong
    /// count. Releases the inner when the count reaches zero.
    pub drop_arc: unsafe extern "C" fn(handle: *const c_void),

    /// Return the schema-declared `metadata.max_payload_bytes` for
    /// the named port — the upper bound on serialized frame size
    /// guaranteed by the iceoryx2 service's publisher-side
    /// configuration. The cdylib allocates `out_buf` to this size
    /// before calling `read_raw`; the publisher cannot loan a
    /// bigger slot, so truncation is structurally impossible.
    ///
    /// Returns the engine-wide `MAX_PAYLOAD_SIZE` default (64 KiB)
    /// for ports without a registered schema or without an
    /// explicit `max_payload_bytes` declaration. Returns `0` for
    /// unknown ports — caller must treat as a wiring error.
    ///
    /// v2 addition.
    pub max_payload_for_port:
        unsafe extern "C" fn(handle: *const c_void, port_ptr: *const u8, port_len: usize) -> usize,
}

// Safety: every field is a primitive or an `extern "C" fn` pointer.
// The vtable's `&'static` storage outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for InputMailboxesVTable {}
unsafe impl Sync for InputMailboxesVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn input_mailboxes_vtable_layout() {
        // header (u32 + u32) + 5 fn pointers @ 8 bytes each =
        // 4 + 4 + 5 * 8 = 48 bytes (v2 appended max_payload_for_port).
        assert_eq!(size_of::<InputMailboxesVTable>(), 48);
        assert_eq!(align_of::<InputMailboxesVTable>(), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, layout_version), 0);
        assert_eq!(offset_of!(InputMailboxesVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(InputMailboxesVTable, read_raw), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, has_data), 16);
        assert_eq!(offset_of!(InputMailboxesVTable, clone_arc), 24);
        assert_eq!(offset_of!(InputMailboxesVTable, drop_arc), 32);
        assert_eq!(offset_of!(InputMailboxesVTable, max_payload_for_port), 40);
    }

    #[test]
    fn input_mailboxes_vtable_layout_version_pinned_at_two() {
        assert_eq!(INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION, 2);
    }
}
