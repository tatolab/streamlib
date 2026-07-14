// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `OutputWriterVTable` callbacks + static vtable + accessor.
//!
//! Each wrapper reconstructs the inner borrow from the raw
//! `Arc::into_raw(Arc<OutputWriterInner>)` handle the cdylib passes,
//! runs the inner method, and serializes the result into the FFI's
//! out-parameter buffers + `i32 + err_buf` shape. All bodies wrapped
//! in `run_host_extern_c` so a panic in the inner method becomes a
//! non-zero return.

use std::ffi::c_void;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_extern_err;

// =============================================================================
// OutputWriterVTable wrappers (issue #894 — LAST shared-Rust-type
// crossing in the plugin ABI). Each wrapper reconstructs the inner
// borrow from the raw `Arc::into_raw(Arc<OutputWriterInner>)` handle
// the cdylib passes, runs the inner method, and serializes the
// result into the FFI's out-parameter buffers + `i32 + err_buf`
// shape. All bodies wrapped in `run_host_extern_c` so a panic in
// the inner method becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<crate::iceoryx2::OutputWriterInner>)`. The
/// leaked strong count keeps the inner alive for the call's
/// duration.
unsafe fn handle_as_output_writer_inner(
    handle: *const c_void,
) -> Option<&'static crate::iceoryx2::OutputWriterInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::iceoryx2::OutputWriterInner) })
}

unsafe extern "C" fn host_output_writer_write_raw(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
    data_ptr: *const u8,
    data_len: usize,
    timestamp_ns: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_output_writer_write_raw",
        || -> i32 {
            let Some(inner) = (unsafe { handle_as_output_writer_inner(handle) }) else {
                write_extern_err(
                    "write_raw: null OutputWriter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if port_ptr.is_null() || (port_len > 0 && data_ptr.is_null() && data_len > 0) {
                write_extern_err(
                    "write_raw: null port_ptr or data_ptr",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let port = match std::str::from_utf8(port_bytes) {
                Ok(s) => s,
                Err(e) => {
                    write_extern_err(
                        &format!("write_raw: port not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let data = if data_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
            };
            match inner.write_raw(port, data, timestamp_ns) {
                Ok(()) => 0,
                Err(e) => {
                    write_extern_err(&e.to_string(), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_output_writer_has_port(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
) -> bool {
    run_host_extern_c(
        "host_output_writer_has_port",
        || -> bool {
            let Some(inner) = (unsafe { handle_as_output_writer_inner(handle) }) else {
                return false;
            };
            if port_ptr.is_null() {
                return false;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let Ok(port) = std::str::from_utf8(port_bytes) else {
                return false;
            };
            inner.has_port(port)
        },
        false,
    )
}

pub(crate) unsafe extern "C" fn host_output_writer_clone_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_output_writer_clone_arc",
        || -> *const c_void {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: handle came from Arc::into_raw. We need to
            // reconstruct a non-owning &Arc<Inner> view to call
            // Arc::increment_strong_count, but Arc::from_raw +
            // ManuallyDrop is the idiomatic way to do that for
            // refcount accounting without consuming the strong ref.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::increment_strong_count(
                    handle as *const crate::iceoryx2::OutputWriterInner,
                );
            }
            handle
        },
        std::ptr::null(),
    )
}

pub(crate) unsafe extern "C" fn host_output_writer_drop_arc(handle: *const c_void) {
    run_host_extern_c(
        "host_output_writer_drop_arc",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle came from Arc::into_raw; we release
            // exactly the strong reference Arc::into_raw leaked.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::decrement_strong_count(
                    handle as *const crate::iceoryx2::OutputWriterInner,
                );
            }
        },
        (),
    )
}

/// Per-DSO host-side static OutputWriter dispatch table.
pub(in crate::core::plugin::host_services) static HOST_OUTPUT_WRITER_VTABLE:
    streamlib_plugin_abi::OutputWriterVTable = streamlib_plugin_abi::OutputWriterVTable {
    layout_version: streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    write_raw: host_output_writer_write_raw,
    has_port: host_output_writer_has_port,
    clone_arc: host_output_writer_clone_arc,
    drop_arc: host_output_writer_drop_arc,
};

/// Pointer to the [`streamlib_plugin_abi::OutputWriterVTable`] this DSO
/// should dispatch through. Host mode resolves to the local static
/// `HOST_OUTPUT_WRITER_VTABLE`; cdylib mode resolves to the
/// host-installed pointer from [`HostServices::output_writer_vtable`].
pub fn host_output_writer_vtable() -> *const streamlib_plugin_abi::OutputWriterVTable {
    match host_callbacks() {
        Some(c) if !c.output_writer_vtable.is_null() => c.output_writer_vtable,
        _ => &HOST_OUTPUT_WRITER_VTABLE,
    }
}

#[cfg(test)]
mod output_writer_vtable_tier1_wire_format_tests {
    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_OUTPUT_WRITER_VTABLE.layout_version,
            streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn write_raw_returns_error_on_null_handle() {
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"any_port";
        let data = b"payload";
        let rc = unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.write_raw)(
                std::ptr::null(),
                port.as_ptr(),
                port.len(),
                data.as_ptr(),
                data.len(),
                0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("null OutputWriter handle"),
            "unexpected err message: {msg}"
        );
    }

    #[test]
    fn write_raw_returns_error_on_invalid_utf8_port() {
        let inner = std::sync::Arc::new(crate::iceoryx2::OutputWriterInner::new());
        let handle = std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let bad_port = b"\xff\xfe"; // not utf-8
        let data = b"payload";
        let rc = unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.write_raw)(
                handle,
                bad_port.as_ptr(),
                bad_port.len(),
                data.as_ptr(),
                data.len(),
                0,
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
            std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn has_port_returns_false_on_null_handle() {
        let port = b"any_port";
        let result = unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.has_port)(std::ptr::null(), port.as_ptr(), port.len())
        };
        assert!(!result);
    }

    #[test]
    fn clone_arc_returns_null_on_null_handle() {
        let result = unsafe { (HOST_OUTPUT_WRITER_VTABLE.clone_arc)(std::ptr::null()) };
        assert!(result.is_null());
    }

    #[test]
    fn drop_arc_is_noop_on_null_handle() {
        // No panic, no segfault — the function returns cleanly.
        unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(std::ptr::null());
        }
    }

    /// End-to-end refcount accounting: clone_arc on a real Arc::into_raw
    /// handle bumps the strong count by one and returns the same handle;
    /// drop_arc decrements. Pair them and the inner survives until the
    /// last decrement.
    #[test]
    fn clone_drop_arc_balance_strong_count() {
        let inner = std::sync::Arc::new(crate::iceoryx2::OutputWriterInner::new());
        let inner_for_test = inner.clone();
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        let raw = std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        // strong_count now 2 again (the into_raw handle counts).
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);

        let cloned = unsafe { (HOST_OUTPUT_WRITER_VTABLE.clone_arc)(raw) };
        assert_eq!(cloned, raw);
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 3);

        unsafe { (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(cloned) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);

        unsafe { (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(raw) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 1);
    }
}
