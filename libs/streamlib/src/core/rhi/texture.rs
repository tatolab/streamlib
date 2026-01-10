// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI texture abstraction.

use std::sync::Arc;

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

/// Texture pixel formats supported by the RHI.
///
/// Platform backends map these to native format constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFormat {
    /// 8-bit RGBA, unsigned normalized.
    Rgba8Unorm = 0,
    /// 8-bit RGBA, sRGB.
    Rgba8UnormSrgb = 1,
    /// 8-bit BGRA, unsigned normalized.
    Bgra8Unorm = 2,
    /// 8-bit BGRA, sRGB.
    Bgra8UnormSrgb = 3,
    /// 16-bit float RGBA.
    Rgba16Float = 4,
    /// 32-bit float RGBA.
    Rgba32Float = 5,
    /// NV12 YUV (for video decode).
    Nv12 = 6,
}

impl TextureFormat {
    /// Bytes per pixel for this format.
    pub fn bytes_per_pixel(&self) -> u32 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
            Self::Nv12 => 1, // Planar format, varies per plane
        }
    }

    /// Whether this format has an sRGB transfer function.
    pub fn is_srgb(&self) -> bool {
        matches!(self, Self::Rgba8UnormSrgb | Self::Bgra8UnormSrgb)
    }
}

/// Texture usage flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureUsages(u32);

impl TextureUsages {
    pub const NONE: Self = Self(0);
    /// Can be copied from.
    pub const COPY_SRC: Self = Self(1 << 0);
    /// Can be copied to.
    pub const COPY_DST: Self = Self(1 << 1);
    /// Can be bound as a texture (sampled).
    pub const TEXTURE_BINDING: Self = Self(1 << 2);
    /// Can be bound as a storage texture (compute read/write).
    pub const STORAGE_BINDING: Self = Self(1 << 3);
    /// Can be used as a render target.
    pub const RENDER_ATTACHMENT: Self = Self(1 << 4);

    pub fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for TextureUsages {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for TextureUsages {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
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
/// This type wraps the platform-specific texture implementation and provides
/// a unified interface. Use the `as_*` methods to "dip down" to the native
/// texture type when needed for platform-specific operations.
///
/// On macOS/iOS, Metal texture storage is always available for Apple platform
/// interop (IOSurface, CVPixelBuffer) regardless of which GPU backend is selected.
#[derive(Clone)]
pub struct StreamTexture {
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
    pub(crate) inner: Arc<crate::vulkan::rhi::VulkanTexture>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: Arc<crate::windows::rhi::DX12Texture>,

    /// Metal texture for Apple platform services (IOSurface, CVPixelBuffer).
    /// On macOS/iOS with Vulkan backend, textures created from IOSurface are
    /// stored here. When Metal is the backend, this duplicates `inner`.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) metal_texture: Option<Arc<crate::metal::rhi::MetalTexture>>,
}

impl StreamTexture {
    /// Texture width in pixels.
    pub fn width(&self) -> u32 {
        // On macOS, prefer metal_texture if available (for IOSurface-backed textures)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.width();
        }
        self.inner.width()
    }

    /// Texture height in pixels.
    pub fn height(&self) -> u32 {
        // On macOS, prefer metal_texture if available (for IOSurface-backed textures)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.height();
        }
        self.inner.height()
    }

    /// Texture format.
    pub fn format(&self) -> TextureFormat {
        // On macOS, prefer metal_texture if available (for IOSurface-backed textures)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ref mt) = self.metal_texture {
            return mt.format();
        }
        self.inner.format()
    }

    /// Get the IOSurface ID for cross-framework sharing.
    ///
    /// Returns `Some(id)` on macOS/iOS if the texture is backed by an IOSurface.
    /// Returns `None` on other platforms or if no IOSurface is available.
    pub fn iosurface_id(&self) -> Option<u32> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // Use metal_texture for IOSurface access
            self.metal_texture.as_ref().and_then(|mt| mt.iosurface_id())
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
    /// - Linux: `DmaBuf { fd }` (when implemented)
    /// - Windows: `DxgiSharedHandle { handle }` (when implemented)
    ///
    /// Returns `None` if no sharing handle is available.
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // Use metal_texture for IOSurface access
            self.metal_texture
                .as_ref()
                .and_then(|mt| mt.iosurface_id())
                .map(|id| NativeTextureHandle::IOSurface { id })
        }
        #[cfg(target_os = "linux")]
        {
            // TODO: Return DmaBuf when implemented
            None
        }
        #[cfg(target_os = "windows")]
        {
            // TODO: Return DxgiSharedHandle when implemented
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
        self.metal_texture
            .as_ref()
            .expect("No Metal texture available")
            .as_metal_texture()
    }

    /// Get the underlying IOSurface if this texture is IOSurface-backed (macOS/iOS only).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_iosurface(&self) -> Option<&objc2_io_surface::IOSurface> {
        self.metal_texture.as_ref().and_then(|mt| mt.iosurface())
    }

    /// Create from a Metal texture.
    ///
    /// When Metal is the GPU backend, this sets both `inner` and `metal_texture`.
    /// When Vulkan is the GPU backend on macOS, this only sets `metal_texture`
    /// (used for Apple platform interop like IOSurface).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn from_metal(texture: crate::metal::rhi::MetalTexture) -> Self {
        let arc_texture = Arc::new(texture);

        Self {
            // When Metal is backend, inner is the MetalTexture
            #[cfg(not(feature = "backend-vulkan"))]
            inner: arc_texture.clone(),
            // When Vulkan is backend on macOS, inner would be VulkanTexture (not set here)
            #[cfg(feature = "backend-vulkan")]
            inner: Arc::new(crate::vulkan::rhi::VulkanTexture::placeholder()),
            metal_texture: Some(arc_texture),
        }
    }

    /// Create from a Vulkan texture (Vulkan backend only).
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub fn from_vulkan(texture: crate::vulkan::rhi::VulkanTexture) -> Self {
        Self {
            inner: Arc::new(texture),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            metal_texture: None,
        }
    }
}

impl std::fmt::Debug for StreamTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamTexture")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("format", &self.format())
            .finish()
    }
}

// Ensure StreamTexture is Send + Sync
unsafe impl Send for StreamTexture {}
unsafe impl Sync for StreamTexture {}
