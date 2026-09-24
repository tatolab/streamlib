// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan texture implementation for RHI.

use std::sync::{Arc, OnceLock};

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::{Error, Result};

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
/// path or directly from the import call for the DMA-BUF path — see
/// [`HostVulkanTexture::vk_memory_binding`].
#[derive(Clone, Copy)]
struct HostVkImageMeta {
    vk_image_tiling: vk::ImageTiling,
    vk_image_usage_flags: vk::ImageUsageFlags,
}

/// DPB direction selector for [`HostVulkanTexture::new_video_dpb`].
/// Picks the `VIDEO_DECODE_DPB_BIT_KHR` vs `VIDEO_ENCODE_DPB_BIT_KHR`
/// usage flag the driver requires on a Decoded Picture Buffer image.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDpbDirection {
    Decode,
    Encode,
}

/// Inputs for [`HostVulkanTexture::new_video_dpb`].
///
/// DPB images are codec reference frames (and, for decode, the
/// decode-target images themselves when the implementation supports
/// in-place output). Each image is bound to a [`vk::VideoProfileInfoKHR`]
/// via `VkVideoProfileListInfoKHR` chained on the image's `pNext` so
/// the driver knows which codec profile the resource serves.
///
/// `array_layers` ≥ 1: pass `dpb_count` for the shared-layered-DPB
/// shape (one `VkImage` with N layers, one per slot), or `1` for the
/// separate-images-per-slot shape (N individual `VkImage`s — used
/// when the driver advertises `VK_VIDEO_CAPABILITY_SEPARATE_REFERENCE_IMAGES_BIT_KHR`).
///
/// `sharing_queue_families`: when the slice has ≥ 2 entries the image
/// is created with `VK_SHARING_MODE_CONCURRENT` and those families; an
/// empty or single-entry slice uses `VK_SHARING_MODE_EXCLUSIVE`. The
/// caller-side check `families.len() > 1` is encapsulated here.
///
/// `additional_usage`: any extra `VkImageUsageFlags` beyond the
/// direction-specific DPB flag — decoder DPBs typically also take
/// `VIDEO_DECODE_DST_KHR | TRANSFER_SRC | SAMPLED`; encoder DPBs
/// usually take nothing extra.
#[cfg(target_os = "linux")]
pub struct VideoDpbTextureDescriptor<'a> {
    pub label: &'a str,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub array_layers: u32,
    pub direction: VideoDpbDirection,
    pub additional_usage: vk::ImageUsageFlags,
    pub sharing_queue_families: &'a [u32],
    pub video_profile: &'a vk::VideoProfileInfoKHR,
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
    /// VMA allocation (always allocated with DMA-BUF export flags via HostVulkanDevice).
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
    /// Mirrors `HostVulkanBuffer::is_opaque_fd_export`.
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
        // VkExportMemoryAllocateInfo. Omitted where the handle type does not
        // exist, or vkCreateImage refuses every allocation; see
        // `CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM`.
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
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image_info = if super::CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM {
            image_info.push_next(&mut external_image_info)
        } else {
            image_info
        };

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
            result
                .map_err(|e| Error::GpuError(format!("Failed to create exportable image: {e}")))?
        };

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
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
        let (image, allocation) = unsafe { allocator.create_image(image_info, &alloc_opts) }
            .map_err(|e| Error::GpuError(format!("Failed to create device-local image: {e}")))?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
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
            return Err(Error::GpuError(
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
        let (image, allocation) = if let Some(pool) = vulkan_device.dma_buf_image_pool_tiled() {
            unsafe { pool.create_image(image_info, &alloc_opts) }
        } else {
            let allocator = vulkan_device.allocator();
            unsafe { allocator.create_image(image_info, &alloc_opts) }
        }
        .map_err(|e| {
            Error::GpuError(format!(
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
                    Error::GpuError(format!(
                        "vkGetImageDrmFormatModifierPropertiesEXT failed: {e}"
                    ))
                })?;
            props.drm_format_modifier
        };

        if !modifier_candidates.contains(&chosen) {
            unsafe { vulkan_device.allocator().destroy_image(image, allocation) };
            return Err(Error::GpuError(format!(
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
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
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

    /// Allocate an OPAQUE_FD-exportable DEVICE_LOCAL `VkImage` for
    /// Vulkan → CUDA `cudaImportExternalMemory` /
    /// `cudaExternalMemoryGetMappedMipmappedArray` interop.
    ///
    /// Engine-layer foundation for the cuda adapter's image-flavored
    /// registration path. Pairs with
    /// [`Self::export_opaque_fd_memory`] (host export side) and
    /// [`streamlib_consumer_rhi::ConsumerVulkanTexture::from_opaque_fd`]
    /// (consumer / subprocess import side).
    ///
    /// CUDA's mipmapped-array import drives every non-negotiable
    /// constraint here:
    ///
    /// - **Format** must be one of
    ///   [`TextureFormat::Rgba8Unorm`] / [`TextureFormat::Rgba16Float`]
    ///   / [`TextureFormat::Rgba32Float`]. CUDA's external-memory
    ///   mapping accepts only R8 / R8G8 / R8G8B8A8 / R16 / R16G16 /
    ///   R16G16B16A16 / R32 / R32G32 / R32G32B32A32 (unsigned, signed,
    ///   float) — closed list. Streamlib's `TextureFormat` enum
    ///   exposes only the 4-channel members of that family today; the
    ///   sRGB, BGR-channel-order, and NV12 variants are CUDA-incompatible
    ///   and rejected at construction.
    /// - **Tiling** is `VK_IMAGE_TILING_OPTIMAL`. LINEAR returns
    ///   `CUDA_ERROR_INVALID_VALUE` from
    ///   `cuExternalMemoryGetMappedMipmappedArray`. No DRM modifier
    ///   chain — OPAQUE_FD + `DRM_FORMAT_MODIFIER_EXT` is not a valid
    ///   combination on NVIDIA.
    /// - **Usage** is `TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE`.
    ///   Fixed by the constructor: TRANSFER_* covers host-side
    ///   `vkCmdCopyBufferToImage` / `vkCmdCopyImageToBuffer` for content
    ///   population and consumer-side round-trip readback;
    ///   SAMPLED + STORAGE cover the CUDA texture-object /
    ///   surface-object backings (`cudaSurfaceObject_t` writes).
    ///   `COLOR_ATTACHMENT` is intentionally absent — rendering into a
    ///   CUDA-imported image isn't a use case the engine supports;
    ///   producers blit / copy into the image instead.
    /// - **Memory** comes from
    ///   [`HostVulkanDevice::opaque_fd_image_pool`] with VMA's
    ///   `DEDICATED_MEMORY` flag set. `cudaExternalMemoryGetMappedMipmappedArray`
    ///   imports the whole `VkDeviceMemory` block, so a non-dedicated
    ///   allocation would be unimportable.
    ///
    /// Returns `Err` when the OPAQUE_FD image pool is unavailable
    /// (external memory unsupported or pool construction failed at
    /// device init) or when the format is outside the CUDA-mappable
    /// subset. Callers must NOT silently fall back to a non-exportable
    /// allocation — the resulting texture would be unusable for CUDA
    /// interop and the failure would surface only at
    /// `vkGetMemoryFdKHR` time.
    #[cfg(target_os = "linux")]
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(width = desc.width, height = desc.height, format = ?desc.format))]
    pub fn new_opaque_fd_export(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        // CUDA-mappable subset of TextureFormat. The check is at
        // construction so misuse fails fast rather than at
        // `cudaImportExternalMemory` time on the subprocess side.
        match desc.format {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba16Float | TextureFormat::Rgba32Float => {
            }
            other => {
                return Err(Error::Configuration(format!(
                    "HostVulkanTexture::new_opaque_fd_export: format {other:?} is not \
                     CUDA-mappable. Supported: Rgba8Unorm, Rgba16Float, Rgba32Float. \
                     sRGB-transfer / BGR-channel-order / NV12 variants are rejected by \
                     `cudaExternalMemoryGetMappedMipmappedArray`."
                )));
            }
        }
        if desc.width == 0 || desc.height == 0 {
            return Err(Error::Configuration(
                "HostVulkanTexture::new_opaque_fd_export: width and height must be > 0".into(),
            ));
        }

        let vk_format = texture_format_to_vk(desc.format);
        // Fixed usage set (see doc-comment): TRANSFER_* + SAMPLED + STORAGE.
        // No COLOR_ATTACHMENT.
        let usage_flags = vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::STORAGE;

        // Declare OPAQUE_FD handle type on the image create info; the
        // matching ExportMemoryAllocateInfo lives on the pool's
        // pNext chain.
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
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

        let pool = vulkan_device.opaque_fd_image_pool().ok_or_else(|| {
            Error::GpuError(
                "OPAQUE_FD image pool unavailable — external memory unsupported \
                 or pool construction failed; CUDA `VkImage` interop requires this pool"
                    .into(),
            )
        })?;
        let (image, allocation) =
            unsafe { pool.create_image(image_info, &alloc_opts) }.map_err(|e| {
                Error::GpuError(format!(
                    "Failed to create OPAQUE_FD exportable image ({}x{} {:?}): {e}",
                    desc.width, desc.height, desc.format,
                ))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            imported_memory: None,
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: true,
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

    /// Allocate a non-exportable DEVICE_LOCAL Vulkan video DPB image
    /// bound to the codec profile in `descriptor.video_profile`.
    ///
    /// Direction (decode / encode) drives the required
    /// `VkImageUsageFlags` DPB bit; `additional_usage` covers
    /// decoder-side `VIDEO_DECODE_DST | TRANSFER_SRC | SAMPLED` or
    /// any other flags the call site needs to keep on a DPB resource.
    /// Sharing mode is `CONCURRENT` over the supplied queue families
    /// when the slice carries ≥ 2 entries; `EXCLUSIVE` otherwise.
    ///
    /// VMA allocation runs with `required_flags = DEVICE_LOCAL` and
    /// **no** `DEDICATED_MEMORY` — codec DPBs share the default pool
    /// budget. The call is wrapped in
    /// [`HostVulkanDevice::lock_device`] so concurrent processor
    /// submissions can't race the create + bind on NVIDIA Linux.
    ///
    /// `width` × `height` × `array_layers` must all be > 0; the
    /// caller is responsible for pre-aligning the extent against
    /// `VkVideoCapabilitiesKHR::pictureAccessGranularity`.
    #[cfg(target_os = "linux")]
    #[tracing::instrument(level = "trace", skip(vulkan_device, descriptor), fields(label = descriptor.label, width = descriptor.width, height = descriptor.height, layers = descriptor.array_layers, dir = ?descriptor.direction))]
    pub fn new_video_dpb(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &VideoDpbTextureDescriptor<'_>,
    ) -> Result<Self> {
        if descriptor.width == 0 || descriptor.height == 0 || descriptor.array_layers == 0 {
            return Err(Error::Configuration(format!(
                "HostVulkanTexture::new_video_dpb ({}): width, height, array_layers must be > 0; \
                 got {}x{}x{}",
                descriptor.label, descriptor.width, descriptor.height, descriptor.array_layers,
            )));
        }

        let vk_format = texture_format_to_vk(descriptor.format);
        let dpb_flag = match descriptor.direction {
            VideoDpbDirection::Decode => vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
            VideoDpbDirection::Encode => vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
        };
        let usage_flags = dpb_flag | descriptor.additional_usage;

        let mut profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(descriptor.video_profile));

        let use_concurrent = descriptor.sharing_queue_families.len() > 1;
        let mut image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: descriptor.width,
                height: descriptor.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(descriptor.array_layers)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut profile_list);

        if use_concurrent {
            image_info = image_info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(descriptor.sharing_queue_families);
        } else {
            image_info = image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        let alloc_opts = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();

        // Acquire the device-level resource lock so concurrent processor
        // submissions can't race the create + bind on NVIDIA Linux.
        let _device_lock = vulkan_device.lock_device();

        let (image, allocation) = unsafe { allocator.create_image(image_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "HostVulkanTexture::new_video_dpb ({}): vmaCreateImage failed: {e}",
                    descriptor.label,
                ))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            imported_memory: None,
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
            chosen_drm_format_modifier: 0,
            width: descriptor.width,
            height: descriptor.height,
            format: descriptor.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
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

    /// Tiling this image was created with — what `vkGetImageSubresourceLayout`
    /// binds its aspect mask to, and the signal that says whether a
    /// driver-chosen DRM format modifier applies to this image at all.
    pub fn vk_image_tiling(&self) -> vk::ImageTiling {
        self.vk_image_meta.vk_image_tiling
    }

    /// Whether the image was created with TRANSFER_SRC usage — the
    /// capability a copy that reads it (`vkCmdCopyImageToBuffer`)
    /// requires; recording one without it is a Vulkan spec violation
    /// (VUID-vkCmdCopyImageToBuffer-srcImage-01998), not an error the
    /// driver reports.
    pub fn supports_transfer_read(&self) -> bool {
        self.vk_image_meta
            .vk_image_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC)
    }

    /// Whether the image was created with TRANSFER_DST usage — the
    /// capability a copy that writes it (`vkCmdCopyBufferToImage`)
    /// requires (VUID-vkCmdCopyBufferToImage-dstImage-01997).
    pub fn supports_transfer_write(&self) -> bool {
        self.vk_image_meta
            .vk_image_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_DST)
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
            Error::GpuError("Cannot create image view: no HostVulkanDevice stored".into())
        })?;
        let image = self
            .image
            .ok_or_else(|| Error::GpuError("Cannot create image view: no image".into()))?;

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
            .map_err(|e| Error::GpuError(format!("Failed to create image view: {e}")))?;

        let _ = self.cached_image_view.set(view);
        Ok(*self.cached_image_view.get().unwrap())
    }

    /// One-shot UNDEFINED → GENERAL barrier on a freshly-allocated
    /// `vk::Image`. Used by [`crate::core::context::GpuContext::transition_storage_image_to_general`]
    /// to give example / processor code a way to bring a storage-image
    /// texture into the layout compute / RT kernels expect — without
    /// pulling vulkanalia into the consumer.
    ///
    /// Synchronous: submits to the graphics queue and waits on a fence
    /// before returning. The image must not have content the caller
    /// cares about; UNDEFINED-source transitions allow the driver to
    /// discard contents.
    #[cfg(target_os = "linux")]
    pub fn transition_to_general(
        vulkan_device: &Arc<HostVulkanDevice>,
        image: vk::Image,
    ) -> Result<()> {
        let device = vulkan_device.device();
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(vulkan_device.queue_family_index())
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: create_command_pool: {e}"))
        })?;
        let cb_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = match unsafe { device.allocate_command_buffers(&cb_info) } {
            Ok(c) => c[0],
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(Error::GpuError(format!(
                    "transition_to_general: allocate_command_buffers: {e}"
                )));
            }
        };
        let begin = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { device.begin_command_buffer(cmd, &begin) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: begin_command_buffer: {e}"))
        })?;
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::SHADER_READ | vk::AccessFlags2::SHADER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build();
        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &dep) };
        unsafe { device.end_command_buffer(cmd) }.map_err(|e| {
            Error::GpuError(format!("transition_to_general: end_command_buffer: {e}"))
        })?;

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build();
        let cmd_infos = [cmd_info];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        let fence_info = vk::FenceCreateInfo::default();
        let fence = unsafe { device.create_fence(&fence_info, None) }
            .map_err(|e| Error::GpuError(format!("transition_to_general: create_fence: {e}")))?;
        let submits = [submit];
        let submit_result = unsafe {
            HostVulkanDevice::submit_to_queue(vulkan_device, vulkan_device.queue(), &submits, fence)
        };
        if let Err(e) = submit_result {
            unsafe {
                device.destroy_fence(fence, None);
                device.destroy_command_pool(pool, None);
            }
            return Err(e);
        }
        let wait_result = unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("transition_to_general: wait_for_fences: {e}")));
        unsafe {
            device.destroy_fence(fence, None);
            device.destroy_command_pool(pool, None);
        }
        wait_result
    }
}

