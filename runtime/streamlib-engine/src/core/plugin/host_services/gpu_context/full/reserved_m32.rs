// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reserved-but-unlanded `GpuContextFullAccessVTable` v11 host bodies
//! (M32 one-shot slot reservation, #1253).
//!
//! Each of the thirteen v11 slots ships a typed NotYetProvided-style
//! stub here: a non-zero return ([`NOT_YET_PROVIDED_RC`]) + a
//! descriptive `write_err` message, wrapped in the `run_host_extern_c`
//! panic net — never `todo!()` / `unimplemented!()`, never an unguarded
//! unwind across the ABI. The per-surface fill-in issues (#1258–#1262)
//! replace these bodies against the frozen slots without touching the
//! vtable struct again.
//!
//! The four drop-only slots (`drop_present_target`,
//! `drop_encoder_session`, `drop_decoder_session`, `drop_texture_readback`)
//! are defensive no-ops until minting lands: no create slot yields a
//! real handle yet, so drop is never called with one.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    ColorTraitsRepr, OpaqueFdExportDescriptorRepr, RawWindowHandleRepr,
    VideoDecoderSessionDescriptorRepr, VideoEncoderSessionDescriptorRepr,
};

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided, write_err};
#[cfg(target_os = "linux")]
use super::super::scope_token::with_full_scope_or_err;

