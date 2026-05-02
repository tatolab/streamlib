// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan texture implementation for RHI.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::{Result, StreamError};

use super::HostVulkanDevice;

#[cfg(target_os = "linux")]
use super::drm_modifier_probe::fourcc;

/// Map a `TextureFormat` to the DRM FOURCC the EGL probe uses to look up
/// render-target-capable modifiers. Returns `None` for formats that aren't
/// part of the cross-language surface ABI (the ones the modifier probe
/// doesn't interrogate).
#[cfg(target_os = "linux")]
fn texture_format_to_fourcc(format: TextureFormat) -> Option<u32> {
    match format {
        // BGRA8_UNORM in Vulkan = ARGB8888 in DRM (channel order matches once
        // little-endian byte layout is taken into account).
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => {
            Some(fourcc::DRM_FORMAT_ARGB8888)
        }
        // RGBA8_UNORM in Vulkan = ABGR8888 in DRM.
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {
            Some(fourcc::DRM_FORMAT_ABGR8888)
        }
        TextureFormat::Nv12 => Some(fourcc::DRM_FORMAT_NV12),
        TextureFormat::Rgba16Float | TextureFormat::Rgba32Float => None,
    }
}

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
/// path or directly from the import call for the DMA-BUF / IOSurface
/// path — see [`HostVulkanTexture::vk_memory_binding`].
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
/// Can be created from scratch or imported from an IOSurface via VK_EXT_metal_objects.
pub struct HostVulkanTexture {
    /// HostVulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Option<Arc<HostVulkanDevice>>,
    image: Option<vk::Image>,
    /// VMA allocation (always allocated with DMA-BUF export flags via HostVulkanDevice).
    allocation: Option<vma::Allocation>,
    /// Imported device memory for DMA-BUF import path (VMA cannot import external memory).
    #[cfg(target_os = "linux")]
    imported_memory: Option<vk::DeviceMemory>,
    /// Allocation size for the imported_memory path (the size we passed
    /// to `vkAllocateMemory` via `import_dma_buf_memory`). Tracked
    /// because `VulkanTextureLike::vk_memory_size` needs it for Skia's
    /// `GrVkAlloc.fSize`.
    #[cfg(target_os = "linux")]
    imported_memory_size: vk::DeviceSize,
    /// Cached DMA-BUF fd to avoid leaking a new fd on each export call.
    #[cfg(target_os = "linux")]
    cached_dma_buf_fd: OnceLock<std::os::unix::io::RawFd>,
    /// Lazy-cached image view for this texture.
    cached_image_view: OnceLock<vk::ImageView>,
    /// Whether this texture was imported from IOSurface (no memory to free).
    imported_from_iosurface: bool,
    /// Whether this texture was imported from a DMA-BUF fd (uses imported_memory path).
    #[cfg(target_os = "linux")]
    imported_from_dma_buf: bool,
    /// DRM format modifier the driver picked for this image. Zero means
    /// `DRM_FORMAT_MOD_LINEAR` or "not applicable" (image was not created
    /// with [`vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT`]). Render-target
    /// adapters propagate this through `SurfaceTransportHandle` so the
    /// consumer's EGL import can pass it via
    /// `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT`.
    #[cfg(target_os = "linux")]
    chosen_drm_format_modifier: u64,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// Per-construction Vulkan image metadata for trait-level
    /// inspection (Skia adapter, debug snapshots).
    vk_image_meta: HostVkImageMeta,
}

