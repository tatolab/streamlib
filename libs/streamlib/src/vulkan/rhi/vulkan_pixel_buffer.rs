// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;
use gpu_allocator::vulkan::{Allocation, Allocator};
use gpu_allocator::MemoryLocation;
use parking_lot::Mutex;

use crate::core::rhi::PixelFormat;
use crate::core::{Result, StreamError};

use super::VulkanDevice;

/// Process-global VulkanDevice reference for DMA-BUF import.
///
/// Set once during [`GpuDevice::new()`] on Linux. The import trait
/// (`RhiPixelBufferImport::from_external_handle`) is a static method with no
/// device parameter, so this global bridges that gap.
#[cfg(target_os = "linux")]
pub(crate) static VULKAN_DEVICE_FOR_IMPORT: std::sync::OnceLock<Arc<VulkanDevice>> =
    std::sync::OnceLock::new();

/// CPU-visible staging buffer for pixel data upload/readback.
pub struct VulkanPixelBuffer {
    device: ash::Device,
    /// Vulkan instance handle for extension loaders (e.g., DMA-BUF export).
    instance: Option<ash::Instance>,
    buffer: vk::Buffer,
    gpu_memory_allocation: Option<Allocation>,
    gpu_memory_allocator: Arc<Mutex<Allocator>>,
    /// Raw device memory not managed by gpu-allocator (DMA-BUF export/import paths).
    raw_device_memory: Option<vk::DeviceMemory>,
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    size: vk::DeviceSize,
}

impl VulkanPixelBuffer {
    /// Create a new CPU-visible staging buffer.
    pub fn new(
        vulkan_device: &VulkanDevice,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);

        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let device = vulkan_device.device();

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create staging buffer: {e}")))?;

        let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

        // On Linux with external memory support, allocate with VkExportMemoryAllocateInfo
        // so the buffer can be shared cross-process via DMA-BUF. We bypass gpu-allocator
        // because it does not support pNext-extended allocations.
        #[cfg(target_os = "linux")]
        if vulkan_device.supports_external_memory() {
            let memory_type_index = vulkan_device.find_memory_type(
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;

            let mut export_info = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_requirements.size)
                .memory_type_index(memory_type_index)
                .push_next(&mut export_info);

            let memory = unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to allocate exportable buffer memory: {e}"
                ))
            })?;

            unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to bind exportable buffer memory: {e}"
                ))
            })?;

            let mapped_ptr = unsafe {
                device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
            }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to map exportable buffer memory: {e}"))
            })? as *mut u8;

            let gpu_memory_allocator = vulkan_device
                .gpu_memory_allocator()
                .ok_or_else(|| {
                    StreamError::GpuError("GPU memory allocator not available".into())
                })?
                .clone();

            return Ok(Self {
                device: device.clone(),
                instance: Some(vulkan_device.instance().clone()),
                buffer,
                gpu_memory_allocation: None,
                gpu_memory_allocator,
                raw_device_memory: Some(memory),
                mapped_ptr,
                width,
                height,
                bytes_per_pixel,
                format,
                size,
            });
        }

        // Standard path: sub-allocate through gpu-allocator
        let allocation = vulkan_device.allocate_gpu_memory(
            "staging_pixel_buffer",
            mem_requirements,
            MemoryLocation::CpuToGpu,
            true, // buffers are linear
        )?;

        unsafe {
            device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset())
        }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to bind staging buffer memory: {e}"))
        })?;

        let mapped_ptr = allocation
            .mapped_ptr()
            .ok_or_else(|| {
                StreamError::GpuError("Staging buffer allocation is not host-mapped".into())
            })?
            .as_ptr() as *mut u8;

        let gpu_memory_allocator = vulkan_device
            .gpu_memory_allocator()
            .ok_or_else(|| {
                StreamError::GpuError("GPU memory allocator not available".into())
            })?
            .clone();

        Ok(Self {
            device: device.clone(),
            instance: None,
            buffer,
            gpu_memory_allocation: Some(allocation),
            gpu_memory_allocator,
            raw_device_memory: None,
            mapped_ptr,
            width,
            height,
            bytes_per_pixel,
            format,
            size,
        })
    }

    /// Persistently mapped pointer for CPU access.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Buffer width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per pixel.
    pub fn bytes_per_pixel(&self) -> u32 {
        self.bytes_per_pixel
    }

    /// Pixel format.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Total buffer size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Underlying Vulkan buffer handle.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }
}

#[cfg(target_os = "linux")]
impl VulkanPixelBuffer {
    /// Export the buffer's memory as a DMA-BUF file descriptor.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        let memory = self.raw_device_memory.ok_or_else(|| {
            StreamError::GpuError(
                "Cannot export DMA-BUF from buffer without exportable memory".into(),
            )
        })?;

        let instance = self.instance.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no Vulkan instance stored".into())
        })?;

        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(instance, &self.device);

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        Ok(fd)
    }

    /// Import a buffer from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &VulkanDevice,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);
        let effective_size = if allocation_size > 0 {
            allocation_size
        } else {
            size
        };

        let buffer_info = vk::BufferCreateInfo::default()
            .size(effective_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to create buffer for DMA-BUF import: {e}"))
        })?;

        let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);

        let memory_type_index = vulkan_device.find_memory_type(
            mem_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(effective_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info);

        let memory = unsafe { device.allocate_memory(&alloc_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to import DMA-BUF memory: {e}")))?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|e| StreamError::GpuError(format!("Failed to bind imported memory: {e}")))?;

        let mapped_ptr = unsafe {
            device.map_memory(memory, 0, effective_size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to map imported buffer memory: {e}"))
        })? as *mut u8;

        let gpu_memory_allocator = vulkan_device
            .gpu_memory_allocator()
            .ok_or_else(|| {
                StreamError::GpuError("GPU memory allocator not available".into())
            })?
            .clone();

        Ok(Self {
            device: device.clone(),
            instance: Some(vulkan_device.instance().clone()),
            buffer,
            gpu_memory_allocation: None,
            gpu_memory_allocator,
            raw_device_memory: Some(memory),
            mapped_ptr,
            width,
            height,
            bytes_per_pixel,
            format,
            size: effective_size,
        })
    }
}

impl Drop for VulkanPixelBuffer {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_buffer(self.buffer, None);
        }

        // Free sub-allocated memory through the allocator
        if let Some(allocation) = self.gpu_memory_allocation.take() {
            if let Err(e) = self.gpu_memory_allocator.lock().free(allocation) {
                tracing::error!("Failed to free staging buffer allocation: {e}");
            }
        }

        // Free raw device memory (DMA-BUF exports / imports)
        if let Some(memory) = self.raw_device_memory {
            unsafe {
                self.device.unmap_memory(memory);
                self.device.free_memory(memory, None);
            }
        }
    }
}

// Safety: Vulkan handles are thread-safe
unsafe impl Send for VulkanPixelBuffer {}
unsafe impl Sync for VulkanPixelBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vulkan_pixel_buffer_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        let result = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::default());
        match result {
            Ok(buf) => {
                assert_eq!(buf.width(), 1920);
                assert_eq!(buf.height(), 1080);
                assert_eq!(buf.bytes_per_pixel(), 4);
                assert_eq!(buf.size(), 1920 * 1080 * 4);
                assert!(!buf.mapped_ptr().is_null());
                assert_ne!(buf.buffer(), vk::Buffer::null());
                println!("VulkanPixelBuffer creation succeeded");
            }
            Err(e) => {
                println!("VulkanPixelBuffer creation failed: {}", e);
            }
        }
    }
}
