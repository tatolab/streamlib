// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `SurfaceStoreVTable` callbacks + static vtable + accessor.
//!
//! Every callback derefs `handle` as `&SurfaceStoreInner` and calls
//! the inner method directly. The Arc strong count keeps the inner
//! alive for the duration of the dispatch.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_plugin_abi::{SURFACE_STORE_VTABLE_LAYOUT_VERSION, SurfaceStoreVTable};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{slice_from_raw, write_err, write_id_bytes};

// =========================================================================
// SurfaceStoreVTable — host-side callbacks
// =========================================================================
//
// Every callback derefs `handle` as `&SurfaceStoreInner` and calls
// the inner method directly. The Arc strong count keeps the inner
// alive for the duration of the dispatch.

#[inline]
unsafe fn ss_inner(
    handle: *const c_void,
) -> Option<&'static crate::core::context::surface_store::SurfaceStoreInner> {
    if handle.is_null() {
        None
    } else {
        // SAFETY: caller-supplied contract: `handle` is
        // `Arc::into_raw(Arc<SurfaceStoreInner>)`-shaped.
        Some(unsafe { &*(handle as *const crate::core::context::surface_store::SurfaceStoreInner) })
    }
}

unsafe extern "C" fn host_ss_clone_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_ss_clone_handle",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::surface_store::SurfaceStoreInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_ss_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_ss_drop_handle",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::surface_store::SurfaceStoreInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_ss_connect(
    handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_connect",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("connect: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match inner.connect() {
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

unsafe extern "C" fn host_ss_disconnect(
    handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_disconnect",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("disconnect: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match inner.disconnect() {
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

unsafe extern "C" fn host_ss_check_in(
    handle: *const c_void,
    pixel_buffer: *const c_void,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_check_in",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("check_in: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err("check_in: null pixel_buffer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            match inner.check_in(pb) {
                Ok(id) => {
                    let bytes = id.as_bytes();
                    write_id_bytes(bytes, out_id_buf, out_id_cap, out_id_len);
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

unsafe extern "C" fn host_ss_check_out(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_check_out",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("check_out: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "check_out: null out_pixel_buffer",
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
                        "check_out: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.check_out(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(out_pixel_buffer as *mut crate::core::rhi::PixelBuffer, pb);
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

unsafe extern "C" fn host_ss_register_buffer(
    handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    pixel_buffer: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_buffer",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_buffer: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err(
                    "register_buffer: null pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let pool_id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let pool_id = match std::str::from_utf8(pool_id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.register_buffer(pool_id, pb) {
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

unsafe extern "C" fn host_ss_lookup_buffer(
    handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_lookup_buffer",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("lookup_buffer: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "lookup_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pool_id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let pool_id = match std::str::from_utf8(pool_id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "lookup_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.lookup_buffer(pool_id) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(out_pixel_buffer as *mut crate::core::rhi::PixelBuffer, pb);
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

unsafe extern "C" fn host_ss_release(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_release",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("release: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "release: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.release(id_str) {
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
unsafe extern "C" fn host_ss_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture: *const c_void,
    produce_done_handle: *const c_void,
    consume_done_handle: *const c_void,
    layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_texture",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_texture: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture.is_null() {
                write_err(
                    "register_texture: null texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let tex = unsafe { &*(texture as *const crate::core::rhi::Texture) };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: produce_done_handle / consume_done_handle, when
            // non-null, each point at the engine-owned
            // `Arc<HostVulkanTimelineSemaphore>` (passed by `&Arc<...>`
            // from engine code through `&*` cast). The
            // single-writer-per-edge model is documented in
            // `docs/architecture/adapter-timeline-single-writer.md`.
            let produce_done = unsafe {
                if produce_done_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(produce_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            let consume_done = unsafe {
                if consume_done_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(consume_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
            match inner.register_texture(id_str, tex, produce_done, consume_done, layout) {
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

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_register_texture(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _texture: *const c_void,
    _produce_done_handle: *const c_void,
    _consume_done_handle: *const c_void,
    _layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "register_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_register_pixel_buffer_with_timeline(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    pixel_buffer: *const c_void,
    produce_done_handle: *const c_void,
    consume_done_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_pixel_buffer_with_timeline",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_pixel_buffer_with_timeline: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err(
                    "register_pixel_buffer_with_timeline: null pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_pixel_buffer_with_timeline: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let produce_done = unsafe {
                if produce_done_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(produce_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            let consume_done = unsafe {
                if consume_done_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(consume_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            match inner.register_pixel_buffer_with_timeline(id_str, pb, produce_done, consume_done)
            {
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

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_register_pixel_buffer_with_timeline(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _pixel_buffer: *const c_void,
    _produce_done_handle: *const c_void,
    _consume_done_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "register_pixel_buffer_with_timeline: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_lookup_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_texture: *mut c_void,
    out_layout_raw: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_lookup_texture",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("lookup_texture: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_texture.is_null() || out_layout_raw.is_null() {
                write_err(
                    "lookup_texture: null out_texture or out_layout_raw",
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
                        "lookup_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.lookup_texture(id_str) {
                Ok((tex, layout)) => {
                    unsafe {
                        std::ptr::write(out_texture as *mut crate::core::rhi::Texture, tex);
                        *out_layout_raw = layout.0;
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

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_lookup_texture(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _out_texture: *mut c_void,
    _out_layout_raw: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "lookup_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_update_image_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_update_image_layout",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "update_image_layout: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "update_image_layout: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
            match inner.update_image_layout(id_str, layout) {
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

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_update_image_layout(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "update_image_layout: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Static [`SurfaceStoreVTable`] installed once per process. Paired
/// with the per-SurfaceStore handle returned by
/// [`HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`]`::surface_store`.
pub static HOST_SURFACE_STORE_VTABLE: SurfaceStoreVTable = SurfaceStoreVTable {
    layout_version: SURFACE_STORE_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    clone_handle: host_ss_clone_handle,
    drop_handle: host_ss_drop_handle,
    connect: host_ss_connect,
    disconnect: host_ss_disconnect,
    check_in: host_ss_check_in,
    check_out: host_ss_check_out,
    register_buffer: host_ss_register_buffer,
    lookup_buffer: host_ss_lookup_buffer,
    release: host_ss_release,
    register_texture: host_ss_register_texture,
    register_pixel_buffer_with_timeline: host_ss_register_pixel_buffer_with_timeline,
    lookup_texture: host_ss_lookup_texture,
    update_image_layout: host_ss_update_image_layout,
};

/// Pointer to the [`SurfaceStoreVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_gpu_context_limited_access_vtable`].
pub fn host_surface_store_vtable() -> *const SurfaceStoreVTable {
    match host_callbacks() {
        Some(c) if !c.surface_store_vtable.is_null() => c.surface_store_vtable,
        _ => &HOST_SURFACE_STORE_VTABLE,
    }
}
#[cfg(test)]
mod surface_store_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_SURFACE_STORE_VTABLE`].
    //!
    //! Every callback on the vtable goes through `ss_inner`, which
    //! already short-circuits on a null handle. This module covers
    //! the full tier-1 contract:
    //!
    //! - `layout_version_matches_constant` — locks the wire-format
    //!   layout version against the cdylib-visible constant.
    //! - `clone_handle` / `drop_handle` null-handle locks — the
    //!   Arc-lifecycle pair.
    //! - For each result-returning callback (10 of them): null-
    //!   handle → rc=1 with per-callback err marker.
    //! - For the 4 Linux-only callbacks (`register_texture`,
    //!   `register_pixel_buffer_with_timeline`, `lookup_texture`,
    //!   `update_image_layout`): same Linux contract; non-Linux
    //!   stubs return rc=1 with "not available on this platform".
    //!
    //! Mental-revert: removing the null-handle guard from
    //! `ss_inner` makes every result-returning test SIGSEGV (the
    //! wrapper would deref a null `*const SurfaceStoreInner`). The
    //! per-callback inner null checks (`if pixel_buffer.is_null()`,
    //! `if texture.is_null()`, `if out_pixel_buffer.is_null()`,
    //! `if out_texture.is_null() || out_layout_raw.is_null()`) are
    //! NOT individually locked by this module — tier-1 scope is the
    //! `ss_inner` null-handle guard plus the layout-version match
    //! plus the per-callback err-marker text. The per-arg inner
    //! checks belong to a deeper coverage tier.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // ------------------------------------------------------------------
    // Layout-version match
    // ------------------------------------------------------------------

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_SURFACE_STORE_VTABLE.layout_version,
            streamlib_plugin_abi::SURFACE_STORE_VTABLE_LAYOUT_VERSION,
        );
    }

    // ------------------------------------------------------------------
    // Handle-lifecycle (clone_handle / drop_handle)
    // ------------------------------------------------------------------

    #[test]
    fn clone_handle_handles_null_no_crash() {
        unsafe {
            (HOST_SURFACE_STORE_VTABLE.clone_handle)(std::ptr::null());
        }
    }

    #[test]
    fn drop_handle_handles_null_no_crash() {
        unsafe {
            (HOST_SURFACE_STORE_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    // ------------------------------------------------------------------
    // Result-returning callbacks: null-handle returns rc=1 with err msg
    // ------------------------------------------------------------------

    #[test]
    fn connect_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.connect)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("connect: null handle"));
    }

    #[test]
    fn disconnect_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.disconnect)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("disconnect: null handle"));
    }

    #[test]
    fn check_in_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 64];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.check_in)(
                std::ptr::null(),
                std::ptr::null(),
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("check_in: null handle"));
    }

    #[test]
    fn check_out_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out = [0u8; 256];
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.check_out)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("check_out: null handle"));
    }

    #[test]
    fn register_buffer_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("register_buffer: null handle"),);
    }

    #[test]
    fn lookup_buffer_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let mut out = [0u8; 256];
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("lookup_buffer: null handle"),);
    }

    #[test]
    fn release_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.release)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("release: null handle"));
    }

    // ------------------------------------------------------------------
    // Linux-only callbacks
    // ------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn register_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("register_texture: null handle"),);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn register_pixel_buffer_with_timeline_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_pixel_buffer_with_timeline)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("register_pixel_buffer_with_timeline: null handle")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lookup_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_tex = [0u8; 256];
        let mut out_layout: i32 = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_tex.as_mut_ptr() as *mut c_void,
                &mut out_layout,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("lookup_texture: null handle"),);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn update_image_layout_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.update_image_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("update_image_layout: null handle"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn register_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("register_texture: not available on this platform")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn register_pixel_buffer_with_timeline_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_pixel_buffer_with_timeline)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("register_pixel_buffer_with_timeline: not available on this platform")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn lookup_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_tex = [0u8; 256];
        let mut out_layout: i32 = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_tex.as_mut_ptr() as *mut c_void,
                &mut out_layout,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("lookup_texture: not available on this platform")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn update_image_layout_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.update_image_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("update_image_layout: not available on this platform")
        );
    }
}
