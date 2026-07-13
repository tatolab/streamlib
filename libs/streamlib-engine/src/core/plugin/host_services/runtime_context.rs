// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `RuntimeContextVTable` callbacks + static vtable + accessor.
//!
//! The host installs `&HOST_RUNTIME_CONTEXT_VTABLE` into [`HostServices`]
//! at `host_services_for_self` time. Every callback derefs the opaque
//! `ctx` pointer back to a host-owned `&RuntimeContext` and routes
//! through that type's normal Rust accessor — the cdylib treats it as
//! opaque, dispatching through fn pointers and reading nothing about
//! layout.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_plugin_abi::{RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, RuntimeContextVTable};

use crate::core::context::{RuntimeContext, SharedAudioClock};
use crate::core::runtime::RuntimeOperations;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_id_bytes;

unsafe extern "C" fn host_rcv_runtime_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    run_host_extern_c(
        "host_rcv_runtime_id_copy",
        || {
            if ctx.is_null() {
                if !out_len.is_null() {
                    // SAFETY: caller-provided `out_len` is writable.
                    unsafe { *out_len = 0 };
                }
                return 0;
            }
            // SAFETY: host-side construction passes &RuntimeContext as ctx.
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            let id_bytes = rc.runtime_id().as_str().as_bytes();
            write_id_bytes(id_bytes, out_buf, out_buf_cap, out_len)
        },
        0,
    )
}

