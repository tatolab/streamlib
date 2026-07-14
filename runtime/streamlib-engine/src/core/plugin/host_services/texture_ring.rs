// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `TextureRingMethodsVTable` callbacks + static vtable +
//! accessor (issue #947 — slot PluginAbiObject + method dispatch).
//!
//! Each wrapper reconstructs the ring borrow from the raw
//! `Arc::into_raw(Arc<TextureRingInner>)` handle the cdylib passes,
//! runs the inner method, and serializes the result into the plugin ABI's
//! out-parameter buffers + `i32 + err_buf` shape. All bodies wrapped
//! in `run_host_extern_c` so a panic in the inner method becomes a
//! non-zero return.

use std::ffi::c_void;

use super::host_callbacks;
use super::run_host_extern_c;
#[cfg(target_os = "linux")]
use super::shared::borrow::make_pixel_buffer_borrow;
use super::shared::wire::write_err;

// =============================================================================
// TextureRingMethodsVTable wrappers (issue #947 — slot PluginAbiObject + method
// dispatch). Each wrapper reconstructs the ring borrow from the raw
// `Arc::into_raw(Arc<TextureRingInner>)` handle the cdylib passes,
// runs the inner method, and serializes the result into the plugin ABI's
// out-parameter buffers + `i32 + err_buf` shape. All bodies are
// wrapped in `run_host_extern_c` so a panic in the inner method
// becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<TextureRingInner>)`. The leaked strong count
/// keeps the ring alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_texture_ring(
    handle: *const c_void,
) -> Option<&'static crate::core::context::TextureRingInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::core::context::TextureRingInner) })
}

