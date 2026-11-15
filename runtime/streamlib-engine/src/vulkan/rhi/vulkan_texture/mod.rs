// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan texture implementation for RHI.

#[cfg(target_os = "linux")]
mod linux_dma_buf_and_opaque_fd_texture_creation;
#[cfg(target_os = "linux")]
mod linux_dma_buf_export_and_import;
#[cfg(target_os = "macos")]
mod macos_iosurface_backing;
mod texture_accessors_layout_and_views;
mod texture_creation;

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::{TextureFormat, TextureUsages};

use super::HostVulkanDevice;

/// Convert RHI TextureFormat to Vulkan format.
fn texture_format_to_vk(format: TextureFormat) -> vk::Format {
    match format {
        TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        TextureFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        TextureFormat::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
    }
}

/// Convert RHI TextureUsages to Vulkan usage flags.
fn texture_usages_to_vk(usage: TextureUsages) -> vk::ImageUsageFlags {
    let mut flags = vk::ImageUsageFlags::empty();

    if usage.contains(TextureUsages::COPY_SRC) {
        flags |= vk::ImageUsageFlags::TRANSFER_SRC;
    }
    if usage.contains(TextureUsages::COPY_DST) {
        flags |= vk::ImageUsageFlags::TRANSFER_DST;
    }
    if usage.contains(TextureUsages::TEXTURE_BINDING) {
        flags |= vk::ImageUsageFlags::SAMPLED;
    }
    if usage.contains(TextureUsages::STORAGE_BINDING) {
        flags |= vk::ImageUsageFlags::STORAGE;
    }
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        flags |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
    }

    // Ensure at least some usage is set
    if flags.is_empty() {
        flags = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC;
    }

    flags
}

/// Per-construction Vulkan image metadata exposed through
/// [`super::VulkanTextureLike`] to surface-adapter consumers (Skia in
/// particular needs the full create-time descriptor to wrap the image
/// as a `GrBackendRenderTarget`).
///
/// Memory binding (`vk_memory`, `vk_memory_offset`, `vk_memory_size`)
/// is populated lazily from VMA's `get_allocation_info` for the VMA
/// path or directly from the import call for the DMA-BUF path — see
/// [`HostVulkanTexture::vk_memory_binding`].
#[derive(Clone, Copy)]
struct HostVkImageMeta {
    vk_image_tiling: vk::ImageTiling,
    vk_image_usage_flags: vk::ImageUsageFlags,
}

impl Default for HostVkImageMeta {
    fn default() -> Self {
        Self {
            vk_image_tiling: vk::ImageTiling::OPTIMAL,
            vk_image_usage_flags: vk::ImageUsageFlags::empty(),
        }
    }
}

/// Vulkan texture wrapper.
///
/// Wraps a VkImage with associated memory and metadata.
///
/// # Cdylib reachability
///
/// Two paths give workspace plugin cdylibs access to an
/// `Arc<HostVulkanTexture>` for adapter `register_host_surface` calls
/// (the opengl / skia / vulkan / cuda surface adapters' DMA-BUF
/// render-target story):
///
/// 1. **High-level acquire (recommended for standard render-target
///    use):** call
///    `GpuContextFullAccess::acquire_render_target_dma_buf_image(w, h, format)`
///    — the FullAccess `acquire_render_target_dma_buf_image` entry
///    point. Returns a `Texture`. Extract the underlying
///    `Arc<HostVulkanTexture>` via the v10 `host_vulkan_texture_arc`
///    bridge (`HostTextureExt::host_vulkan_texture_arc`). The slot's
///    host-side body does FOURCC mapping, queries the device's RT-capable
///    DRM modifier list, and allocates the VkImage through
///    `new_render_target_dma_buf` internally — the cdylib never touches
///    the modifier list directly for this path.
///
///    Once the cdylib holds the Arc, the `pub` methods
///    [`Self::export_dma_buf_fd`], [`Self::dma_buf_plane_layout`], and
///    [`Self::chosen_drm_format_modifier`] are all reachable for
///    building the adapter's `HostSurfaceRegistration` (DMA-BUF FD +
///    plane offset/stride + modifier).
///
/// 2. **Low-level allocation (advanced — for explicit modifier choice
///    such as NVIDIA sampler-only `external_only=TRUE` modifiers):**
///    obtain `Arc<HostVulkanDevice>` via the v9 `host_vulkan_device_arc`
///    slot, query
///    `device_arc.drm_modifier_table().rt_modifiers(fourcc)` for the
///    candidate list (or build a custom list), and call
///    `HostVulkanTexture::new_render_target_dma_buf(device_arc.device(), &desc, &modifiers)`
///    directly. The constructor body uses only `pub` accessors on
///    `HostVulkanDevice` (`allocator`, `dma_buf_image_pool_tiled`, …)
///    plus `vulkanalia-vma` — no `host_inner()` deref.
///
/// Adding a `host_inner()` guard inside any of
/// the `new*` constructor bodies (`new`, `new_render_target_dma_buf`,
/// `new_opaque_fd_export`, etc.) would break path 2 silently — reviewers
/// touching constructor bodies must keep them guard-free. The cdylib
/// surface-adapter dlopen smoke tests exercise the full end-to-end
/// path.
pub struct HostVulkanTexture {
    /// HostVulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Option<Arc<HostVulkanDevice>>,
    image: Option<vk::Image>,
    /// VMA allocation, carrying whatever export flavour the pool it came from declares.
    allocation: Option<vma::Allocation>,
    /// Raw device memory for the paths VMA cannot serve — a DMA-BUF import, or
    /// the binding an IOSurface-backed image takes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    imported_memory: Option<vk::DeviceMemory>,
    /// Allocation size for the imported_memory path (the size we passed
    /// to `vkAllocateMemory` via `import_dma_buf_memory`). Tracked
    /// because `VulkanTextureLike::vk_memory_size` needs it for Skia's
    /// `GrVkAlloc.fSize`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    imported_memory_size: vk::DeviceSize,
    /// Lazy-cached image view for this texture.
    cached_image_view: OnceLock<vk::ImageView>,
    /// The private IOSurface this image's storage is, when it was allocated
    /// to cross to a helper process.
    #[cfg(target_os = "macos")]
    backing_iosurface: Option<crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads>,
    /// Whether this texture was allocated from the OPAQUE_FD image
    /// pool. Gates `export_opaque_fd_memory`: callers that
    /// allocated via [`Self::new`] / `_render_target_dma_buf` / `_device_local`
    /// must NOT call the OPAQUE_FD export accessor because the underlying
    /// memory carries `DMA_BUF_EXT` (or no) export handle types, and
    /// `vkGetMemoryFdKHR` with `OPAQUE_FD` would fail at the driver.
    #[cfg(target_os = "linux")]
    is_opaque_fd_export: bool,
    /// DRM format modifier the driver picked for this image. Meaningful
    /// only when [`HostVkImageMeta::vk_image_tiling`] is
    /// [`vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT`] — zero is
    /// `DRM_FORMAT_MOD_LINEAR`, a real modifier, and reads the same as the
    /// zero left here by every path that never went through
    /// `VK_EXT_image_drm_format_modifier`. Render-target adapters propagate
    /// this through `SurfaceTransportHandle` so the consumer's EGL import
    /// can pass it via `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT`.
    #[cfg(target_os = "linux")]
    chosen_drm_format_modifier: u64,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// Per-construction Vulkan image metadata for trait-level
    /// inspection (Skia adapter, debug snapshots).
    vk_image_meta: HostVkImageMeta,
}

