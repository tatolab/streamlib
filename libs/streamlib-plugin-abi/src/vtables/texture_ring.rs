// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `TextureRingMethodsVTable` — per-type vtable for `TextureRing` method dispatch.

use core::ffi::c_void;

/// Layout version of [`crate::TextureRingMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: `TextureRingSlot` β-shape lands (fixed-size POD
///   `surface_id_bytes: [u8; 64]` + `surface_id_len: u32` +
///   `slot_index: u32`, replacing the heap `String` surface_id) and
///   the method slots `acquire_next` / `copy_pixel_buffer_to_slot` /
///   `slot` get wired through. Each cross-DSO call uses caller-
///   provided out-parameter buffers for the slot's typed POD bytes;
///   the slot's `texture` β-shape is itself a `(handle, vtable,
///   POD)` triple cloned through its own per-type Clone vtable.
pub const TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Per-type method-dispatch vtable for the `TextureRing` β-shape
/// (issue #907 Phase E + #947 slot β-shape + method dispatch).
///
/// `TextureRing` keeps `clone_*` / `drop_*` dispatch on the parent
/// [`crate::GpuContextFullAccessVTable`] (PR #918's Phase D shape); this
/// vtable adds per-method slots for everything *else* the β-shape
/// exposes — `acquire_next`, `copy_pixel_buffer_to_slot`, `slot`.
/// POD getters (`len`, `is_empty`, `width`, `height`, `format`) are
/// cached on the β-shape struct itself and don't need vtable slots.
///
/// **Slot-return shape (v2):** `acquire_next` and `slot` return the
/// slot's owned typed POD bytes via caller-provided out-parameter
/// buffers (`out_texture_handle` + cached POD slot for the slot's
/// `Texture`, plus `out_surface_id_bytes` + `out_surface_id_len` +
/// `out_slot_index`). The slot's `Texture` β-shape itself manages
/// its Arc lifetime through the parent
/// [`crate::GpuContextLimitedAccessVTable`]'s `clone_texture` /
/// `drop_texture` slots — when the host wrapper hands a cloned
/// `Texture` handle back, the cdylib's `Texture::Drop` will fire
/// `drop_texture` to balance the clone. Surface IDs travel inline
/// as fixed 64-byte buffers (UUIDs are 36 ASCII chars; the 64-byte
/// budget leaves headroom for the bytes + length without a heap
/// allocation crossing the DSO boundary).
///
/// **`copy_pixel_buffer_to_slot` (v2):** the caller passes the
/// slot's `slot_index` (looks up the slot's pre-allocated upload
/// resources host-side) and `surface_id_bytes` + `surface_id_len`
/// (used to refresh the texture-cache registration's layout to
/// `SHADER_READ_ONLY_OPTIMAL` post-upload). No slot deref needed
/// across the boundary — the cdylib's `TextureRingSlot` carries
/// both fields as inline POD.
#[repr(C)]
pub struct TextureRingMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Rotate to the next slot. Writes the slot's `Texture` handle
    /// (cloned through the parent limited-access vtable, so the
    /// returned handle carries its own Arc strong count balanced
    /// by `Texture::Drop`), the cached `Texture` POD descriptors
    /// (width/height/format), the slot's `surface_id` bytes +
    /// length, and the slot index into the caller-provided
    /// out-parameter buffers. Returns 0 on success; non-zero with
    /// UTF-8 message in `err_buf` on failure (e.g. null ring
    /// handle).
    pub acquire_next: unsafe extern "C" fn(
        ring_handle: *const c_void,
        out_texture_handle: *mut *const c_void,
        out_texture_width: *mut u32,
        out_texture_height: *mut u32,
        out_texture_format_raw: *mut u32,
        out_surface_id_bytes: *mut [u8; 64],
        out_surface_id_len: *mut u32,
        out_slot_index: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Write a host-staged pixel buffer's contents into the slot's
    /// pre-allocated texture (the Limited-safe per-frame primitive).
    /// `slot_index` identifies the slot's pre-allocated upload
    /// resources host-side; `surface_id_bytes` + `surface_id_len`
    /// identify the texture-cache registration whose layout is
    /// refreshed to `SHADER_READ_ONLY_OPTIMAL` post-upload. Returns
    /// 0 on success; non-zero with UTF-8 message in `err_buf` on
    /// failure (slot_index out of range, surface_id not valid
    /// UTF-8, GPU submit error, etc.).
    pub copy_pixel_buffer_to_slot: unsafe extern "C" fn(
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
    ) -> i32,

    /// Look up a slot by index. Same out-parameter shape as
    /// `acquire_next`. Returns 0 on success; returns -1 (NOT 1) with
    /// no `err_buf` write when `index` is out of range — the
    /// distinction lets the caller `Option::None` cleanly without
    /// allocating an error string. Returns 1 on a hard failure
    /// (null ring handle, etc.) with UTF-8 message in `err_buf`.
    pub slot: unsafe extern "C" fn(
        ring_handle: *const c_void,
        index: usize,
        out_texture_handle: *mut *const c_void,
        out_texture_width: *mut u32,
        out_texture_height: *mut u32,
        out_texture_format_raw: *mut u32,
        out_surface_id_bytes: *mut [u8; 64],
        out_surface_id_len: *mut u32,
        out_slot_index: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for TextureRingMethodsVTable {}
unsafe impl Sync for TextureRingMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_ring_methods_vtable_layout() {
        // v2 (slot β-shape + method-dispatch slots added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   acquire_next                @ 8   (8 bytes, fn pointer)
        //   copy_pixel_buffer_to_slot   @ 16
        //   slot                        @ 24
        // Total = 32 bytes, align = 8.
        assert_eq!(size_of::<TextureRingMethodsVTable>(), 32);
        assert_eq!(align_of::<TextureRingMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(TextureRingMethodsVTable, acquire_next), 8);
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, copy_pixel_buffer_to_slot),
            16
        );
        assert_eq!(offset_of!(TextureRingMethodsVTable, slot), 24);
    }
}
