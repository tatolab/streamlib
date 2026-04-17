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
    /// VulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Option<Arc<VulkanDevice>>,
    image: Option<vk::Image>,
    /// VMA allocation (always allocated with DMA-BUF export flags via VulkanDevice).
    allocation: Option<vma::Allocation>,
    /// Imported device memory for DMA-BUF import path (VMA cannot import external memory).
    #[cfg(target_os = "linux")]
    imported_memory: Option<vk::DeviceMemory>,
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
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl VulkanTexture {
    /// Create a new DMA-BUF exportable Vulkan texture via the device's
    /// dedicated VMA export pool.
    ///
    /// The export pool is configured with `pMemoryAllocateNext` set to
    /// `VkExportMemoryAllocateInfo::DMA_BUF_EXT`, isolating exportable
    /// allocations from the default VMA pool. This avoids NVIDIA driver
    /// failures where global export configuration causes OOM after swapchain
    /// creation.
    pub fn new(vulkan_device: &Arc<VulkanDevice>, desc: &TextureDescriptor) -> Result<Self> {
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
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            width: desc.width,
            height: desc.height,
            format: desc.format,
        })
    }

    /// Create a non-exportable DEVICE_LOCAL texture via the default VMA allocator.
    ///
    /// Unlike [`new`] which uses the DMA-BUF export pool, this uses the default
    /// VMA allocator with no external memory info. For same-process textures that
    /// don't need cross-process sharing.
    pub fn new_device_local(
        vulkan_device: &Arc<VulkanDevice>,
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
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
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
        })
    }

    /// Create a placeholder texture for cases where a VulkanTexture is needed
    /// but the actual texture is stored elsewhere (e.g., Metal texture on macOS).
    pub fn placeholder() -> Self {
        Self {
            vulkan_device: None,
            image: None,
            allocation: None,
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
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
            StreamError::GpuError("Cannot create image view: no VulkanDevice stored".into())
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
impl VulkanTexture {
    /// Export the texture's memory as a DMA-BUF file descriptor.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        if let Some(&fd) = self.cached_dma_buf_fd.get() {
            return Ok(fd);
        }

        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no VulkanDevice stored".into())
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
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
            )
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
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            imported_from_dma_buf: true,
            width,
            height,
            format,
        })
    }
}

impl Clone for VulkanTexture {
    fn clone(&self) -> Self {
        Self {
            vulkan_device: None,
            image: None,
            allocation: None,
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            cached_dma_buf_fd: OnceLock::new(),
            cached_image_view: OnceLock::new(),
            imported_from_iosurface: false,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}

impl Drop for VulkanTexture {
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

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = VulkanTexture::new(&device, &desc).expect("texture creation failed");

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
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
        let texture = VulkanTexture::new(&device, &desc).expect("texture creation failed");
        drop(texture);

        println!("Texture drop completed without panic");
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

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);

        let t0 = VulkanTexture::new(&device, &desc).expect("texture 0 failed");
        let t1 = VulkanTexture::new(&device, &desc).expect("texture 1 failed");
        let t2 = VulkanTexture::new(&device, &desc).expect("texture 2 failed");
        let t3 = VulkanTexture::new(&device, &desc).expect("texture 3 failed");

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
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let allocator = device.allocator();
        let vk_device = device.device();
        let width = 1920u32;
        let height = 1080u32;

        // Step 1: Camera pixel buffers via VulkanPixelBuffer (raw exportable allocation)
        use crate::vulkan::rhi::VulkanPixelBuffer;
        use crate::core::rhi::PixelFormat;
        let mut pixel_buffers = Vec::new();
        for i in 0..4 {
            let buf = VulkanPixelBuffer::new(&device, width, height, 4, PixelFormat::Bgra32)
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

    #[test]
    fn test_device_local_texture_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);
        let texture = VulkanTexture::new_device_local(&device, &desc)
            .expect("device-local texture creation failed");

        assert!(texture.image().is_some());
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);

        println!("Device-local texture created: {}x{}", texture.width(), texture.height());
    }

    #[test]
    fn test_lazy_image_view() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm);
        let texture = VulkanTexture::new(&device, &desc)
            .expect("texture creation failed");

        // First call creates the image view
        let view1 = texture.image_view().expect("image_view() failed");
        // Second call returns the cached view
        let view2 = texture.image_view().expect("cached image_view() failed");
        assert_eq!(view1, view2, "image_view() should return the same cached view");

        println!("Lazy image view: created and cached successfully");
    }

    #[test]
    fn test_ring_texture_lifecycle() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);

        // Create 2 ring textures (matches RING_TEXTURE_COUNT)
        let t0 = VulkanTexture::new(&device, &desc).expect("ring texture 0 failed");
        let t1 = VulkanTexture::new(&device, &desc).expect("ring texture 1 failed");

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
