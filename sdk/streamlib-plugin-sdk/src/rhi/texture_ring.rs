// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twins of the engine's [`TextureRing`] / [`TextureRingSlot`]
//! PluginAbiObjects.
//!
//! Pre-allocated ring of textures rotated per-frame. Construction is
//! privileged (host-side, via
//! [`crate::context::GpuContextFullAccess::create_texture_ring`]);
//! per-frame rotation via [`TextureRing::acquire_next`] is sandbox-safe
//! and dispatches through the per-type
//! [`streamlib_plugin_abi::TextureRingMethodsVTable`].
//!
//! The host `TextureRingInner` backing + the CPU-upload
//! `copy_pixel_buffer_to_slot` primitive (which names the host-only
//! `PixelBuffer`) stay in the engine. GPU-native producers like the
//! Vulkan-compute JPEG backend write directly into a slot's `texture`
//! via their own compute kernel and never call the CPU-upload primitive.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{GpuContextFullAccessVTable, TextureRingMethodsVTable};

use streamlib_consumer_rhi::TextureFormat;

use crate::rhi::Texture;

/// Maximum on-the-wire `surface_id` length in bytes — fits any UUID
/// representation without crossing the plugin ABI as a heap `String`.
pub const TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES: usize = 64;

/// A single slot in a [`TextureRing`].
///
/// Layout-stable `#[repr(C)]` PluginAbiObject. `Clone` is structural —
/// `Texture`'s `Clone` runs (Arc-bumped through its own per-type vtable)
/// and the POD bytes copy. There is no `Drop` impl: `Texture::Drop`
/// decrements the texture's Arc, and the inline bytes have no destructor.
#[repr(C)]
pub struct TextureRingSlot {
    /// Pre-allocated texture handle for this slot.
    pub texture: Texture,
    /// Stable per-slot `surface_id` registered in the host's
    /// same-process texture cache. Stored as the first
    /// [`surface_id_len`](Self::surface_id_len) bytes of this fixed
    /// buffer (UTF-8, validated once at construction host-side).
    pub(crate) surface_id_bytes: [u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    /// Valid UTF-8 byte length in
    /// [`surface_id_bytes`](Self::surface_id_bytes).
    pub(crate) surface_id_len: u32,
    /// Index of this slot within the ring's slot vector.
    pub(crate) slot_index: u32,
}

// SAFETY: `Texture` is Send + Sync (PluginAbiObject over an Arc); the
// inline POD bytes carry no thread-state.
unsafe impl Send for TextureRingSlot {}
unsafe impl Sync for TextureRingSlot {}

impl TextureRingSlot {
    /// The slot's `surface_id` as a UTF-8 string. The bytes are valid
    /// UTF-8 by the construction invariant (always sourced from a
    /// host-side `&str`).
    pub fn surface_id(&self) -> &str {
        let len = (self.surface_id_len as usize).min(TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES);
        // SAFETY: bytes [0, len) are valid UTF-8 by the construction
        // invariant (always sourced from `&str`-bytes host-side).
        unsafe { std::str::from_utf8_unchecked(&self.surface_id_bytes[..len]) }
    }

    /// Index of this slot within the ring's slot vector.
    pub fn slot_index(&self) -> u32 {
        self.slot_index
    }
}

impl Clone for TextureRingSlot {
    fn clone(&self) -> Self {
        Self {
            texture: self.texture.clone(),
            surface_id_bytes: self.surface_id_bytes,
            surface_id_len: self.surface_id_len,
            slot_index: self.slot_index,
        }
    }
}

impl std::fmt::Debug for TextureRingSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureRingSlot")
            .field("surface_id", &self.surface_id())
            .field("slot_index", &self.slot_index)
            .field("texture", &self.texture)
            .finish()
    }
}

/// Pre-allocated ring of textures rotated per-frame.
///
/// Layout-stable `#[repr(C)]` PluginAbiObject. Lifecycle (Clone / Drop)
/// dispatches through the parent [`GpuContextFullAccessVTable`]'s
/// `clone_texture_ring` / `drop_texture_ring`; per-method dispatch is
/// reached through the per-type [`TextureRingMethodsVTable`]. The four
/// POD getters read cached fields directly — no plugin ABI hop.
#[repr(C)]
pub struct TextureRing {
    /// Opaque handle to the host's `Arc<TextureRingInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const TextureRingMethodsVTable,
    /// Cached slot count.
    pub(crate) cached_len: u32,
    /// Cached pixel width every slot's texture was allocated with.
    pub(crate) cached_width: u32,
    /// Cached pixel height every slot's texture was allocated with.
    pub(crate) cached_height: u32,
    /// Cached pixel format (plugin-ABI-stable `u32` discriminant).
    pub(crate) cached_format: u32,
}

