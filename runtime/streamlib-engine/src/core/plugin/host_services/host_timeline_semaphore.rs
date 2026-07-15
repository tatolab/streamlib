// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `HostTimelineSemaphoreMethodsVTable` static + accessor
//! (#1260).
//!
//! Each method dispatches against the host's
//! [`crate::vulkan::rhi::HostVulkanTimelineSemaphore`] behind the
//! `handle` field of the `HostTimelineSemaphore` PluginAbiObject.
//! `clone_handle` / `drop_handle` run
//! `Arc::increment/decrement_strong_count` in host-compiled code; the
//! remaining slots borrow the inner (no refcount touch) and forward to
//! the real Vulkan path (`vkWaitSemaphores`, `vkSignalSemaphore`,
//! `vkGetSemaphoreCounterValue`, `vkGetSemaphoreFdKHR`). Every body is
//! wrapped in the `run_host_extern_c` panic net; a null handle is a
//! typed error (never a deref), and `export_opaque_fd` writes `-1` on
//! any non-zero return (double-close guard).
//!
//! Linux-only on the host side — the exportable timeline rides the
//! Vulkan RHI path. Non-Linux hosts ship clean-error stubs (no
//! `create_exportable_timeline_semaphore` yields a handle there, so the
//! method slots are never reached with a real one).

use std::ffi::c_void;