// ============================================================================
// Present target (#1258)
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_present_target(
    _gpu_handle: *const c_void,
    _window: *const RawWindowHandleRepr,
    _width: u32,
    _height: u32,
    _vsync: u32,
    _color: *const ColorTraitsRepr,
    _out_present_target: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_present_target",
        || not_yet_provided("create_present_target", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_present_target(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_present_target",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

// ============================================================================
// Hardware video encode/decode (#1259)
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_encoder_session(
    _gpu_handle: *const c_void,
    _desc: *const VideoEncoderSessionDescriptorRepr,
    _out_session: *mut *const c_void,
    _out_aligned_width: *mut u32,
    _out_aligned_height: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_encoder_session",
        || not_yet_provided("create_encoder_session", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_encoder_session(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_encoder_session",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_decoder_session(
    _gpu_handle: *const c_void,
    _desc: *const VideoDecoderSessionDescriptorRepr,
    _out_session: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_decoder_session",
        || not_yet_provided("create_decoder_session", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_decoder_session(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_decoder_session",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

// ============================================================================
// Exportable timeline semaphore (#1260)
// ============================================================================

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_exportable_timeline_semaphore(
    gpu_handle: *const c_void,
    initial_value: u64,
    out_timeline: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_exportable_timeline_semaphore",
        || -> i32 {
            if out_timeline.is_null() {
                write_err(
                    "create_exportable_timeline_semaphore: null out_timeline pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_exportable_timeline_semaphore",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_exportable_timeline_semaphore(initial_value),
            );
            match result {
                Some(Ok(arc)) => {
                    let wire = crate::core::rhi::HostTimelineSemaphore::from_arc(arc);
                    // SAFETY: `out_timeline` checked non-null; the cdylib
                    // provided a 16-byte `MaybeUninit<HostTimelineSemaphore>`
                    // slot. `ptr::write` moves the wire envelope in without
                    // dropping the uninitialized destination.
                    unsafe {
                        std::ptr::write(
                            out_timeline as *mut crate::core::rhi::HostTimelineSemaphore,
                            wire,
                        )
                    };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_exportable_timeline_semaphore(
    _gpu_handle: *const c_void,
    _initial_value: u64,
    _out_timeline: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_exportable_timeline_semaphore",
        || {
            write_err(
                "create_exportable_timeline_semaphore: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

// Texture readback (#1261) is no longer reserved — its real host bodies
// (`host_gpu_full_create_texture_readback` /
// `host_gpu_full_drop_texture_readback`) landed in the sibling
// `texture_readback` module against the frozen v11 slots.

// ============================================================================
// OPAQUE_FD / CUDA buffer surface (#1262)
// ============================================================================

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_opaque_fd_export_buffer(
    _gpu_handle: *const c_void,
    _byte_size: u64,
    _device_local: u8,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_opaque_fd_export_buffer",
        || {
            not_yet_provided(
                "create_opaque_fd_export_buffer",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_export_storage_buffer_opaque_fd(
    _gpu_handle: *const c_void,
    _buffer: *const c_void,
    out_descriptor: *mut OpaqueFdExportDescriptorRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_export_storage_buffer_opaque_fd",
        || {
            // FD-failure convention: write `fd = -1` so a caller never
            // reads a stale live fd from the descriptor on the error
            // path (double-close guard).
            if !out_descriptor.is_null() {
                // SAFETY: caller-provided out-pointer; the reserved stub
                // only writes the fd sentinel field.
                unsafe { (*out_descriptor).fd = -1 };
            }
            not_yet_provided(
                "export_storage_buffer_opaque_fd",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_wrap_storage_buffer_as_pixel_buffer(
    _gpu_handle: *const c_void,
    _storage_buffer: *const c_void,
    _width: u32,
    _height: u32,
    _bytes_per_pixel: u32,
    _format_raw: u32,
    _out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wrap_storage_buffer_as_pixel_buffer",
        || {
            not_yet_provided(
                "wrap_storage_buffer_as_pixel_buffer",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_copy_texture_to_storage_buffer_and_signal(
    _gpu_handle: *const c_void,
    _texture_handle: *const c_void,
    _source_layout_raw: i32,
    _storage_buffer: *const c_void,
    _consume_done_handle: *const c_void,
    _consume_done_wait_value: u64,
    _produce_done_handle: *const c_void,
    _produce_done_signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_copy_texture_to_storage_buffer_and_signal",
        || {
            not_yet_provided(
                "copy_texture_to_storage_buffer_and_signal",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        NOT_YET_PROVIDED_RC,
    )
}

#[cfg(test)]
mod reserved_m32_wire_format_tests {
    //! Tier-1 wire-format tests for the reserved v11 FullAccess slots.
    //! Each i32-returning slot must return [`NOT_YET_PROVIDED_RC`] with a
    //! "not yet provided" message; each drop slot is a null-safe no-op.
    //!
    //! Mental-revert: replace a stub body with `unimplemented!()` and the
    //! matching test aborts the process instead of asserting the typed
    //! refusal — the panic net + typed non-zero is exactly what these
    //! lock in until the fill-in issues land the real bodies.

    use std::ffi::c_void;

    use super::super::super::HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE;
    use super::NOT_YET_PROVIDED_RC;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn create_present_target_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_present_target)(
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                1,
                std::ptr::null(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(
            err_buf_as_str(&buf, len).contains("create_present_target: not yet provided"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn create_encoder_session_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let mut aw: u32 = 0;
        let mut ah: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_encoder_session)(
                std::ptr::null(),
                std::ptr::null(),
                &mut out,
                &mut aw,
                &mut ah,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("create_encoder_session: not yet provided"));
    }

    #[test]
    fn create_decoder_session_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_decoder_session)(
                std::ptr::null(),
                std::ptr::null(),
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("create_decoder_session: not yet provided"));
    }

    // #1260 landed the exportable-timeline mint slot: the reserved
    // NotYetProvided stub is replaced by a real host body. These two
    // GPU-free wire tests lock the arg-guard behavior (the positive mint
    // round-trip is hardware-gated in `tests/`). Mental-revert: drop the
    // `out_timeline.is_null()` guard and the null-out test below UB-writes
    // a 16-byte `HostTimelineSemaphore` through a null pointer.
    #[test]
    fn create_exportable_timeline_semaphore_reports_null_out_param() {
        let (mut buf, mut len) = make_err_buf();
        // Null scope token is fine here — the null-out guard fires first.
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_exportable_timeline_semaphore)(
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: null out_timeline pointer"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: not available on this platform")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_exportable_timeline_semaphore_reports_invalid_scope_on_null_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 16];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_exportable_timeline_semaphore)(
                std::ptr::null(),
                0,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: invalid escalate scope"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn create_opaque_fd_export_buffer_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 32];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_opaque_fd_export_buffer)(
                std::ptr::null(),
                4096,
                1,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(
            err_buf_as_str(&buf, len).contains("create_opaque_fd_export_buffer: not yet provided")
        );
    }

    #[test]
    fn export_storage_buffer_opaque_fd_writes_minus_one_fd_on_refusal() {
        let (mut buf, mut len) = make_err_buf();
        let mut desc = streamlib_plugin_abi::OpaqueFdExportDescriptorRepr {
            fd: 7, // start non-negative to prove the stub writes -1
            handle_type_raw: 0,
            size: 0,
            device_uuid: [0u8; 16],
        };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.export_storage_buffer_opaque_fd)(
                std::ptr::null(),
                std::ptr::null(),
                &mut desc,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        // FD-failure convention: fd written -1 on any non-zero return.
        assert_eq!(desc.fd, -1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("export_storage_buffer_opaque_fd: not yet provided")
        );
    }

    #[test]
    fn wrap_storage_buffer_as_pixel_buffer_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                4,
                0,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("wrap_storage_buffer_as_pixel_buffer: not yet provided")
        );
    }

    #[test]
    fn copy_texture_to_storage_buffer_and_signal_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.copy_texture_to_storage_buffer_and_signal)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("copy_texture_to_storage_buffer_and_signal: not yet provided")
        );
    }

    #[test]
    fn drop_slots_are_null_safe_no_ops() {
        // Every drop slot must be a null-safe no-op — a caller dropping a
        // never-populated handle (e.g. after a failed create) must not
        // deref or panic. `drop_texture_readback`'s real body landed with
        // #1261 but its null path is still part of this contract.
        unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_present_target)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_encoder_session)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_decoder_session)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_texture_readback)(std::ptr::null());
        }
    }
}