impl HostVulkanTexture {
    /// Create a new DMA-BUF exportable Vulkan texture via the device's
    /// dedicated VMA export pool.
    ///
    /// The export pool is configured with `pMemoryAllocateNext` set to
    /// `VkExportMemoryAllocateInfo::DMA_BUF_EXT`, isolating exportable
    /// allocations from the default VMA pool. This avoids NVIDIA driver
    /// failures where global export configuration causes OOM after swapchain
    /// creation.
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, desc: &TextureDescriptor) -> Result<Self> {
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // Declare DMA-BUF handle type at image creation — required by Vulkan spec
        // (VUID-vkBindImageMemory-memory-02728) when memory will be allocated with
        // VkExportMemoryAllocateInfo.
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_image_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        // Prefer the DMA-BUF image pool; fall back to default allocator (no export)
        // if the pool isn't available (e.g., external memory unsupported).
        let (image, allocation) = {
            #[cfg(target_os = "linux")]
            let result = if let Some(pool) = vulkan_device.dma_buf_image_pool() {
                unsafe { pool.create_image(image_info, &alloc_opts) }
            } else {
                let allocator = vulkan_device.allocator();
                unsafe { allocator.create_image(image_info, &alloc_opts) }
            };
            #[cfg(not(target_os = "linux"))]
            let result = {
                let allocator = vulkan_device.allocator();
                unsafe { allocator.create_image(image_info, &alloc_opts) }
            };
            result.map_err(|e| {
                StreamError::GpuError(format!("Failed to create exportable image: {e}"))
            })?
        };

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_memory_size: 0,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Create a non-exportable DEVICE_LOCAL texture via the default VMA allocator.
    ///
    /// Unlike [`new`] which uses the DMA-BUF export pool, this uses the default
    /// VMA allocator with no external memory info. For same-process textures that
    /// don't need cross-process sharing.
    pub fn new_device_local(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let alloc_opts = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();
        let (image, allocation) =
            unsafe { allocator.create_image(image_info, &alloc_opts) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create device-local image: {e}"))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_memory_size: 0,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Create a render-target-capable DMA-BUF exportable texture using
    /// `VK_EXT_image_drm_format_modifier`.
    ///
    /// `modifier_candidates` MUST come from
    /// [`crate::vulkan::rhi::drm_modifier_probe::DrmModifierTable::rt_modifiers`]
    /// — every entry has `external_only=FALSE` per the EGL probe, so the
    /// exported FD can be imported on the consumer side as a
    /// `GL_TEXTURE_2D` and bound as an FBO color attachment. The driver
    /// picks one modifier from the list at allocation time; the choice is
    /// available via [`Self::chosen_drm_format_modifier`] after
    /// construction.
    ///
    /// Empty `modifier_candidates` ⇒ `Err` — there is no fallback to
    /// linear at this entry point because linear DMA-BUFs are sampler-only
    /// on NVIDIA Linux (see
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`). Callers that
    /// want a linear allocation should use [`Self::new`].
    #[cfg(target_os = "linux")]
    pub fn new_render_target_dma_buf(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
        modifier_candidates: &[u64],
    ) -> Result<Self> {
        if modifier_candidates.is_empty() {
            return Err(StreamError::GpuError(
                "new_render_target_dma_buf: empty modifier list — EGL did not advertise an external_only=FALSE modifier for this format. Linear DMA-BUF is sampler-only on NVIDIA; refusing to allocate.".into(),
            ));
        }

        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // VK_EXT_image_drm_format_modifier requires the modifier list to
        // outlive the ImageCreateInfo. Hold the slice in a local — the
        // builder borrows from it via the pNext chain pointer, and the
        // chain is consumed by create_image before this function returns.
        let mut modifier_list_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
            .drm_format_modifiers(modifier_candidates);

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut modifier_list_info)
            .push_next(&mut external_image_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        // Prefer the dedicated tiled DMA-BUF image pool; fall back to the
        // default allocator (still with the export-info pNext chain) only
        // when external memory isn't supported. The pool's underlying
        // `VkDeviceMemory` block is pre-warmed at `HostVulkanDevice::new()`
        // (see `nvidia-dma-buf-after-swapchain.md`), so the post-swapchain
        // NVIDIA cap doesn't apply to the pooled path.
        let (image, allocation) = if let Some(pool) =
            vulkan_device.dma_buf_image_pool_tiled()
        {
            unsafe { pool.create_image(image_info, &alloc_opts) }
        } else {
            let allocator = vulkan_device.allocator();
            unsafe { allocator.create_image(image_info, &alloc_opts) }
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create render-target DMA-BUF image (modifiers={:?}): {e}",
                modifier_candidates
            ))
        })?;

        // Read back which modifier the driver actually chose.
        let chosen = {
            use vulkanalia::vk::ExtImageDrmFormatModifierExtensionDeviceCommands;
            let mut props = vk::ImageDrmFormatModifierPropertiesEXT::default();
            let device = vulkan_device.device();
            unsafe { device.get_image_drm_format_modifier_properties_ext(image, &mut props) }
                .map_err(|e| {
                    // Image leaks on this branch — the allocator owns it. We
                    // destroy it explicitly so the caller doesn't need to.
                    unsafe { vulkan_device.allocator().destroy_image(image, allocation) };
                    StreamError::GpuError(format!(
                        "vkGetImageDrmFormatModifierPropertiesEXT failed: {e}"
                    ))
                })?;
            props.drm_format_modifier
        };

        if !modifier_candidates.contains(&chosen) {
            unsafe { vulkan_device.allocator().destroy_image(image, allocation) };
            return Err(StreamError::GpuError(format!(
                "Driver picked modifier 0x{:016x} that wasn't in our candidate list {:?} — VUID violation",
                chosen, modifier_candidates
            )));
        }

        tracing::info!(
            "HostVulkanTexture render-target DMA-BUF: {}x{} {:?} → modifier 0x{:016x}",
            desc.width,
            desc.height,
            desc.format,
            chosen
        );

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            imported_memory: None,
            imported_memory_size: 0,
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            imported_from_dma_buf: false,
            chosen_drm_format_modifier: chosen,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Import a texture from an IOSurface via VK_EXT_metal_objects.
    ///
    /// This creates a Vulkan image backed by the same GPU memory as the IOSurface,
    /// enabling zero-copy interop between Metal and Vulkan.
    ///
    /// # Arguments
    /// * `device` - The Vulkan device
    /// * `iosurface_ref` - Raw pointer to the IOSurfaceRef
    /// * `width` - Texture width in pixels
    /// * `height` - Texture height in pixels
    /// * `format` - Texture format
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn from_iosurface(
        device: &vulkanalia::Device,
        iosurface_ref: *const std::ffi::c_void,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Self> {
        if iosurface_ref.is_null() {
            return Err(StreamError::TextureError(
                "Cannot import null IOSurface".into(),
            ));
        }

        let vk_format = texture_format_to_vk(format);

        // Create import info for IOSurface
        let import_info = vk::ImportMetalIOSurfaceInfoEXT {
            io_surface: iosurface_ref as *mut _,
            ..Default::default()
        };

        // Create image with import info in pNext chain
        let image_info = vk::ImageCreateInfo {
            image_type: vk::ImageType::_2D,
            format: vk_format,
            extent: vk::Extent3D {
                width,
                height,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
            p_next: &import_info as *const _ as *const _,
            ..Default::default()
        };

        let image = unsafe { device.create_image(&image_info, None) }
            .map(|r| r)
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create image from IOSurface: {e}"))
            })?;

        tracing::debug!(
            "Imported IOSurface as Vulkan image: {}x{} {:?}",
            width,
            height,
            format
        );

        Ok(Self {
            vulkan_device: None,
            image: Some(image),
            allocation: None,
            imported_from_iosurface: true,
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            },
        })
    }

    /// Create a placeholder texture for cases where a HostVulkanTexture is needed
    /// but the actual texture is stored elsewhere (e.g., Metal texture on macOS).
    pub fn placeholder() -> Self {
        Self {
            vulkan_device: None,
            image: None,
            allocation: None,
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_memory_size: 0,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: 0,
            height: 0,
            format: TextureFormat::Rgba8Unorm,
            vk_image_meta: HostVkImageMeta::default(),
        }
    }

    /// Get the underlying Vulkan image handle.
    pub fn image(&self) -> Option<vk::Image> {
        self.image
    }

    /// Texture width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Lazy-cached image view for this texture.
    ///
    /// Creates the image view on first call, returns the cached handle on
    /// subsequent calls. The view uses the texture's own format and full
    /// subresource range.
    pub fn image_view(&self) -> Result<vk::ImageView> {
        if let Some(&view) = self.cached_image_view.get() {
            return Ok(view);
        }

        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot create image view: no HostVulkanDevice stored".into())
        })?;
        let image = self.image.ok_or_else(|| {
            StreamError::GpuError("Cannot create image view: no image".into())
        })?;

        let vk_format = texture_format_to_vk(self.format);
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(vk_format)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();

        let view = unsafe { vk_dev.device().create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create image view: {e}")))?;

        let _ = self.cached_image_view.set(view);
        Ok(*self.cached_image_view.get().unwrap())
    }
}

#[cfg(target_os = "linux")]
impl HostVulkanTexture {
    /// DRM format modifier the driver picked at allocation time.
    ///
    /// Zero for textures created via [`Self::new`] / [`Self::new_device_local`]
    /// or imported via [`Self::from_dma_buf_fd`] — those paths do not go
    /// through `VK_EXT_image_drm_format_modifier`. Render-target textures
    /// allocated via [`Self::new_render_target_dma_buf`] return the modifier
    /// the driver chose from the candidate list.
    pub fn chosen_drm_format_modifier(&self) -> u64 {
        self.chosen_drm_format_modifier
    }

    /// Per-plane DMA-BUF layout for this texture, in plane index order.
    ///
    /// Each entry is `(offset_bytes, row_pitch_bytes)` from
    /// `vkGetImageSubresourceLayout` — the values consumer-side EGL imports
    /// must pass in `EGL_DMA_BUF_PLANE{N}_OFFSET_EXT` /
    /// `EGL_DMA_BUF_PLANE{N}_PITCH_EXT`.
    ///
    /// For single-plane formats (BGRA/RGBA) returns one entry. NV12 returns
    /// two (Y plane, then UV). Returns `Err` for textures without a backing
    /// image or when the format's plane count isn't supported by this RHI
    /// build.
    pub fn dma_buf_plane_layout(&self) -> Result<Vec<(u64, u64)>> {
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("dma_buf_plane_layout: no HostVulkanDevice".into())
        })?;
        let image = self.image.ok_or_else(|| {
            StreamError::GpuError("dma_buf_plane_layout: no image".into())
        })?;

        let plane_count = match self.format {
            TextureFormat::Nv12 => 2,
            _ => 1,
        };

        let mut planes = Vec::with_capacity(plane_count);
        for plane_idx in 0..plane_count {
            // For DRM_FORMAT_MODIFIER_EXT images use the MEMORY_PLANE aspect;
            // for OPTIMAL/LINEAR images use COLOR (or PLANE_0/_1 for NV12).
            let aspect_mask = if self.chosen_drm_format_modifier != 0 {
                match plane_idx {
                    0 => vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                    1 => vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
                    2 => vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
                    3 => vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
                    _ => return Err(StreamError::GpuError(format!(
                        "dma_buf_plane_layout: plane index {plane_idx} out of range"
                    ))),
                }
            } else if matches!(self.format, TextureFormat::Nv12) {
                match plane_idx {
                    0 => vk::ImageAspectFlags::PLANE_0,
                    1 => vk::ImageAspectFlags::PLANE_1,
                    _ => return Err(StreamError::GpuError(format!(
                        "dma_buf_plane_layout: NV12 plane {plane_idx} out of range"
                    ))),
                }
            } else {
                vk::ImageAspectFlags::COLOR
            };

            let subres = vk::ImageSubresource::builder()
                .aspect_mask(aspect_mask)
                .mip_level(0)
                .array_layer(0)
                .build();
            let layout = unsafe {
                vk_dev
                    .device()
                    .get_image_subresource_layout(image, &subres)
            };
            planes.push((layout.offset, layout.row_pitch));
        }

        Ok(planes)
    }

    /// Export the texture's memory as a DMA-BUF file descriptor.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            return Ok(fd);
        }

        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no HostVulkanDevice stored".into())
        })?;

        // Get DeviceMemory from raw allocation (export/import path) or VMA allocation
        let device_memory = if let Some(memory) = self.imported_memory {
            memory
        } else if let Some(allocation) = self.allocation.as_ref() {
            let alloc_info = vk_dev.allocator().get_allocation_info(*allocation);
            alloc_info.deviceMemory
        } else {
            return Err(StreamError::GpuError(
                "Cannot export DMA-BUF from texture without memory".into(),
            ));
        };

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { vk_dev.device().get_memory_fd_khr(&get_fd_info) }
            .map(|r| r)
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        let _ = self.cached_dma_buf_fd.set(fd);
        Ok(fd)
    }

    /// Subprocess-side import of a render-target DMA-BUF image.
    ///
    /// The host allocated the image via [`Self::new_render_target_dma_buf`]
    /// with a tiled DRM modifier (LINEAR is sampler-only on NVIDIA). The
    /// subprocess receives:
    /// - `fds` — DMA-BUF file descriptors, one per plane.
    /// - `plane_offsets` / `plane_strides` — exact layout the host's
    ///   `vkGetImageSubresourceLayout` reported.
    /// - `drm_format_modifier` — the modifier the host's driver chose;
    ///   non-zero, must match an `external_only=FALSE` modifier on the
    ///   subprocess's GPU.
    ///
    /// Builds a subprocess-local `VkImage` with
    /// `VkImageDrmFormatModifierExplicitCreateInfoEXT` chained, imports
    /// the DMA-BUF memory, and binds the image. Symmetric to the host
    /// allocation; same modifier on both sides keeps the GPU memory
    /// layout consistent.
    ///
    /// fd ownership: the subprocess transfers ownership to Vulkan on
    /// success (the driver `dup`s internally and releases on
    /// `vkFreeMemory`). On error the caller still owns `fds[0]`.
    pub fn import_render_target_dma_buf(
        vulkan_device: &Arc<HostVulkanDevice>,
        fds: &[std::os::unix::io::RawFd],
        plane_offsets: &[u64],
        plane_strides: &[u64],
        drm_format_modifier: u64,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        if fds.is_empty() {
            return Err(StreamError::GpuError(
                "import_render_target_dma_buf: empty fd vec".into(),
            ));
        }
        if plane_offsets.len() != fds.len() || plane_strides.len() != fds.len() {
            return Err(StreamError::GpuError(format!(
                "import_render_target_dma_buf: plane arrays length mismatch — fds={} offsets={} strides={}",
                fds.len(),
                plane_offsets.len(),
                plane_strides.len()
            )));
        }
        if drm_format_modifier == 0 {
            return Err(StreamError::GpuError(
                "import_render_target_dma_buf: zero (LINEAR) modifier — host should have allocated a tiled modifier; LINEAR DMA-BUFs are sampler-only on NVIDIA"
                    .into(),
            ));
        }

        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);
        // Same usage set as the create_info builder below — tracked
        // separately so VulkanTextureLike::vk_image_usage_flags can
        // report it without re-reading the image_create_info chain.
        // TRANSFER_DST is required by Skia's `check_image_info` gate
        // (and by symmetric host↔consumer parity); see the matching
        // comment in
        // `streamlib::core::context::GpuContext::acquire_render_target_dma_buf_image`.
        let usage_flags = vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::COLOR_ATTACHMENT
            // STORAGE for subprocess compute shaders that bind the
            // imported VkImage as a storage image (#531). Must match
            // the host's `acquire_render_target_dma_buf_image` usage
            // flags or the cross-process import fails.
            | vk::ImageUsageFlags::STORAGE;

        let plane_layouts: Vec<vk::SubresourceLayout> = plane_offsets
            .iter()
            .zip(plane_strides.iter())
            .map(|(off, stride)| vk::SubresourceLayout {
                offset: *off,
                size: 0,
                row_pitch: *stride,
                array_pitch: 0,
                depth_pitch: 0,
            })
            .collect();

        let mut explicit_modifier_info =
            vk::ImageDrmFormatModifierExplicitCreateInfoEXT::builder()
                .drm_format_modifier(drm_format_modifier)
                .plane_layouts(&plane_layouts);

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut explicit_modifier_info)
            .push_next(&mut external_image_info);

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "import_render_target_dma_buf: create_image failed (modifier=0x{:016x}): {e}",
                drm_format_modifier
            ))
        })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

        // Use plane 0's fd for the import; multi-plane DRM modifiers
        // bind separate memory per plane via VkBindImageMemoryInfo +
        // VkBindImagePlaneMemoryInfo, which we'll wire when a multi-plane
        // consumer surfaces. Single-plane covers BGRA / RGBA — the
        // formats #510 publishes RT modifiers for today.
        let memory = vulkan_device
            .import_dma_buf_memory(
                fds[0],
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }
            .map_err(|e| {
                vulkan_device.free_imported_memory(memory);
                unsafe { device.destroy_image(image, None) };
                StreamError::GpuError(format!(
                    "import_render_target_dma_buf: bind_image_memory failed: {e}"
                ))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            imported_from_dma_buf: true,
            chosen_drm_format_modifier: drm_format_modifier,
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Import a texture from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<HostVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);
        let usage_flags = vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::SAMPLED;

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        let image = unsafe { device.create_image(&image_info, None) }
            .map(|r| r)
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create image for DMA-BUF import: {e}"))
            })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

        // VMA cannot import external memory — use raw import path in the RHI
        let memory = vulkan_device
            .import_dma_buf_memory(
                fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }
            .map(|_| ())
            .map_err(|e| {
                vulkan_device.free_imported_memory(memory);
                unsafe { device.destroy_image(image, None) };
                StreamError::GpuError(format!("Failed to bind imported memory: {e}"))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            imported_from_dma_buf: true,
            chosen_drm_format_modifier: 0,
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::LINEAR,
                vk_image_usage_flags: usage_flags,
            },
        })
    }
}

impl Clone for HostVulkanTexture {
    fn clone(&self) -> Self {
        Self {
            vulkan_device: None,
            image: None,
            allocation: None,
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_memory_size: 0,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
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
        if let Some(&view) = self.cached_image_view.get() {
            if let Some(vk_dev) = &self.vulkan_device {
                unsafe { vk_dev.device().destroy_image_view(view, None) };
            }
        }

        #[cfg(target_os = "linux")]
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            unsafe { libc::close(fd) };
        }

        if self.imported_from_iosurface {
            // IOSurface manages the memory — only destroy the image handle
            if let (Some(vk_dev), Some(image)) = (&self.vulkan_device, self.image) {
                unsafe { vk_dev.device().destroy_image(image, None) };
            }
            return;
        }

        #[cfg(target_os = "linux")]
        if self.imported_from_dma_buf {
            // DMA-BUF import path: raw DeviceMemory, not VMA
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
    /// allocators, the imported `VkDeviceMemory` for the DMA-BUF
    /// import paths, or `(null, 0, 0)` for placeholder / IOSurface-
    /// import textures (Skia consumers must check before relying on
    /// `vk_memory()`).
    fn vk_memory_binding(&self) -> (vk::DeviceMemory, vk::DeviceSize, vk::DeviceSize) {
        // VMA path: query allocation_info on demand. The lookup is a
        // simple struct read from VMA's internal allocation handle.
        if let (Some(vk_dev), Some(allocation)) =
            (&self.vulkan_device, self.allocation.as_ref())
        {
            let info = vk_dev.allocator().get_allocation_info(*allocation);
            return (info.deviceMemory, info.offset, info.size);
        }
        // DMA-BUF import path (Linux only).
        #[cfg(target_os = "linux")]
        if let Some(memory) = self.imported_memory {
            return (memory, 0, self.imported_memory_size);
        }
        // Placeholder / IOSurface — no Vulkan memory binding.
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
        self.vk_image_meta.vk_image_tiling
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
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[test]
    fn test_pool_texture_creation_1920x1080_bgra8() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

        assert!(texture.image().is_some());
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.format(), TextureFormat::Bgra8Unorm);

        println!(
            "Pool texture created: {}x{} {:?}",
            texture.width(),
            texture.height(),
            texture.format(),
        );
    }

    #[test]
    fn test_texture_drop_frees_memory() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");
        drop(texture);

        println!("Texture drop completed without panic");
    }

    #[test]
    fn test_multiple_textures_coexist() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);

        let t0 = HostVulkanTexture::new(&device, &desc).expect("texture 0 failed");
        let t1 = HostVulkanTexture::new(&device, &desc).expect("texture 1 failed");
        let t2 = HostVulkanTexture::new(&device, &desc).expect("texture 2 failed");
        let t3 = HostVulkanTexture::new(&device, &desc).expect("texture 3 failed");

        assert!(t0.image().is_some());
        assert!(t1.image().is_some());
        assert!(t2.image().is_some());
        assert!(t3.image().is_some());

        println!("4 textures coexist");

        drop(t0);
        drop(t1);
        drop(t2);
        drop(t3);

        println!("All dropped successfully");
    }

    #[test]
    fn test_dma_buf_export() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

        let fd = texture.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

        println!("DMA-BUF exported: fd={fd}");
        // fd is closed by HostVulkanTexture::drop via cached_dma_buf_fd
    }

    #[test]
    fn test_placeholder_has_no_resources() {
        let tex = HostVulkanTexture::placeholder();
        assert!(tex.image().is_none());
        assert_eq!(tex.width(), 0);
        assert_eq!(tex.height(), 0);
        assert!(tex.allocation.is_none());
        assert!(tex.vulkan_device.is_none());

        println!("Placeholder verified: no image, no memory, no device");
    }

    /// Validates the camera-display allocation pattern after the fix:
    /// 1. Camera: HOST_VISIBLE pixel buffers via raw exportable allocation (DMA-BUF)
    /// 2. Camera: DEVICE_LOCAL compute output image via VMA (no export)
    /// 3. Display: DEVICE_LOCAL camera textures via raw dedicated allocation (no export)
    ///
    /// The original bug: VMA's global pTypeExternalMemoryHandleTypes made ALL block
    /// allocations DMA-BUF exportable. On NVIDIA, after creating a swapchain, the
    /// driver rejected additional DMA-BUF exportable DEVICE_LOCAL block allocations.
    ///
    /// The fix: remove global export config from VMA. Exportable allocations (pixel
    /// buffers, textures for IPC) use raw vkAllocateMemory with VkExportMemoryAllocateInfo.
    /// Internal allocations (display camera textures) use raw vkAllocateMemory with
    /// dedicated allocation + multi-type fallback (no export flags).
    #[test]
    fn test_camera_display_allocation_pattern() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let allocator = device.allocator();
        let vk_device = device.device();
        let width = 1920u32;
        let height = 1080u32;

        // Step 1: Camera pixel buffers via HostVulkanPixelBuffer (raw exportable allocation)
        use crate::vulkan::rhi::HostVulkanPixelBuffer;
        use crate::core::rhi::PixelFormat;
        let mut pixel_buffers = Vec::new();
        for i in 0..4 {
            let buf = HostVulkanPixelBuffer::new(&device, width, height, 4, PixelFormat::Bgra32)
                .unwrap_or_else(|e| panic!("pixel buffer [{i}] creation failed: {e}"));
            assert!(!buf.mapped_ptr().is_null());
            pixel_buffers.push(buf);
        }
        println!("Step 1: {} pixel buffers created (raw exportable)", pixel_buffers.len());

        // Step 2: Camera compute output image via VMA (no export, no dedicated)
        let compute_img_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        let compute_alloc_opts = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let (compute_img, compute_alloc) =
            unsafe { allocator.create_image(compute_img_info, &compute_alloc_opts) }
                .expect("compute output image creation failed");
        println!("Step 2: compute output image created (VMA, DEVICE_LOCAL)");

        // Step 3: Display camera textures via raw dedicated allocation (no export)
        // This was the allocation that failed before the fix.
        let mut camera_textures: Vec<(vk::Image, vk::DeviceMemory, vk::ImageView)> = Vec::new();
        for i in 0..4 {
            let img_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .build();

            let image = unsafe { vk_device.create_image(&img_info, None) }
                .unwrap_or_else(|e| panic!("camera image [{i}] creation failed: {e}"));

            let mem_reqs = unsafe { vk_device.get_image_memory_requirements(image) };

            // Try each compatible memory type with dedicated allocation
            let mut memory = None;
            for type_idx in 0..32u32 {
                if (mem_reqs.memory_type_bits & (1 << type_idx)) == 0 {
                    continue;
                }
                let mut dedicated = vk::MemoryDedicatedAllocateInfo::builder()
                    .image(image)
                    .build();
                let alloc_info = vk::MemoryAllocateInfo::builder()
                    .allocation_size(mem_reqs.size)
                    .memory_type_index(type_idx)
                    .push_next(&mut dedicated)
                    .build();
                if let Ok(mem) = unsafe { vk_device.allocate_memory(&alloc_info, None) } {
                    memory = Some(mem);
                    break;
                }
            }

            let memory = memory.unwrap_or_else(|| {
                unsafe { vk_device.destroy_image(image, None) };
                panic!("camera texture [{i}] memory allocation failed — all memory types rejected");
            });

            unsafe { vk_device.bind_image_memory(image, memory, 0) }
                .unwrap_or_else(|e| panic!("camera texture [{i}] bind failed: {e}"));

            let view_info = vk::ImageViewCreateInfo::builder()
                .image(image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1)
                        .build(),
                )
                .build();

            let image_view = unsafe { vk_device.create_image_view(&view_info, None) }
                .unwrap_or_else(|e| panic!("camera texture view [{i}] failed: {e}"));

            camera_textures.push((image, memory, image_view));
        }
        println!("Step 3: {} camera textures created (raw dedicated, no export)", camera_textures.len());

        // Cleanup
        unsafe {
            for (image, memory, view) in camera_textures {
                vk_device.destroy_image_view(view, None);
                vk_device.free_memory(memory, None);
                vk_device.destroy_image(image, None);
            }
            allocator.destroy_image(compute_img, compute_alloc);
        }
        drop(pixel_buffers);
        println!("All resources cleaned up successfully");
    }

    #[test]
    fn test_various_formats() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let formats = [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];

        for format in formats {
            let desc = TextureDescriptor::new(1920, 1080, format);
            let texture = HostVulkanTexture::new(&device, &desc)
                .unwrap_or_else(|e| panic!("Failed to create texture with {format:?}: {e}"));

            assert!(texture.image().is_some());
            assert_eq!(texture.format(), format);
            println!("Format {:?}: OK", format);
        }
    }

    #[test]
    fn test_device_local_texture_creation() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);
        let texture = HostVulkanTexture::new_device_local(&device, &desc)
            .expect("device-local texture creation failed");

        assert!(texture.image().is_some());
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);

        println!("Device-local texture created: {}x{}", texture.width(), texture.height());
    }

    #[test]
    fn test_lazy_image_view() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc)
            .expect("texture creation failed");

        // First call creates the image view
        let view1 = texture.image_view().expect("image_view() failed");
        // Second call returns the cached view
        let view2 = texture.image_view().expect("cached image_view() failed");
        assert_eq!(view1, view2, "image_view() should return the same cached view");

        println!("Lazy image view: created and cached successfully");
    }

    /// Round-trip test for the render-target DMA-BUF path:
    /// 1. Pull RT-capable modifiers for ARGB8888 from the device's EGL probe.
    /// 2. Skip if none — vivid CI / headless boxes have no modifiers.
    /// 3. Allocate a 1920x1080 BGRA render-target VkImage with the candidate list.
    /// 4. Assert the driver-chosen modifier is in the candidate list.
    /// 5. Export the DMA-BUF fd and read back the per-plane layout.
    /// 6. Assert plane[0].row_pitch >= 1920 * 4 (BGRA stride is at least
    ///    pixel-tight, possibly aligned up by tiling).
    #[cfg(target_os = "linux")]
    #[test]
    fn test_render_target_dma_buf_round_trip() {
        use crate::vulkan::rhi::drm_modifier_probe::fourcc;

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(e) => {
                println!("Skipping — no Vulkan device: {e}");
                return;
            }
        };
        let table = device.drm_modifier_table();
        let modifiers = table.rt_modifiers(fourcc::DRM_FORMAT_ARGB8888);
        if modifiers.is_empty() {
            println!("Skipping — EGL probe returned no RT-capable modifiers for ARGB8888");
            return;
        }
        if device.dma_buf_image_pool_tiled().is_none() {
            println!("Skipping — tiled DMA-BUF pool not created");
            return;
        }

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC,
        );
        let texture = HostVulkanTexture::new_render_target_dma_buf(&device, &desc, modifiers)
            .expect("RT DMA-BUF allocation must succeed when modifiers exist");

        assert!(texture.image().is_some());
        let chosen = texture.chosen_drm_format_modifier();
        assert!(
            modifiers.contains(&chosen),
            "driver picked modifier 0x{:016x} not in candidate list {:?}",
            chosen,
            modifiers
        );
        // Modifier 0 is DRM_FORMAT_MOD_LINEAR; the EGL probe's RT-capable
        // list is supposed to be tiled-only.
        assert_ne!(
            chosen, 0,
            "RT-capable modifier must not be DRM_FORMAT_MOD_LINEAR"
        );

        let layout = texture
            .dma_buf_plane_layout()
            .expect("plane layout must be queryable");
        assert_eq!(layout.len(), 1, "BGRA is single-plane");
        let (offset, row_pitch) = layout[0];
        assert!(
            row_pitch >= 1920 * 4,
            "row_pitch {row_pitch} must be at least pixel-tight 1920*4"
        );
        println!(
            "RT DMA-BUF: chosen modifier=0x{:016x}, plane[0]: offset={}, row_pitch={}",
            chosen, offset, row_pitch
        );

        let fd = texture
            .export_dma_buf_fd()
            .expect("DMA-BUF export must succeed");
        assert!(fd >= 0, "DMA-BUF fd must be non-negative");
    }

    /// `new_render_target_dma_buf` with an empty modifier list must fail
    /// loudly rather than silently fall back to LINEAR (which is sampler-
    /// only on NVIDIA — see docs/learnings/nvidia-egl-dmabuf-render-target.md).
    #[cfg(target_os = "linux")]
    #[test]
    fn test_render_target_dma_buf_empty_modifiers_rejected() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(e) => {
                println!("Skipping — no Vulkan device: {e}");
                return;
            }
        };
        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm)
            .with_usage(TextureUsages::RENDER_ATTACHMENT);
        let result = HostVulkanTexture::new_render_target_dma_buf(&device, &desc, &[]);
        let err = match result {
            Ok(_) => panic!("empty modifier list must reject, but allocation succeeded"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("empty modifier list") || msg.contains("EGL"),
            "error must explain the missing-EGL-modifier root cause: {msg}"
        );
    }

    #[test]
    fn test_ring_texture_lifecycle() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);

        // Create 2 ring textures (matches RING_TEXTURE_COUNT)
        let t0 = HostVulkanTexture::new(&device, &desc).expect("ring texture 0 failed");
        let t1 = HostVulkanTexture::new(&device, &desc).expect("ring texture 1 failed");

        // Both should have valid images and image views
        assert!(t0.image().is_some());
        assert!(t1.image().is_some());
        let v0 = t0.image_view().expect("ring texture 0 image_view failed");
        let v1 = t1.image_view().expect("ring texture 1 image_view failed");
        assert_ne!(v0, v1, "ring textures should have different image views");

        // Both should be DMA-BUF exportable (created via new(), not new_device_local())
        let fd0 = t0.export_dma_buf_fd().expect("ring texture 0 DMA-BUF export failed");
        let fd1 = t1.export_dma_buf_fd().expect("ring texture 1 DMA-BUF export failed");
        assert!(fd0 >= 0);
        assert!(fd1 >= 0);
        assert_ne!(fd0, fd1);

        println!("Ring texture lifecycle: 2 textures created, image views cached, DMA-BUF exported");

        drop(t0);
        drop(t1);
        println!("Ring textures dropped cleanly");
    }
}
