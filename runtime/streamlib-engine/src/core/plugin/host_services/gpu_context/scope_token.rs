// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-issued scope-token bridge for FullAccess callback bodies.
//!
//! Post-C3 the cdylib reaches the FullAccess vtable by first calling
//! the LimitedAccess vtable's `escalate_begin` callback, which mints
//! an opaque `u64` token bound to an `Arc<GpuContext>` in the
//! engine's `escalate_scope_registry`. The cdylib hands that token
//! (cast to `*const c_void`) back as the `gpu_handle` slot on every
//! FullAccess method. Each callback body validates the token via
//! [`with_full_scope_or_err`] before dispatching to the resolved
//! context; a stale, never-issued, or null token returns an
//! "invalid escalate scope" error without touching the vtable's
//! out-params.

use std::ffi::c_void;
use std::sync::Arc;

use super::super::shared::wire::write_err;

/// Resolve a scope token to its bound `Arc<GpuContext>` and run the
/// closure. On miss (null token, stale token, never-issued token)
/// writes an "invalid escalate scope" message into `err_buf` and
/// returns `None`. FullAccess vtable callback bodies use this in
/// place of the host-mode `Box<Arc<GpuContext>>` deref (which is
/// never reached from cdylib code post-C3).
pub(in crate::core::plugin::host_services) fn with_full_scope_or_err<F, R>(
    scope_token: *const c_void,
    op: &str,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    f: F,
) -> Option<R>
where
    F: FnOnce(&Arc<crate::core::context::GpuContext>) -> R,
{
    let token = scope_token as u64;
    match crate::core::context::escalate_scope_registry::with_scope(token, f) {
        Some(r) => Some(r),
        None => {
            write_err(
                &format!(
                    "{op}: invalid escalate scope (token stale, never-issued, \
                     or null)"
                ),
                err_buf,
                err_buf_cap,
                err_len,
            );
            None
        }
    }
}
