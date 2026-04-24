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

/// One extra plane of a multi-plane DMA-BUF import (planes 1..N on the
/// Linux side). Plane 0 lives in [`VulkanPixelBuffer::buffer`] /
/// `imported_memory` / `mapped_ptr` for back-compat with the
/// single-plane accessors; additional planes are stored here so an
/// exported multi-plane format (e.g. NV12 with disjoint Y and UV
/// allocations) survives the round-trip.
#[cfg(target_os = "linux")]
struct VulkanImportedPlane {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
}

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
    /// Persistently mapped CPU pointer — plane 0 for multi-plane imports.
    mapped_ptr: *mut u8,
    /// Planes 1..N for multi-plane DMA-BUF imports. Empty for single-plane
    /// imports and for VMA-allocated buffers.
    #[cfg(target_os = "linux")]
    extra_imported_planes: Vec<VulkanImportedPlane>,
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
            #[cfg(target_os = "linux")]
            extra_imported_planes: Vec::new(),
            width,
            height,
            bytes_per_pixel,
            format,
            size,
        })
    }

    /// Persistently mapped pointer for CPU access — plane 0 for
    /// multi-plane imports. Use [`Self::plane_mapped_ptr`] for any plane
    /// index.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Number of DMA-BUF planes backing this pixel buffer. Always `>= 1`:
    /// `1` for VMA-allocated buffers and single-plane imports, `N` for
    /// multi-plane DMA-BUF imports.
    pub fn plane_count(&self) -> u32 {
        #[cfg(target_os = "linux")]
        {
            1 + self.extra_imported_planes.len() as u32
        }
        #[cfg(not(target_os = "linux"))]
        {
            1
        }
    }

    /// Mapped base address of a specific plane, or null if `plane_index`
    /// is out of range. Plane 0 returns [`Self::mapped_ptr`]; planes 1..N
    /// return the matching entry from the multi-plane import set.
    pub fn plane_mapped_ptr(&self, plane_index: u32) -> *mut u8 {
        if plane_index == 0 {
            return self.mapped_ptr;
        }
        #[cfg(target_os = "linux")]
        {
            self.extra_imported_planes
                .get(plane_index as usize - 1)
                .map(|p| p.mapped_ptr)
                .unwrap_or(std::ptr::null_mut())
        }
        #[cfg(not(target_os = "linux"))]
        {
            std::ptr::null_mut()
        }
    }

    /// Byte size of a specific plane, or `0` if `plane_index` is out of
    /// range.
    pub fn plane_size(&self, plane_index: u32) -> vk::DeviceSize {
        if plane_index == 0 {
            return self.size;
        }
        #[cfg(target_os = "linux")]
        {
            self.extra_imported_planes
                .get(plane_index as usize - 1)
                .map(|p| p.size)
                .unwrap_or(0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
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

    /// Import a buffer from a single-plane DMA-BUF file descriptor.
    ///
    /// Thin wrapper over [`Self::from_dma_buf_fds`] for back-compat with
    /// existing single-plane callers — new code should prefer the
    /// multi-plane signature even when there is only one plane.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<VulkanDevice>,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        Self::from_dma_buf_fds(
            vulkan_device,
            &[fd],
            &[allocation_size],
            width,
            height,
            bytes_per_pixel,
            format,
        )
    }

    /// Import one `vk::Buffer` + `vk::DeviceMemory` pair per plane from
    /// the given DMA-BUF fds. `plane_sizes[i]` must be the allocation
    /// size of plane `i` (0 falls back to `width*height*bytes_per_pixel`
    /// on plane 0, required for every other plane).
    ///
    /// Partial-failure semantics: if plane N fails to import, every
    /// plane 0..N that already succeeded is torn down (buffer destroyed,
    /// memory unmapped + freed) before the error is returned. The
    /// caller retains ownership of the fds — each fd is consumed by
    /// `vkAllocateMemory` only on success.
    pub fn from_dma_buf_fds(
        vulkan_device: &Arc<VulkanDevice>,
        fds: &[std::os::unix::io::RawFd],
        plane_sizes: &[vk::DeviceSize],
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        if fds.is_empty() {
            return Err(StreamError::Configuration(
                "DMA-BUF import: fd vec must be non-empty".into(),
            ));
        }
        if fds.len() != plane_sizes.len() {
            return Err(StreamError::Configuration(format!(
                "DMA-BUF import: plane_sizes length ({}) must match fds length ({})",
                plane_sizes.len(),
                fds.len()
            )));
        }
        if fds.len() > streamlib_broker_client::MAX_DMA_BUF_PLANES {
            return Err(StreamError::Configuration(format!(
                "DMA-BUF import: plane count {} exceeds MAX_DMA_BUF_PLANES ({})",
                fds.len(),
                streamlib_broker_client::MAX_DMA_BUF_PLANES
            )));
        }

        let size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);

        // Import every plane. Stash each successful import in a vec so we
        // can unwind on partial failure.
        let mut imported: Vec<VulkanImportedPlane> = Vec::with_capacity(fds.len());
        for (idx, (&fd, &plane_size)) in fds.iter().zip(plane_sizes.iter()).enumerate() {
            let effective_size = if plane_size > 0 {
                plane_size
            } else if idx == 0 {
                size
            } else {
                // Planes 1..N need an explicit size — we can't derive it
                // from width/height since subsampling and format
                // modifiers vary per plane.
                for plane in imported.into_iter() {
                    teardown_imported_plane(vulkan_device, plane);
                }
                return Err(StreamError::Configuration(format!(
                    "DMA-BUF import: plane {} has size=0 and cannot be derived from width*height",
                    idx
                )));
            };

            match import_single_plane(vulkan_device, fd, effective_size) {
                Ok(plane) => imported.push(plane),
                Err(e) => {
                    for plane in imported.into_iter() {
                        teardown_imported_plane(vulkan_device, plane);
                    }
                    return Err(e);
                }
            }
        }

        // Split plane 0 out into the existing back-compat fields; the
        // rest become `extra_imported_planes`.
        let plane0 = imported.remove(0);
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane0.buffer,
            allocation: None,
            imported_memory: Some(plane0.memory),
            imported_from_dma_buf: true,
            mapped_ptr: plane0.mapped_ptr,
            extra_imported_planes: imported,
            width,
            height,
            bytes_per_pixel,
            format,
            size: plane0.size,
        })
    }
}

