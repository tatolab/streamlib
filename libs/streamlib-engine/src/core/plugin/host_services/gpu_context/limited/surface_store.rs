// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` surface_store accessors.
//!
//! - `surface_store` returns the host's `SurfaceStore` `Arc::into_raw`
//!   pointer (the cdylib reconstitutes into a borrow), or a null PluginAbiObject
//!   when no store is registered.
//! - `check_out_surface` resolves a surface_id via the store, packaging
//!   the resulting FD-bearing record for the cdylib's
//!   `SurfaceLookup` consumer.

use std::ffi::c_void;

use super::super::shared::handle_as_gpu_context;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{slice_from_raw, write_err};

// -------------------------------------------------------------------------
// GpuContextLimitedAccessVTable — surface_store accessors
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_surface_store(
    gpu_handle: *const c_void,
    out_store: *mut c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_surface_store",
        || {
            // Always-clear: write a null-handle PluginAbiObject first so the
            // caller has a defined state even on error paths.
            if !out_store.is_null() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        crate::core::context::SurfaceStore::null(),
                    );
                }
            }
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                return;
            };
            if out_store.is_null() {
                return;
            }
            // `gpu.surface_store()` returns `Option<SurfaceStore>` —
            // a fresh PluginAbiObject with Arc refcount already bumped when
            // Some. We write it into the out-param; the caller (cdylib
            // or host) takes ownership.
            if let Some(store) = gpu.surface_store() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        store,
                    );
                }
            }
            // else: out_store already holds the null-handle PluginAbiObject.
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_check_out_surface(
    gpu_handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_check_out_surface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "check_out_surface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "check_out_surface: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "check_out_surface: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.check_out_surface(id_str) {
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


