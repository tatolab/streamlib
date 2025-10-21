//! GPU texture abstraction

/// Platform-specific GPU texture handle
///
/// Wraps native GPU texture types for zero-copy operations:
/// - Metal (macOS/iOS): MTLTexture from IOSurface
/// - Vulkan (Linux): VkImage from DMA-BUF
/// - D3D12 (Windows): ID3D12Resource
#[derive(Debug)]
pub enum GpuTextureHandle {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Metal {
        texture: u64, // Retained<ProtocolObject<dyn MTLTexture>> stored as raw pointer
    },

    #[cfg(target_os = "linux")]
    Vulkan {
        image: u64, // VkImage handle
    },

    #[cfg(target_os = "windows")]
    D3D12 {
        resource: u64, // ID3D12Resource pointer
    },
}

/// Platform-agnostic GPU texture
///
/// This is the universal currency for GPU textures in streamlib.
/// All platform-specific code converts to/from this type for portability.
#[derive(Debug)]
pub struct GpuTexture {
    pub handle: GpuTextureHandle,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        // Platform-specific cleanup
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // The actual Metal texture cleanup is handled by streamlib-apple
            // when the Retained<MTLTexture> is reconstructed and dropped
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    R8Unorm,
}
