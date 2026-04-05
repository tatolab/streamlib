// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;

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
    /// VulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Option<Arc<VulkanDevice>>,
    buffer: vk::Buffer,
    /// Device memory (allocated with DMA-BUF export flags via VulkanDevice).
    device_memory: vk::DeviceMemory,
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    format: PixelFormat,
    size: vk::DeviceSize,
}

impl VulkanPixelBuffer {
    /// Create a new CPU-visible staging buffer.
    pub fn new(
        vulkan_device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
        bits_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bits_per_pixel as vk::DeviceSize)
            / 8;

        let device = vulkan_device.device();

        // Declare DMA-BUF handle type at buffer creation — required by Vulkan spec
        // when memory will be allocated with VkExportMemoryAllocateInfo::handleTypes.
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create staging buffer: {e}")))?;

        let memory = vulkan_device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                true,
            )
            .map_err(|e| {
                unsafe { device.destroy_buffer(buffer, None) };
                e
            })?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("Failed to bind staging buffer memory: {e}"))
        })?;

        let mapped_ptr = vulkan_device.map_device_memory(memory, size).map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            e
        })?;

        Ok(Self {
            device: device.clone(),
            vulkan_device: Some(Arc::clone(vulkan_device)),
            buffer,
            device_memory: memory,
            mapped_ptr,
            width,
            height,
            bits_per_pixel,
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

    /// Bits per pixel.
    pub fn bits_per_pixel(&self) -> u32 {
        self.bits_per_pixel
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
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            StreamError::GpuError("Cannot export DMA-BUF: no VulkanDevice stored".into())
        })?;

        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(self.device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(vk_dev.instance(), vk_dev.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        Ok(fd)
    }

    /// Import a buffer from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<VulkanDevice>,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        bits_per_pixel: u32,
        format: PixelFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bits_per_pixel as vk::DeviceSize)
            / 8;
        let effective_size = if allocation_size > 0 {
            allocation_size
        } else {
            size
        };

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let buffer_info = vk::BufferCreateInfo::default()
            .size(effective_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to create buffer for DMA-BUF import: {e}"))
        })?;

        let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let alloc_size = effective_size.max(mem_requirements.size);

        let memory = vulkan_device
            .import_dma_buf_memory(
                fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .map_err(|e| {
                unsafe { device.destroy_buffer(buffer, None) };
                e
            })?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("Failed to bind imported memory: {e}"))
        })?;

        let mapped_ptr = vulkan_device.map_device_memory(memory, effective_size).map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            e
        })?;

        Ok(Self {
            device: device.clone(),
            vulkan_device: Some(Arc::clone(vulkan_device)),
            buffer,
            device_memory: memory,
            mapped_ptr,
            width,
            height,
            bits_per_pixel,
            format,
            size: effective_size,
        })
    }
}