/// Write the slot's POD identity bytes into the caller-provided
/// out-parameter buffers. The texture handle is bumped through the
/// host's limited-access `clone_texture` slot so the resulting
/// cdylib-side `Texture` PluginAbiObject owns the matching `Drop`-side
/// decrement.
#[cfg(target_os = "linux")]
unsafe fn write_slot_out_params(
    slot: &crate::core::context::TextureRingSlot,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
) {
    // Bump the texture's Arc through the parent limited-access
    // vtable's `clone_texture` slot — same contract every plugin ABI
    // Texture-bearing return uses. The cdylib-side `Texture`
    // PluginAbiObject's `Drop` will fire `drop_texture` to balance.
    if !slot.texture.handle.is_null() && !slot.texture.vtable.is_null() {
        unsafe {
            ((*slot.texture.vtable).clone_texture)(slot.texture.handle);
        }
    }
    unsafe {
        *out_texture_handle = slot.texture.handle;
        *out_texture_width = slot.texture.width_cached;
        *out_texture_height = slot.texture.height_cached;
        *out_texture_format_raw = slot.texture.format_raw;
        // Copy the slot's full 64-byte surface_id buffer (inline POD).
        // The cdylib reads it back through `TextureRingSlot::surface_id()`
        // which slices to `surface_id_len`.
        std::ptr::copy_nonoverlapping(
            slot.surface_id_bytes.as_ptr(),
            (*out_surface_id_bytes).as_mut_ptr(),
            crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
        );
        *out_surface_id_len = slot.surface_id_len;
        *out_slot_index = slot.slot_index;
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_texture_ring_acquire_next(
    ring_handle: *const c_void,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_acquire_next",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err(
                    "acquire_next: null ring handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture_handle.is_null()
                || out_texture_width.is_null()
                || out_texture_height.is_null()
                || out_texture_format_raw.is_null()
                || out_surface_id_bytes.is_null()
                || out_surface_id_len.is_null()
                || out_slot_index.is_null()
            {
                write_err(
                    "acquire_next: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // `acquire_next` returns an owned slot (cloned from the
            // pre-allocated `self.slots[idx]`). We write its POD
            // identity bytes through the out-params and let the
            // owned slot drop — `Texture::Drop` decrements the
            // Arc strong count `clone_texture` bumped on this side,
            // but `write_slot_out_params` already bumped a SECOND
            // strong count for the cdylib's eventual `Drop`. Net
            // effect: the cdylib's slot owns +1 strong count
            // balanced by its own Drop, exactly as if the cdylib
            // had called `Arc::into_raw(Arc::clone(...))` itself.
            let slot = ring.acquire_next();
            unsafe {
                write_slot_out_params(
                    &slot,
                    out_texture_handle,
                    out_texture_width,
                    out_texture_height,
                    out_texture_format_raw,
                    out_surface_id_bytes,
                    out_surface_id_len,
                    out_slot_index,
                );
            }
            0
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_texture_ring_copy_pixel_buffer_to_slot(
    ring_handle: *const c_void,
    slot_index: u32,
    surface_id_bytes: *const u8,
    surface_id_len: u32,
    pixel_buffer_handle: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_copy_pixel_buffer_to_slot",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err(
                    "copy_pixel_buffer_to_slot: null ring handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "copy_pixel_buffer_to_slot: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if surface_id_bytes.is_null() {
                write_err(
                    "copy_pixel_buffer_to_slot: null surface_id_bytes pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_len = (surface_id_len as usize)
                .min(crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES);
            let id_bytes = unsafe { std::slice::from_raw_parts(surface_id_bytes, id_len) };
            let Ok(surface_id) = std::str::from_utf8(id_bytes) else {
                write_err(
                    "copy_pixel_buffer_to_slot: surface_id_bytes is not valid UTF-8",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match ring
                .copy_pixel_buffer_to_slot_by_index(slot_index, surface_id, &*borrow, width, height)
            {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("copy_pixel_buffer_to_slot: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_texture_ring_slot(
    ring_handle: *const c_void,
    index: usize,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_slot",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err("slot: null ring handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_texture_handle.is_null()
                || out_texture_width.is_null()
                || out_texture_height.is_null()
                || out_texture_format_raw.is_null()
                || out_surface_id_bytes.is_null()
                || out_surface_id_len.is_null()
                || out_slot_index.is_null()
            {
                write_err(
                    "slot: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // -1 signals "index out of range" without an err_buf
            // write; the cdylib dispatch path translates this to
            // `Option::None`. Any other non-zero is a hard error.
            let Some(slot) = ring.slot(index) else {
                return -1;
            };
            unsafe {
                write_slot_out_params(
                    slot,
                    out_texture_handle,
                    out_texture_width,
                    out_texture_height,
                    out_texture_format_raw,
                    out_surface_id_bytes,
                    out_surface_id_len,
                    out_slot_index,
                );
            }
            0
        },
        1,
    )
}

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_acquire_next(
    _ring_handle: *const c_void,
    _out_texture_handle: *mut *const c_void,
    _out_texture_width: *mut u32,
    _out_texture_height: *mut u32,
    _out_texture_format_raw: *mut u32,
    _out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    _out_surface_id_len: *mut u32,
    _out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_next: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_copy_pixel_buffer_to_slot(
    _ring_handle: *const c_void,
    _slot_index: u32,
    _surface_id_bytes: *const u8,
    _surface_id_len: u32,
    _pixel_buffer_handle: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "copy_pixel_buffer_to_slot: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_slot(
    _ring_handle: *const c_void,
    _index: usize,
    _out_texture_handle: *mut *const c_void,
    _out_texture_width: *mut u32,
    _out_texture_height: *mut u32,
    _out_texture_format_raw: *mut u32,
    _out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    _out_surface_id_len: *mut u32,
    _out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "slot: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `TextureRingMethodsVTable` wired to the per-method
/// wrappers above (issue #947 — `TextureRingSlot` PluginAbiObject +
/// method dispatch).
pub static HOST_TEXTURE_RING_METHODS_VTABLE: streamlib_plugin_abi::TextureRingMethodsVTable =
    streamlib_plugin_abi::TextureRingMethodsVTable {
        layout_version: streamlib_plugin_abi::TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        acquire_next: host_texture_ring_acquire_next,
        copy_pixel_buffer_to_slot: host_texture_ring_copy_pixel_buffer_to_slot,
        slot: host_texture_ring_slot,
    };

/// Accessor for the host's static `TextureRingMethodsVTable` — used
/// by `TextureRing::from_arc_into_raw` to populate the PluginAbiObject's
/// `methods_vtable` field.
pub fn host_texture_ring_methods_vtable() -> *const streamlib_plugin_abi::TextureRingMethodsVTable {
    &HOST_TEXTURE_RING_METHODS_VTABLE
}
#[cfg(all(test, target_os = "linux"))]
mod texture_ring_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `TextureRingMethodsVTable` (issue #947). Each wrapper must
    //! reject a null ring handle before reaching any ring-side state
    //! so cdylib callers get a clean error return on the wire-format
    //! path instead of UB.
    //!
    //! End-to-end coverage (real ring + valid handles + slot
    //! round-trip) is locked by the dlopen integration test for the
    //! cross-rustc fixture, which exercises `acquire_next` +
    //! `copy_pixel_buffer_to_slot` end-to-end after the v2 wire-up.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn acquire_next_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut h: *const c_void = std::ptr::null();
        let mut w: u32 = 0;
        let mut hgt: u32 = 0;
        let mut fmt: u32 = 0;
        let mut id_bytes = [0u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut id_len: u32 = 0;
        let mut slot_index: u32 = 0;
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.acquire_next)(
                std::ptr::null(),
                &mut h as *mut *const c_void,
                &mut w as *mut u32,
                &mut hgt as *mut u32,
                &mut fmt as *mut u32,
                &mut id_bytes as *mut _,
                &mut id_len as *mut u32,
                &mut slot_index as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("acquire_next: null ring handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn copy_pixel_buffer_to_slot_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.copy_pixel_buffer_to_slot)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                32,
                32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("copy_pixel_buffer_to_slot: null ring handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn slot_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut h: *const c_void = std::ptr::null();
        let mut w: u32 = 0;
        let mut hgt: u32 = 0;
        let mut fmt: u32 = 0;
        let mut id_bytes = [0u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut id_len: u32 = 0;
        let mut slot_index: u32 = 0;
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.slot)(
                std::ptr::null(),
                0,
                &mut h as *mut *const c_void,
                &mut w as *mut u32,
                &mut hgt as *mut u32,
                &mut fmt as *mut u32,
                &mut id_bytes as *mut _,
                &mut id_len as *mut u32,
                &mut slot_index as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("slot: null ring handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}
