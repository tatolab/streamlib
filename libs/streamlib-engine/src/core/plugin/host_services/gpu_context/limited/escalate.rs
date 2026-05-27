// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` escalate scope transition callbacks
//! (Phase C3).
//!
//! `escalate_begin` mints an opaque `u64` scope token bound to the
//! caller's `Arc<GpuContext>` in the engine's
//! `escalate_scope_registry`; `escalate_end` removes the binding,
//! runs `wait_device_idle`, and releases the gate. The cdylib hands
//! the token back as the `gpu_handle` slot on every FullAccess
//! method (validated via `super::super::scope_token::with_full_scope_or_err`).

use std::ffi::c_void;
use std::sync::Arc;

use super::super::shared::handle_as_gpu_context;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;

/// Begin an escalate scope on the supplied `gpu_handle`. Mints a
/// unique opaque token via
/// [`crate::core::context::escalate_scope_registry::begin_escalate_scope`]
/// and writes it into `*out_scope_token`. Blocking on the gate is
/// expected — the host's escalate gate serializes against any
/// concurrent escalate scope on the same `GpuContext`.
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_escalate_begin(
    handle: *const c_void,
    out_scope_token: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_begin",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "escalate_begin: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            };
            if out_scope_token.is_null() {
                write_err(
                    "escalate_begin: null out_scope_token",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            }
            // begin_escalate_scope clones the Arc into the registry
            // and enters the gate; both operations succeed without
            // returning a fallible value.
            let token = crate::core::context::escalate_scope_registry::begin_escalate_scope(
                Arc::clone(gpu),
            );
            // SAFETY: out_scope_token is non-null per the check above.
            // Token encoding is just the u64 serial reinterpreted as
            // pointer-shaped; cdylib treats it as opaque.
            unsafe { *out_scope_token = token as *const c_void };
            0
        },
        1,
    )
}

/// End an escalate scope. Removes the bound `Arc<GpuContext>` from
/// the registry (releasing the escalate gate), then runs
/// [`GpuContext::wait_device_idle`] to match the host-mode escalate
/// path's scope-end semantics. Idempotent for stale or never-issued
/// tokens.
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_escalate_end(
    _handle: *const c_void,
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_end",
        || {
            let token = scope_token as u64;
            // Resolve the Arc BEFORE removing it from the registry so
            // we can call wait_device_idle. If the token is stale or
            // never-issued, this returns None — silently no-op (the
            // gate was never acquired by this token, so there's
            // nothing to release).
            let arc_clone = crate::core::context::escalate_scope_registry::with_scope(
                token,
                Arc::clone,
            );
            let removed = crate::core::context::escalate_scope_registry::end_escalate_scope(token);
            if !removed {
                return 0i32;
            }
            match arc_clone.as_ref().map(|arc| arc.wait_device_idle()) {
                Some(Ok(())) | None => 0,
                Some(Err(e)) => {
                    write_err(
                        &format!("escalate_end: wait_device_idle failed: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}
