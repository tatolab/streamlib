// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side Vulkan pixel buffer — imports a host-allocated
//! HOST_VISIBLE DMA-BUF as a `VkBuffer` and exposes a CPU-mapped
//! pointer for staging upload / readback.
//!
//! Mirrors [`crate::ConsumerVulkanTexture`] for buffer handles.
//! Single-plane and multi-plane import constructors only — no
//! allocation, no DMA-BUF export.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::{ConsumerRhiError, ConsumerVulkanDevice, PixelFormat, Result, VulkanPixelBufferLike};

/// One imported plane: buffer + memory + mapped pointer + size.
struct ConsumerImportedPlane {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
}

/// Consumer-side CPU-visible imported staging buffer. See module docs.
pub struct ConsumerVulkanPixelBuffer {
    vulkan_device: Arc<ConsumerVulkanDevice>,
    /// Plane 0's `VkBuffer`. Single-plane imports use only this; multi-
    /// plane imports keep planes 1..N in [`Self::extra_imported_planes`].
    buffer: vk::Buffer,
    imported_memory: vk::DeviceMemory,
    /// Persistently mapped CPU pointer for plane 0.
    mapped_ptr: *mut u8,
    extra_imported_planes: Vec<ConsumerImportedPlane>,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    /// Size of plane 0 in bytes.
    size: vk::DeviceSize,
}

impl ConsumerVulkanPixelBuffer {
    /// Import a single-plane DMA-BUF as a HOST_VISIBLE staging buffer.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
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

    /// Import an OPAQUE_FD as a HOST_VISIBLE staging buffer.
    ///
    /// Pairs with the host's
    /// [`crate::HostVulkanPixelBuffer::new_opaque_fd_export`] +
    /// `export_opaque_fd_memory`. This is the constructor #589 / #590
    /// CUDA cdylibs use after looking up a surface registered with
    /// `handle_type="opaque_fd"` on the surface-share wire — the resulting
    /// `VkBuffer`'s memory is also what `cudaImportExternalMemory` reaches
    /// for via the same FD.
    ///
    /// Single-FD only: OPAQUE_FD has no multi-plane semantics (CUDA imports
    /// flat memory; multi-plane DMA-BUFs go through [`Self::from_dma_buf_fds`]).
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// `fd`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, allocation_size, width, height))]
    pub fn from_opaque_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let derived_size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);
        let effective_size = if allocation_size > 0 {
            allocation_size
        } else if width > 0 && height > 0 && bytes_per_pixel > 0 {
            derived_size
        } else {
            return Err(ConsumerRhiError::Configuration(
                "ConsumerVulkanPixelBuffer::from_opaque_fd: allocation_size=0 \
                 and width*height*bpp cannot derive a size"
                    .into(),
            ));
        };

        let plane = import_single_plane_with_handle_type(
            vulkan_device,
            fd,
            effective_size,
            ImportHandleType::OpaqueFd,
        )?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            imported_memory: plane.memory,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            width,
            height,
            bytes_per_pixel,
            format,
            size: plane.size,
        })
    }

    /// Import N planes from N DMA-BUF FDs — each gets its own
    /// `VkBuffer` + imported `VkDeviceMemory` + mapping.
    ///
    /// Partial-failure semantics: every plane that succeeded is torn
    /// down before the error is returned. fd ownership transfers to
    /// the Vulkan driver on success per plane.
    pub fn from_dma_buf_fds(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fds: &[std::os::unix::io::RawFd],
        plane_sizes: &[vk::DeviceSize],
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        if fds.is_empty() {
            return Err(ConsumerRhiError::Configuration(
                "ConsumerVulkanPixelBuffer: fd vec must be non-empty".into(),
            ));
        }
        if fds.len() != plane_sizes.len() {
            return Err(ConsumerRhiError::Configuration(format!(
                "ConsumerVulkanPixelBuffer: plane_sizes length ({}) must match fds length ({})",
                plane_sizes.len(),
                fds.len()
            )));
        }
        if fds.len() > streamlib_surface_client::MAX_DMA_BUF_PLANES {
            return Err(ConsumerRhiError::Configuration(format!(
                "ConsumerVulkanPixelBuffer: plane count {} exceeds MAX_DMA_BUF_PLANES ({})",
                fds.len(),
                streamlib_surface_client::MAX_DMA_BUF_PLANES
            )));
        }

        let derived_size = (width as vk::DeviceSize)
            * (height as vk::DeviceSize)
            * (bytes_per_pixel as vk::DeviceSize);

        let mut imported: Vec<ConsumerImportedPlane> = Vec::with_capacity(fds.len());
        for (idx, (&fd, &plane_size)) in fds.iter().zip(plane_sizes.iter()).enumerate() {
            let effective_size = if plane_size > 0 {
                plane_size
            } else if idx == 0 {
                derived_size
            } else {
                for plane in imported.into_iter() {
                    teardown_plane(vulkan_device, plane);
                }
                return Err(ConsumerRhiError::Configuration(format!(
                    "ConsumerVulkanPixelBuffer: plane {} size=0 cannot be derived from width*height",
                    idx
                )));
            };

            match import_single_plane(vulkan_device, fd, effective_size) {
                Ok(plane) => imported.push(plane),
                Err(e) => {
                    for plane in imported.into_iter() {
                        teardown_plane(vulkan_device, plane);
                    }
                    return Err(e);
                }
            }
        }

        let plane0 = imported.remove(0);
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane0.buffer,
            imported_memory: plane0.memory,
            mapped_ptr: plane0.mapped_ptr,
            extra_imported_planes: imported,
            width,
            height,
            bytes_per_pixel,
            format,
            size: plane0.size,
        })
    }

    /// Persistently mapped CPU pointer for plane 0. Use
    /// [`Self::plane_mapped_ptr`] for any plane.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Number of planes — `1` for single-plane imports, `N` for multi-
    /// plane.
    pub fn plane_count(&self) -> u32 {
        1 + self.extra_imported_planes.len() as u32
    }

    /// Mapped CPU pointer for plane `plane_index`, or null if out of
    /// range.
    pub fn plane_mapped_ptr(&self, plane_index: u32) -> *mut u8 {
        if plane_index == 0 {
            return self.mapped_ptr;
        }
        self.extra_imported_planes
            .get(plane_index as usize - 1)
            .map(|p| p.mapped_ptr)
            .unwrap_or(std::ptr::null_mut())
    }

    /// Byte size of plane `plane_index`, or `0` if out of range.
    pub fn plane_size(&self, plane_index: u32) -> vk::DeviceSize {
        if plane_index == 0 {
            return self.size;
        }
        self.extra_imported_planes
            .get(plane_index as usize - 1)
            .map(|p| p.size)
            .unwrap_or(0)
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

    /// Plane 0 size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Underlying `VkBuffer` for plane 0.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }
}