/// Aspect mask `vkGetImageSubresourceLayout` requires to report memory
/// plane `plane_index` of a DMA-BUF-exportable image.
///
/// `VUID-vkGetImageSubresourceLayout-tiling-09433` binds the aspect to the
/// tiling the image was *created* with, never to the modifier the driver
/// picked: `DRM_FORMAT_MOD_LINEAR` is numerically zero, so a modifier-tiled
/// image is indistinguishable from an image created without a modifier by
/// the modifier's value alone.
#[cfg(target_os = "linux")]
fn dma_buf_plane_layout_query_aspect_mask(
    vk_image_tiling: vk::ImageTiling,
    format: TextureFormat,
    plane_index: usize,
) -> Result<vk::ImageAspectFlags> {
    const MEMORY_PLANE_ASPECTS: [vk::ImageAspectFlags; 4] = [
        vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
    ];
    const FORMAT_PLANE_ASPECTS: [vk::ImageAspectFlags; 2] =
        [vk::ImageAspectFlags::PLANE_0, vk::ImageAspectFlags::PLANE_1];
    const COLOR_ASPECTS: [vk::ImageAspectFlags; 1] = [vk::ImageAspectFlags::COLOR];

    let (aspects, image_shape) = if vk_image_tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
        (&MEMORY_PLANE_ASPECTS[..], "modifier-tiled")
    } else if vk_image_tiling == vk::ImageTiling::LINEAR {
        if matches!(format, TextureFormat::Nv12) {
            (&FORMAT_PLANE_ASPECTS[..], "linear-tiled NV12")
        } else {
            (&COLOR_ASPECTS[..], "linear-tiled single-plane")
        }
    } else {
        return Err(Error::GpuError(format!(
            "dma_buf_plane_layout: {vk_image_tiling:?} images have driver-opaque layouts — \
             only LINEAR and DRM_FORMAT_MODIFIER_EXT expose queryable plane layouts"
        )));
    };

    aspects.get(plane_index).copied().ok_or_else(|| {
        Error::GpuError(format!(
            "dma_buf_plane_layout: plane index {plane_index} out of range — this RHI names \
             {} plane aspect(s) for a {image_shape} image",
            aspects.len()
        ))
    })
}

