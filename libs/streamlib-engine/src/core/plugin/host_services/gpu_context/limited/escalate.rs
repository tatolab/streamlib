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
            // Drain the device (`wait_device_idle`) WHILE the escalate
            // gate is still held, then release it. The prior shape
            // released the gate first and waited afterward, racing
            // another scope's gated `vkCreateComputePipelines` on
            // NVIDIA — see `end_escalate_scope_draining` and
            // `docs/learnings/concurrent-vkdevicewaitidle-threading.md`.
            // `None` = stale / never-issued token: a silent no-op (the
            // gate was never claimed by this token).
            match crate::core::context::escalate_scope_registry::end_escalate_scope_draining(token) {
                None | Some(Ok(())) => 0,
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
#[cfg(test)]
mod gpu_lim_escalate_vtable_tests {
    //! Tier-1 wire-format + round-trip tests for C3's escalate_begin
    //! and escalate_end vtable entries.
    //!
    //! Tests that construct a real `GpuContext` carry `#[serial]` to
    //! prevent the NVIDIA Linux dual-`VkDevice` SIGSEGV
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`) when run
    //! against other VkDevice-creating tests in the workspace lib
    //! suite.

    use std::ffi::c_void;
    use std::sync::Arc;

    use super::super::super::{
        HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE, HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    };
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    /// Build a host-mode gpu_handle (the `Box<Arc<GpuContext>>`-shaped
    /// pointer that `GpuContextLimitedAccess::new` produces) so the
    /// `escalate_begin` callback can run end-to-end against a real
    /// `Arc<GpuContext>`. Skips when no GPU device is available.
    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    /// Free a host_handle minted by `make_host_handle` — pairs with
    /// the `Box::into_raw`.
    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe {
            Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>)
        };
    }

    #[test]
    fn escalate_begin_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                std::ptr::null(),
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("escalate_begin: null gpu handle"), "got: {msg}");
        assert!(token.is_null(), "scope token must not be written on error");
    }

    #[test]
    #[serial]
    fn escalate_begin_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping escalate_begin null-out test: no GPU device"
            );
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("escalate_begin: null out_scope_token"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    fn escalate_end_is_idempotent_for_stale_token() {
        // escalate_end with a never-issued token is a clean no-op
        // (returns 0; doesn't release any gate). Documented as
        // idempotent in the registry.
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                std::ptr::null(),
                u64::MAX as *const c_void, // never-issued token
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(len, 0, "no error message expected for stale token");
    }

    #[test]
    #[serial]
    fn round_trip_begin_then_end_releases_gate() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping round-trip test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let begin_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin_rc, 0);
        assert!(!token.is_null(), "scope token must be written on success");

        let end_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(end_rc, 0);

        // Begin again on the same handle — gate must have been
        // released, so this succeeds without blocking. (If the gate
        // hadn't released, this would deadlock.)
        let mut token2: *const c_void = std::ptr::null();
        let begin2_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin2_rc, 0);
        assert!(!token2.is_null());
        assert_ne!(token, token2, "tokens must be unique per begin call");

        let _ = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_with_valid_token_resolves_scope() {
        // End-to-end: begin a scope, get a valid token, invoke a
        // FullAccess vtable callback with the token + a valid
        // descriptor. The callback's scope-token lookup must succeed
        // (no "invalid escalate scope" error). The actual allocation
        // may succeed or fail depending on the Vulkan environment
        // (render-target DMA-BUF availability, EGL DRM modifier
        // probe), but EITHER outcome proves the scope lookup passed:
        // a success returns rc=0 with `out_texture` populated; a
        // failure returns rc=1 with an error message that does NOT
        // contain "invalid escalate scope".
        //
        // (Mentally revert `with_full_scope_or_err` to always return
        // None — this test fails because the error message would
        // then contain "invalid escalate scope".)
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping valid-token test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        assert!(!token.is_null());

        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // Rgba8Unorm — valid format; forces scope lookup to run
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };

        if rc != 0 {
            // Allocation failed for an environment reason; assert the
            // failure was NOT a scope-lookup miss.
            let msg = err_buf_as_str(&buf2, len2);
            assert!(
                !msg.contains("invalid escalate scope"),
                "scope-token lookup must succeed inside an active \
                 scope; got: {msg}"
            );
        } else {
            // Allocation succeeded — definitively proves scope lookup
            // worked. The Texture in `out` owns a live handle; its
            // Drop will fire the vtable's drop_texture as the test
            // returns.
            assert!(!out.handle.is_null(), "out_texture handle populated");
            // SAFETY: `out` was overwritten by `ptr::write` from the
            // callback with a valid Texture; let its normal Drop run
            // to release the underlying handle via the vtable.
        }

        // Clean up the scope.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_fails_after_escalate_end() {
        // Closes the scope-token validation loop: a token used after
        // escalate_end fires returns the InvalidEscalateScope error
        // (matches the "calls after escalate_end return
        // InvalidEscalateScope" exit criterion).
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping post-end test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }

        // Token is now stale — using it on any FullAccess callback
        // returns "invalid escalate scope".
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // valid format
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf2, len2);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid escalate scope"
            ),
            "got: {msg}"
        );

        unsafe { free_host_handle(handle) };
    }
}

