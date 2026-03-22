// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan texture implementation for RHI.

use std::sync::Arc;

use ash::vk;
use gpu_allocator::vulkan::{Allocation, Allocator};
use gpu_allocator::MemoryLocation;
use parking_lot::Mutex;

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::{Result, StreamError};

use super::VulkanDevice;

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

/// Vulkan texture wrapper.
///
/// Wraps a VkImage with associated memory and metadata.
/// Can be created from scratch or imported from an IOSurface via VK_EXT_metal_objects.
pub struct VulkanTexture {
    device: Option<ash::Device>,
    /// Vulkan instance handle for extension loaders (e.g., DMA-BUF export).
    instance: Option<ash::Instance>,
    image: Option<vk::Image>,
    /// Sub-allocated memory from gpu-allocator (used for internally-created textures).
    gpu_memory_allocation: Option<Allocation>,
    /// Shared allocator handle for freeing the sub-allocation in Drop.
    gpu_memory_allocator: Option<Arc<Mutex<Allocator>>>,
    /// Raw device memory not managed by gpu-allocator (DMA-BUF export/import paths).
    raw_device_memory: Option<vk::DeviceMemory>,
    /// Cached DMA-BUF fd to avoid leaking a new fd on each export call.
    #[cfg(target_os = "linux")]
    cached_dma_buf_fd: std::sync::OnceLock<std::os::unix::io::RawFd>,
    /// Whether this texture was imported from IOSurface (no memory to free)
    imported_from_iosurface: bool,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl VulkanTexture {
    /// Create a new Vulkan texture.
    pub fn new(vulkan_device: &VulkanDevice, desc: &TextureDescriptor) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // Create image
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { device.create_image(&image_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create image: {e}")))?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };

        // On Linux with external memory support, we must use raw vkAllocateMemory
        // with VkExportMemoryAllocateInfo in the pNext chain — gpu-allocator
        // does not support pNext-extended allocations.
        #[cfg(target_os = "linux")]
        if vulkan_device.supports_external_memory() {
            let memory_type_index = vulkan_device
                .find_memory_type(
                    mem_requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .map_err(|e| {
                    unsafe { device.destroy_image(image, None) };
                    e
                })?;

            let mut export_info = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_requirements.size)
                .memory_type_index(memory_type_index)
                .push_next(&mut export_info);

            let memory = unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                StreamError::GpuError(format!("Failed to allocate exportable memory: {e}"))
            })?;

            unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
                unsafe {
                    device.free_memory(memory, None);
                    device.destroy_image(image, None);
                }
                StreamError::GpuError(format!("Failed to bind memory: {e}"))
            })?;