#[cfg(target_os = "linux")]
impl HostVulkanTexture {
    /// DRM format modifier the driver picked at allocation time.
    ///
    /// Zero for textures created via [`Self::new`] / [`Self::new_device_local`]
    /// or imported via [`Self::from_dma_buf_fd`] — those paths do not go
    /// through `VK_EXT_image_drm_format_modifier`. Render-target textures
    /// allocated via [`Self::new_render_target_dma_buf`] return the modifier
    /// the driver chose from the candidate list, which is zero when that
    /// choice was `DRM_FORMAT_MOD_LINEAR`. Read [`Self::vk_image_tiling`] to
    /// tell those two zeros apart.
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
        let vk_dev = self
            .vulkan_device
            .as_ref()
            .ok_or_else(|| Error::GpuError("dma_buf_plane_layout: no HostVulkanDevice".into()))?;
        let image = self
            .image
            .ok_or_else(|| Error::GpuError("dma_buf_plane_layout: no image".into()))?;

        // `vkGetImageSubresourceLayout` (the call below) requires LINEAR
        // or DRM_FORMAT_MODIFIER tiling per VUID-vkGetImageSubresourceLayout-image-07790.
        // OPTIMAL-tiled images have driver-opaque layouts; their plane
        // strides aren't meaningful for DMA-BUF export (those textures
        // aren't DMA-BUF importable anyway). Refuse the query so the
        // caller (`surface_store::register_texture`) takes the fallback
        // `vec![(0, 0)]` path it already tolerates.
        if self.vk_image_meta.vk_image_tiling == vk::ImageTiling::OPTIMAL {
            return Err(Error::GpuError(
                "dma_buf_plane_layout: not applicable for VK_IMAGE_TILING_OPTIMAL textures \
                 (only LINEAR or DRM_FORMAT_MODIFIER tilings expose queryable plane layouts)"
                    .into(),
            ));
        }

        let plane_count = match self.format {
            TextureFormat::Nv12 => 2,
            _ => 1,
        };

        let mut planes = Vec::with_capacity(plane_count);
        for plane_idx in 0..plane_count {
            let aspect_mask = dma_buf_plane_layout_query_aspect_mask(
                self.vk_image_meta.vk_image_tiling,
                self.format,
                plane_idx,
            )?;

            let subres = vk::ImageSubresource::builder()
                .aspect_mask(aspect_mask)
                .mip_level(0)
                .array_layer(0)
                .build();
            let layout = unsafe { vk_dev.device().get_image_subresource_layout(image, &subres) };
            planes.push((layout.offset, layout.row_pitch));
        }

