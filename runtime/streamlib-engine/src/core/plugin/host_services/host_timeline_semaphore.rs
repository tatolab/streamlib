// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `HostTimelineSemaphoreMethodsVTable` static + accessor
//! (M32 reservation, #1260).
//!
//! Every slot ships a typed NotYetProvided-style stub under the panic
//! net until the exportable-timeline fill-in (#1260) lands the real
//! bodies. `clone_handle` / `drop_handle` are null-safe no-ops until
//! minting exists (no `create_exportable_timeline_semaphore` yields a
//! real handle yet, so refcount slots are never called with one).

use std::ffi::c_void;

use streamlib_plugin_abi::{
    HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION, HostTimelineSemaphoreMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

unsafe extern "C" fn host_timeline_semaphore_clone_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_clone_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

unsafe extern "C" fn host_timeline_semaphore_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

unsafe extern "C" fn host_timeline_semaphore_wait(
    _handle: *const c_void,
    _value: u64,
    _timeout_ns: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_wait",
        || not_yet_provided("wait", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_timeline_semaphore_signal(
    _handle: *const c_void,
    _value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_signal",
        || not_yet_provided("signal", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_timeline_semaphore_current_value(
    _handle: *const c_void,
    _out_value: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_current_value",
        || not_yet_provided("current_value", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_timeline_semaphore_export_opaque_fd(
    _handle: *const c_void,
    out_fd: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_export_opaque_fd",
        || {
            // FD-failure convention: write -1 so a caller never reads a
            // stale live fd on the error path (double-close guard).
            if !out_fd.is_null() {
                // SAFETY: caller-provided out-pointer; the reserved stub
                // only writes the fd sentinel.
                unsafe { *out_fd = -1 };
            }
            not_yet_provided("export_opaque_fd", err_buf, err_buf_cap, err_len)
        },
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `HostTimelineSemaphoreMethodsVTable`, wired to the reserved
/// stubs. #1260 replaces the stub bodies (and wires clone/drop to
/// `Arc::increment/decrement_strong_count` against
/// `HostVulkanTimelineSemaphore`).
pub static HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE: HostTimelineSemaphoreMethodsVTable =
    HostTimelineSemaphoreMethodsVTable {
        layout_version: HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_timeline_semaphore_clone_handle,
        drop_handle: host_timeline_semaphore_drop_handle,
        wait: host_timeline_semaphore_wait,
        signal: host_timeline_semaphore_signal,
        current_value: host_timeline_semaphore_current_value,
        export_opaque_fd: host_timeline_semaphore_export_opaque_fd,
    };

/// Accessor for the host's static `HostTimelineSemaphoreMethodsVTable`.
pub fn host_timeline_semaphore_methods_vtable() -> *const HostTimelineSemaphoreMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.host_timeline_semaphore_methods_vtable.is_null() => {
            c.host_timeline_semaphore_methods_vtable
        }
        _ => &HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.layout_version,
            HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn clone_and_drop_handle_are_null_safe_no_ops() {
        unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.clone_handle)(std::ptr::null());
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    #[test]
    fn wait_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.wait)(
                std::ptr::null(),
                1,
                u64::MAX,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("wait: not yet provided"));
    }

    #[test]
    fn signal_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.signal)(
                std::ptr::null(),
                1,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("signal: not yet provided"));
    }

    #[test]
    fn current_value_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut v = 0u64;
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.current_value)(
                std::ptr::null(),
                &mut v,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("current_value: not yet provided"));
    }

    #[test]
    fn export_opaque_fd_writes_minus_one_fd_on_refusal() {
        let (mut buf, mut len) = make_err_buf();
        let mut fd: i32 = 7; // start non-negative to prove the stub writes -1
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.export_opaque_fd)(
                std::ptr::null(),
                &mut fd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert_eq!(fd, -1);
        assert!(err_buf_as_str(&buf, len).contains("export_opaque_fd: not yet provided"));
    }
}
