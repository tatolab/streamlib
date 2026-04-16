// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

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
    /// VulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Arc<VulkanDevice>,
    buffer: vk::Buffer,
    /// VMA allocation (HOST_VISIBLE | DEDICATED_MEMORY for DMA-BUF export).
    allocation: Option<vma::Allocation>,
    /// Imported device memory for DMA-BUF import path (VMA cannot import external memory).
    #[cfg(target_os = "linux")]
    imported_memory: Option<vk::DeviceMemory>,
    /// Whether this buffer was imported from a DMA-BUF fd.
    #[cfg(target_os = "linux")]
    imported_from_dma_buf: bool,
    /// Persistently mapped CPU pointer.
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    size: vk::DeviceSize,
}

impl VulkanPixelBuffer {
    /// Create a new DMA-BUF exportable CPU-visible staging buffer via the
    /// device's dedicated VMA export pool.
    ///
    /// The export pool isolates DMA-BUF allocations from the default VMA pool,
    /// avoiding NVIDIA driver failures where global export configuration causes
    /// OOM after swapchain creation.
    pub fn new(
        vulkan_device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);

        // Declare DMA-BUF handle type at buffer creation — required by Vulkan spec
        // when memory will be allocated with VkExportMemoryAllocateInfo::handleTypes.
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        // DEDICATED_MEMORY: required for DMA-BUF export per VMA docs.
        // MAPPED: persistent CPU mapping.
        // HOST_ACCESS_SEQUENTIAL_WRITE: hints VMA to pick host-visible memory type.
        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();

        // Prefer the DMA-BUF buffer pool; fall back to default allocator if pool unavailable.
        let (buffer, allocation) = {
            #[cfg(target_os = "linux")]
            let result = if let Some(pool) = vulkan_device.dma_buf_buffer_pool() {
                unsafe { pool.create_buffer(buffer_info, &alloc_opts) }
            } else {
                unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
            };
            #[cfg(not(target_os = "linux"))]
            let result = unsafe { allocator.create_buffer(buffer_info, &alloc_opts) };
            result.map_err(|e| {
                StreamError::GpuError(format!("Failed to create exportable buffer: {e}"))
            })?
        };

        // Retrieve the persistently mapped pointer from VMA
        let alloc_info = allocator.get_allocation_info(allocation);
        let mapped_ptr = alloc_info.pMappedData.cast::<u8>();

        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(buffer, allocation) };
            return Err(StreamError::GpuError(
                "VMA staging buffer mapped pointer is null — expected persistent mapping".into(),
            ));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
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
        // Determine which DeviceMemory to export from
        let device_memory = if let Some(allocation) = &self.allocation {
            let alloc_info = self.vulkan_device.allocator().get_allocation_info(*allocation);
            alloc_info.deviceMemory
        } else if let Some(memory) = self.imported_memory {
            memory
        } else {
            return Err(StreamError::GpuError(
                "Cannot export DMA-BUF: buffer has no allocation or imported memory".into(),
            ));
        };

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { self.vulkan_device.device().get_memory_fd_khr(&get_fd_info) }
            .map(|r| r)
            .map_err(|e| StreamError::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        Ok(fd)
    }

    /// Import a buffer from a DMA-BUF file descriptor.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<VulkanDevice>,
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
        let effective_size = if allocation_size > 0 { allocation_size } else { size };

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(effective_size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info)
            .build();

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }
            .map(|r| r)
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create buffer for DMA-BUF import: {e}"))
            })?;

        let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let alloc_size = effective_size.max(mem_requirements.size);

        // VMA cannot import external memory — use raw import path in the RHI
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

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }
            .map(|_| ())
            .map_err(|e| {
                vulkan_device.free_imported_memory(memory);
                unsafe { device.destroy_buffer(buffer, None) };
                StreamError::GpuError(format!("Failed to bind imported memory: {e}"))
            })?;

        let mapped_ptr = vulkan_device
            .map_imported_memory(memory, effective_size)
            .map_err(|e| {
                vulkan_device.free_imported_memory(memory);
                unsafe { device.destroy_buffer(buffer, None) };
                e
            })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: None,
            imported_memory: Some(memory),
            imported_from_dma_buf: true,
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
        #[cfg(target_os = "linux")]
        if self.imported_from_dma_buf {
            // DMA-BUF import path: raw DeviceMemory, not VMA
            unsafe { self.vulkan_device.device().destroy_buffer(self.buffer, None) };
            if let Some(memory) = self.imported_memory.take() {
                self.vulkan_device.unmap_imported_memory(memory);
                self.vulkan_device.free_imported_memory(memory);
            }
            return;
        }

        // VMA path: destroy_buffer frees both the buffer and the allocation
        if let Some(allocation) = self.allocation.take() {
            unsafe { self.vulkan_device.allocator().destroy_buffer(self.buffer, allocation) };
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

        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer creation failed");

        assert_eq!(buf.width(), 1920);
        assert_eq!(buf.height(), 1080);
        assert_eq!(buf.bytes_per_pixel(), 4);
        assert_eq!(buf.size(), 1920 * 1080 * 4);
        assert!(!buf.mapped_ptr().is_null());
        assert_ne!(buf.buffer(), vk::Buffer::null());

        println!(
            "Pool buffer created: {}x{}x{} = {} bytes",
            buf.width(),
            buf.height(),
            buf.bytes_per_pixel(),
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

        let buf = VulkanPixelBuffer::new(&device, 64, 64, 4, PixelFormat::Bgra32)
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

        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
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

        let b0 = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer 0 failed");
        let b1 = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer 1 failed");
        let b2 = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer 2 failed");
        let b3 = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer 3 failed");

        assert_ne!(b0.buffer(), vk::Buffer::null());
        assert_ne!(b1.buffer(), vk::Buffer::null());
        assert_ne!(b2.buffer(), vk::Buffer::null());
        assert_ne!(b3.buffer(), vk::Buffer::null());

        println!("4 buffers coexist");

        drop(b0);
        drop(b1);
        drop(b2);
        drop(b3);

        println!("All dropped successfully");
    }

    #[test]
    fn test_drop_frees_without_panic() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = VulkanPixelBuffer::new(&device, 1920, 1080, 4, PixelFormat::Bgra32)
            .expect("buffer creation failed");
        drop(buf);

        println!("Buffer drop completed without panic");
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
        let bpp = 4u32;
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
                    [b, g, r, a],
                    pattern,
                    "imported data mismatch at byte offset {i}"
                );
            }
        }

        println!("DMA-BUF round-trip verified: {} bytes, fd={fd}", size);
    }
}
