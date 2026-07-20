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

/// Upper bound on grow-and-retry passes in [`grow_and_retry_read`]. The
/// host stashes an oversized frame and re-delivers it at the exact required
/// size, so two passes suffice; the small bound guards against a pathological
/// producer growing the frame between calls.
const MAX_GROW_AND_RETRY_ATTEMPTS: usize = 8;

/// Run the [`InputMailboxesVTable::read_raw`] grow-and-retry protocol,
/// shared by the host's and the engine-free SDK's `InputMailboxes::read_raw`
/// wrappers so both arms of the ABI stay lock-step.
///
/// Starts with a `start_cap`-byte receive buffer and, when the host reports
/// the next frame is larger (`*out_len > out_cap`, `*has_data == true`),
/// resizes to `*out_len` and reads again — the host holds the oversized frame
/// across the two calls, so nothing is dropped. Returns
/// `Ok(Some((body, timestamp_ns)))` on a delivered frame, `Ok(None)` when the
/// mailbox is empty, and `Err(message)` on a host-side failure or when the
/// frame keeps growing past [`MAX_GROW_AND_RETRY_ATTEMPTS`]. Callers wrap the
/// message in their own link-error variant.
///
/// # Safety
///
/// `vtable` and `handle` must both be non-null and point at a live host-side
/// `InputMailboxesInner` and its vtable (the caller's `is_configured()`
/// guarantees this).
pub unsafe fn grow_and_retry_read(
    vtable: *const InputMailboxesVTable,
    handle: *const c_void,
    port: &str,
    start_cap: usize,
) -> Result<Option<(Vec<u8>, i64)>, String> {
    let mut cap = start_cap;
    for _ in 0..MAX_GROW_AND_RETRY_ATTEMPTS {
        let mut buf = vec![0u8; cap];
        let mut out_len = 0usize;
        let mut out_timestamp = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        // SAFETY: `vtable` and `handle` are non-null and live per the caller's
        // contract; the out-pointers all address stack locals valid for the call.
        let rc = unsafe {
            ((*vtable).read_raw)(
                handle,
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_timestamp as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if rc != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(format!(
                "InputMailboxes::read_raw(port='{}') failed: {}",
                port, msg
            ));
        }
        if !has_data {
            return Ok(None);
        }
        if out_len > buf.len() {
            cap = out_len;
            continue;
        }
        buf.truncate(out_len);
        return Ok(Some((buf, out_timestamp)));
    }
    Err(format!(
        "InputMailboxes::read_raw(port='{}'): frame kept growing across \
         grow-and-retry attempts — giving up to avoid an unbounded loop",
        port
    ))
}

/// Byte length of the frame a read would return next from a native SDK's local
/// `pending` receive queue, so the Python and Deno natives share one peek rule.
/// `read_next_in_order` selects the FIFO front (`0`); otherwise the SkipToLatest
/// newest (`queue.len() - 1`). The caller guarantees `queue` is non-empty (its
/// `is_empty()` continue runs first) and compares the returned length against
/// its receive buffer to decide whether to grow before consuming the frame.
pub fn next_read_required_len(queue: &[(Vec<u8>, i64)], read_next_in_order: bool) -> usize {
    let next_index = if read_next_in_order {
        0
    } else {
        queue.len() - 1
    };
    queue[next_index].0.len()
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn next_read_required_len_picks_front_or_back_by_read_mode() {
        let queue: Vec<(Vec<u8>, i64)> = vec![
            (vec![0u8; 4], 1),
            (vec![0u8; 16], 2),
            (vec![0u8; 64], 3),
        ];
        assert_eq!(next_read_required_len(&queue, true), 4);
        assert_eq!(next_read_required_len(&queue, false), 64);
    }

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
