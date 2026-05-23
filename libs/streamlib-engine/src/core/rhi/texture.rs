// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI texture abstraction.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape: every field
//! is either a primitive or an opaque pointer, so the type
//! round-trips across the cdylib DSO boundary unchanged. The
//! handle is `Arc::into_raw(Arc<TextureInner>)` produced by host
//! code; the vtable's `clone_texture` / `drop_texture` callbacks
//! manage the Arc refcount in host-compiled code, so Clone/Drop
//! work correctly regardless of the cdylib's compiled `Arc` layout.
//!
//! Platform-specific Arcs (`HostVulkanTexture` on Linux,
//! `MetalTexture` on macOS, `DX12Texture` on Windows) live on the
//! private [`TextureInner`] type behind the opaque handle. Engine code
//! reaches them via the [`crate::host_rhi::HostTextureExt`] extension
//! trait; cdylib code never sees them.
//!
//! `TextureFormat` and `TextureUsages` are defined in
//! [`streamlib_consumer_rhi`] so subprocess-shape dep graphs can name
//! them without pulling streamlib. They're re-exported from
//! [`crate::core::rhi`] so existing in-tree call sites compile
//! unchanged.

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_consumer_rhi::{TextureFormat, TextureUsages};
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

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

/// Host-only rich data backing a [`Texture`]. Cdylib code never sees
/// this type; it reaches the public [`Texture`] surface through the
/// `(handle, vtable, POD)` β-shape.
///
/// Holds the platform-specific Arc(s) the engine RHI and surface
/// adapters need (raw `VkImage`, `MTLTexture`, IOSurface, etc.).
pub(crate) struct TextureInner {
    // Metal backend: when vulkan NOT requested AND (explicit metal feature OR macOS/iOS)
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub(crate) inner: Arc<crate::metal::rhi::MetalTexture>,

    // Vulkan backend: explicit feature OR Linux default (when metal not requested)
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub(crate) inner: Arc<crate::vulkan::rhi::HostVulkanTexture>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: Arc<crate::windows::rhi::DX12Texture>,

    /// Metal texture for Apple platform services (IOSurface, CVPixelBuffer).
    /// On macOS/iOS with Vulkan backend, textures created from IOSurface are
    /// stored here. When Metal is the backend, this duplicates `inner`.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) metal_texture: Option<Arc<crate::metal::rhi::MetalTexture>>,
}

impl TextureInner {
    /// Texture width in pixels.
    pub(crate) fn width(&self) -> u32 {
        // On macOS, prefer metal_texture if available (for IOSurface-backed textures)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.width();
        }
        self.inner.width()
    }

    /// Texture height in pixels.
    pub(crate) fn height(&self) -> u32 {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.height();
        }
        self.inner.height()
    }

    /// Texture format.
    pub(crate) fn format(&self) -> TextureFormat {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.format();
        }
        self.inner.format()
    }
}