impl Clone for HostVulkanTexture {
    fn clone(&self) -> Self {
        Self {
            vulkan_device: None,
            image: None,
            allocation: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
            #[cfg(target_os = "linux")]
            is_opaque_fd_export: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: self.width,
            height: self.height,
            format: self.format,
            vk_image_meta: HostVkImageMeta::default(),
        }
    }
}

impl Drop for HostVulkanTexture {
    fn drop(&mut self) {
        // Destroy cached image view before the image it references
        if let Some(&view) = self.cached_image_view.get()
            && let Some(vk_dev) = &self.vulkan_device
        {
            unsafe { vk_dev.device().destroy_image_view(view, None) };
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.imported_memory.is_some() {
            if let Some(vk_dev) = &self.vulkan_device {
                if let Some(image) = self.image {
                    unsafe { vk_dev.device().destroy_image(image, None) };
                }
                if let Some(memory) = self.imported_memory.take() {
                    vk_dev.free_imported_memory(memory);
                }
            }
            return;
        }

        // VMA path: destroy_image frees both the image and the allocation
        if let (Some(vk_dev), Some(image), Some(allocation)) =
            (&self.vulkan_device, self.image, self.allocation.take())
        {
            unsafe { vk_dev.allocator().destroy_image(image, allocation) };
        }
    }
}

// HostVulkanTexture is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for HostVulkanTexture {}
unsafe impl Sync for HostVulkanTexture {}

impl HostVulkanTexture {
    /// Memory binding tuple `(memory, offset, size)` resolved against
    /// whichever path created the image — VMA for the standard
    /// allocators, or the imported `VkDeviceMemory` for the DMA-BUF
    /// import paths.
    fn vk_memory_binding(&self) -> (vk::DeviceMemory, vk::DeviceSize, vk::DeviceSize) {
        // VMA path: query allocation_info on demand. The lookup is a
        // simple struct read from VMA's internal allocation handle.
        if let (Some(vk_dev), Some(allocation)) = (&self.vulkan_device, self.allocation.as_ref()) {
            let info = vk_dev.allocator().get_allocation_info(*allocation);
            return (info.deviceMemory, info.offset, info.size);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(memory) = self.imported_memory {
            return (memory, 0, self.imported_memory_size);
        }
        (vk::DeviceMemory::null(), 0, 0)
    }
}

impl super::VulkanTextureLike for HostVulkanTexture {
    fn image(&self) -> Option<vk::Image> {
        HostVulkanTexture::image(self)
    }
    fn chosen_drm_format_modifier(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            HostVulkanTexture::chosen_drm_format_modifier(self)
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }
    fn width(&self) -> u32 {
        HostVulkanTexture::width(self)
    }
    fn height(&self) -> u32 {
        HostVulkanTexture::height(self)
    }
    fn format(&self) -> crate::core::rhi::TextureFormat {
        HostVulkanTexture::format(self)
    }
    fn vk_format(&self) -> vk::Format {
        texture_format_to_vk(self.format)
    }
    fn vk_image_tiling(&self) -> vk::ImageTiling {
        HostVulkanTexture::vk_image_tiling(self)
    }
    fn vk_image_usage_flags(&self) -> vk::ImageUsageFlags {
        self.vk_image_meta.vk_image_usage_flags
    }
    fn vk_memory(&self) -> vk::DeviceMemory {
        self.vk_memory_binding().0
    }
    fn vk_memory_offset(&self) -> vk::DeviceSize {
        self.vk_memory_binding().1
    }
    fn vk_memory_size(&self) -> vk::DeviceSize {
        self.vk_memory_binding().2
    }
}

#[cfg(test)]
mod tests;
