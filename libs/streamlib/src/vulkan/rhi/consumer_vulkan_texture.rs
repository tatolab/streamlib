// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side Vulkan texture — imports a host-allocated DMA-BUF
//! into the consumer's `VkDevice` and exposes the resulting `VkImage`.
//!
//! Surface adapters running inside a cdylib hand subprocess customers
//! a [`crate::core::rhi::StreamTexture`] backed by one of these. The
//! type carries only the carve-out methods: import constructors, raw
//! accessors, lazy image-view creation. There is no allocation path
//! and no DMA-BUF export — the host owns those.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{TextureFormat, TextureUsages};
use crate::core::{Result, StreamError};

use super::ConsumerVulkanDevice;

/// Convert RHI [`TextureFormat`] to the matching `vk::Format`.
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

/// Convert RHI [`TextureUsages`] to Vulkan usage flags.
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
    if flags.is_empty() {
        flags = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC;
    }
    flags
}

/// Consumer-side Vulkan texture. See module docs.
pub struct ConsumerVulkanTexture {
    /// Owning consumer device, kept for `Drop` and lazy-view creation.
    vulkan_device: Arc<ConsumerVulkanDevice>,
    image: vk::Image,
    /// Imported `VkDeviceMemory` from the host's DMA-BUF FD.
    imported_memory: vk::DeviceMemory,
    cached_image_view: OnceLock<vk::ImageView>,
    /// DRM format modifier the host's driver chose at allocation time.
    /// Zero is reserved for `DRM_FORMAT_MOD_LINEAR` — sampler-only on
    /// NVIDIA, refused at the render-target import path.
    drm_format_modifier: u64,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl ConsumerVulkanTexture {
    /// Import a host-allocated render-target DMA-BUF as a tiled image
    /// on the consumer device.
    ///
    /// The host pre-chose a non-LINEAR DRM modifier
    /// (`new_render_target_dma_buf` on the host side); the consumer
    /// reproduces the exact image-create state via
    /// `VkImageDrmFormatModifierExplicitCreateInfoEXT` so the GPU
    /// memory layout is consistent across the IPC boundary.
    ///
    /// fd ownership: the consumer transfers ownership to Vulkan on
    /// success (the driver dups internally and releases on
    /// `vkFreeMemory`). On error the caller still owns `fds[0]`.
    pub fn import_render_target_dma_buf(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
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
                "ConsumerVulkanTexture::import_render_target_dma_buf: empty fd vec".into(),
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
                "import_render_target_dma_buf: zero (LINEAR) modifier — host should have allocated a tiled modifier; LINEAR DMA-BUFs are sampler-only on NVIDIA".into(),
            ));
        }

        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);

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
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::STORAGE,
            )
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

        // Single-plane import covers BGRA / RGBA — the formats #510
        // currently publishes RT modifiers for. Multi-plane import via
        // VkBindImagePlaneMemoryInfo is added when a multi-plane
        // consumer surfaces.
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

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!(
                "import_render_target_dma_buf: bind_image_memory failed: {e}"
            ))
        })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            image,
            imported_memory: memory,
            cached_image_view: OnceLock::new(),
            drm_format_modifier,
            width,
            height,
            format,
        })
    }

    /// Import a single-plane LINEAR DMA-BUF as a sampler-only image.
    ///
    /// Use [`Self::import_render_target_dma_buf`] when the consumer
    /// will render INTO the imported image — LINEAR is sampler-only on
    /// NVIDIA.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "ConsumerVulkanTexture::from_dma_buf_fd: create_image failed: {e}"
            ))
        })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

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

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!(
                "ConsumerVulkanTexture::from_dma_buf_fd: bind_image_memory failed: {e}"
            ))
        })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            image,
            imported_memory: memory,
            cached_image_view: OnceLock::new(),
            drm_format_modifier: 0,
            width,
            height,
            format,
        })
    }

    /// Underlying `VkImage` handle.
    pub fn image(&self) -> vk::Image {
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

    /// DRM format modifier propagated from the host's allocation. Zero
    /// for textures imported via [`Self::from_dma_buf_fd`] (LINEAR).
    pub fn chosen_drm_format_modifier(&self) -> u64 {
        self.drm_format_modifier
    }

    /// Lazy-cached `VkImageView` covering the texture's full subresource
    /// range. Created on first call; subsequent calls return the cached
    /// handle.
    pub fn image_view(&self) -> Result<vk::ImageView> {
        if let Some(&view) = self.cached_image_view.get() {
            return Ok(view);
        }
        let vk_format = texture_format_to_vk(self.format);
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(self.image)
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
        let view = unsafe { self.vulkan_device.device().create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("create_image_view failed: {e}")))?;
        let _ = self.cached_image_view.set(view);
        Ok(*self.cached_image_view.get().unwrap())
    }
}

impl Drop for ConsumerVulkanTexture {
    fn drop(&mut self) {
        if let Some(&view) = self.cached_image_view.get() {
            unsafe { self.vulkan_device.device().destroy_image_view(view, None) };
        }
        unsafe { self.vulkan_device.device().destroy_image(self.image, None) };
        self.vulkan_device.free_imported_memory(self.imported_memory);
    }
}

unsafe impl Send for ConsumerVulkanTexture {}
unsafe impl Sync for ConsumerVulkanTexture {}

impl super::VulkanTextureLike for ConsumerVulkanTexture {
    fn image(&self) -> Option<vk::Image> {
        Some(ConsumerVulkanTexture::image(self))
    }
    fn chosen_drm_format_modifier(&self) -> u64 {
        ConsumerVulkanTexture::chosen_drm_format_modifier(self)
    }
}