// SAFETY: handle points at an `Arc<TextureRingInner>`; inner state is
// Send+Sync (atomic counter + GPU resources guarded by host queue mutex).
unsafe impl Send for TextureRing {}
unsafe impl Sync for TextureRing {}

impl TextureRing {
    /// Rotate to the next slot. Thread-safe (atomic counter host-side).
    /// Dispatches through the per-type methods vtable's `acquire_next`
    /// slot — the host wrapper writes the slot's POD bytes into the
    /// caller-provided out-parameters.
    pub fn acquire_next(&self) -> TextureRingSlot {
        self.dispatch_acquire_next_via_vtable()
            .expect("acquire_next vtable dispatch failed")
    }

    /// Number of slots in the ring. Cached POD — no plugin ABI hop.
    pub fn len(&self) -> usize {
        self.cached_len as usize
    }

    /// Ring is always non-empty in practice. Cached POD — no plugin ABI hop.
    pub fn is_empty(&self) -> bool {
        self.cached_len == 0
    }

    /// Width of every slot's texture, in pixels. Cached POD.
    pub fn width(&self) -> u32 {
        self.cached_width
    }

    /// Height of every slot's texture, in pixels. Cached POD.
    pub fn height(&self) -> u32 {
        self.cached_height
    }

    /// Format every slot's texture was allocated with. Cached POD.
    pub fn format(&self) -> TextureFormat {
        match self.cached_format {
            0 => TextureFormat::Rgba8Unorm,
            1 => TextureFormat::Rgba8UnormSrgb,
            2 => TextureFormat::Bgra8Unorm,
            3 => TextureFormat::Bgra8UnormSrgb,
            4 => TextureFormat::Rgba16Float,
            5 => TextureFormat::Rgba32Float,
            6 => TextureFormat::Nv12,
            _ => TextureFormat::Rgba8Unorm,
        }
    }

    /// Borrow a slot by index — debug / introspection only. Dispatches
    /// through the per-type methods vtable's `slot` slot.
    #[doc(hidden)]
    pub fn slot(&self, index: usize) -> Option<TextureRingSlot> {
        self.dispatch_slot_via_vtable(index)
    }

