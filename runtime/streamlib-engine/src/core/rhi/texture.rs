// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI texture abstraction.
//!
//! `(handle, cached POD)` shape: the handle is
//! `Arc::into_raw(Arc<TextureInner>)`; Clone/Drop refcount it directly.
//!
//! The platform-specific Arc (`HostVulkanTexture` on Linux and macOS,
//! `DX12Texture` on Windows) lives on the
//! private [`TextureInner`] type behind the opaque handle. Engine code
//! reaches them via the [`crate::host_rhi::HostTextureExt`] extension
//! trait.
//!
//! `TextureFormat` and `TextureUsages` are defined in
//! [`streamlib_consumer_rhi`] so subprocess-shape dep graphs can name
//! them without pulling streamlib. They're re-exported from
//! [`crate::core::rhi`] so existing in-tree call sites compile
//! unchanged.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_consumer_rhi::{TextureFormat, TextureUsages};

/// Platform-specific native handle for cross-framework texture sharing.
///
/// Use this when you need to pass textures to external libraries that can
/// handle multiple platform sharing mechanisms (e.g., pygfx, wgpu-py).
#[derive(Debug, Clone)]
pub enum NativeTextureHandle {
    /// macOS/iOS: IOSurface ID for cross-process GPU memory sharing.
    /// Use `IOSurfaceLookup(id)` to get the IOSurface handle.
    IOSurface { id: u32 },

    /// Linux: DMA-BUF file descriptor for GPU memory sharing.
    /// Import via `EGL_EXT_image_dma_buf_import` or Vulkan external memory.
    ///
    /// **The FD is owned by the receiver**: the export mints a fresh one
    /// per call and the [`Texture`] keeps no copy, so nothing else will
    /// ever close it. Close it, or hand it to an API that takes ownership
    /// (`vkImportMemoryFdKHR`, `cudaImportExternalMemory`) — never both,
    /// and never `dup(2)` first expecting someone else to close the
    /// original.
    DmaBuf { fd: i32 },

    /// Windows: DXGI shared handle for cross-process GPU memory sharing.
    /// Import via `ID3D11Device1::OpenSharedResource1` or similar.
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

/// Rich data backing a [`Texture`], held behind the opaque handle.
///
/// Holds the platform-specific Arc the engine RHI and surface adapters
/// need (the raw `VkImage` and its allocation).
pub(crate) struct TextureInner {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanTexture>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: Arc<crate::windows::rhi::DX12Texture>,
}

impl TextureInner {
    /// Texture width in pixels.
    pub(crate) fn width(&self) -> u32 {
        self.inner.width()
    }

    /// Texture height in pixels.
    pub(crate) fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Texture format.
    pub(crate) fn format(&self) -> TextureFormat {
        self.inner.format()
    }

    /// Whether a recorded copy may read this texture (Vulkan:
    /// TRANSFER_SRC usage; the non-Vulkan backends do not usage-gate
    /// copies).
    pub(crate) fn supports_transfer_read(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return self.inner.supports_transfer_read();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        true
    }

    /// Whether a recorded copy may write this texture (Vulkan:
    /// TRANSFER_DST usage; the non-Vulkan backends do not usage-gate
    /// copies).
    pub(crate) fn supports_transfer_write(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return self.inner.supports_transfer_write();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        true
    }
}

/// Platform-agnostic texture wrapper.
///
/// The platform-specific [`TextureInner`] is hidden behind the opaque
/// `handle`; engine-internal callers reach it through the
/// [`crate::host_rhi::HostTextureExt`] extension trait. Clone bumps the
/// `Arc<TextureInner>` strong count and Drop decrements it.
pub struct Texture {
    /// Opaque handle to the host's `Arc<TextureInner>` (produced by
    /// `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Cached width (queried once at construction).
    pub(crate) width_cached: u32,
    /// Cached height (queried once at construction).
    pub(crate) height_cached: u32,
    /// Cached pixel format `#[repr(u32)]` discriminant. Read back via
    /// [`Texture::format`] which round-trips through the well-defined
    /// `repr(u32)` mapping.
    pub(crate) format_raw: u32,
    /// Reserved padding (keeps total size at 32 bytes for a clean
    /// 8-byte-aligned shape; zero today, never read).
    pub(crate) _padding: u32,
}

// SAFETY: `handle` points at an `Arc<TextureInner>` whose interior is
// Send+Sync (platform-specific texture types — `HostVulkanTexture`,
// `DX12Texture` — are themselves Send+Sync).
unsafe impl Send for Texture {}
unsafe impl Sync for Texture {}

impl Texture {
    /// Construct from a fully-populated [`TextureInner`]. Engine-only;
    /// surface adapters and RHI helpers reach this through
    /// [`crate::host_rhi::HostTextureExt::from_vulkan`] or the
    /// equivalent DX12 entry point.
    pub(crate) fn from_inner(inner: TextureInner) -> Self {
        let width = inner.width();
        let height = inner.height();
        let format = inner.format();
        let arc = Arc::new(inner);
        Self::from_arc_into_raw(arc, width, height, format)
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, capture the host-mode vtable, and build the
    /// `(handle, vtable, POD)` shape.
    pub(crate) fn from_arc_into_raw(
        arc: Arc<TextureInner>,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            width_cached: width,
            height_cached: height,
            format_raw: format as u32,
            _padding: 0,
        }
    }

