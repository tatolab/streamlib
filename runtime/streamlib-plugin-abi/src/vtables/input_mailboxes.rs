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
/// - v2: appended `max_payload_for_port` so the cdylib could size its read
///   buffer to the schema-declared authored budget up-front, relying on the
///   publisher never loaning a bigger slot.
/// - v3 (#1421): removes `max_payload_for_port`. Publishers now open under
///   `AllocationStrategy::PowerOfTwo` and grow their data segment on the first
///   oversized loan, so the "publisher can't loan bigger than the authored
///   budget" invariant v2 relied on no longer holds. `read_raw` becomes a
///   grow-and-retry protocol: the cdylib starts with a default buffer and, when
///   the host reports the next frame is larger (`*out_len > out_cap`,
///   `*has_data == true`), resizes to `*out_len` and reads again. The host
///   stashes the oversized frame across the two calls, so nothing is dropped.
pub const INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION: u32 = 3;

/// `extern "C" fn` dispatch table for the cdylib's `InputMailboxes`
/// PluginAbiObject. Replaces the shared-Rust-type `&mut InputMailboxes`
/// crossing the cdylib used to expose to the host via
/// `ProcessorVTable::get_iceoryx2_input_mailboxes_mut`.
///
/// The cdylib's `process()` body reaches input data through this
/// vtable: `read_raw` consumes the next queued frame for a port
/// according to its read mode, `has_data` queries without consuming.
/// All other `InputMailboxes` methods (`add_port`, `add_channel_subscriber`,
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
/// On success, `*has_data` distinguishes "a frame is available"
/// (`true`) from "no frames queued" (`false`). When `*has_data ==
/// true` and `*out_len <= out_cap`, the callee wrote the raw
/// msgpack-encoded frame body to `out_buf` and the timestamp to
/// `*out_timestamp`. When `*has_data == true` and `*out_len >
/// out_cap`, the next frame is larger than the caller's buffer:
/// nothing was written, the host is holding the frame, and the
/// cdylib must resize `out_buf` to `*out_len` and call again
/// (grow-and-retry — see [`INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION`]).
///
/// `has_data` is infallible.
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
    /// - `*has_data = true`, `*out_len <= out_cap`: a frame was
    ///   delivered — its msgpack-encoded body is copied to
    ///   `out_buf[..*out_len]` and its monotonic timestamp to
    ///   `*out_timestamp`.
    /// - `*has_data = true`, `*out_len > out_cap`: the next frame is
    ///   larger than `out_buf`; nothing was copied. The host is
    ///   holding the frame — resize `out_buf` to `*out_len` and call
    ///   again (grow-and-retry).
    /// - `*has_data = false`: the mailbox was empty.
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
        // header (u32 + u32) + 4 fn pointers @ 8 bytes each =
        // 4 + 4 + 4 * 8 = 40 bytes (v3 removed max_payload_for_port).
        assert_eq!(size_of::<InputMailboxesVTable>(), 40);
        assert_eq!(align_of::<InputMailboxesVTable>(), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, layout_version), 0);
        assert_eq!(offset_of!(InputMailboxesVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(InputMailboxesVTable, read_raw), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, has_data), 16);
        assert_eq!(offset_of!(InputMailboxesVTable, clone_arc), 24);
        assert_eq!(offset_of!(InputMailboxesVTable, drop_arc), 32);
    }

    #[test]
    fn input_mailboxes_vtable_layout_version_pinned_at_three() {
        assert_eq!(INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION, 3);
    }
}
