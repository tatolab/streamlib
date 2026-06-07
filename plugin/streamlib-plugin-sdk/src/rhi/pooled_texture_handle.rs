// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`PooledTextureHandle`] PluginAbiObject.
//!
//! Layout-stable `#[repr(C)] (handle, vtable, Texture, cached POD)` shape
//! mirroring the engine's `core/context/texture_pool.rs::PooledTextureHandle`.
//! Deliberately **not** `Clone` — Drop releases the underlying pool slot via
//! the vtable's `drop_pooled_texture_handle` callback exactly once, and the
//! embedded [`Texture`]'s own Drop releases the texture Arc. The host
//! `PooledTextureHandleInner` backing stays in the engine.

use std::ffi::c_void;

use streamlib_consumer_rhi::{TextureFormat, TextureUsages};
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

use crate::rhi::{NativeTextureHandle, Texture};

/// Request descriptor for acquiring a pooled texture. Engine-free mirror of
/// the engine's `TexturePoolDescriptor`.
#[derive(Clone, Debug)]
pub struct TexturePoolDescriptor {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
    pub label: Option<&'static str>,
}

impl TexturePoolDescriptor {
    /// Create a new pool descriptor (default usage:
    /// `TEXTURE_BINDING | COPY_SRC`).
    pub fn new(width: u32, height: u32, format: TextureFormat) -> Self {
        Self {
            width,
            height,
            format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
            label: None,
        }
    }

    /// Set usage flags.
    pub fn with_usage(mut self, usage: TextureUsages) -> Self {
        self.usage = usage;
        self
    }

    /// Set a debug label.
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

/// Handle to a pooled texture. Returns the texture to the pool on Drop.
///
/// Layout-stable: every field is a primitive, an opaque pointer, or the
/// layout-stable [`Texture`] twin embedded by value.
///
/// Deliberately **not** `Clone`: Drop releases the underlying pool slot
/// exactly once via [`GpuContextLimitedAccessVTable::drop_pooled_texture_handle`].
/// Cloning would duplicate the raw `handle` and double-release the slot.
/// Consumers needing shared access wrap the handle in `Arc<PooledTextureHandle>`.
#[repr(C)]
pub struct PooledTextureHandle {
    /// Opaque host handle (`Box::into_raw(Box<PooledTextureHandleInner>)`).
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Drop dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// The pooled texture. Embedded by value (the [`Texture`] twin is itself
    /// `#[repr(C)]`, 32 bytes); its own Drop releases the texture Arc.
    pub(crate) texture: Texture,
    /// Cached width (mirrors the pool key width at allocation time).
    pub(crate) width_cached: u32,
    /// Cached height (mirrors the pool key height).
    pub(crate) height_cached: u32,
    /// Cached `#[repr(u32)]` discriminant of [`TextureFormat`].
    pub(crate) format_raw: u32,
    /// Reserved padding (keeps total size at 64 bytes; zero, never read).
    pub(crate) _padding: u32,
}

// SAFETY: `handle` points at a host-owned `Box<PooledTextureHandleInner>`
// that is Send+Sync; the embedded `texture` is Send+Sync per its own unsafe
// impls. Pool-slot release runs in host-compiled code via the vtable callback.
unsafe impl Send for PooledTextureHandle {}
unsafe impl Sync for PooledTextureHandle {}

impl PooledTextureHandle {
    /// Borrow the underlying texture.
    pub fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Clone the underlying texture (bumps the host's texture Arc).
    pub fn texture_clone(&self) -> Texture {
        self.texture.clone()
    }

    /// Texture width in pixels. Cached at construction; pure field read.
    pub fn width(&self) -> u32 {
        self.width_cached
    }

    /// Texture height in pixels. Cached at construction; pure field read.
    pub fn height(&self) -> u32 {
        self.height_cached
    }

    /// Texture format. Cached at construction; pure field read.
    pub fn format(&self) -> TextureFormat {
        match self.format_raw {
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

    /// Platform-native sharing handle for the underlying texture (Linux
    /// DMA-BUF FD). Delegates to [`Texture::native_handle`].
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        self.texture.native_handle()
    }
}

impl Drop for PooledTextureHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Box::into_raw`. The vtable's
            // `drop_pooled_texture_handle` callback runs `Box::from_raw +
            // drop` host-side, firing `Drop for PooledTextureHandleInner`
            // which releases the pool slot exactly once. The embedded
            // `texture` field's own Drop (running after this) decrements the
            // texture Arc — mirroring the engine's two-drop shape.
            unsafe {
                ((*self.vtable).drop_pooled_texture_handle)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for PooledTextureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledTextureHandle")
            .field("width", &self.width_cached)
            .field("height", &self.height_cached)
            .field("format", &self.format())
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn pooled_texture_handle_layout() {
        // Must match the engine's
        // `core/context/texture_pool.rs::PooledTextureHandle`:
        //   handle @ 0, vtable @ 8, texture @ 16 (32B), width_cached @ 48,
        //   height_cached @ 52, format_raw @ 56, _padding @ 60.
        // Total 64 bytes, align 8.
        assert_eq!(size_of::<PooledTextureHandle>(), 64);
        assert_eq!(align_of::<PooledTextureHandle>(), 8);
        assert_eq!(offset_of!(PooledTextureHandle, handle), 0);
        assert_eq!(offset_of!(PooledTextureHandle, vtable), 8);
        assert_eq!(offset_of!(PooledTextureHandle, texture), 16);
        assert_eq!(offset_of!(PooledTextureHandle, width_cached), 48);
        assert_eq!(offset_of!(PooledTextureHandle, height_cached), 52);
        assert_eq!(offset_of!(PooledTextureHandle, format_raw), 56);
        assert_eq!(offset_of!(PooledTextureHandle, _padding), 60);
    }

    #[test]
    fn pooled_texture_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PooledTextureHandle>();
    }

    /// `PooledTextureHandle` must NOT be `Clone` — Drop releases the pool
    /// slot exactly once.
    ///
    /// ```compile_fail
    /// fn assert_not_clone<T: Clone>() {}
    /// assert_not_clone::<streamlib_plugin_sdk::sdk::rhi::PooledTextureHandle>();
    /// ```
    #[allow(dead_code)]
    fn not_clone_doctest_anchor() {}
}