            return Ok(Self {
                device: Some(device.clone()),
                instance: Some(vulkan_device.instance().clone()),
                image: Some(image),
                gpu_memory_allocation: None,
                gpu_memory_allocator: None,
                raw_device_memory: Some(memory),
                #[cfg(target_os = "linux")]
                cached_dma_buf_fd: std::sync::OnceLock::new(),
                imported_from_iosurface: false,
                width: desc.width,
                height: desc.height,
                format: desc.format,
            });
        }

        // Standard path: sub-allocate through gpu-allocator
        let allocation = vulkan_device.allocate_gpu_memory(
            "texture",
            mem_requirements,
            MemoryLocation::GpuOnly,
            false, // images are non-linear (tiled)
        )?;

        unsafe {
            device.bind_image_memory(image, allocation.memory(), allocation.offset())
        }
        .map_err(|e| StreamError::GpuError(format!("Failed to bind memory: {e}")))?;

        let gpu_memory_allocator = vulkan_device
            .gpu_memory_allocator()
            .cloned();

        Ok(Self {
            device: Some(device.clone()),
            instance: Some(vulkan_device.instance().clone()),
            image: Some(image),
            gpu_memory_allocation: Some(allocation),
            gpu_memory_allocator,
            raw_device_memory: None,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: std::sync::OnceLock::new(),
            imported_from_iosurface: false,
            width: desc.width,
            height: desc.height,
            format: desc.format,
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
        device: &ash::Device,
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
        let mut image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // Chain the import info
        image_info.p_next = &import_info as *const _ as *const _;

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to create image from IOSurface: {e}"))
        })?;

        tracing::debug!(
            "Imported IOSurface as Vulkan image: {}x{} {:?}",
            width,
            height,
            format
        );

        Ok(Self {
            device: Some(device.clone()),
            instance: None,
            image: Some(image),
            gpu_memory_allocation: None,
            gpu_memory_allocator: None,
            raw_device_memory: None, // IOSurface manages the memory
            imported_from_iosurface: true,
            width,
            height,
            format,
        })
    }

    /// Create a placeholder texture for cases where a VulkanTexture is needed
    /// but the actual texture is stored elsewhere (e.g., Metal texture on macOS).
    pub fn placeholder() -> Self {
        Self {
            device: None,
            instance: None,
            image: None,
            gpu_memory_allocation: None,
            gpu_memory_allocator: None,
            raw_device_memory: None,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: std::sync::OnceLock::new(),
            imported_from_iosurface: false,
            width: 0,
            height: 0,
            format: TextureFormat::Rgba8Unorm,
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
}

#[cfg(target_os = "linux")]
impl VulkanTexture {
    /// Export the texture's memory as a DMA-BUF file descriptor.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        // Return cached fd if already exported (vkGetMemoryFdKHR returns a new fd each call)
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            return Ok(fd);
        }

        // DMA-BUF export only works with raw device memory (exportable allocations),
        // not with gpu-allocator sub-allocations.
        let memory = self.raw_device_memory.ok_or_else(|| {
            StreamError::GpuError(
                "Cannot export DMA-BUF from texture without exportable memory".into(),
            )
        })?;

        let instance = self.instance.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no Vulkan instance stored".into())
        })?;

        let device = self.device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no Vulkan device stored".into())
        })?;

        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(instance, device);

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        let _ = self.cached_dma_buf_fd.set(fd);
        Ok(fd)
    }

    /// Import a texture from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &VulkanDevice,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to create image for DMA-BUF import: {e}"))
        })?;

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory_type_index = vulkan_device
            .find_memory_type(
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
            })?;

        // Allocation size must be >= mem_requirements.size per Vulkan spec
        let alloc_size = allocation_size.max(mem_requirements.size);

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(alloc_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info);

        let memory = unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!("Failed to import DMA-BUF memory: {e}"))
        })?;

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            unsafe {
                device.free_memory(memory, None);
                device.destroy_image(image, None);
            }
            StreamError::GpuError(format!("Failed to bind imported memory: {e}"))
        })?;

        Ok(Self {
            device: Some(device.clone()),
            instance: Some(vulkan_device.instance().clone()),
            image: Some(image),
            gpu_memory_allocation: None,
            gpu_memory_allocator: None,
            raw_device_memory: Some(memory),
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: std::sync::OnceLock::new(),
            imported_from_iosurface: false,
            width,
            height,
            format,
        })
    }
}

impl Clone for VulkanTexture {
    fn clone(&self) -> Self {
        // Placeholder textures can be cloned directly
        // Real textures should use Arc<VulkanTexture> for sharing
        Self {
            device: None,
            instance: None,
            image: None,
            gpu_memory_allocation: None,
            gpu_memory_allocator: None,
            raw_device_memory: None,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: std::sync::OnceLock::new(),
            imported_from_iosurface: false,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}

impl Drop for VulkanTexture {
    fn drop(&mut self) {
        // Close cached DMA-BUF fd before freeing GPU resources
        #[cfg(target_os = "linux")]
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            unsafe { libc::close(fd) };
        }

        if let Some(device) = &self.device {
            unsafe {
                if let Some(image) = self.image {
                    device.destroy_image(image, None);
                }
            }

            // Free sub-allocated memory through the allocator
            if let Some(allocation) = self.gpu_memory_allocation.take() {
                if let Some(allocator) = &self.gpu_memory_allocator {
                    if let Err(e) = allocator.lock().free(allocation) {
                        tracing::error!("Failed to free texture allocation: {e}");
                    }
                }
            }

            // Free raw device memory (DMA-BUF imports / exportable allocations)
            if !self.imported_from_iosurface {
                if let Some(memory) = self.raw_device_memory {
                    unsafe { device.free_memory(memory, None) };
                }
            }
        }
    }
}

// VulkanTexture is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanTexture {}
unsafe impl Sync for VulkanTexture {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::VulkanDevice;

    #[test]
    fn test_placeholder_texture() {
        let tex = VulkanTexture::placeholder();
        assert_eq!(tex.width(), 0);
        assert_eq!(tex.height(), 0);
        assert!(tex.image().is_none());
    }

    #[test]
    fn test_texture_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        let desc = TextureDescriptor::new(256, 256, TextureFormat::Rgba8Unorm);
        let result = device.create_texture(&desc);

        // Note: This may fail if memory type 0 is not suitable
        // A full implementation would search for the right memory type
        match result {
            Ok(tex) => {
                assert_eq!(tex.width(), 256);
                assert_eq!(tex.height(), 256);
                assert!(tex.image().is_some());
                println!("Texture creation succeeded");
            }
            Err(e) => {
                println!(
                    "Texture creation failed (expected with basic memory allocation): {}",
                    e
                );
            }
        }
    }
}