unsafe extern "C" fn host_rcv_processor_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> isize {
    run_host_extern_c(
        "host_rcv_processor_id_copy",
        || {
            if ctx.is_null() {
                // Mirror the panic-default — `-1` encodes "no processor
                // id" (shared/global ctx), which is the closest defined
                // value to "ctx unavailable". The cdylib treats `-1` as
                // Option::None.
                if !out_len.is_null() {
                    // SAFETY: caller-provided `out_len` is writable.
                    unsafe { *out_len = 0 };
                }
                return -1;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            match rc.processor_id() {
                Some(pid) => {
                    let bytes = pid.as_str().as_bytes();
                    write_id_bytes(bytes, out_buf, out_buf_cap, out_len) as isize
                }
                None => -1,
            }
        },
        -1,
    )
}

unsafe extern "C" fn host_rcv_is_paused(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_is_paused",
        || {
            if ctx.is_null() {
                // Conservative default — a null ctx means the host's
                // RuntimeContext is unreachable, so the processor
                // should not keep running. Mirrors the panic-default.
                return true;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.is_paused()
        },
        // Pause-on-panic is the conservative default: a panicking
        // is_paused() callback shouldn't keep a runaway processor
        // running. `true` halts further work until the host clears
        // the panic state.
        true,
    )
}

unsafe extern "C" fn host_rcv_should_process(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_should_process",
        || {
            if ctx.is_null() {
                // Same conservative default — null ctx halts further
                // work until the host clears state.
                return false;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.should_process()
        },
        // Same conservative default as is_paused — false halts the
        // processor until the host clears state.
        false,
    )
}

unsafe extern "C" fn host_rcv_gpu_full_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_full_access",
        || {
            // FullAccess is engine-only today — the cdylib-facing
            // shim embeds `GpuContextFullAccess` by value alongside
            // its handle/vtable pair, so the cdylib never reaches
            // through this callback. Returns null until a future
            // phase wires plugin ABI FullAccess dispatch.
            std::ptr::null()
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_gpu_limited_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_limited_access",
        || std::ptr::null(),
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_audio_clock_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_audio_clock_handle",
        || {
            if ctx.is_null() {
                return std::ptr::null();
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // The shim's audio-clock handle is a `&SharedAudioClock` —
            // the accompanying [`HOST_AUDIO_CLOCK_VTABLE`] callbacks
            // cast it back to that type and invoke the Rust trait
            // methods.
            rc.audio_clock() as *const SharedAudioClock as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_runtime_ops_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_runtime_ops_handle",
        || {
            if ctx.is_null() {
                return std::ptr::null();
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // `rc.runtime()` produces an owned `Arc<dyn
            // RuntimeOperations>` each call; the per-RuntimeContext
            // handle we hand the cdylib must outlive the call
            // boundary. We keep the canonical handle as
            // `&Arc<dyn RuntimeOperations>` borrowed out of the
            // RuntimeContext's internal storage, which lives as long
            // as the RuntimeContext itself.
            rc.runtime_operations_ref() as *const Arc<dyn RuntimeOperations> as *const c_void
        },
        std::ptr::null(),
    )
}

/// Static [`RuntimeContextVTable`] installed once per process and
/// reused for every cdylib's `RuntimeContext*Access` shim
/// construction. The host-side `RuntimeContextFullAccess::new` /
/// `RuntimeContextLimitedAccess::new` constructors capture
/// `&HOST_RUNTIME_CONTEXT_VTABLE` directly.
pub static HOST_RUNTIME_CONTEXT_VTABLE: RuntimeContextVTable = RuntimeContextVTable {
    layout_version: RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    runtime_id_copy: host_rcv_runtime_id_copy,
    processor_id_copy: host_rcv_processor_id_copy,
    is_paused: host_rcv_is_paused,
    should_process: host_rcv_should_process,
    gpu_full_access: host_rcv_gpu_full_access,
    gpu_limited_access: host_rcv_gpu_limited_access,
    audio_clock_handle: host_rcv_audio_clock_handle,
    runtime_ops_handle: host_rcv_runtime_ops_handle,
};

/// Pointer to the [`RuntimeContextVTable`] this plugin should dispatch
/// through. In the host process this returns the host's local
/// `&HOST_RUNTIME_CONTEXT_VTABLE` static (the canonical vtable). In
/// a cdylib `install_host_services` has populated the cached pointer
/// from `HostServices`, so this returns the HOST'S vtable — meaning
/// every callback invocation lands in host-resident extern "C"
/// functions, not in the cdylib's local copy of those functions.
/// That distinction is load-bearing: the host's functions read
/// host-owned Rust types (`RuntimeContext`) with the host's compiled
/// layout, while the cdylib's local copies would re-interpret the
/// same memory through the cdylib's compiled layout.
pub fn host_runtime_context_vtable() -> *const RuntimeContextVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_context_vtable.is_null() => c.runtime_context_vtable,
        _ => &HOST_RUNTIME_CONTEXT_VTABLE,
    }
}

#[cfg(test)]
mod runtime_context_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `RuntimeContextVTable` callbacks. Each test calls the wrapper
    //! with a null `ctx` and asserts the documented default return
    //! value (matching `run_host_extern_c`'s panic-default). Without
    //! the guard the wrapper would deref a null `*const RuntimeContext`
    //! before returning, SIGSEGVing the test runner.
    //!
    //! Mental-revert check: removing any guard reverts the wrapper to
    //! `unsafe { &*(null) }` then a field read on the resulting
    //! reference — SIGSEGV, test failure (process abort) rather than
    //! the documented default.

    use super::*;

    #[test]
    fn runtime_id_copy_returns_zero_on_null_ctx() {
        let mut out = [0u8; 16];
        let mut len: usize = 999;
        let n = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.runtime_id_copy)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        assert_eq!(n, 0);
        assert_eq!(len, 0, "out_len must be cleared on null ctx");
    }

    #[test]
    fn processor_id_copy_returns_minus_one_on_null_ctx() {
        let mut out = [0u8; 16];
        let mut len: usize = 999;
        let n = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.processor_id_copy)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        assert_eq!(n, -1, "-1 encodes Option::None");
        assert_eq!(len, 0, "out_len must be cleared on null ctx");
    }

    #[test]
    fn is_paused_returns_true_on_null_ctx() {
        let v = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.is_paused)(std::ptr::null()) };
        assert!(v, "pause-on-failure is the conservative default");
    }

    #[test]
    fn should_process_returns_false_on_null_ctx() {
        let v = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.should_process)(std::ptr::null()) };
        assert!(!v, "halt-on-failure is the conservative default");
    }

    /// Locks the documented placeholder behaviour of
    /// `gpu_full_access`: the wrapper ignores `ctx` and returns null
    /// unconditionally because plugin ABI FullAccess wiring lives on
    /// the inline-by-value shim today, not through this callback.
    /// This is NOT a null-handle-guard lock (no guard to revert);
    /// it's a placeholder-shape lock — if a future change wires
    /// real FullAccess dispatch here, this test fails and forces
    /// the implementor to revisit.
    #[test]
    fn gpu_full_access_returns_null_unconditionally_today() {
        let p = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.gpu_full_access)(std::ptr::null()) };
        assert!(p.is_null());
    }

    /// Companion to [`gpu_full_access_returns_null_unconditionally_today`].
    /// Same placeholder-shape lock; same caveat (not a null-handle
    /// guard — the wrapper ignores `_ctx`).
    #[test]
    fn gpu_limited_access_returns_null_unconditionally_today() {
        let p = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.gpu_limited_access)(std::ptr::null()) };
        assert!(p.is_null());
    }

    #[test]
    fn audio_clock_handle_returns_null_on_null_ctx() {
        let p = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.audio_clock_handle)(std::ptr::null()) };
        assert!(p.is_null());
    }

    #[test]
    fn runtime_ops_handle_returns_null_on_null_ctx() {
        let p = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.runtime_ops_handle)(std::ptr::null()) };
        assert!(p.is_null());
    }
}

#[cfg(test)]
mod runtime_context_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_RUNTIME_CONTEXT_VTABLE`].
    //!
    //! Per-callback null-handle coverage lives in
    //! [`runtime_context_vtable_null_handle_guards`] above. This module
    //! completes the tier-1 set with the wire-format invariant the
    //! null-handle suite doesn't cover: the static vtable's
    //! `layout_version` field must match the constant cdylibs read
    //! against.
    //!
    //! No callback on `RuntimeContextVTable` takes an out-param or
    //! a variant-typed input, so the "null out-param" and
    //! "invalid input" tier-1 categories don't apply here.

    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RUNTIME_CONTEXT_VTABLE.layout_version,
            streamlib_plugin_abi::RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
        );
    }
}