        Ok(planes)
    }

    /// True iff this texture was allocated via
    /// [`Self::new_opaque_fd_export`]. Gates the OPAQUE_FD export
    /// accessor; the buffer-side mirror is
    /// `HostVulkanBuffer::is_opaque_fd_export`.
    pub fn is_opaque_fd_export(&self) -> bool {
        self.is_opaque_fd_export
    }

    /// Allocation size in bytes from VMA, used to thread the
    /// `allocation_size` argument across to
    /// [`streamlib_consumer_rhi::ConsumerVulkanTexture::from_opaque_fd`]
    /// on the consumer side. Returns 0 for placeholders / imported
    /// images (where the allocation lives on the foreign side).
    pub fn vma_allocation_size(&self) -> vk::DeviceSize {
        let Some(allocation) = self.allocation.as_ref() else {
            return 0;
        };
        let Some(vk_dev) = self.vulkan_device.as_ref() else {
            return 0;
        };
        vk_dev.allocator().get_allocation_info(*allocation).size as vk::DeviceSize
    }

    /// Memory type index (`vmaGetAllocationInfo().memoryType`) of this
    /// texture's allocation — what a conforming consumer-side
    /// `vkAllocateMemory(VkImportMemoryFdInfoKHR)` must state. `None` for
    /// placeholders / imported images (where the allocation lives on the
    /// foreign side); never defaulted, because every index including `0`
    /// is a real value.
    pub fn vma_allocation_memory_type_index(&self) -> Option<u32> {
        let allocation = self.allocation.as_ref()?;
        let vk_dev = self.vulkan_device.as_ref()?;
        Some(
            vk_dev
                .allocator()
                .get_allocation_info(*allocation)
                .memoryType,
        )
    }

    /// The owning device's `VkPhysicalDeviceIDProperties::deviceUUID` —
    /// the device-binding contract for an OPAQUE_FD export (importing on
    /// the wrong GPU of a multi-GPU rig corrupts silently). Sourced from
    /// this texture's own `HostVulkanDevice`, never the current
    /// `GpuContext`'s, per the buffer-export precedent. `None` for
    /// placeholders / imported images with no stored device.
    pub fn exporting_physical_device_uuid(&self) -> Option<[u8; 16]> {
        Some(self.vulkan_device.as_ref()?.physical_device_uuid())
    }

    /// Export the texture's OPAQUE_FD memory as a file descriptor.
    ///
    /// Only valid for textures created via [`Self::new_opaque_fd_export`];
    /// returns `Err` for DMA-BUF-flavored allocations (call
    /// [`Self::export_dma_buf_fd`] instead). Each call returns a fresh
    /// kernel fd (the driver dups internally) — the caller owns it and
    /// is responsible for closing it (or for transferring ownership via
    /// SCM_RIGHTS / `cudaImportExternalMemory`, both of which `dup`
    /// again on receipt).
    ///
    /// Mirrors `HostVulkanBuffer::export_opaque_fd_memory` for images.
    pub fn export_opaque_fd_memory(&self) -> Result<std::os::unix::io::RawFd> {
        if !self.is_opaque_fd_export {
            return Err(Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: texture was not created \
                 with `new_opaque_fd_export`; the underlying memory carries DMA_BUF_EXT \
                 (or no) export flags and OPAQUE_FD export will fail at the driver"
                    .into(),
            ));
        }
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: no HostVulkanDevice stored".into(),
            )
        })?;
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: texture has no VMA allocation".into(),
            )
        })?;
        let alloc_info = vk_dev.allocator().get_allocation_info(*allocation);
        let memory = alloc_info.deviceMemory;

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { vk_dev.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export OPAQUE_FD memory fd: {e}")))?;
        Ok(fd)
    }

    /// Export the texture's memory as a DMA-BUF file descriptor.
    ///
    /// Each call returns a fresh kernel fd (the driver dups internally) —
    /// the caller owns it and is responsible for closing it (or for
    /// transferring ownership via SCM_RIGHTS / an external-memory import,
    /// both of which `dup` again on receipt). The texture keeps no copy and
    /// will never close it.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            Error::GpuError("Cannot export DMA-BUF: no HostVulkanDevice stored".into())
        })?;

        // Get DeviceMemory from raw allocation (export/import path) or VMA allocation
        let device_memory = if let Some(memory) = self.imported_memory {
            memory
        } else if let Some(allocation) = self.allocation.as_ref() {
            let alloc_info = vk_dev.allocator().get_allocation_info(*allocation);
            alloc_info.deviceMemory
        } else {
            return Err(Error::GpuError(
                "Cannot export DMA-BUF from texture without memory".into(),
            ));
        };

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        unsafe { vk_dev.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export DMA-BUF fd: {e}")))
    }

    /// Subprocess-side import of a render-target DMA-BUF image.
    ///
    /// The host allocated the image via [`Self::new_render_target_dma_buf`]
    /// with a tiled DRM modifier (LINEAR is sampler-only on NVIDIA). The
    /// subprocess receives:
    /// - `plane_fds` — DMA-BUF file descriptors, one per plane.
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
    /// Consumes every plane fd: plane 0 is handed to the driver at
    /// `vkAllocateMemory`, whatever that call returns (see
    /// [`HostVulkanDevice::import_dma_buf_memory`]), and the rest close
    /// here — multi-plane binding is not wired yet.
    pub fn import_render_target_dma_buf(
        vulkan_device: &Arc<HostVulkanDevice>,
        plane_fds: Vec<std::os::fd::OwnedFd>,
        plane_offsets: &[u64],
        plane_strides: &[u64],
        drm_format_modifier: u64,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        if plane_fds.is_empty() {
            return Err(Error::GpuError(
                "import_render_target_dma_buf: empty fd vec".into(),
            ));
        }
        if plane_offsets.len() != plane_fds.len() || plane_strides.len() != plane_fds.len() {
            return Err(Error::GpuError(format!(
                "import_render_target_dma_buf: plane arrays length mismatch — fds={} offsets={} strides={}",
                plane_fds.len(),
                plane_offsets.len(),
                plane_strides.len()
            )));
        }
        if drm_format_modifier == 0 {
            return Err(Error::GpuError(
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
        // `streamlib::sdk::context::GpuContext::acquire_render_target_dma_buf_image`.
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

        let mut explicit_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::builder()
            .drm_format_modifier(drm_format_modifier)
            .plane_layouts(&plane_layouts);

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

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
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut explicit_modifier_info)
            .push_next(&mut external_image_info);

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            Error::GpuError(format!(
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
        let mut plane_fds = plane_fds.into_iter();
        let plane_0_fd = plane_fds.next().expect("checked non-empty above");
        drop(plane_fds);
        let memory = vulkan_device
            .import_dma_buf_memory(
                plane_0_fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_image(image, None) };
            Error::GpuError(format!(
                "import_render_target_dma_buf: bind_image_memory failed: {e}"
            ))
        })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
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
    ///
    /// Takes the fd by value because the caller holds nothing after this
    /// returns: the fd is handed to the driver at `vkAllocateMemory` — the
    /// driver's from then on, whatever that call returned (see
    /// [`HostVulkanDevice::import_dma_buf_memory`]) — or closed here on an
    /// exit before that point.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<HostVulkanDevice>,
        dma_buf_fd: std::os::fd::OwnedFd,
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
                Error::GpuError(format!("Failed to create image for DMA-BUF import: {e}"))
            })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

        // VMA cannot import external memory — use raw import path in the RHI
        let memory = vulkan_device
            .import_dma_buf_memory(
                dma_buf_fd,
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
                Error::GpuError(format!("Failed to bind imported memory: {e}"))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
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

#[cfg(target_os = "macos")]
impl HostVulkanTexture {
    /// An image whose storage is a fresh private IOSurface, so it can cross to
    /// a helper process as a Mach port.
    ///
    /// MoltenVK binds the surface when the image is created — there is no
    /// attach after the fact — and checks only its width, height and
    /// bytes-per-element against the format's block size. The image stays
    /// `OPTIMAL`; its rows are the surface's, at the surface's stride. The
    /// binding takes a device-local memory type that is not host-visible:
    /// MoltenVK backs a host-visible binding with a private `MTLBuffer` of its
    /// own for every image.
    pub fn new_iosurface_backed(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::new_iosurface_backed";
        if desc.format.plane_count() != 1 {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: {:?} is planar; an IOSurface-backed image carries one plane",
                desc.format
            )));
        }
        let iosurface = crate::apple::iosurface::create_private_iosurface_for_a_gpu_image(
            desc.width,
            desc.height,
            desc.format.bytes_per_pixel(),
        )?;
        Self::created_over_iosurface(
            vulkan_device,
            desc.format,
            texture_usages_to_vk(desc.usage),
            iosurface,
        )
    }

    /// An image over an IOSurface another process allocated — a registered
    /// texture's surface, resolved from its Mach port. Refused when the
    /// surface's element size is not `format`'s, or `usage` carries a bit no
    /// `VkImageUsageFlagBits` names.
    pub fn from_iosurface(
        vulkan_device: &Arc<HostVulkanDevice>,
        iosurface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
        format: TextureFormat,
        usage: streamlib_consumer_rhi::VulkanImageUsage,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::from_iosurface";
        if let Some(refusal) =
            streamlib_consumer_rhi::refusal_of_an_iosurface_for_an_image_of_format(
                &iosurface, format,
            )
        {
            return Err(Error::NotSupported(format!("{OPERATION}: {refusal}")));
        }
        let usage_flags = usage
            .as_vk_or_refusal()
            .map_err(|refusal| Error::NotSupported(format!("{OPERATION}: {refusal}")))?;
        Self::created_over_iosurface(vulkan_device, format, usage_flags, iosurface)
    }

    /// The image over `iosurface`, taking its extent from the surface; see
    /// [`streamlib_consumer_rhi::create_image_over_iosurface`] for the
    /// contract.
    fn created_over_iosurface(
        vulkan_device: &Arc<HostVulkanDevice>,
        format: TextureFormat,
        usage_flags: vk::ImageUsageFlags,
        iosurface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::created_over_iosurface";
        if !vulkan_device.supports_metal_objects_interop() {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: VK_EXT_metal_objects is not enabled on this device, so an image \
                 cannot be created over an IOSurface"
            )));
        }
        let (width, height) = (iosurface.width() as u32, iosurface.height() as u32);
        let device = vulkan_device.device();
        // SAFETY: the extension is enabled (checked above) and `iosurface` is
        // retained below for the image's whole life.
        let image = unsafe {
            streamlib_consumer_rhi::create_image_over_iosurface(
                device,
                &iosurface,
                width,
                height,
                texture_format_to_vk(format),
                usage_flags,
            )
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "{OPERATION}: the driver refused a {width}x{height} {format:?} image over an \
                 IOSurface: {e}"
            ))
        })?;
        // SAFETY: `image` was just created on this device.
        let memory_requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory = vulkan_device
            .allocate_device_local_memory_for_an_iosurface_backed_image(
                memory_requirements.size,
                memory_requirements.memory_type_bits,
            )
            // SAFETY: nothing else holds the image yet.
            .inspect_err(|_| unsafe { device.destroy_image(image, None) })?;
        // SAFETY: `memory` was allocated for this image's requirements.
        if let Err(e) = unsafe { device.bind_image_memory(image, memory, 0) } {
            // SAFETY: nothing else holds either handle yet.
            unsafe { device.destroy_image(image, None) };
            vulkan_device.free_imported_memory(memory);
            return Err(Error::GpuError(format!(
                "{OPERATION}: binding the IOSurface-backed image's memory failed: {e}"
            )));
        }

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: memory_requirements.size,
            cached_image_view: OnceLock::new(),
            backing_iosurface: Some(
                crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
            ),
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// The IOSurface this image's storage is, when it was allocated to cross
    /// to a helper process.
    pub fn backing_iosurface(
        &self,
    ) -> Option<&crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads> {
        self.backing_iosurface.as_ref()
    }

    /// A fresh send right naming this image's IOSurface, for the surface-share
    /// wire. Refused for an image that is not IOSurface-backed.
    pub fn export_iosurface_mach_send_right(
        &self,
    ) -> Result<streamlib_surface_client::OwnedMachSendRight> {
        let iosurface = self.backing_iosurface().ok_or_else(|| {
            Error::NotSupported(
                "export_iosurface_mach_send_right: this image's storage is not an IOSurface".into(),
            )
        })?;
        crate::apple::iosurface::create_iosurface_mach_send_right(iosurface)
    }
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
        if let Some(&view) = self.cached_image_view.get() {
            if let Some(vk_dev) = &self.vulkan_device {
                unsafe { vk_dev.device().destroy_image_view(view, None) };
            }
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
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    /// The bytes an IOSurface-backed image's surface holds, row by row at
    /// the surface's own stride, trimmed to the image's width.
    #[cfg(target_os = "macos")]
    fn packed_rows_of_the_backing_iosurface(texture: &HostVulkanTexture) -> Vec<u8> {
        use objc2_io_surface::IOSurfaceLockOptions;

        let iosurface = texture
            .backing_iosurface()
            .expect("an IOSurface-backed image keeps its surface");
        let row_byte_len = (texture.width() * texture.format().bytes_per_pixel()) as usize;
        let locked =
            unsafe { iosurface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        assert_eq!(locked, 0, "IOSurfaceLock");
        let base = iosurface.base_address().as_ptr().cast::<u8>();
        let mut packed_rows = Vec::with_capacity(row_byte_len * texture.height() as usize);
        for row in 0..texture.height() as usize {
            let row_bytes = unsafe {
                std::slice::from_raw_parts(base.add(row * iosurface.bytes_per_row()), row_byte_len)
            };
            packed_rows.extend_from_slice(row_bytes);
        }
        unsafe { iosurface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        packed_rows
    }

    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn an_upload_into_an_iosurface_backed_image_lands_in_the_surface_at_its_stride() {
        let Ok(device) = HostVulkanDevice::new() else {
            println!("Skipping - no Vulkan device available");
            return;
        };
        // An odd width, so a surface that pads its rows reads wrong at the
        // image's packed stride.
        let (width, height) = (37, 5);
        let texture = HostVulkanTexture::new_iosurface_backed(
            &device,
            &TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm).with_usage(
                TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST
                    | TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING,
            ),
        )
        .expect("an IOSurface-backed image");
        assert_eq!(texture.vk_image_tiling(), vk::ImageTiling::OPTIMAL);
        assert_ne!(texture.vk_memory_binding().0, vk::DeviceMemory::null());

        let pattern: Vec<u8> = (0..(width * height * 4) as usize)
            .map(|index| (index.wrapping_mul(31).wrapping_add(7)) as u8)
            .collect();
        assert_ne!(
            packed_rows_of_the_backing_iosurface(&texture),
            pattern,
            "the surface already held the pattern before the engine wrote it"
        );

        let staging = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
            &device,
            pattern.len() as u64,
        )
        .expect("a staging buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), staging.mapped_ptr(), pattern.len());
            let _final_texture_layout = device
                .upload_buffer_to_image(staging.buffer(), &texture, width, height)
                .expect("the upload");
        }
        assert_eq!(packed_rows_of_the_backing_iosurface(&texture), pattern);
    }

    /// MoltenVK checks an IOSurface's element size against the format's block
    /// size, so the 8- and 16-byte float formats take a surface of their own
    /// element size — and an upload lands in it as it does for 4 bytes.
    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn every_single_plane_format_takes_an_iosurface_backed_image() {
        let Ok(device) = HostVulkanDevice::new() else {
            println!("Skipping - no Vulkan device available");
            return;
        };
        let (width, height) = (19, 3);
        for format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
        ] {
            let texture = HostVulkanTexture::new_iosurface_backed(
                &device,
                &TextureDescriptor::new(width, height, format).with_usage(
                    TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST
                        | TextureUsages::TEXTURE_BINDING
                        | TextureUsages::STORAGE_BINDING,
                ),
            )
            .unwrap_or_else(|refusal| panic!("{format:?} over an IOSurface: {refusal}"));
            let pattern: Vec<u8> = (0..(width * height * format.bytes_per_pixel()) as usize)
                .map(|index| (index.wrapping_mul(13).wrapping_add(5)) as u8)
                .collect();
            let staging = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
                &device,
                pattern.len() as u64,
            )
            .expect("a staging buffer");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pattern.as_ptr(),
                    staging.mapped_ptr(),
                    pattern.len(),
                );
                let _final_texture_layout = device
                    .upload_buffer_to_image(staging.buffer(), &texture, width, height)
                    .expect("the upload");
            }
            assert_eq!(
                packed_rows_of_the_backing_iosurface(&texture),
                pattern,
                "{format:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_planar_format_is_refused_an_iosurface_backed_image_by_name() {
        let Ok(device) = HostVulkanDevice::new() else {
            println!("Skipping - no Vulkan device available");
            return;
        };
        let refused = HostVulkanTexture::new_iosurface_backed(
            &device,
            &TextureDescriptor::new(64, 64, TextureFormat::Nv12),
        )
        .err()
        .expect("NV12 has two planes");
        assert!(refused.to_string().contains("planar"), "{refused}");
    }

    #[cfg(target_os = "linux")]
    fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
        let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(unsafe { file_status.assume_init() }.st_ino as u64)
    }

    /// The device-side half of the lookup's fd contract. A 4 KB buffer's
    /// DMA-BUF cannot back a 16 MB image allocation, so the driver refuses
    /// at `vkAllocateMemory` — and closes the fd as it does (NVIDIA), which
    /// is why the importer must not close it again: in a debug build a
    /// second close of an `OwnedFd` is an IO-safety abort of this very
    /// test binary. Nothing may be left open, and nothing closed twice.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_texture_import_the_driver_refuses_leaves_no_fd_behind_and_closes_none_twice() {
        use std::os::fd::{FromRawFd as _, OwnedFd};

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let source =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
                .expect("source buffer allocation failed");
        let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");
        let inode = inode_of(fd).expect("an exported DMA-BUF must stat");

        let live_imports_before = device.live_import_allocation_count();
        let result = HostVulkanTexture::from_dma_buf_fd(
            &device,
            unsafe { OwnedFd::from_raw_fd(fd) },
            64,
            64,
            TextureFormat::Rgba8Unorm,
            16 * 1024 * 1024,
        );
        assert!(
            result.is_err(),
            "a 4 KB DMA-BUF cannot back a 16 MB image; the driver accepted it and the \
             close-on-failure contract went unexercised"
        );
        drop(result);
        assert_eq!(
            device.live_import_allocation_count(),
            live_imports_before,
            "a refused import must not leave VkDeviceMemory live"
        );
        assert_ne!(
            inode_of(fd),
            Some(inode),
            "the refused import left the DMA-BUF fd open — no owner remains to close it"
        );
    }
    #[cfg(target_os = "linux")]
    use crate::vulkan::rhi::video_profile_test_fixture::{
        VideoProfileWithOwnedCodecExtensionChain, device_supports_h264_for_dpb_direction,
    };

    /// `DRM_FORMAT_MOD_LINEAR` is zero, so a modifier-tiled image is not
    /// recognisable by its modifier's value. Keying the aspect choice on
    /// that value routes a linear-modifier image down the COLOR branch and
    /// trips `VUID-vkGetImageSubresourceLayout-tiling-09433` (#1915).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_linear_modifier_image_is_queried_through_the_memory_plane_aspect() {
        assert_eq!(
            dma_buf_plane_layout_query_aspect_mask(
                vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                TextureFormat::Bgra8Unorm,
                0,
            )
            .expect("plane 0 of a single-plane modifier-tiled image"),
            vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_modifier_tiled_nv12_image_uses_the_memory_plane_aspect_not_the_format_plane_aspect() {
        assert_eq!(
            dma_buf_plane_layout_query_aspect_mask(
                vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                TextureFormat::Nv12,
                1,
            )
            .expect("plane 1 of a modifier-tiled NV12 image"),
            vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
        );
    }

    /// The counterpart the modifier's value cannot express: an image
    /// created without `VK_EXT_image_drm_format_modifier` also reports a
    /// zero modifier, and must keep the COLOR aspect.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_linear_tiled_image_created_without_a_modifier_keeps_the_color_aspect() {
        assert_eq!(
            dma_buf_plane_layout_query_aspect_mask(
                vk::ImageTiling::LINEAR,
                TextureFormat::Bgra8Unorm,
                0,
            )
            .expect("plane 0 of a linear-tiled single-plane image"),
            vk::ImageAspectFlags::COLOR,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_linear_tiled_nv12_image_keeps_the_format_plane_aspects() {
        let aspects: Vec<vk::ImageAspectFlags> = (0..2)
            .map(|plane_index| {
                dma_buf_plane_layout_query_aspect_mask(
                    vk::ImageTiling::LINEAR,
                    TextureFormat::Nv12,
                    plane_index,
                )
                .expect("both planes of a linear-tiled NV12 image")
            })
            .collect();
        assert_eq!(
            aspects,
            vec![vk::ImageAspectFlags::PLANE_0, vk::ImageAspectFlags::PLANE_1],
        );
    }

    /// OPTIMAL layouts are driver-opaque. The caller refuses them before it
    /// reaches this helper, but the helper is independently reachable, so it
    /// must not answer COLOR for a tiling it cannot describe.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_optimal_tiled_image_has_no_queryable_plane_aspect() {
        assert!(
            dma_buf_plane_layout_query_aspect_mask(
                vk::ImageTiling::OPTIMAL,
                TextureFormat::Bgra8Unorm,
                0,
            )
            .is_err(),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_plane_index_past_what_the_image_exposes_is_refused() {
        for (tiling, format, plane_index) in [
            (
                vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                TextureFormat::Bgra8Unorm,
                4,
            ),
            (vk::ImageTiling::LINEAR, TextureFormat::Nv12, 2),
            (vk::ImageTiling::LINEAR, TextureFormat::Bgra8Unorm, 1),
        ] {
            assert!(
                dma_buf_plane_layout_query_aspect_mask(tiling, format, plane_index).is_err(),
                "plane {plane_index} of a {tiling:?} {format:?} image must be refused, \
                 not silently answered with another plane's aspect"
            );
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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
        unsafe { libc::close(fd) };
    }

    /// A DMA-BUF-exportable texture on the rig's device, or `None` when
    /// there is no Vulkan device to build one on.
    #[cfg(target_os = "linux")]
    fn dma_buf_exportable_texture_or_skip() -> Option<HostVulkanTexture> {
        let device = match HostVulkanDevice::new() {
            Ok(device) => device,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return None;
            }
        };
        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm);
        Some(HostVulkanTexture::new(&device, &desc).expect("texture creation failed"))
    }

    /// Every DMA-BUF export mints its own fd and surrenders it — the rule
    /// the OPAQUE_FD exporters already follow. A memoized fd hands two
    /// owners the same descriptor: the caller that closes after its
    /// hand-off, and the texture that closes again at drop (#1880).
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn every_dma_buf_export_mints_a_fresh_fd_the_caller_owns() {
        let Some(texture) = dma_buf_exportable_texture_or_skip() else {
            return;
        };

        let first_export = texture
            .export_dma_buf_fd()
            .expect("first DMA-BUF export failed");
        let second_export = texture
            .export_dma_buf_fd()
            .expect("second DMA-BUF export failed");
        assert_ne!(
            first_export, second_export,
            "each export must mint its own fd; the same number twice is one \
             descriptor with two owners"
        );

        drop(texture);

        for exported_fd in [first_export, second_export] {
            let survived = unsafe { libc::fcntl(exported_fd, libc::F_GETFD) } != -1;
            if survived {
                unsafe { libc::close(exported_fd) };
            }
            assert!(
                survived,
                "fd {exported_fd} was closed by the texture that exported it; an \
                 exporter surrenders every fd it mints"
            );
        }
    }

    /// The #1880 teardown corruption, reproduced without the wire: the
    /// registration path closes the fd it was handed, an unrelated owner
    /// lands on that descriptor number, and the texture's own drop closes
    /// it a second time — out from under whoever holds it now.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn dropping_a_texture_never_closes_an_fd_its_export_surrendered() {
        let Some(texture) = dma_buf_exportable_texture_or_skip() else {
            return;
        };

        let exported_fd = texture.export_dma_buf_fd().expect("DMA-BUF export failed");

        // Opened while `exported_fd` is still held, so the kernel's
        // lowest-free rule cannot hand back the same number.
        let unrelated_owner_fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(
            unrelated_owner_fd >= 0,
            "could not open the unrelated owner"
        );
        assert_ne!(
            unrelated_owner_fd, exported_fd,
            "the unrelated owner must be a descriptor of its own"
        );

        // One `dup2` is both halves of the hand-off: it closes `exported_fd`
        // exactly as the receiver of an SCM_RIGHTS send does, and seats the
        // unrelated owner on that number atomically — no window in which
        // another thread could claim it.
        assert_eq!(
            unsafe { libc::dup2(unrelated_owner_fd, exported_fd) },
            exported_fd,
            "could not seat an unrelated owner on the surrendered number"
        );

        drop(texture);

        let survived = unsafe { libc::fcntl(exported_fd, libc::F_GETFD) } != -1;
        unsafe {
            if survived {
                libc::close(exported_fd);
            }
            libc::close(unrelated_owner_fd);
        }
        assert!(
            survived,
            "the dropped texture closed fd {exported_fd}, which by then belonged to \
             an unrelated owner — the double-close that corrupts bystander \
             subsystems at teardown"
        );
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
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

        // Step 1: Camera pixel buffers via HostVulkanBuffer (raw exportable allocation)
        use crate::vulkan::rhi::HostVulkanBuffer;
        let mut pixel_buffers = Vec::new();
        for i in 0..4 {
            let buf = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (4 as u64))
                .unwrap_or_else(|e| panic!("pixel buffer [{i}] creation failed: {e}"));
            assert!(!buf.mapped_ptr().is_null());
            pixel_buffers.push(buf);
        }
        println!(
            "Step 1: {} pixel buffers created (raw exportable)",
            pixel_buffers.len()
        );

        // Step 2: Camera compute output image via VMA (no export, no dedicated)
        let compute_img_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
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
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
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
        println!(
            "Step 3: {} camera textures created (raw dedicated, no export)",
            camera_textures.len()
        );

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

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

        println!(
            "Device-local texture created: {}x{}",
            texture.width(),
            texture.height()
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

        // First call creates the image view
        let view1 = texture.image_view().expect("image_view() failed");
        // Second call returns the cached view
        let view2 = texture.image_view().expect("cached image_view() failed");
        assert_eq!(
            view1, view2,
            "image_view() should return the same cached view"
        );

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
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    /// The rig half of the aspect-mask contract: allocating against
    /// `DRM_FORMAT_MOD_LINEAR` yields a modifier-tiled image whose modifier
    /// reads zero, and querying its plane layout must raise no validation
    /// finding. Keying the aspect on the modifier's value instead of the
    /// tiling trips `VUID-vkGetImageSubresourceLayout-tiling-09433` here
    /// (#1915). NVIDIA advertises LINEAR as its only sampler-only ARGB8888
    /// modifier, which is the shape real camera DMA-BUFs take on that
    /// driver.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_linear_modifier_plane_layout_query_raises_no_validation_finding() {
        const DRM_FORMAT_MOD_LINEAR: u64 = 0;

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(e) => {
                println!("Skipping — no Vulkan device: {e}");
                return;
            }
        };
        let counts_before = device.validation_layer_message_counts();
        if counts_before.is_none() {
            println!(
                "Skipping — no validation messenger installed. Re-run with \
                 STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
            );
            return;
        }
        if device.dma_buf_image_pool_tiled().is_none() {
            println!("Skipping — tiled DMA-BUF pool not created");
            return;
        }

        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
        );
        let texture = match HostVulkanTexture::new_render_target_dma_buf(
            &device,
            &desc,
            &[DRM_FORMAT_MOD_LINEAR],
        ) {
            Ok(t) => t,
            Err(e) => {
                // A driver may advertise LINEAR for EGL import yet refuse it
                // under `VkImageDrmFormatModifierListCreateInfoEXT` for this
                // usage set. The aspect-mask contract is what is under test,
                // not the allocator's modifier acceptance.
                println!("Skipping — allocation against DRM_FORMAT_MOD_LINEAR refused: {e}");
                return;
            }
        };

        assert_eq!(
            texture.chosen_drm_format_modifier(),
            DRM_FORMAT_MOD_LINEAR,
            "the candidate list held only LINEAR, so the driver must have picked it"
        );
        assert_eq!(
            texture.vk_image_tiling(),
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            "a LINEAR-modifier image is still modifier-tiled by creation — the \
             recorded tiling is what says so, because the modifier reads zero"
        );

        let layout = texture
            .dma_buf_plane_layout()
            .expect("plane layout of a linear-modifier image must be queryable");
        assert_eq!(layout.len(), 1, "BGRA is single-plane");

        assert_eq!(
            device.validation_layer_message_counts(),
            counts_before,
            "querying a linear-modifier image's plane layout must go through the \
             MEMORY_PLANE aspects, not COLOR"
        );
    }

    /// `new_render_target_dma_buf` with an empty modifier list must fail
    /// loudly rather than silently fall back to LINEAR (which is sampler-
    /// only on NVIDIA — see docs/learnings/nvidia-egl-dmabuf-render-target.md).
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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
        let fd0 = t0
            .export_dma_buf_fd()
            .expect("ring texture 0 DMA-BUF export failed");
        let fd1 = t1
            .export_dma_buf_fd()
            .expect("ring texture 1 DMA-BUF export failed");
        assert!(fd0 >= 0);
        assert!(fd1 >= 0);
        assert_ne!(fd0, fd1);

        println!(
            "Ring texture lifecycle: 2 textures created, image views cached, DMA-BUF exported"
        );

        drop(t0);
        drop(t1);
        println!("Ring textures dropped cleanly");
    }

    /// `new_opaque_fd_export` allocates from the OPAQUE_FD image pool,
    /// reports `is_opaque_fd_export() == true`, exports a valid kernel
    /// fd, and the consumer-rhi import side accepts the same fd (alloc-
    /// size round-trip). Cross-flavor export is rejected (calling
    /// `export_opaque_fd_memory` on a DMA-BUF-allocated texture
    /// produces an error).
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn opaque_fd_image_export_round_trip_and_cross_flavor_rejection() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        if device.opaque_fd_image_pool().is_none() {
            println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
            return;
        }

        // Positive: OPAQUE_FD-allocated image exports an OPAQUE_FD fd.
        let desc = TextureDescriptor::new(128, 128, TextureFormat::Rgba8Unorm);
        let texture = HostVulkanTexture::new_opaque_fd_export(&device, &desc)
            .expect("new_opaque_fd_export failed");
        assert!(
            texture.is_opaque_fd_export(),
            "is_opaque_fd_export() must report true for OPAQUE_FD-allocated images"
        );
        assert_eq!(texture.width(), 128);
        assert_eq!(texture.height(), 128);
        assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);
        let alloc_size = texture.vma_allocation_size();
        assert!(
            alloc_size >= (128 * 128 * 4) as vk::DeviceSize,
            "VMA allocation size {alloc_size} must cover at least 128x128x4 bytes"
        );
        let fd = texture
            .export_opaque_fd_memory()
            .expect("export_opaque_fd_memory failed");
        assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
        unsafe { libc::close(fd) };

        // Negative: DMA-BUF-allocated texture rejects OPAQUE_FD export.
        let dma_buf_desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING);
        let dma_buf_tex = HostVulkanTexture::new(&device, &dma_buf_desc)
            .expect("DMA-BUF texture creation failed");
        assert!(
            !dma_buf_tex.is_opaque_fd_export(),
            "is_opaque_fd_export() must be false for DMA-BUF-allocated images"
        );
        match dma_buf_tex.export_opaque_fd_memory() {
            Err(crate::core::Error::GpuError(msg)) => {
                assert!(
                    msg.contains("not created with `new_opaque_fd_export`"),
                    "error must call out the cross-flavor mismatch, got: {msg}"
                );
            }
            other => panic!("expected cross-flavor rejection on DMA-BUF texture, got {other:?}"),
        }
    }

    /// Format validation: only the CUDA-mappable subset
    /// (`Rgba8Unorm`, `Rgba16Float`, `Rgba32Float`) is accepted;
    /// every other `TextureFormat` variant is rejected at
    /// construction with a message naming the bad format. Locks
    /// the closed-list invariant from
    /// `cudaExternalMemoryGetMappedMipmappedArray`.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn opaque_fd_image_rejects_non_cuda_mappable_formats() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        if device.opaque_fd_image_pool().is_none() {
            println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
            return;
        }

        for bad_format in [
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Nv12,
        ] {
            let desc = TextureDescriptor::new(64, 64, bad_format);
            match HostVulkanTexture::new_opaque_fd_export(&device, &desc) {
                Err(crate::core::Error::Configuration(msg)) => {
                    assert!(
                        msg.contains("CUDA-mappable"),
                        "error must explain the CUDA-mappable constraint, got: {msg}"
                    );
                    assert!(
                        msg.contains(&format!("{bad_format:?}")),
                        "error must name the rejected format {bad_format:?}, got: {msg}"
                    );
                }
                Err(e) => panic!(
                    "format {bad_format:?} must be rejected with Configuration error; \
                     got {e}"
                ),
                Ok(_) => panic!("format {bad_format:?} must be rejected for OPAQUE_FD; got Ok"),
            }
        }

        // Positive controls: every CUDA-mappable format must succeed.
        for good_format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
        ] {
            let desc = TextureDescriptor::new(64, 64, good_format);
            let texture =
                HostVulkanTexture::new_opaque_fd_export(&device, &desc).unwrap_or_else(|e| {
                    panic!("CUDA-mappable format {good_format:?} must succeed, got: {e:?}")
                });
            assert!(texture.is_opaque_fd_export());
        }
    }

    /// Zero width/height is rejected — these aren't CUDA-specific
    /// constraints but mirror the buffer-side input validation.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn opaque_fd_image_rejects_zero_dimensions() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        if device.opaque_fd_image_pool().is_none() {
            println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
            return;
        }

        for (w, h) in [(0, 64), (64, 0), (0, 0)] {
            let desc = TextureDescriptor::new(w, h, TextureFormat::Rgba8Unorm);
            match HostVulkanTexture::new_opaque_fd_export(&device, &desc) {
                Err(crate::core::Error::Configuration(_)) => {}
                Err(e) => panic!(
                    "dimensions {w}x{h} must be rejected with Configuration error; \
                     got {e}"
                ),
                Ok(_) => panic!("dimensions {w}x{h} must be rejected for OPAQUE_FD; got Ok"),
            }
        }
    }

    /// `new_video_dpb` rejects `width == 0`, `height == 0`, and
    /// `array_layers == 0` with a [`Error::Configuration`] **before**
    /// reaching the VMA allocator. Mentally reverting the early-guard
    /// would let the call fall through to `vmaCreateImage`, which
    /// surfaces a driver-level [`Error::GpuError`] instead — the test
    /// distinguishes the two by error variant and would fail. Locks
    /// the early-validation contract that codec sites depend on (the
    /// decoder's `start_video_sequence` and encoder's
    /// `create_dpb_images` both pass values derived from
    /// driver-reported caps, so a zero coming through is the
    /// observable upstream-cap bug we want surfaced as Configuration,
    /// not GpuError).
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_dpb_rejects_zero_dimensions() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let profile = VideoProfileWithOwnedCodecExtensionChain::h264_decode_420_8bit();
        for (w, h, layers) in [(0, 64, 4), (64, 0, 4), (64, 64, 0), (0, 0, 0)] {
            let descriptor = VideoDpbTextureDescriptor {
                label: "test/zero-dim",
                width: w,
                height: h,
                format: TextureFormat::Nv12,
                array_layers: layers,
                direction: VideoDpbDirection::Decode,
                additional_usage: vk::ImageUsageFlags::empty(),
                sharing_queue_families: &[],
                video_profile: profile.video_profile_info(),
            };
            match HostVulkanTexture::new_video_dpb(&device, &descriptor) {
                Err(crate::core::Error::Configuration(msg)) => {
                    assert!(
                        msg.contains("must be > 0"),
                        "rejection must explain the constraint, got: {msg}"
                    );
                    assert!(
                        msg.contains("test/zero-dim"),
                        "rejection must carry the descriptor label, got: {msg}"
                    );
                }
                Err(e) => panic!("{w}x{h}x{layers}: expected Configuration error, got {e}"),
                Ok(_) => {
                    panic!("{w}x{h}x{layers}: zero-component descriptor must be rejected, got Ok")
                }
            }
        }
    }

    /// `new_video_dpb` for both directions allocates a non-null
    /// `VkImage`, picks the correct DPB usage flag, and lands as a
    /// DEVICE_LOCAL allocation (verified indirectly: a non-null
    /// VkImage post-construction means VMA accepted the chain with
    /// the codec profile + DPB usage). Hardware-gated because real
    /// VMA + driver are needed.
    ///
    /// A direction the device has no H.264 support for — the codec extension
    /// enabled and a queue family for it — is skipped; a direction it does
    /// support must allocate. Swallowing the driver's rejection instead would
    /// leave the test unable to fail.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_dpb_succeeds_for_both_directions() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        for direction in [VideoDpbDirection::Decode, VideoDpbDirection::Encode] {
            if !device_supports_h264_for_dpb_direction(&device, direction) {
                println!("Skipping {direction:?} - device has no H.264 support in that direction");
                continue;
            }
            let additional_usage = match direction {
                VideoDpbDirection::Decode => {
                    vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::SAMPLED
                }
                VideoDpbDirection::Encode => vk::ImageUsageFlags::empty(),
            };
            let profile =
                VideoProfileWithOwnedCodecExtensionChain::h264_for_dpb_direction(direction);
            let descriptor = VideoDpbTextureDescriptor {
                label: "test/dpb-positive",
                width: 1920,
                height: 1088,
                format: TextureFormat::Nv12,
                array_layers: 4,
                direction,
                additional_usage,
                sharing_queue_families: &[],
                video_profile: profile.video_profile_info(),
            };
            let texture = HostVulkanTexture::new_video_dpb(&device, &descriptor)
                .unwrap_or_else(|e| panic!("{direction:?}: DPB image allocation failed: {e}"));
            assert_ne!(
                texture.image(),
                Some(vk::Image::null()),
                "{direction:?}: VkImage handle must be non-null"
            );
            assert!(
                texture.image().is_some(),
                "{direction:?}: image() must return Some after successful construction"
            );
        }
    }
}
