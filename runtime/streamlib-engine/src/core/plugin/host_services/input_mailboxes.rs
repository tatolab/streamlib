// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `InputMailboxesVTable` callbacks + static vtable + accessor.
//!
//! Each wrapper reconstructs the inner borrow from the raw
//! `Arc::into_raw(Arc<InputMailboxesInner>)` handle the cdylib passes,
//! runs the inner method, and serializes the result into the FFI's
//! out-parameter buffers + `i32 + err_buf` shape. All bodies wrapped
//! in `run_host_extern_c` so a panic in the inner method becomes a
//! non-zero return.

use std::ffi::c_void;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_extern_err;

// =============================================================================
// InputMailboxesVTable wrappers (issue #894)
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<crate::iceoryx2::InputMailboxesInner>)`. The
/// leaked strong count keeps the inner alive for the call's
/// duration.
unsafe fn handle_as_input_mailboxes_inner(
    handle: *const c_void,
) -> Option<&'static crate::iceoryx2::InputMailboxesInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::iceoryx2::InputMailboxesInner) })
}

unsafe extern "C" fn host_input_mailboxes_read_raw(
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
) -> i32 {
    run_host_extern_c(
        "host_input_mailboxes_read_raw",
        || -> i32 {
            if !out_len.is_null() {
                unsafe {
                    *out_len = 0;
                }
            }
            if !has_data.is_null() {
                unsafe {
                    *has_data = false;
                }
            }
            let Some(inner) = (unsafe { handle_as_input_mailboxes_inner(handle) }) else {
                write_extern_err(
                    "read_raw: null InputMailboxes handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if port_ptr.is_null() {
                write_extern_err("read_raw: null port_ptr", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let port = match std::str::from_utf8(port_bytes) {
                Ok(s) => s,
                Err(e) => {
                    write_extern_err(
                        &format!("read_raw: port not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // Grow-and-retry contract (#1421): a publisher under PowerOfTwo
            // growth can deliver a frame larger than the cdylib's current
            // buffer. When that happens the host stashes the frame and reports
            // its required length (`*has_data = true`, `*out_len = required`, no
            // copy, return 0); the cdylib resizes to `*out_len` and reads again,
            // getting the stashed frame back at the exact size. Nothing is
            // dropped — this retires the pre-#1421 "publisher can't loan bigger
            // than the authored max" invariant, which PowerOfTwo growth broke.
            match inner.read_raw_bounded(port, out_cap) {
                Ok(crate::iceoryx2::BoundedReadOutcome::Frame { data, timestamp_ns }) => {
                    if !has_data.is_null() {
                        unsafe {
                            *has_data = true;
                        }
                    }
                    if !out_timestamp.is_null() {
                        unsafe {
                            *out_timestamp = timestamp_ns;
                        }
                    }
                    if !out_len.is_null() {
                        unsafe {
                            *out_len = data.len();
                        }
                    }
                    if !out_buf.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(data.as_ptr(), out_buf, data.len());
                        }
                    }
                    0
                }
                Ok(crate::iceoryx2::BoundedReadOutcome::NeedsLargerBuffer { required_bytes }) => {
                    // Frame held for the retry; report the size the cdylib must
                    // grow to. `out_len > out_cap` is the resize signal.
                    if !has_data.is_null() {
                        unsafe {
                            *has_data = true;
                        }
                    }
                    if !out_len.is_null() {
                        unsafe {
                            *out_len = required_bytes;
                        }
                    }
                    0
                }
                Ok(crate::iceoryx2::BoundedReadOutcome::Empty) => 0, // has_data stays false
                Err(e) => {
                    write_extern_err(&e.to_string(), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_input_mailboxes_has_data(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
) -> bool {
    run_host_extern_c(
        "host_input_mailboxes_has_data",
        || -> bool {
            let Some(inner) = (unsafe { handle_as_input_mailboxes_inner(handle) }) else {
                return false;
            };
            if port_ptr.is_null() {
                return false;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let Ok(port) = std::str::from_utf8(port_bytes) else {
                return false;
            };
            inner.has_data(port)
        },
        false,
    )
}

pub(crate) unsafe extern "C" fn host_input_mailboxes_clone_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_input_mailboxes_clone_arc",
        || -> *const c_void {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: handle came from Arc::into_raw.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::increment_strong_count(
                    handle as *const crate::iceoryx2::InputMailboxesInner,
                );
            }
            handle
        },
        std::ptr::null(),
    )
}

pub(crate) unsafe extern "C" fn host_input_mailboxes_drop_arc(handle: *const c_void) {
    run_host_extern_c(
        "host_input_mailboxes_drop_arc",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle came from Arc::into_raw.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                    handle as *const crate::iceoryx2::InputMailboxesInner,
                );
            }
        },
        (),
    )
}

/// Per-DSO host-side static InputMailboxes dispatch table.
pub(in crate::core::plugin::host_services) static HOST_INPUT_MAILBOXES_VTABLE:
    streamlib_plugin_abi::InputMailboxesVTable = streamlib_plugin_abi::InputMailboxesVTable {
    layout_version: streamlib_plugin_abi::INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    read_raw: host_input_mailboxes_read_raw,
    has_data: host_input_mailboxes_has_data,
    clone_arc: host_input_mailboxes_clone_arc,
    drop_arc: host_input_mailboxes_drop_arc,
};

/// Pointer to the [`streamlib_plugin_abi::InputMailboxesVTable`] this
/// DSO should dispatch through.
pub fn host_input_mailboxes_vtable() -> *const streamlib_plugin_abi::InputMailboxesVTable {
    match host_callbacks() {
        Some(c) if !c.input_mailboxes_vtable.is_null() => c.input_mailboxes_vtable,
        _ => &HOST_INPUT_MAILBOXES_VTABLE,
    }
}

#[cfg(test)]
mod input_mailboxes_vtable_tier1_wire_format_tests {
    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_INPUT_MAILBOXES_VTABLE.layout_version,
            streamlib_plugin_abi::INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn read_raw_returns_error_on_null_handle() {
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"any_port";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                std::ptr::null(),
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("null InputMailboxes handle"),
            "unexpected err message: {msg}"
        );
        assert!(!has_data);
        assert_eq!(out_len, 0);
    }

    #[test]
    fn read_raw_returns_error_on_invalid_utf8_port() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        let handle = std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let bad_port = b"\xff\xfe";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                handle,
                bad_port.as_ptr(),
                bad_port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("port not UTF-8"),
            "unexpected err message: {msg}"
        );
        unsafe {
            std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn read_raw_returns_no_data_on_empty_mailbox() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        inner.add_port("p", 8, crate::iceoryx2::ReadMode::ReadNextInOrder);
        let handle = std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = true; // start true to verify the wrapper sets it false
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"p";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                handle,
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 0);
        assert!(!has_data);
        assert_eq!(out_len, 0);
        unsafe {
            std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn has_data_returns_false_on_null_handle() {
        let port = b"any";
        let result = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.has_data)(std::ptr::null(), port.as_ptr(), port.len())
        };
        assert!(!result);
    }

    #[test]
    fn clone_arc_returns_null_on_null_handle() {
        let result = unsafe { (HOST_INPUT_MAILBOXES_VTABLE.clone_arc)(std::ptr::null()) };
        assert!(result.is_null());
    }

    #[test]
    fn drop_arc_is_noop_on_null_handle() {
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(std::ptr::null()) };
    }

    /// End-to-end refcount accounting: clone_arc bumps strong count
    /// and returns the same handle; drop_arc decrements. Mirrors the
    /// OutputWriter sibling test.
    #[test]
    fn clone_drop_arc_balance_strong_count() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        let inner_for_test = inner.clone();
        let raw = std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        let cloned = unsafe { (HOST_INPUT_MAILBOXES_VTABLE.clone_arc)(raw) };
        assert_eq!(cloned, raw);
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 3);
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(cloned) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(raw) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 1);
    }
}