/// Platform-agnostic texture wrapper.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer. The platform-specific [`TextureInner`] is hidden behind
/// the opaque `handle`; engine-internal callers reach it through the
/// [`crate::host_rhi::HostTextureExt`] extension trait, cdylib callers
/// route through the vtable.
///
/// Clone bumps the host's `Arc<TextureInner>` strong count via
/// [`GpuContextLimitedAccessVTable::clone_texture`]; Drop decrements
/// via [`GpuContextLimitedAccessVTable::drop_texture`]. Both run in
/// host-compiled code regardless of the calling DSO.
#[repr(C)]
pub struct Texture {
    /// Opaque handle to the host's `Arc<TextureInner>` (produced by
    /// `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Vtable for cross-DSO Clone/Drop dispatch. Resolved through the
    /// DSO-routed accessor at construction; host mode points at
    /// `&HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`, cdylib mode at the
    /// host-installed pointer from
    /// `HostServices::gpu_context_limited_access_vtable`.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
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
// `MetalTexture`, `DX12Texture` — are themselves Send+Sync). Refcount
// management crosses the cdylib boundary through the vtable, but the
// underlying Arc bookkeeping runs in host-compiled code regardless.
unsafe impl Send for Texture {}
unsafe impl Sync for Texture {}

impl Texture {
    /// Construct from a fully-populated [`TextureInner`]. Engine-only;
    /// surface adapters and RHI helpers reach this through
    /// [`crate::host_rhi::HostTextureExt::from_vulkan`] or the
    /// equivalent Metal / DX12 entry points.
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
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            width_cached: width,
            height_cached: height,
            format_raw: format as u32,
            _padding: 0,
        }
    }

    /// Assemble a [`Texture`] β-shape from a raw `Arc::into_raw`-
    /// shaped handle plus cached POD bytes. Used by cdylib-side
    /// dispatch paths that receive a freshly-cloned handle through
    /// an FFI out-parameter (e.g.
    /// [`crate::core::context::TextureRing::acquire_next`] in cdylib
    /// mode) — the host wrapper bumped the texture's Arc through
    /// the limited-access vtable's `clone_texture` slot, and the
    /// returned [`Texture`] owns the matching `Drop`-side decrement
    /// when it falls out of scope.
    ///
    /// The vtable pointer is resolved through the DSO-routed
    /// accessor [`crate::core::plugin::host_services::host_gpu_context_limited_access_vtable`]
    /// so cdylib code reaches the host's pointer (matching the
    /// `clone_texture` slot used to mint the handle) and host code
    /// reaches its own static.
    ///
    /// # Safety
    ///
    /// `handle` must come from a host-side
    /// `Arc::into_raw(Arc<TextureInner>)` whose Arc strong count
    /// the caller is responsible for (one strong count per
    /// returned [`Texture`]). `format_raw` must match
    /// [`TextureFormat`]'s `#[repr(u32)]` discriminant for the
    /// texture's actual format; out-of-range values fall back to
    /// `Rgba8Unorm` via [`Self::format`].
    pub(crate) unsafe fn from_raw_handle_for_cdylib(
        handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
    ) -> Self {
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            width_cached: width,
            height_cached: height,
            format_raw,
            _padding: 0,
        }
    }

    /// Engine-internal borrow of the host-owned [`TextureInner`].
    ///
    /// **Panics if called from cdylib code.** The `TextureInner` type's
    /// in-memory layout is host-private; cdylib code that reads it
    /// would deref host-written bytes under cdylib's view of
    /// `TextureInner`'s layout, which is UB under the deployment model
    /// the plugin ABI supports.
    ///
    /// The panic is caught by `run_host_extern_c` at the FFI boundary
    /// (host extern "C" callbacks all route through `catch_unwind`),
    /// so a misconfigured cdylib reaching this method gets a clean
    /// "callback panicked" log entry instead of UB.
    pub(crate) fn host_inner(&self) -> &TextureInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "Texture::host_inner() reached from cdylib code; this method must \
                 dispatch through the GpuContextLimitedAccessVTable. The panic is \
                 caught by run_host_extern_c at the FFI boundary."
            );
        }
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<TextureInner>)`
        // (see `from_arc_into_raw`). The leaked strong count keeps the
        // `TextureInner` alive at least until `Drop` runs.
        unsafe { &*(self.handle as *const TextureInner) }
    }

    /// Texture width in pixels. Cached at construction; pure field
    /// read with no cross-DSO dispatch.
    pub fn width(&self) -> u32 {
        self.width_cached
    }

    /// Texture height in pixels. Cached at construction; pure field
    /// read with no cross-DSO dispatch.
    pub fn height(&self) -> u32 {
        self.height_cached
    }

    /// Texture format. Cached at construction; pure field read with
    /// no cross-DSO dispatch.
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

    /// Get the IOSurface ID for cross-framework sharing.
    ///
    /// Returns `Some(id)` on macOS/iOS if the texture is backed by an IOSurface.
    /// Returns `None` on other platforms or if no IOSurface is available.
    ///
    /// Engine-internal: reads the host's `TextureInner` directly; cdylib
    /// callers reach this through future per-method vtable callbacks
    /// (not wired today — `host_inner()` panics with `catch_unwind` at
    /// the FFI boundary).
    pub fn iosurface_id(&self) -> Option<u32> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            self.host_inner()
                .metal_texture
                .as_ref()
                .and_then(|mt| mt.iosurface_id())
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            None
        }
    }

    /// Get the platform-native sharing handle for this texture.
    ///
    /// Returns the appropriate handle type for the current platform:
    /// - macOS/iOS: `IOSurface { id }`
    /// - Linux: `DmaBuf { fd }` (cross-DSO safe — dispatches through
    ///   [`GpuContextLimitedAccessVTable::texture_native_dma_buf_fd`]
    ///   so cdylib subprocess adapters can export DMA-BUF FDs to a
    ///   different GPU API — CUDA, OpenGL, downstream IPC — without
    ///   touching host-internal `TextureInner` layout).
    /// - Windows: `DxgiSharedHandle { handle }` (when implemented).
    ///
    /// Returns `None` if no sharing handle is available (no Vulkan
    /// backing, export failed, or the platform doesn't expose one).
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // macOS / iOS native-handle path stays host-only until
            // macOS cdylib adapter work resumes (#908 deferred list).
            // The `host_inner()` panic guard catches the cdylib case
            // at the FFI boundary.
            self.host_inner()
                .metal_texture
                .as_ref()
                .and_then(|mt| mt.iosurface_id())
                .map(|id| NativeTextureHandle::IOSurface { id })
        }
        #[cfg(target_os = "linux")]
        {
            // Linux DMA-BUF export is cross-DSO safe: the slot returns
            // the FD as a primitive `i64` (sentinel `-1` encodes
            // `None`), which never crosses an Arc<TextureInner>
            // layout boundary. Host mode resolves the slot pointer
            // to `&HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`'s callback
            // (which in turn calls `HostVulkanTexture::export_dma_buf_fd`);
            // cdylib mode resolves to the host-installed pointer
            // routed through `HostServices::gpu_context_limited_access_vtable`.
            if self.handle.is_null() || self.vtable.is_null() {
                return None;
            }
            // SAFETY: `vtable` and `handle` were paired at construction
            // by `from_arc_into_raw` / `from_raw_handle_for_cdylib`;
            // the `texture_native_dma_buf_fd` slot accepts the texture
            // handle directly and returns `-1` (no FD) or a non-
            // negative `RawFd` widened to `i64`.
            let fd_i64 = unsafe {
                ((*self.vtable).texture_native_dma_buf_fd)(self.handle)
            };
            if fd_i64 < 0 {
                None
            } else {
                Some(NativeTextureHandle::DmaBuf { fd: fd_i64 as i32 })
            }
        }
        #[cfg(target_os = "windows")]
        {
            // Windows DXGI shared handle: deferred until Windows
            // cdylib adapter work begins.
            None
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "windows"
        )))]
        {
            None
        }
    }

    /// Get the underlying Metal texture (macOS/iOS only).
    ///
    /// Panics if no Metal texture is available.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_metal_texture(&self) -> &metal::TextureRef {
        self.host_inner()
            .metal_texture
            .as_ref()
            .expect("No Metal texture available")
            .as_metal_texture()
    }

    /// Get the underlying IOSurface if this texture is IOSurface-backed (macOS/iOS only).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_iosurface(&self) -> Option<&objc2_io_surface::IOSurface> {
        self.host_inner()
            .metal_texture
            .as_ref()
            .and_then(|mt| mt.iosurface())
    }

    /// Create from a Metal texture.
    ///
    /// When Metal is the GPU backend, this sets both `inner` and `metal_texture`.
    /// When Vulkan is the GPU backend on macOS, this only sets `metal_texture`
    /// (used for Apple platform interop like IOSurface).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn from_metal(texture: crate::metal::rhi::MetalTexture) -> Self {
        let arc_texture = Arc::new(texture);

        let inner = TextureInner {
            // When Metal is backend, inner is the MetalTexture
            #[cfg(not(feature = "backend-vulkan"))]
            inner: arc_texture.clone(),
            // When Vulkan is backend on macOS, inner would be HostVulkanTexture (not set here)
            #[cfg(feature = "backend-vulkan")]
            inner: Arc::new(crate::vulkan::rhi::HostVulkanTexture::placeholder()),
            metal_texture: Some(arc_texture),
        };
        Self::from_inner(inner)
    }
}

// Privileged Host-flavor accessors (`from_vulkan`, `vulkan_inner`)
// live on the [`crate::host_rhi::HostTextureExt`] extension
// trait — type-system-enforced boundary so the SDK's public inherent
// impl stays Host-free. Engine RHI helpers and in-tree adapters
// `use crate::host_rhi::HostTextureExt;` to surface them.

impl Clone for Texture {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction by
            // `from_arc_into_raw`; the vtable's `clone_texture` contract
            // is `Arc::increment_strong_count(handle)` on the host side.
            // Balanced by the Drop impl below.
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
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `clone_texture` bumps.
            // `drop_texture` decrements the host-side Arc; when refcount
            // hits zero the underlying `TextureInner` is freed in
            // host-compiled code.
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

// =============================================================================
// Layout regression tests
// =============================================================================

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_layout() {
        // Pin the byte-level shape of the cross-DSO
        // `Texture`. Fields:
        //   handle       : *const c_void  → offset 0,  size 8
        //   vtable       : *const VTable  → offset 8,  size 8
        //   width_cached : u32            → offset 16, size 4
        //   height_cached: u32            → offset 20, size 4
        //   format_raw   : u32            → offset 24, size 4
        //   _padding     : u32            → offset 28, size 4
        // Total: 32 bytes, 8-byte alignment (pinned by the pointer fields).
        assert_eq!(size_of::<Texture>(), 32);
        assert_eq!(align_of::<Texture>(), 8);
        assert_eq!(offset_of!(Texture, handle), 0);
        assert_eq!(offset_of!(Texture, vtable), 8);
        assert_eq!(offset_of!(Texture, width_cached), 16);
        assert_eq!(offset_of!(Texture, height_cached), 20);
        assert_eq!(offset_of!(Texture, format_raw), 24);
        assert_eq!(offset_of!(Texture, _padding), 28);
    }

    /// Compile-time witness that `Texture` is Send + Sync.
    #[test]
    fn texture_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Texture>();
    }
}