/// Create one `VkBuffer` + bind one imported `VkDeviceMemory` + map it.
/// Used by [`VulkanPixelBuffer::from_dma_buf_fds`] once per plane.
#[cfg(target_os = "linux")]
fn import_single_plane(
    vulkan_device: &Arc<VulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
) -> Result<VulkanImportedPlane> {
    let device = vulkan_device.device();

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

    Ok(VulkanImportedPlane {
        buffer,
        memory,
        mapped_ptr: mapped_ptr,
        size: effective_size,
    })
}

/// Partial-unwind helper for [`VulkanPixelBuffer::from_dma_buf_fds`] —
/// tears down one already-imported plane when a later plane fails.
#[cfg(target_os = "linux")]
fn teardown_imported_plane(vulkan_device: &Arc<VulkanDevice>, plane: VulkanImportedPlane) {
    unsafe {
        vulkan_device.device().destroy_buffer(plane.buffer, None);
    }
    vulkan_device.unmap_imported_memory(plane.memory);
    vulkan_device.free_imported_memory(plane.memory);
}

impl Drop for VulkanPixelBuffer {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if self.imported_from_dma_buf {
            // DMA-BUF import path: raw DeviceMemory, not VMA.
            unsafe { self.vulkan_device.device().destroy_buffer(self.buffer, None) };
            if let Some(memory) = self.imported_memory.take() {
                self.vulkan_device.unmap_imported_memory(memory);
                self.vulkan_device.free_imported_memory(memory);
            }
            // Tear down every extra plane — each owns its own buffer +
            // imported memory + mapping.
            for plane in self.extra_imported_planes.drain(..) {
                teardown_imported_plane(&self.vulkan_device, plane);
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

    /// Multi-plane `from_dma_buf_fds` round-trip: import two independently
    /// allocated + pattern-written DMA-BUFs as the two planes of a single
    /// `VulkanPixelBuffer`, confirm `plane_count()` reports 2, and each
    /// plane's bytes survive intact. Mirrors the symmetry the polyglot
    /// Python and Deno shims provide via `*_gpu_surface_plane_{count,size,mmap,base_address}`.
    #[test]
    fn test_dma_buf_import_multi_plane_round_trip() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let width = 64u32;
        let height = 4u32;
        let bpp = 4u32;
        let plane_size = (width * height * bpp) as vk::DeviceSize;

        // Two independent source buffers, each seeded with a distinct
        // byte pattern so a cross-plane swap would be visible in the
        // readback.
        let src0 = VulkanPixelBuffer::new(&device, width, height, bpp, PixelFormat::Bgra32)
            .expect("source plane 0 creation failed");
        let src1 = VulkanPixelBuffer::new(&device, width, height, bpp, PixelFormat::Bgra32)
            .expect("source plane 1 creation failed");
        let pattern0: [u8; 4] = [0xA0, 0xA1, 0xA2, 0xA3];
        let pattern1: [u8; 4] = [0xB0, 0xB1, 0xB2, 0xB3];
        unsafe {
            for i in (0..plane_size as usize).step_by(4) {
                std::ptr::copy_nonoverlapping(pattern0.as_ptr(), src0.mapped_ptr().add(i), 4);
                std::ptr::copy_nonoverlapping(pattern1.as_ptr(), src1.mapped_ptr().add(i), 4);
            }
        }

        let fd0 = src0.export_dma_buf_fd().expect("plane 0 export failed");
        let fd1 = src1.export_dma_buf_fd().expect("plane 1 export failed");

        // Import both as planes of a single pixel buffer.
        let imported = VulkanPixelBuffer::from_dma_buf_fds(
            &device,
            &[fd0, fd1],
            &[plane_size, plane_size],
            width,
            height,
            bpp,
            PixelFormat::Bgra32,
        )
        .expect("multi-plane DMA-BUF import failed");

        assert_eq!(imported.plane_count(), 2, "plane_count must report 2");
        assert_eq!(imported.plane_size(0), plane_size);
        assert_eq!(imported.plane_size(1), plane_size);
        assert_eq!(imported.plane_size(2), 0, "out-of-range plane size must be 0");

        let p0 = imported.plane_mapped_ptr(0);
        let p1 = imported.plane_mapped_ptr(1);
        assert!(!p0.is_null() && !p1.is_null());
        assert_eq!(
            imported.plane_mapped_ptr(2),
            std::ptr::null_mut(),
            "out-of-range plane ptr must be null"
        );

        // Content check: plane 0 matches pattern0, plane 1 matches
        // pattern1. Byte-exact, no cross-contamination.
        unsafe {
            for i in (0..plane_size as usize).step_by(4) {
                let b0 = [
                    *p0.add(i),
                    *p0.add(i + 1),
                    *p0.add(i + 2),
                    *p0.add(i + 3),
                ];
                let b1 = [
                    *p1.add(i),
                    *p1.add(i + 1),
                    *p1.add(i + 2),
                    *p1.add(i + 3),
                ];
                assert_eq!(b0, pattern0, "plane 0 mismatch at offset {i}");
                assert_eq!(b1, pattern1, "plane 1 mismatch at offset {i}");
            }
        }

        println!(
            "Multi-plane DMA-BUF import round-trip verified: 2 planes × {} bytes",
            plane_size
        );
    }

    /// Oversize vec rejection: we refuse to import a pixel buffer with
    /// more planes than the broker's `MAX_DMA_BUF_PLANES` cap (4 today).
    /// Covers the Rust half of the consistency the wire helpers already
    /// enforce on sends/receives.
    #[test]
    fn test_dma_buf_import_rejects_oversize_plane_vec() {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // fds/sizes vecs with one more entry than the cap. Negative fds
        // are fine — we expect the length check to fire before any
        // syscall touches them.
        let fds: Vec<std::os::unix::io::RawFd> =
            (0..=streamlib_broker_client::MAX_DMA_BUF_PLANES as i32)
                .map(|_| -1i32)
                .collect();
        let sizes: Vec<vk::DeviceSize> = vec![1024; fds.len()];

        let result = VulkanPixelBuffer::from_dma_buf_fds(
            &device,
            &fds,
            &sizes,
            64,
            4,
            4,
            PixelFormat::Bgra32,
        );
        match result {
            Ok(_) => panic!("oversize plane vec must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("MAX_DMA_BUF_PLANES"),
                "error should name the cap, got: {e}"
            ),
        }
    }
}