use streamlib_plugin_abi::{
    HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION, HostTimelineSemaphoreMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_err;

// ============================================================================
// Handle lifetime — Arc refcount accounting in host-compiled code.
// ============================================================================

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_clone_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_clone_handle",
        || {
            if !handle.is_null() {
                // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`
                // minted by `HostTimelineSemaphore::from_arc`; incrementing
                // the strong count balances a future `drop_handle`.
                unsafe {
                    std::sync::Arc::increment_strong_count(
                        handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore,
                    );
                }
            }
        },
        (),
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_timeline_semaphore_clone_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_clone_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_drop_handle",
        || {
            if !handle.is_null() {
                // SAFETY: matched with the `Arc::into_raw` in
                // `HostTimelineSemaphore::from_arc` and any `clone_handle`
                // bumps; releases the semaphore at strong-count zero.
                unsafe {
                    std::sync::Arc::decrement_strong_count(
                        handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore,
                    );
                }
            }
        },
        (),
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_timeline_semaphore_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_timeline_semaphore_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

// ============================================================================
// Method dispatch.
// ============================================================================

/// Borrow the inner `HostVulkanTimelineSemaphore` behind a wire handle
/// without touching the refcount. `None` when `handle` is null.
#[cfg(target_os = "linux")]
unsafe fn timeline_inner<'a>(
    handle: *const c_void,
) -> Option<&'a crate::vulkan::rhi::HostVulkanTimelineSemaphore> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`;
    // the leaked strong count outlives the dispatch (the cdylib holds an
    // owned handle for the duration of the call).
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore) })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_wait(
    handle: *const c_void,
    value: u64,
    timeout_ns: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_wait",
        || -> i32 {
            let Some(sem) = (unsafe { timeline_inner(handle) }) else {
                write_err("wait: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match sem.wait(value, timeout_ns) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_signal(
    handle: *const c_void,
    value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_signal",
        || -> i32 {
            let Some(sem) = (unsafe { timeline_inner(handle) }) else {
                write_err("signal: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match sem.signal_host(value) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_current_value(
    handle: *const c_void,
    out_value: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_current_value",
        || -> i32 {
            if out_value.is_null() {
                write_err(
                    "current_value: null out_value pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let Some(sem) = (unsafe { timeline_inner(handle) }) else {
                write_err("current_value: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match sem.current_value() {
                Ok(v) => {
                    // SAFETY: caller-provided out-pointer, checked non-null.
                    unsafe { std::ptr::write(out_value, v) };
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_timeline_semaphore_export_opaque_fd(
    handle: *const c_void,
    out_fd: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_export_opaque_fd",
        || -> i32 {
            // FD-failure convention: write -1 up front so every non-zero
            // return path leaves a sentinel, never a stale live fd
            // (double-close guard).
            if !out_fd.is_null() {
                // SAFETY: caller-provided out-pointer.
                unsafe { *out_fd = -1 };
            }
            let Some(sem) = (unsafe { timeline_inner(handle) }) else {
                write_err(
                    "export_opaque_fd: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match sem.export_opaque_fd() {
                Ok(fd) => {
                    if out_fd.is_null() {
                        // Caller gave no out slot but owns the fd it can't
                        // receive — close it rather than leak.
                        // SAFETY: `fd` is a fresh kernel fd from vkGetSemaphoreFdKHR.
                        unsafe { libc::close(fd) };
                        write_err(
                            "export_opaque_fd: null out_fd pointer",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    // SAFETY: caller-provided out-pointer, checked non-null.
                    unsafe { *out_fd = fd };
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ---------------------------------------------------------------------------
// Non-Linux stubs — clean typed error; never reached with a real handle.
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
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
        || {
            write_err(
                "wait: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_timeline_semaphore_signal(
    _handle: *const c_void,
    _value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_signal",
        || {
            write_err(
                "signal: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_timeline_semaphore_current_value(
    _handle: *const c_void,
    _out_value: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_timeline_semaphore_current_value",
        || {
            write_err(
                "current_value: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
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
            if !out_fd.is_null() {
                // SAFETY: caller-provided out-pointer; write the fd sentinel.
                unsafe { *out_fd = -1 };
            }
            write_err(
                "export_opaque_fd: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

/// Host-side `HostTimelineSemaphoreMethodsVTable`, wired to the real
/// dispatch bodies (#1260).
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
    //! Tier-1 wire-format coverage for the exportable-timeline methods
    //! vtable. The null-handle refusal + FD `-1` sentinel are GPU-free
    //! (they short-circuit before any Vulkan call); the positive
    //! signal/wait/export round-trip below is hardware-gated (needs a live
    //! device with `VK_KHR_external_semaphore_fd`).
    //!
    //! Mental-revert: drop the `handle.is_null()` guard in
    //! `timeline_inner` and `host_timeline_semaphore_wait` UB-derefs a
    //! null pointer as `*const HostVulkanTimelineSemaphore` — the runner
    //! SIGSEGVs instead of asserting the typed refusal below.

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
    fn wait_reports_null_handle() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("wait: null handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("wait: not available on this platform"));
    }

    #[test]
    fn signal_reports_null_handle() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("signal: null handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("signal: not available on this platform"));
    }

    #[test]
    fn current_value_reports_null_handle_and_leaves_out_untouched() {
        let (mut buf, mut len) = make_err_buf();
        let mut v = 4242u64; // sentinel — must be untouched on the error path
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.current_value)(
                std::ptr::null(),
                &mut v,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(v, 4242, "out_value must be untouched on the error path");
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("current_value: null handle"));
    }

    #[test]
    fn current_value_reports_null_out_param() {
        // Linux-only: the non-Linux stub refuses before the out-param
        // check, so the specific "null out_value" message is Linux-only.
        #[cfg(target_os = "linux")]
        {
            let (mut buf, mut len) = make_err_buf();
            // A non-null (but bogus) handle so the out-param guard is the
            // first thing to fire. The body checks `out_value.is_null()`
            // BEFORE any handle deref, so the dangling handle is never
            // touched.
            let bogus_handle = std::ptr::NonNull::<c_void>::dangling().as_ptr() as *const c_void;
            let rc = unsafe {
                (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.current_value)(
                    bogus_handle,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            };
            assert_eq!(rc, 1);
            assert!(err_buf_as_str(&buf, len).contains("current_value: null out_value pointer"));
        }
    }

    #[test]
    fn export_opaque_fd_writes_minus_one_fd_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut fd: i32 = 7; // start non-negative to prove the body writes -1
        let rc = unsafe {
            (HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE.export_opaque_fd)(
                std::ptr::null(),
                &mut fd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(fd, -1, "FD-failure convention: -1 on every non-zero return");
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("export_opaque_fd: null handle"));
    }

    /// Positive path: mint an exportable timeline exactly as the FullAccess
    /// `create_exportable_timeline_semaphore` slot does
    /// (`HostVulkanTimelineSemaphore::new_exportable` →
    /// `HostTimelineSemaphore::from_arc`), then drive it through the real
    /// methods-vtable bodies: `signal` → `current_value` → `wait` (on an
    /// already-reached value) → `export_opaque_fd` (valid kernel fd) →
    /// `clone_handle`/`drop_handle` refcount round-trip. Locks the #1260
    /// host bodies end-to-end against a live device.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn exportable_timeline_round_trips_through_methods_vtable() {
        use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => return, // Vulkan unavailable in this env — skip.
        };
        let arc = match HostVulkanTimelineSemaphore::new_exportable(device.device(), 0) {
            Ok(s) => std::sync::Arc::new(s),
            Err(_) => return, // VK_KHR_external_semaphore_fd unavailable — skip.
        };

        // Mint the wire envelope exactly as the FullAccess slot does.
        let wire = crate::core::rhi::HostTimelineSemaphore::from_arc(arc);
        let handle = wire.handle;
        let vt = &HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE;
        let (mut buf, mut len) = make_err_buf();

        // signal → 5
        let rc = unsafe { (vt.signal)(handle, 5, buf.as_mut_ptr(), buf.len(), &mut len) };
        assert_eq!(rc, 0, "signal failed: {}", err_buf_as_str(&buf, len));

        // current_value == 5
        let mut v = 0u64;
        let rc =
            unsafe { (vt.current_value)(handle, &mut v, buf.as_mut_ptr(), buf.len(), &mut len) };
        assert_eq!(rc, 0, "current_value failed: {}", err_buf_as_str(&buf, len));
        assert_eq!(v, 5);

        // wait on an already-reached value returns immediately.
        let rc = unsafe { (vt.wait)(handle, 5, 0, buf.as_mut_ptr(), buf.len(), &mut len) };
        assert_eq!(rc, 0, "wait failed: {}", err_buf_as_str(&buf, len));

        // export a valid OPAQUE_FD.
        let mut fd: i32 = -1;
        let rc = unsafe {
            (vt.export_opaque_fd)(handle, &mut fd, buf.as_mut_ptr(), buf.len(), &mut len)
        };
        assert_eq!(
            rc,
            0,
            "export_opaque_fd failed: {}",
            err_buf_as_str(&buf, len)
        );
        assert!(fd >= 0, "exported sync fd should be a valid kernel fd");
        unsafe { libc::close(fd) };

        // clone/drop refcount round-trip (balanced — no leak, no double-free).
        unsafe {
            (vt.clone_handle)(handle);
            (vt.drop_handle)(handle);
        }
        drop(wire);
    }
}
