// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod host_pointer_import;
mod host_visible_buffer_creation_and_accessors;
#[cfg(target_os = "linux")]
mod linux_opaque_fd_and_dma_buf_export_and_import;
#[cfg(target_os = "macos")]
mod macos_iosurface_backing;

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::vulkan::rhi::HostVulkanDevice;
#[cfg(target_os = "linux")]
use linux_opaque_fd_and_dma_buf_export_and_import::teardown_imported_plane;

/// Process-global HostVulkanDevice reference for external-handle import —
/// DMA-BUF on Linux, IOSurface on macOS.
///
/// Set once, when [`GpuDevice::new()`] creates the process's one device. The import trait
/// (`RhiPixelBufferImport::from_external_handle`) is a static method with no
/// device parameter, so this global bridges that gap.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) static VULKAN_DEVICE_FOR_IMPORT: std::sync::OnceLock<Arc<HostVulkanDevice>> =
    std::sync::OnceLock::new();

/// One extra plane of a multi-plane DMA-BUF import (planes 1..N on the
/// Linux side). Plane 0 lives in [`HostVulkanBuffer::buffer`] /
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

pub struct HostVulkanBuffer {
    /// HostVulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Arc<HostVulkanDevice>,
    buffer: vk::Buffer,
    /// VMA allocation (HOST_VISIBLE | DEDICATED_MEMORY for DMA-BUF export).
    allocation: Option<vma::Allocation>,
    /// Imported device memory for the DMA-BUF and host-pointer import paths
    /// (VMA cannot import external memory).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    imported_memory: Option<vk::DeviceMemory>,
    /// Where this buffer's memory came from: what it exports as and how it
    /// is freed.
    memory_origin: HostVulkanBufferMemoryOrigin,
    /// Whatever owns the pages a host-pointer import aliases, released only
    /// after the importing `VkDeviceMemory` is freed. `None` when the caller
    /// keeps the range alive itself, and on every other allocation path.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    imported_host_range_owner: Option<Box<dyn Send + Sync>>,
    /// Persistently mapped CPU pointer — plane 0 for multi-plane imports.
    /// `null` for DEVICE_LOCAL allocations.
    mapped_ptr: *mut u8,
    /// Planes 1..N for multi-plane DMA-BUF imports. Empty for single-plane
    /// imports and for VMA-allocated buffers.
    #[cfg(target_os = "linux")]
    extra_imported_planes: Vec<VulkanImportedPlane>,
    /// The IOSurface whose pages this buffer's memory is, released only after
    /// that memory is freed. `None` on every other allocation path.
    #[cfg(target_os = "macos")]
    backing_iosurface: Option<crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads>,
    /// Plane 0 size in bytes.
    size: vk::DeviceSize,
}

/// Where a [`HostVulkanBuffer`]'s memory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostVulkanBufferMemoryOrigin {
    /// A VMA allocation; DMA-BUF exportable on a device that exports DMA-BUF.
    VmaAllocation,
    /// A VMA allocation from an OPAQUE_FD export pool; exports only as OPAQUE_FD.
    #[cfg(target_os = "linux")]
    VmaAllocationExportableAsOpaqueFd,
    /// Imported from DMA-BUF fds, one `VkDeviceMemory` per plane.
    #[cfg(target_os = "linux")]
    ImportedFromDmaBuf,
    /// Imported from an OPAQUE_FD at the exporter's stated memory type index.
    #[cfg(target_os = "linux")]
    ImportedFromOpaqueFd,
    /// A caller-owned host range imported through `VK_EXT_external_memory_host`.
    /// Never mapped by the driver: `mapped_ptr` is the caller's own pointer,
    /// and teardown frees the `VkDeviceMemory` without unmapping.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ImportedFromHostPointer,
}

