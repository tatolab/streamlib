//! GPU texture abstraction

/// Platform-agnostic GPU texture handle
///
/// This is an opaque type that wraps platform-specific GPU texture handles:
/// - Metal: MTLTexture
/// - Vulkan: VkImage
#[derive(Debug, Clone)]
pub struct GpuTexture {
    pub(crate) handle: u64,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    R8Unorm,
}
