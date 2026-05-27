// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` PixelBuffer Arc-handle lifecycle +
//! pool-acquire / per-id-get / surface-resolve method dispatch.
//!
//! Combines two banner-bounded sections of the original file:
//!
//! - PixelBuffer Arc-handle lifecycle: `clone_pixel_buffer`,
//!   `drop_pixel_buffer`, `strong_count_pixel_buffer`, and the per-plane
//!   `plane_base_address` / `plane_size` accessors.
//! - PixelBuffer acquire / get / resolve method dispatch: pool-side
//!   `acquire_pixel_buffer`, slot-id `get_pixel_buffer`, and the
//!   surface_id-keyed `resolve_pixel_buffer_by_surface_id`.

use std::ffi::c_void;
use std::sync::Arc;

use super::super::shared::{handle_as_gpu_context, pixel_format_from_raw};
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{slice_from_raw, write_err, write_id_bytes};

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<PixelBufferRef>)` produced by
            // `PixelBuffer::new` (host-side). Re-interpreting it as
            // `*const PixelBufferRef` and bumping the strong count is the
            // documented `Arc::increment_strong_count` contract.
            unsafe {
                Arc::increment_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `host_gpu_lim_clone_pixel_buffer` and
            // `PixelBuffer::new`'s `Arc::into_raw` initial bump.
            // `Arc::decrement_strong_count` decrements; when refcount hits
            // zero the underlying `PixelBufferRef` is dropped along with
            // its platform buffer.
            unsafe {
                Arc::decrement_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_strong_count_pixel_buffer(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_gpu_lim_strong_count_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped
            // (see `PixelBuffer::new`'s `from_arc_into_raw`). We
            // reconstruct the `Arc` temporarily, read the strong count, and
            // immediately re-leak it via `Arc::into_raw` so the strong count
            // returns to its pre-call value — `Arc::strong_count_from_raw`
            // is not part of the public stable API. The reconstruction runs
            // in HOST-COMPILED code regardless of caller DSO, so the cdylib
            // never has to know `PixelBufferRef`'s in-memory layout.
            unsafe {
                let arc =
                    Arc::from_raw(handle as *const crate::core::rhi::PixelBufferRef);
                let count = Arc::strong_count(&arc);
                let _ = Arc::into_raw(arc);
                count
            }
        },
        0,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_plane_base_address_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> *mut u8 {
    run_host_extern_c(
        "host_gpu_lim_plane_base_address_pixel_buffer",
        || {
            if handle.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped;
            // the leaked strong count keeps the `PixelBufferRef` alive for
            // the duration of the call. We borrow `&PixelBufferRef` rather
            // than reconstructing the Arc to avoid touching the refcount.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_base_address(plane_index)
            }
        },
        core::ptr::null_mut(),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_plane_size_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> u64 {
    run_host_extern_c(
        "host_gpu_lim_plane_size_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: same as `host_gpu_lim_plane_base_address_pixel_buffer`.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_size(plane_index)
            }
        },
        0,
    )
}
// -------------------------------------------------------------------------
// PixelBuffer acquire / get / resolve method-dispatch
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_pixel_buffer(
    gpu_handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_pool_id_buf: *mut u8,
    out_pool_id_cap: usize,
    out_pool_id_len: *mut usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "acquire_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "acquire_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match pixel_format_from_raw(format_raw) {
                Some(f) => f,
                None => {
                    let msg = format!(
                        "acquire_pixel_buffer: invalid format_raw 0x{:08x}",
                        format_raw
                    );
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            match gpu.acquire_pixel_buffer(width, height, format) {
                Ok((pool_id, pb)) => {
                    write_id_bytes(
                        pool_id.as_str().as_bytes(),
                        out_pool_id_buf,
                        out_pool_id_cap,
                        out_pool_id_len,
                    );
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
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

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_get_pixel_buffer(
    gpu_handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_get_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "get_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "get_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "get_pixel_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let pool_id = crate::core::rhi::PixelBufferPoolId::from_str(id_str);
            match gpu.get_pixel_buffer(&pool_id) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
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

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_resolve_pixel_buffer_by_surface_id(
    gpu_handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_pixel_buffer_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.resolve_pixel_buffer_by_surface_id(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
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