    /// Cdylib path: dispatch `acquire_next` through the per-type methods
    /// vtable. The host wrapper writes the slot's POD bytes into the
    /// caller-provided out-parameter buffers; the `out_texture_handle`
    /// is a freshly-cloned Arc (the host wrapper bumped the texture's
    /// Arc through its limited-access vtable), and the returned
    /// `TextureRingSlot`'s `Texture::Drop` balances when the slot drops.
    fn dispatch_acquire_next_via_vtable(&self) -> Result<TextureRingSlot> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_next: ring methods vtable is null".into(),
            ));
        }
        let mut out_texture_handle: *const c_void = std::ptr::null();
        let mut out_texture_width: u32 = 0;
        let mut out_texture_height: u32 = 0;
        let mut out_texture_format_raw: u32 = 0;
        let mut out_surface_id_bytes = [0u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut out_surface_id_len: u32 = 0;
        let mut out_slot_index: u32 = 0;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; handle paired
        // with it at mint time. All out-params point at owned stack
        // storage the host writes on success.
        let status = unsafe {
            ((*self.methods_vtable).acquire_next)(
                self.handle,
                &mut out_texture_handle as *mut *const c_void,
                &mut out_texture_width as *mut u32,
                &mut out_texture_height as *mut u32,
                &mut out_texture_format_raw as *mut u32,
                &mut out_surface_id_bytes as *mut [u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
                &mut out_surface_id_len as *mut u32,
                &mut out_slot_index as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        Ok(slot_from_out_params(
            out_texture_handle,
            out_texture_width,
            out_texture_height,
            out_texture_format_raw,
            out_surface_id_bytes,
            out_surface_id_len,
            out_slot_index,
        ))
    }

    /// Cdylib path: dispatch `slot(index)` through the per-type methods
    /// vtable. A non-zero status (e.g. index out of range) maps to
    /// `None`; `0` is success.
    fn dispatch_slot_via_vtable(&self, index: usize) -> Option<TextureRingSlot> {
        if self.methods_vtable.is_null() {
            return None;
        }
        let mut out_texture_handle: *const c_void = std::ptr::null();
        let mut out_texture_width: u32 = 0;
        let mut out_texture_height: u32 = 0;
        let mut out_texture_format_raw: u32 = 0;
        let mut out_surface_id_bytes = [0u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut out_surface_id_len: u32 = 0;
        let mut out_slot_index: u32 = 0;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see dispatch_acquire_next_via_vtable.
        let status = unsafe {
            ((*self.methods_vtable).slot)(
                self.handle,
                index,
                &mut out_texture_handle as *mut *const c_void,
                &mut out_texture_width as *mut u32,
                &mut out_texture_height as *mut u32,
                &mut out_texture_format_raw as *mut u32,
                &mut out_surface_id_bytes as *mut [u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
                &mut out_surface_id_len as *mut u32,
                &mut out_slot_index as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return None;
        }
        Some(slot_from_out_params(
            out_texture_handle,
            out_texture_width,
            out_texture_height,
            out_texture_format_raw,
            out_surface_id_bytes,
            out_surface_id_len,
            out_slot_index,
        ))
    }
}

/// Assemble a `TextureRingSlot` from vtable out-parameters. The
/// `texture_handle` is consumed (its strong count is the freshly-cloned
/// one the host wrapper produced); the resulting slot owns the Drop side
/// of that clone via its `Texture` field.
fn slot_from_out_params(
    texture_handle: *const c_void,
    texture_width: u32,
    texture_height: u32,
    texture_format_raw: u32,
    surface_id_bytes: [u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    surface_id_len: u32,
    slot_index: u32,
) -> TextureRingSlot {
    // SAFETY: `texture_handle` is the host-cloned
    // `Arc::into_raw(Arc<TextureInner>)` pointer; the host paired the Arc
    // bump with the limited-access vtable's `clone_texture`, so
    // `Texture::Drop` calls the matching `drop_texture` to balance.
    let texture = unsafe {
        Texture::from_raw_handle_for_cdylib(
            texture_handle,
            texture_width,
            texture_height,
            texture_format_raw,
        )
    };
    TextureRingSlot {
        texture,
        surface_id_bytes,
        surface_id_len,
        slot_index,
    }
}

impl Clone for TextureRing {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at mint time; the vtable's
            // `clone_texture_ring` contract is
            // `Arc::increment_strong_count` host-side.
            unsafe {
                ((*self.vtable).clone_texture_ring)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            methods_vtable: self.methods_vtable,
            cached_len: self.cached_len,
            cached_width: self.cached_width,
            cached_height: self.cached_height,
            cached_format: self.cached_format,
        }
    }
}

impl Drop for TextureRing {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_texture_ring` bumps.
            unsafe {
                ((*self.vtable).drop_texture_ring)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for TextureRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureRing").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_ring_layout() {
        // Must match the engine's
        // `core/context/texture_ring.rs::TextureRing`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16, cached_len @ 24,
        //   cached_width @ 28, cached_height @ 32, cached_format @ 36.
        // Total 40 bytes, align 8.
        assert_eq!(size_of::<TextureRing>(), 40);
        assert_eq!(align_of::<TextureRing>(), 8);
        assert_eq!(offset_of!(TextureRing, handle), 0);
        assert_eq!(offset_of!(TextureRing, vtable), 8);
        assert_eq!(offset_of!(TextureRing, methods_vtable), 16);
        assert_eq!(offset_of!(TextureRing, cached_len), 24);
        assert_eq!(offset_of!(TextureRing, cached_width), 28);
        assert_eq!(offset_of!(TextureRing, cached_height), 32);
        assert_eq!(offset_of!(TextureRing, cached_format), 36);
    }

    #[test]
    fn texture_ring_slot_layout() {
        // Must match the engine's `TextureRingSlot`:
        //   texture @ 0 (32 bytes), surface_id_bytes @ 32 ([u8; 64]),
        //   surface_id_len @ 96, slot_index @ 100.
        // Total 104 bytes, align 8 (inherited from Texture).
        assert_eq!(size_of::<TextureRingSlot>(), 104);
        assert_eq!(align_of::<TextureRingSlot>(), 8);
        assert_eq!(offset_of!(TextureRingSlot, texture), 0);
        assert_eq!(offset_of!(TextureRingSlot, surface_id_bytes), 32);
        assert_eq!(offset_of!(TextureRingSlot, surface_id_len), 96);
        assert_eq!(offset_of!(TextureRingSlot, slot_index), 100);
    }

    #[test]
    fn texture_ring_slot_surface_id_max_bytes_constant() {
        assert_eq!(TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, 64);
    }

    #[test]
    fn texture_ring_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TextureRing>();
        assert_send_sync::<TextureRingSlot>();
    }
}
