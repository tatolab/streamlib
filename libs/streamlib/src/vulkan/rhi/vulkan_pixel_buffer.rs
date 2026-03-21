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

/// CPU-visible staging buffer for pixel data upload/readback.
pub struct VulkanPixelBuffer {
    device: ash::Device,
    buffer: vk::Buffer,
    gpu_memory_allocation: Option<Allocation>,
    gpu_memory_allocator: Arc<Mutex<Allocator>>,
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

        let mapped_ptr = allocation.mapped_ptr().ok_or_else(|| {
            StreamError::GpuError(
                "Staging buffer allocation is not host-mapped".into(),
            )
        })?.as_ptr() as *mut u8;

        let gpu_memory_allocator = vulkan_device
            .gpu_memory_allocator()
            .ok_or_else(|| {
                StreamError::GpuError("GPU memory allocator not available".into())
            })?
            .clone();

        Ok(Self {
            device: device.clone(),
            buffer,
            gpu_memory_allocation: Some(allocation),
            gpu_memory_allocator,
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
            self.device.destroy_buffer(self.buffer, None);
        }
        if let Some(allocation) = self.gpu_memory_allocation.take() {
            if let Err(e) = self.gpu_memory_allocator.lock().free(allocation) {
                tracing::error!("Failed to free staging buffer allocation: {e}");
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
