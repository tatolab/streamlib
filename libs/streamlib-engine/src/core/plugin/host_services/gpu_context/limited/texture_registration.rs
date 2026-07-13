// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` TextureRegistration Arc-handle
//! lifecycle + method dispatch (v6).
//!
//! Combines two banner-bounded sections of the original file: the
//! `Arc<TextureRegistration>` clone/drop pair plus the per-method
//! dispatch wrappers (`texture`, `current_layout`, `update_layout`,
//! `resolve_by_surface_id`). See
//! `docs/architecture/texture-registration.md` for the engine-wide
//! per-surface lifecycle record this vtable surface exposes to the
//! cdylib.

use std::ffi::c_void;
use std::sync::Arc;

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{slice_from_raw, write_err};
use super::super::shared::handle_as_gpu_context;

// -------------------------------------------------------------------------
// TextureRegistration Arc-handle lifecycle
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_texture_registration(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_texture_registration(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `TextureRegistration::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// TextureRegistration method dispatch (v6)
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_texture_registration_texture(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_texture",
        || {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped;
            // the Arc's strong count keeps the inner alive. We return
            // a pointer to the inner's `texture` field; the caller
            // (cdylib) deref's it as `*const Texture`. The pointer is
            // alive as long as the caller's `TextureRegistration` is.
            unsafe {
                let inner = &*(handle
                    as *const crate::core::context::texture_registration::TextureRegistrationInner);
                &inner.texture as *const crate::core::rhi::Texture as *const c_void
            }
        },
        std::ptr::null(),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_texture_registration_current_layout(
    handle: *const c_void,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_current_layout",
        || {
            if handle.is_null() {
                return 0; // VK_IMAGE_LAYOUT_UNDEFINED
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `handle` is `Arc::into_raw(...)`-shaped.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .load(std::sync::atomic::Ordering::Acquire)
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = handle;
                0
            }
        },
        0,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_texture_registration_update_layout(
    handle: *const c_void,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_update_layout",
        || {
            if handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: same shape as
                // `host_gpu_lim_texture_registration_current_layout`.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .store(layout_raw, std::sync::atomic::Ordering::Release);
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (handle, layout_raw);
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_resolve_texture_registration_by_surface_id(
    handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    has_layout: i32,
    layout_raw: i32,
    width: u32,
    height: u32,
    out_registration: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_registration_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_registration_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_registration.is_null() {
                write_err(
                    "resolve_texture_registration_by_surface_id: null out_registration",
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
                        "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_registration_by_surface_id(
                id_str,
                texture_layout,
                width,
                height,
            ) {
                Ok(reg) => {
                    // SAFETY: out_registration points at caller-allocated
                    // stack storage for a `TextureRegistration` value.
                    unsafe {
                        std::ptr::write(
                            out_registration as *mut crate::core::context::TextureRegistration,
                            reg,
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
