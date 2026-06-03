// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's RHI [`Texture`] PluginAbiObject.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape: every field is a
//! primitive or an opaque pointer, so the type round-trips across the
//! plugin ABI unchanged. The host `TextureInner` backing + the
//! `from_arc_into_raw` / `host_inner` constructors stay in the engine;
//! this twin carries only the cdylib field reads + the vtable-dispatched
//! Clone/Drop/native-handle methods.

use std::ffi::c_void;

use streamlib_consumer_rhi::{TextureFormat, TextureUsages};
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Platform-specific native handle for cross-framework texture sharing.
#[derive(Debug, Clone)]
pub enum NativeTextureHandle {
    /// macOS/iOS: IOSurface ID for cross-process GPU memory sharing.
    IOSurface { id: u32 },
    /// Linux: DMA-BUF file descriptor for GPU memory sharing.
    ///
    /// **The FD is borrowed from the host-owning [`Texture`]**: it stays
    /// valid only while that [`Texture`] keeps the underlying allocation
    /// alive, and the host closes it on drop. Callers handing the FD to
    /// an API that takes ownership on success MUST `dup(2)` it first.
    DmaBuf { fd: i32 },
    /// Windows: DXGI shared handle for cross-process GPU memory sharing.
    DxgiSharedHandle { handle: u64 },
}

/// Descriptor for creating a texture.
#[derive(Debug, Clone)]
pub struct TextureDescriptor<'a> {
    pub label: Option<&'a str>,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
}

impl<'a> TextureDescriptor<'a> {
    /// Create a new texture descriptor.
    pub fn new(width: u32, height: u32, format: TextureFormat) -> Self {
        Self {
            label: None,
            width,
            height,
            format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
        }
    }

    /// Set the label for debugging.
    pub fn with_label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Set the usage flags.
    pub fn with_usage(mut self, usage: TextureUsages) -> Self {
        self.usage = usage;
        self
    }
}

/// Platform-agnostic texture wrapper.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer. Clone bumps the host's `Arc<TextureInner>` strong count via
/// [`GpuContextLimitedAccessVTable::clone_texture`]; Drop decrements via
/// [`GpuContextLimitedAccessVTable::drop_texture`]. Both run in
/// host-compiled code regardless of the calling plugin.
#[repr(C)]
pub struct Texture {
    /// Opaque handle to the host's `Arc<TextureInner>` (produced by
    /// `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop dispatch — the host-installed
    /// pointer from `HostServices::gpu_context_limited_access_vtable`.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// Cached width (queried once at construction host-side).
    pub(crate) width_cached: u32,
    /// Cached height (queried once at construction host-side).
    pub(crate) height_cached: u32,
    /// Cached pixel format `#[repr(u32)]` discriminant.
    pub(crate) format_raw: u32,
    /// Reserved padding (keeps total size at 32 bytes; zero, never read).
    pub(crate) _padding: u32,
}

// SAFETY: `handle` points at an `Arc<TextureInner>` whose interior is
// Send+Sync. Refcount management crosses the plugin ABI through the
// vtable, but the underlying Arc bookkeeping runs in host-compiled code.
unsafe impl Send for Texture {}
unsafe impl Sync for Texture {}

impl Texture {
    /// Assemble a [`Texture`] PluginAbiObject from a raw
    /// `Arc::into_raw`-shaped handle plus cached POD bytes. Used by
    /// cdylib-side dispatch paths that receive a freshly-cloned handle
    /// through a plugin ABI out-parameter (e.g.
    /// [`crate::rhi::TextureRing::acquire_next`]) — the host wrapper
    /// bumped the texture's Arc through the limited-access vtable's
    /// `clone_texture` slot, and the returned [`Texture`] owns the
    /// matching `Drop`-side decrement when it falls out of scope.
    ///
    /// # Safety
    ///
    /// `handle` must come from a host-side `Arc::into_raw(Arc<TextureInner>)`
    /// whose strong count the caller is responsible for (one per returned
    /// [`Texture`]). `format_raw` must match [`TextureFormat`]'s
    /// `#[repr(u32)]` discriminant; out-of-range values fall back to
    /// `Rgba8Unorm` via [`Self::format`].
    pub(crate) unsafe fn from_raw_handle_for_cdylib(
        handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
    ) -> Self {
        let vtable = crate::rhi::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            width_cached: width,
            height_cached: height,
            format_raw,
            _padding: 0,
        }
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

    /// Get the platform-native sharing handle for this texture.
    ///
    /// On Linux dispatches through
    /// [`GpuContextLimitedAccessVTable::texture_native_dma_buf_fd`]
    /// (sentinel `-1` encodes `None`). On other platforms returns
    /// `None` until that platform's cdylib path lands.
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        if self.handle.is_null() || self.vtable.is_null() {
            return None;
        }
        // SAFETY: `vtable` and `handle` were paired at construction; the
        // `texture_native_dma_buf_fd` slot accepts the texture handle
        // directly and returns `-1` (no FD) or a non-negative `RawFd`
        // widened to `i64`.
        let fd_i64 = unsafe { ((*self.vtable).texture_native_dma_buf_fd)(self.handle) };
        if fd_i64 < 0 {
            None
        } else {
            i32::try_from(fd_i64)
                .ok()
                .map(|fd| NativeTextureHandle::DmaBuf { fd })
        }
    }
}

impl Clone for Texture {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at construction; the vtable's
            // `clone_texture` contract is `Arc::increment_strong_count`
            // host-side. Balanced by the Drop impl below.
            unsafe {
                ((*self.vtable).clone_texture)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            width_cached: self.width_cached,
            height_cached: self.height_cached,
            format_raw: self.format_raw,
            _padding: 0,
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_texture` bumps.
            unsafe {
                ((*self.vtable).drop_texture)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for Texture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Texture")
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
    fn texture_layout() {
        // Must match the engine's `core/rhi/texture.rs::Texture`:
        //   handle        @ 0,  vtable @ 8, width_cached @ 16,
        //   height_cached @ 20, format_raw @ 24, _padding @ 28.
        // Total 32 bytes, align 8.
        assert_eq!(size_of::<Texture>(), 32);
        assert_eq!(align_of::<Texture>(), 8);
        assert_eq!(offset_of!(Texture, handle), 0);
        assert_eq!(offset_of!(Texture, vtable), 8);
        assert_eq!(offset_of!(Texture, width_cached), 16);
        assert_eq!(offset_of!(Texture, height_cached), 20);
        assert_eq!(offset_of!(Texture, format_raw), 24);
        assert_eq!(offset_of!(Texture, _padding), 28);
    }

    #[test]
    fn texture_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Texture>();
    }
}