    /// Engine-internal borrow of the owned [`TextureInner`].
    pub(crate) fn host_inner(&self) -> &TextureInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<TextureInner>)`
        // (see `from_arc_into_raw`). The leaked strong count keeps the
        // `TextureInner` alive at least until `Drop` runs.
        unsafe { &*(self.handle as *const TextureInner) }
    }

    /// Texture width in pixels. Cached at construction; pure field read.
    pub fn width(&self) -> u32 {
        self.width_cached
    }

    /// Texture height in pixels. Cached at construction; pure field read.
    pub fn height(&self) -> u32 {
        self.height_cached
    }

    /// Texture format. Cached at construction; pure field read with
    /// no plugin ABI dispatch.
    pub fn format(&self) -> TextureFormat {
        // SAFETY: `format_raw` is the `#[repr(u32)]` discriminant of a
        // `TextureFormat` value captured at construction. The mapping
        // is the identity round-trip the `repr(u32)` enum guarantees.
        match self.format_raw {
            0 => TextureFormat::Rgba8Unorm,
            1 => TextureFormat::Rgba8UnormSrgb,
            2 => TextureFormat::Bgra8Unorm,
            3 => TextureFormat::Bgra8UnormSrgb,
            4 => TextureFormat::Rgba16Float,
            5 => TextureFormat::Rgba32Float,
            6 => TextureFormat::Nv12,
            // Fall back to Rgba8Unorm for unknown discriminants
            // (preserves type safety; never reached because
            // `format_raw` is always sourced from a valid value).
            _ => TextureFormat::Rgba8Unorm,
        }
    }

    /// Whether a recorded copy may read this texture (Vulkan:
    /// TRANSFER_SRC usage; the non-Vulkan backends do not usage-gate
    /// copies). Engine-internal: reads the host's `TextureInner`
    /// directly, which panics for a cdylib caller.
    pub fn supports_transfer_read(&self) -> bool {
        self.host_inner().supports_transfer_read()
    }

    /// Whether a recorded copy may write this texture (Vulkan:
    /// TRANSFER_DST usage; the non-Vulkan backends do not usage-gate
    /// copies). Engine-internal: reads the host's `TextureInner`
    /// directly, which panics for a cdylib caller.
    pub fn supports_transfer_write(&self) -> bool {
        self.host_inner().supports_transfer_write()
    }

    /// Get the platform-native sharing handle for this texture.
    ///
    /// Returns the appropriate handle type for the current platform:
    /// - Linux: `DmaBuf { fd }` — adapters export DMA-BUF FDs to a
    ///   different GPU API (CUDA, OpenGL, downstream IPC) without
    ///   touching host-internal `TextureInner` layout. The fd is freshly
    ///   minted and its ownership transfers to the caller, who must close
    ///   it or hand it to an import that dups on receipt; the texture
    ///   keeps no copy and will not close it (#1880).
    /// - Windows: `DxgiSharedHandle { handle }` (when implemented).
    ///
    /// Returns `None` if no sharing handle is available (no Vulkan
    /// backing, export failed, or the platform doesn't expose one).
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        #[cfg(target_os = "linux")]
        {
            if self.handle.is_null() {
                return None;
            }
            use crate::host_rhi::HostTextureExt;
            match self.vulkan_inner().export_dma_buf_fd() {
                Ok(fd) => Some(NativeTextureHandle::DmaBuf { fd }),
                Err(_) => None,
            }
        }
        #[cfg(target_os = "windows")]
        {
            // Windows DXGI shared handle: deferred until Windows
            // cdylib adapter work begins.
            None
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }
}

// Privileged Host-flavor accessors (`from_vulkan`, `vulkan_inner`)
// live on the [`crate::host_rhi::HostTextureExt`] extension
// trait — type-system-enforced boundary so the SDK's public inherent
// impl stays Host-free. Engine RHI helpers and in-tree adapters
// `use crate::host_rhi::HostTextureExt;` to surface them.

impl Clone for Texture {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureInner>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const TextureInner);
            }
        }
        Self {
            handle: self.handle,
            width_cached: self.width_cached,
            height_cached: self.height_cached,
            format_raw: self.format_raw,
            _padding: 0,
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const TextureInner);
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

// =============================================================================
// Layout regression tests
// =============================================================================

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;

    /// Compile-time witness that `Texture` is Send + Sync.
    #[test]
    fn texture_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Texture>();
    }
}