/// Which `vkImportMemoryFdInfoKHR.handleType` to chain through when
/// importing a plane.
#[derive(Copy, Clone, Debug)]
enum ImportHandleType {
    DmaBuf,
    OpaqueFd,
}

fn import_single_plane(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
) -> Result<ConsumerImportedPlane> {
    import_single_plane_with_handle_type(
        vulkan_device,
        fd,
        effective_size,
        ImportHandleType::DmaBuf,
    )
}

fn import_single_plane_with_handle_type(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
    handle_type: ImportHandleType,
) -> Result<ConsumerImportedPlane> {
    let device = vulkan_device.device();

    let vk_handle_type = match handle_type {
        ImportHandleType::DmaBuf => vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        ImportHandleType::OpaqueFd => vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
    };

    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk_handle_type)
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
        ConsumerRhiError::Gpu(format!(
            "ConsumerVulkanPixelBuffer: create_buffer failed: {e}"
        ))
    })?;

    let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let alloc_size = effective_size.max(mem_requirements.size);

    let memory = match handle_type {
        ImportHandleType::DmaBuf => vulkan_device.import_dma_buf_memory(
            fd,
            alloc_size,
            mem_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ),
        ImportHandleType::OpaqueFd => vulkan_device.import_opaque_fd_memory(
            fd,
            alloc_size,
            mem_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ),
    }
    .map_err(|e| {
        unsafe { device.destroy_buffer(buffer, None) };
        e
    })?;

    unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
        vulkan_device.free_imported_memory(memory);
        unsafe { device.destroy_buffer(buffer, None) };
        ConsumerRhiError::Gpu(format!(
            "ConsumerVulkanPixelBuffer: bind_buffer_memory failed: {e}"
        ))
    })?;

    let mapped_ptr = vulkan_device
        .map_imported_memory(memory, effective_size)
        .map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            e
        })?;

    Ok(ConsumerImportedPlane {
        buffer,
        memory,
        mapped_ptr,
        size: effective_size,
    })
}

fn teardown_plane(vulkan_device: &Arc<ConsumerVulkanDevice>, plane: ConsumerImportedPlane) {
    unsafe { vulkan_device.device().destroy_buffer(plane.buffer, None) };
    vulkan_device.unmap_imported_memory(plane.memory);
    vulkan_device.free_imported_memory(plane.memory);
}

impl Drop for ConsumerVulkanPixelBuffer {
    fn drop(&mut self) {
        unsafe {
            self.vulkan_device.device().destroy_buffer(self.buffer, None);
        }
        self.vulkan_device.unmap_imported_memory(self.imported_memory);
        self.vulkan_device.free_imported_memory(self.imported_memory);
        for plane in self.extra_imported_planes.drain(..) {
            teardown_plane(&self.vulkan_device, plane);
        }
    }
}

unsafe impl Send for ConsumerVulkanPixelBuffer {}
unsafe impl Sync for ConsumerVulkanPixelBuffer {}

impl VulkanPixelBufferLike for ConsumerVulkanPixelBuffer {
    fn buffer(&self) -> vk::Buffer {
        ConsumerVulkanPixelBuffer::buffer(self)
    }
    fn mapped_ptr(&self) -> *mut u8 {
        ConsumerVulkanPixelBuffer::mapped_ptr(self)
    }
    fn size(&self) -> vk::DeviceSize {
        ConsumerVulkanPixelBuffer::size(self)
    }
    fn width(&self) -> u32 {
        ConsumerVulkanPixelBuffer::width(self)
    }
    fn height(&self) -> u32 {
        ConsumerVulkanPixelBuffer::height(self)
    }
    fn bytes_per_pixel(&self) -> u32 {
        ConsumerVulkanPixelBuffer::bytes_per_pixel(self)
    }
}
