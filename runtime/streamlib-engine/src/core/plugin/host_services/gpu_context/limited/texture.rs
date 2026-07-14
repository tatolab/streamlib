// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` Texture / PooledTextureHandle
//! lifecycle + texture-cache method dispatch (v4 — #957, #908).
//!
//! Covers four banner-bounded concerns from the original file:
//!
//! - **Texture Arc-handle lifecycle**: `clone_texture` / `drop_texture`
//!   bumping the Arc<TextureInner> refcount the cdylib carries.
//! - **Texture native DMA-BUF FD export** (Phase F, #957): host side
//!   of `Texture::native_handle`.
//! - **PooledTextureHandle drop**: paired with the `Box::into_raw` in
//!   `PooledTextureHandle::from_parts`.
//! - **Texture-cache method dispatch**: register, update layout,
//!   acquire, resolve by surface_id, unregister.

use std::ffi::c_void;
use std::sync::Arc;

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{slice_from_raw, write_err};
use super::super::shared::handle_as_gpu_context;

// -------------------------------------------------------------------------
// Texture Arc-handle lifecycle
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_texture(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<TextureInner>)` produced by host
            // code (see `Texture::from_arc_into_raw`).
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_texture(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `Texture::from_arc_into_raw` and any prior
            // `clone_texture` bumps.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Texture::native_handle DMA-BUF FD export (Phase F, #957)
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_texture_native_dma_buf_fd(
    texture_handle: *const c_void,
) -> i64 {
    run_host_extern_c(
        "host_gpu_lim_texture_native_dma_buf_fd",
        || {
            if texture_handle.is_null() {
                return -1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `texture_handle` is the
                // `Arc::into_raw(Arc<TextureInner>)` pointer carried as the
                // cdylib-side `Texture::handle` field. Borrowing as
                // `&TextureInner` does not touch the refcount — the
                // caller's `Texture` keeps the Arc alive for the duration
                // of this dispatch.
                let inner =
                    unsafe { &*(texture_handle as *const crate::core::rhi::texture::TextureInner) };
                match inner.inner.export_dma_buf_fd() {
                    Ok(fd) => i64::from(fd),
                    Err(_) => -1,
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                // DMA-BUF is a Linux concept. macOS / Windows native
                // handles are deferred until those cdylib adapter paths
                // resume (see #908's AI Agent Notes).
                let _ = texture_handle;
                -1
            }
        },
        -1,
    )
}

// -------------------------------------------------------------------------
// PooledTextureHandle lifecycle — drop-only (v4)
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_pooled_texture_handle(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_pooled_texture_handle",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw(Box<...>)` in
            // `PooledTextureHandle::from_parts`. Reclaiming via
            // `Box::from_raw` runs `Drop for PooledTextureHandleInner`
            // which releases the pool slot exactly once.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::context::texture_pool::PooledTextureHandleInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Method dispatch — Texture-related (v4)
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture_handle: *const c_void,
    initial_layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_register_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if texture_handle.is_null() {
                return;
            }
            // SAFETY: `texture_handle` is `Arc::into_raw(Arc<TextureInner>)`-shaped.
            // Bump the refcount so we can hand the cache its own owned
            // Arc; the caller's Texture continues to own its own.
            unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
            // SAFETY: same shape as above; from_raw + the bump above
            // gives us a fresh Arc with the right refcount.
            let texture_arc = unsafe {
                Arc::from_raw(texture_handle as *const crate::core::rhi::texture::TextureInner)
            };
            let inner_ref = &*texture_arc;
            let width = inner_ref.width();
            let height = inner_ref.height();
            let format = inner_ref.format();
            // Re-wrap into a Texture via the host's from_arc_into_raw
            // helper — leaks the Arc back into the texture cache shape.
            let texture = crate::core::rhi::texture::Texture::from_arc_into_raw(
                texture_arc,
                width,
                height,
                format,
            );
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(initial_layout_raw);
                gpu.register_texture_with_layout(id_str, texture, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = initial_layout_raw;
                gpu.register_texture(id_str, texture);
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_update_texture_registration_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_update_texture_registration_layout",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
                gpu.update_texture_registration_layout(id_str, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (id_str, layout_raw);
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_texture(
    handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    out_pooled_handle: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_texture: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pooled_handle.is_null() {
                write_err(
                    "acquire_texture: null out_pooled_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    let msg = format!("acquire_texture: invalid format_raw {}", format_raw);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            let usage = streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let desc = crate::core::context::TexturePoolDescriptor {
                width,
                height,
                format,
                usage,
                label: None,
            };
            match gpu.acquire_texture(&desc) {
                Ok(pooled) => {
                    // Move the host-built PooledTextureHandle into the
                    // caller's out-slot. The caller (cdylib) owns it
                    // after this — its Drop runs `drop_pooled_texture_handle`.
                    unsafe {
                        std::ptr::write(
                            out_pooled_handle as *mut crate::core::context::PooledTextureHandle,
                            pooled,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_resolve_texture_by_surface_id(
    handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    has_layout: i32,
    layout_raw: i32,
    width: u32,
    height: u32,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture.is_null() {
                write_err(
                    "resolve_texture_by_surface_id: null out_texture",
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
                        "resolve_texture_by_surface_id: surface_id not valid UTF-8",
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
            match gpu.resolve_texture_by_surface_id(id_str, texture_layout, width, height) {
                Ok(texture) => {
                    // Hand the texture to the caller's out-slot. The
                    // caller (cdylib) owns it after this — its Drop
                    // runs `drop_texture`.
                    unsafe {
                        std::ptr::write(out_texture as *mut crate::core::rhi::Texture, texture);
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_unregister_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
) {
    run_host_extern_c(
        "host_gpu_lim_unregister_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            gpu.unregister_texture(id_str);
        },
        (),
    )
}

#[cfg(test)]
mod tier1_wire_format_tests {
    //! Tier-1 wire-format test for the Phase F
    //! `texture_native_dma_buf_fd` slot (#908 / #957). The slot is the
    //! plugin ABI landing for `Texture::native_handle` on Linux and
    //! returns the DMA-BUF FD widened to `i64`; sentinel `-1` encodes
    //! the `Option::None` case. A null texture handle must be a clean
    //! `-1` (no panic, no UB) — the wrapper short-circuits before any
    //! cast through `*const TextureInner`.

    use super::super::super::HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE;

    #[test]
    fn texture_native_dma_buf_fd_returns_minus_one_on_null_handle() {
        // Null texture_handle is the cdylib-shaped "Texture wasn't
        // minted yet / was already dropped" case. The slot returns
        // `-1` (= `Option::None` in the Rust-side wrapper) without
        // panicking and without touching the null pointer.
        let fd = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_native_dma_buf_fd)(std::ptr::null())
        };
        assert_eq!(
            fd, -1,
            "null texture_handle must produce -1 sentinel (None)"
        );
    }
}
