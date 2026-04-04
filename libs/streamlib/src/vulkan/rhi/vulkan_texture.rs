// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan texture implementation for RHI.

use std::sync::Arc;

use ash::vk;

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
    /// Raw device handle for Vulkan API calls.
    device: Option<ash::Device>,
    /// VulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Option<Arc<VulkanDevice>>,
    image: Option<vk::Image>,
    /// Device memory (always allocated with DMA-BUF export flags via VulkanDevice).
    device_memory: Option<vk::DeviceMemory>,
    /// Cached DMA-BUF fd to avoid leaking a new fd on each export call.
    #[cfg(target_os = "linux")]
    cached_dma_buf_fd: std::sync::OnceLock<std::os::unix::io::RawFd>,
    /// Whether this texture was imported from IOSurface (no memory to free).
    imported_from_iosurface: bool,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl VulkanTexture {
    /// Create a new Vulkan texture.
    pub fn new(vulkan_device: &Arc<VulkanDevice>, desc: &TextureDescriptor) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // Declare DMA-BUF handle type at image creation — required by Vulkan spec
        // (VUID-vkBindImageMemory-memory-02728) when memory will be allocated with
        // VkExportMemoryAllocateInfo. Without this, binding exportable memory to the
        // image is undefined behavior and fails on NVIDIA.
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

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
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_image_info);

        let image = unsafe { device.create_image(&image_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create image: {e}")))?;

        let memory = vulkan_device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, true)
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!("Failed to bind memory: {e}"))
        })?;

        Ok(Self {
            device: Some(device.clone()),
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            device_memory: Some(memory),
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
            vulkan_device: None,
            image: Some(image),
            device_memory: None, // IOSurface manages the memory
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
            vulkan_device: None,
            image: None,
            device_memory: None,
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
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            return Ok(fd);
        }

        let memory = self.device_memory.ok_or_else(|| {
            StreamError::GpuError(
                "Cannot export DMA-BUF from texture without device memory".into(),
            )
        })?;

        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no VulkanDevice stored".into())
        })?;

        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(vk_dev.instance(), vk_dev.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        let _ = self.cached_dma_buf_fd.set(fd);
        Ok(fd)
    }

    /// Import a texture from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<VulkanDevice>,
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
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!("Failed to bind imported memory: {e}"))
        })?;

        Ok(Self {
            device: Some(device.clone()),
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            device_memory: Some(memory),
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
        Self {
            device: None,
            vulkan_device: None,
            image: None,
            device_memory: None,
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
        }

        // Free tracked memory through VulkanDevice RHI
        if !self.imported_from_iosurface {
            if let Some(memory) = self.device_memory {
                if let Some(vk_dev) = &self.vulkan_device {
                    vk_dev.free_device_memory(memory);
                } else if let Some(device) = &self.device {
                    // Fallback for macOS IOSurface path (no VulkanDevice)
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
    fn test_pool_texture_creation_1920x1080_bgra8() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let before = device.live_allocation_count();
        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = VulkanTexture::new(&device, &desc).expect("texture creation failed");

        assert!(texture.image().is_some());
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.format(), TextureFormat::Bgra8Unorm);
        assert_eq!(device.live_allocation_count(), before + 1);

        println!(
            "Pool texture created: {}x{} {:?}, allocations: {}",
            texture.width(),
            texture.height(),
            texture.format(),
            device.live_allocation_count()
        );
    }

    #[test]
    fn test_texture_drop_frees_memory() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let before = device.live_allocation_count();
        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = VulkanTexture::new(&device, &desc).expect("texture creation failed");
        assert_eq!(device.live_allocation_count(), before + 1);

        drop(texture);
        assert_eq!(device.live_allocation_count(), before);

        println!("Texture drop freed memory: allocations back to {}", before);
    }

    #[test]
    fn test_multiple_textures_coexist() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let before = device.live_allocation_count();
        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);

        let t0 = VulkanTexture::new(&device, &desc).expect("texture 0 failed");
        let t1 = VulkanTexture::new(&device, &desc).expect("texture 1 failed");
        let t2 = VulkanTexture::new(&device, &desc).expect("texture 2 failed");
        let t3 = VulkanTexture::new(&device, &desc).expect("texture 3 failed");

        assert_eq!(device.live_allocation_count(), before + 4);
        assert!(t0.image().is_some());
        assert!(t1.image().is_some());
        assert!(t2.image().is_some());
        assert!(t3.image().is_some());

        println!(
            "4 textures coexist, allocations: {}",
            device.live_allocation_count()
        );

        drop(t0);
        drop(t1);
        drop(t2);
        drop(t3);

        assert_eq!(device.live_allocation_count(), before);
        println!("All dropped, allocations back to {}", before);
    }

    #[test]
    fn test_dma_buf_export() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = VulkanTexture::new(&device, &desc).expect("texture creation failed");

        let fd = texture.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

        println!("DMA-BUF exported: fd={fd}");
        // fd is closed by VulkanTexture::drop via cached_dma_buf_fd
    }

    #[test]
    fn test_placeholder_has_no_resources() {
        let tex = VulkanTexture::placeholder();
        assert!(tex.image().is_none());
        assert_eq!(tex.width(), 0);
        assert_eq!(tex.height(), 0);
        assert!(tex.device_memory.is_none());
        assert!(tex.vulkan_device.is_none());

        println!("Placeholder verified: no image, no memory, no device");
    }

    #[test]
    fn test_various_formats() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let formats = [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];

        for format in formats {
            let desc = TextureDescriptor::new(1920, 1080, format);
            let texture = VulkanTexture::new(&device, &desc)
                .unwrap_or_else(|e| panic!("Failed to create texture with {format:?}: {e}"));

            assert!(texture.image().is_some());
            assert_eq!(texture.format(), format);
            println!("Format {:?}: OK", format);
        }
    }
}