/// How the host is going to touch a formatless HOST_VISIBLE buffer, and
/// whether it must be DMA-BUF exportable — the two axes the shared
/// constructor spine varies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostVisibleAllocationIntent {
    /// Producer-write memory from the device's export pool: the CPU fills it
    /// once per frame and another process may import it.
    SequentialWriteExportable,
    /// CPU-read memory the GPU writes — private staging on a HOST_CACHED
    /// type wherever the device has one, never write-combined, never
    /// exported.
    #[cfg(target_os = "linux")]
    RandomReadPrivate,
}

impl HostVisibleAllocationIntent {
    /// Whether the allocation declares a DMA-BUF export handle type on
    /// `vulkan_device`.
    ///
    /// An exportable intent on a device that exports no DMA-BUF degrades to
    /// a local allocation rather than failing.
    fn declares_dma_buf_export_on_device(self, vulkan_device: &HostVulkanDevice) -> bool {
        vulkan_device.supports_dma_buf_export() && matches!(self, Self::SequentialWriteExportable)
    }

    fn vma_allocation_create_flags(self) -> vma::AllocationCreateFlags {
        match self {
            Self::SequentialWriteExportable => {
                vma::AllocationCreateFlags::DEDICATED_MEMORY
                    | vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
            }
            #[cfg(target_os = "linux")]
            Self::RandomReadPrivate => {
                vma::AllocationCreateFlags::MAPPED | vma::AllocationCreateFlags::HOST_ACCESS_RANDOM
            }
        }
    }

    fn vma_preferred_memory_property_flags(self) -> vk::MemoryPropertyFlags {
        match self {
            Self::SequentialWriteExportable => vk::MemoryPropertyFlags::empty(),
            #[cfg(target_os = "linux")]
            Self::RandomReadPrivate => vk::MemoryPropertyFlags::HOST_CACHED,
        }
    }
}

impl crate::vulkan::rhi::VulkanRhiBuffer for HostVulkanBuffer {
    fn buffer(&self) -> vk::Buffer {
        HostVulkanBuffer::buffer(self)
    }
    fn mapped_ptr(&self) -> *mut u8 {
        HostVulkanBuffer::mapped_ptr(self)
    }
    fn size(&self) -> vk::DeviceSize {
        HostVulkanBuffer::size(self)
    }
}

impl Drop for HostVulkanBuffer {
    fn drop(&mut self) {
        match self.memory_origin {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            HostVulkanBufferMemoryOrigin::ImportedFromHostPointer => {
                unsafe {
                    self.vulkan_device
                        .device()
                        .destroy_buffer(self.buffer, None)
                };
                if let Some(memory) = self.imported_memory.take() {
                    self.vulkan_device.free_imported_memory(memory);
                }
                // VK_EXT_external_memory_host needs the range valid for as long
                // as the memory importing it exists, so the owner goes last.
                drop(self.imported_host_range_owner.take());
            }
            #[cfg(target_os = "linux")]
            HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf
            | HostVulkanBufferMemoryOrigin::ImportedFromOpaqueFd => {
                unsafe {
                    self.vulkan_device
                        .device()
                        .destroy_buffer(self.buffer, None)
                };
                if let Some(memory) = self.imported_memory.take() {
                    self.vulkan_device.unmap_imported_memory(memory);
                    self.vulkan_device.free_imported_memory(memory);
                }
                for plane in self.extra_imported_planes.drain(..) {
                    teardown_imported_plane(&self.vulkan_device, plane);
                }
            }
            #[cfg(target_os = "linux")]
            HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd => {
                self.destroy_vma_allocated_buffer();
            }
            HostVulkanBufferMemoryOrigin::VmaAllocation => {
                self.destroy_vma_allocated_buffer();
            }
        }
    }
}

impl HostVulkanBuffer {
    /// Free a VMA-allocated buffer: `vmaDestroyBuffer` frees the buffer and
    /// its allocation together.
    fn destroy_vma_allocated_buffer(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            unsafe {
                self.vulkan_device
                    .allocator()
                    .destroy_buffer(self.buffer, allocation)
            };
        }
    }
}

// Safety: Vulkan handles are thread-safe
unsafe impl Send for HostVulkanBuffer {}
unsafe impl Sync for HostVulkanBuffer {}

#[cfg(test)]
mod tests;
