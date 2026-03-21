// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

use crate::core::rhi::PixelFormat;
use crate::core::{Result, StreamError};

use super::VulkanDevice;

/// CPU-visible staging buffer for pixel data upload/readback.
pub struct VulkanPixelBuffer {
    device: ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
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

        let memory_type_index = vulkan_device.find_memory_type(
            mem_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_requirements.size)
            .memory_type_index(memory_type_index);

        let memory = unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to allocate staging memory: {e}"))
        })?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to bind staging buffer memory: {e}"))
        })?;

        let mapped_ptr = unsafe {
            device.map_memory(
                memory,
                0,
                mem_requirements.size,
                vk::MemoryMapFlags::empty(),
            )
        }
        .map_err(|e| StreamError::GpuError(format!("Failed to map staging buffer memory: {e}")))?
            as *mut u8;

        Ok(Self {
            device: device.clone(),
            buffer,
            memory,
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

impl Drop for VulkanPixelBuffer {
    fn drop(&mut self) {
        unsafe {
            self.device.unmap_memory(self.memory);
            self.device.destroy_buffer(self.buffer, None);
            self.device.free_memory(self.memory, None);
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