impl Drop for VulkanPixelBuffer {
    fn drop(&mut self) {
        unsafe { self.device.destroy_buffer(self.buffer, None) };

        if let Some(vk_dev) = &self.vulkan_device {
            vk_dev.unmap_device_memory(self.device_memory);
            vk_dev.free_device_memory(self.device_memory);
        } else {
            unsafe {
                self.device.unmap_memory(self.device_memory);
                self.device.free_memory(self.device_memory, None);
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
    fn test_pool_buffer_creation_1920x1080_bgra32() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer creation failed");

        assert_eq!(buf.width(), 1920);
        assert_eq!(buf.height(), 1080);
        assert_eq!(buf.bits_per_pixel(), 32);
        assert_eq!(buf.size(), 1920 * 1080 * 4);
        assert!(!buf.mapped_ptr().is_null());
        assert_ne!(buf.buffer(), vk::Buffer::null());

        println!(
            "Pool buffer created: {}x{}x{} bpp = {} bytes",
            buf.width(),
            buf.height(),
            buf.bits_per_pixel(),
            buf.size()
        );
    }

    #[test]
    fn test_buffer_write_and_readback() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = VulkanPixelBuffer::new(&device, 64, 64, 32, PixelFormat::Bgra32)
            .expect("buffer creation failed");

        let size = buf.size() as usize;
        let ptr = buf.mapped_ptr();

        // Write a repeating BGRA pattern
        let pattern: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
        unsafe {
            for i in (0..size).step_by(4) {
                std::ptr::copy_nonoverlapping(pattern.as_ptr(), ptr.add(i), 4);
            }
        }

        // Read back and verify
        unsafe {
            for i in (0..size).step_by(4) {
                let b = std::ptr::read(ptr.add(i));
                let g = std::ptr::read(ptr.add(i + 1));
                let r = std::ptr::read(ptr.add(i + 2));
                let a = std::ptr::read(ptr.add(i + 3));
                assert_eq!([b, g, r, a], pattern, "mismatch at byte offset {i}");
            }
        }

        println!("Write/readback verified for {} bytes", size);
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

        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer creation failed");

        let fd = buf.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

        println!("DMA-BUF exported: fd={fd}");
        // fd ownership is caller's — close it
        unsafe { libc::close(fd) };
    }

    #[test]
    fn test_multiple_buffers_coexist() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let before = device.live_allocation_count();

        let b0 = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer 0 failed");
        let b1 = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer 1 failed");
        let b2 = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer 2 failed");
        let b3 = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer 3 failed");

        assert_eq!(device.live_allocation_count(), before + 4);
        assert_ne!(b0.buffer(), vk::Buffer::null());
        assert_ne!(b1.buffer(), vk::Buffer::null());
        assert_ne!(b2.buffer(), vk::Buffer::null());
        assert_ne!(b3.buffer(), vk::Buffer::null());

        println!(
            "4 buffers coexist, allocations: {}",
            device.live_allocation_count()
        );

        drop(b0);
        drop(b1);
        drop(b2);
        drop(b3);

        assert_eq!(device.live_allocation_count(), before);
        println!("All dropped, allocations back to {}", before);
    }

    #[test]
    fn test_drop_frees_and_unmaps() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let before = device.live_allocation_count();
        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 32, PixelFormat::Bgra32)
            .expect("buffer creation failed");
        assert_eq!(device.live_allocation_count(), before + 1);

        drop(buf);
        assert_eq!(device.live_allocation_count(), before);

        println!("Buffer drop freed memory: allocations back to {}", before);
    }

    #[test]
    fn test_dma_buf_import_round_trip() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // Create source buffer and write a known pattern
        let width = 64u32;
        let height = 64u32;
        let bpp = 32u32;
        let src = VulkanPixelBuffer::new(&device, width, height, bpp, PixelFormat::Bgra32)
            .expect("source buffer creation failed");

        let size = src.size() as usize;
        let pattern: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
        unsafe {
            for i in (0..size).step_by(4) {
                std::ptr::copy_nonoverlapping(pattern.as_ptr(), src.mapped_ptr().add(i), 4);
            }
        }

        // Export DMA-BUF fd
        let fd = src.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0);

        // Import into a new buffer from the DMA-BUF fd
        let imported = VulkanPixelBuffer::from_dma_buf_fd(
            &device,
            fd,
            width,
            height,
            bpp,
            PixelFormat::Bgra32,
            src.size(),
        )
        .expect("DMA-BUF import failed");

        // Verify imported buffer has the same data
        unsafe {
            for i in (0..size).step_by(4) {
                let b = std::ptr::read(imported.mapped_ptr().add(i));
                let g = std::ptr::read(imported.mapped_ptr().add(i + 1));
                let r = std::ptr::read(imported.mapped_ptr().add(i + 2));
                let a = std::ptr::read(imported.mapped_ptr().add(i + 3));
                assert_eq!(
                    [b, g, r, a], pattern,
                    "imported data mismatch at byte offset {i}"
                );
            }
        }

        println!(
            "DMA-BUF round-trip verified: {} bytes, fd={fd}",
            size
        );
    }
}
