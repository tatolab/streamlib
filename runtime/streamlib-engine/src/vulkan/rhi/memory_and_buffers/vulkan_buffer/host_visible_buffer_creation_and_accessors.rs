// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Creating host-visible buffers, and a buffer's mapped pointers, planes, size and handle.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::device::vulkan_device::{
    VmaFrameAllocationSource, memory_type_index_is_host_cached,
};
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::DeviceMemoryBudgetAdmission;

use super::{HostVisibleAllocationIntent, HostVulkanBuffer, HostVulkanBufferMemoryOrigin};

impl HostVulkanBuffer {
    /// Create a HOST_VISIBLE `VkBuffer` with
    /// `STORAGE_BUFFER | TRANSFER_SRC | TRANSFER_DST` usage, allocated
    /// through the device's DMA-BUF export VMA pool on a device that exports
    /// DMA-BUF and locally on one that does not.
    ///
    /// Thin alias for [`Self::new_storage_buffer_host_visible`] for the
    /// "default DMA-BUF storage buffer" use case. Callers that need
    /// pixel-shape metadata wrap the result in
    /// [`crate::core::rhi::PixelBuffer`]; the primitive itself carries no
    /// pixel semantics.
    ///
    /// The export pool isolates DMA-BUF allocations from the default VMA pool,
    /// avoiding NVIDIA driver failures where global export configuration causes
    /// OOM after swapchain creation.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(size))]
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, size: u64) -> Result<Self> {
        Self::new_with_device_memory_budget_admission(
            vulkan_device,
            size,
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
        )
    }

    /// [`Self::new`], weighed against the device-memory budget as `admission` says.
    pub(crate) fn new_with_device_memory_budget_admission(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
        admission: DeviceMemoryBudgetAdmission,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            HostVisibleAllocationIntent::SequentialWriteExportable,
            admission,
            "HostVulkanBuffer::new",
        )
    }

    /// Internal: allocate a HOST_VISIBLE + HOST_COHERENT mapped buffer with
    /// the given usage flags. Shared spine for every formatless host-visible
    /// buffer constructor on this type; `intent` picks the memory the host
    /// touches it with and whether it is DMA-BUF exportable, `admission`
    /// whether the device-memory budget may refuse it.
    pub(super) fn new_host_visible_with_usage(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
        usage: vk::BufferUsageFlags,
        intent: HostVisibleAllocationIntent,
        admission: DeviceMemoryBudgetAdmission,
        constructor_label: &'static str,
    ) -> Result<Self> {
        if byte_size == 0 {
            return Err(Error::Configuration(format!(
                "{constructor_label}: byte_size must be > 0"
            )));
        }
        if byte_size > u32::MAX as u64 {
            return Err(Error::Configuration(format!(
                "{constructor_label}: byte_size {byte_size} \
                 exceeds 4 GB synthetic-width cap"
            )));
        }
        let size = byte_size as vk::DeviceSize;

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let mut buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let declares_dma_buf_export = intent.declares_dma_buf_export_on_device(vulkan_device);
        if declares_dma_buf_export {
            buffer_info = buffer_info.push_next(&mut external_buffer_info);
        }

        let alloc_opts = vma::AllocationOptions {
            flags: intent.vma_allocation_create_flags(),
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            preferred_flags: intent.vma_preferred_memory_property_flags(),
            ..Default::default()
        };

        #[cfg(target_os = "linux")]
        let allocation_source = VmaFrameAllocationSource::export_pool_or_the_default_allocator(
            vulkan_device
                .dma_buf_buffer_pool_pinned_to_one_memory_type()
                .filter(|_| declares_dma_buf_export),
        );
        #[cfg(not(target_os = "linux"))]
        let allocation_source = VmaFrameAllocationSource::DefaultAllocator;
        let (buffer, allocation) = vulkan_device
            .create_frame_buffer_within_the_device_memory_budget(
                admission,
                allocation_source,
                &buffer_info,
                &alloc_opts,
                constructor_label,
                |e| Error::GpuError(format!("{constructor_label}: vmaCreateBuffer failed: {e}")),
            )?;

        let allocator = vulkan_device.allocator();
        let alloc_info = allocator.get_allocation_info(allocation);
        let mapped_ptr = alloc_info.pMappedData.cast::<u8>();
        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(buffer, allocation) };
            return Err(Error::GpuError(format!(
                "{constructor_label}: VMA mapped pointer is null — expected persistent mapping"
            )));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory: None,
            memory_origin: HostVulkanBufferMemoryOrigin::VmaAllocation,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_host_range_owner: None,
            mapped_ptr,
            #[cfg(target_os = "linux")]
            extra_imported_planes: Vec::new(),
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
            size,
        })
    }

    /// Create a formatless HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    ///
    /// Sibling of [`Self::new`] for callers that have raw bytes rather than
    /// formatted pixel data (V4L2 MMAP frames, audio→GPU compute, ML upload).
    /// Same memory shape — HOST_VISIBLE | HOST_COHERENT, persistently mapped,
    /// DMA-BUF exportable via the device's `dma_buf_buffer_pool` where it has one —
    /// but `byte_size` is taken flat: no `PixelFormat` interrogation, no
    /// `width * height * bpp` derivation.
    ///
    /// `byte_size` must fit in `u32` (4 GB cap) — SSBOs larger than that
    /// are not a current consumer need; file a follow-up if they become
    /// one.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(byte_size))]
    pub fn new_storage_buffer_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            byte_size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            HostVisibleAllocationIntent::SequentialWriteExportable,
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
            "HostVulkanBuffer::new_storage_buffer_host_visible",
        )
    }

    /// Create a formatless HOST_VISIBLE uniform buffer for small per-draw
    /// shader parameters (UBOs). DMA-BUF exportable (shared
    /// `dma_buf_buffer_pool`) so cross-process consumers can reach it
    /// when needed; usage carries `UNIFORM_BUFFER | TRANSFER_SRC | TRANSFER_DST`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(byte_size))]
    pub fn new_uniform_buffer_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            byte_size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::UNIFORM_BUFFER,
            HostVisibleAllocationIntent::SequentialWriteExportable,
            DeviceMemoryBudgetAdmission::EngineAllocationTheDeviceMemoryBudgetNeverRefuses,
            "HostVulkanBuffer::new_uniform_buffer_host_visible",
        )
    }

    /// Create a formatless HOST_VISIBLE vertex buffer for graphics pipeline
    /// vertex input. Usage carries `VERTEX_BUFFER | TRANSFER_SRC | TRANSFER_DST`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(byte_size))]
    pub fn new_vertex_buffer_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            byte_size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::VERTEX_BUFFER,
            HostVisibleAllocationIntent::SequentialWriteExportable,
            DeviceMemoryBudgetAdmission::EngineAllocationTheDeviceMemoryBudgetNeverRefuses,
            "HostVulkanBuffer::new_vertex_buffer_host_visible",
        )
    }

    /// Create a formatless HOST_VISIBLE index buffer for graphics pipeline
    /// indexed draws. Usage carries `INDEX_BUFFER | TRANSFER_SRC | TRANSFER_DST`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(byte_size))]
    pub fn new_index_buffer_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            byte_size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::INDEX_BUFFER,
            HostVisibleAllocationIntent::SequentialWriteExportable,
            DeviceMemoryBudgetAdmission::EngineAllocationTheDeviceMemoryBudgetNeverRefuses,
            "HostVulkanBuffer::new_index_buffer_host_visible",
        )
    }

    /// Persistently mapped pointer for CPU access — plane 0 for
    /// multi-plane imports. `null` for DEVICE_LOCAL allocations. Use
    /// [`Self::plane_mapped_ptr`] for any plane index.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Number of imported planes backing this buffer. Always `>= 1`:
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

    /// Plane 0 size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Whether this buffer's allocation sits on a HOST_CACHED memory
    /// type — what separates a readback staging a CPU can traverse at
    /// speed from write-combined memory. `None` for imported buffers,
    /// which have no VMA allocation here to read.
    ///
    /// Callers outside the RHI ask through this rather than reading
    /// `VkMemoryPropertyFlags` themselves; raw `vulkanalia` stays in the
    /// RHI.
    pub fn vma_allocation_is_host_cached(&self) -> Option<bool> {
        Some(memory_type_index_is_host_cached(
            self.vulkan_device.allocator().get_memory_properties(),
            self.vma_allocation_memory_type_index()?,
        ))
    }

    /// Memory type index (`vmaGetAllocationInfo().memoryType`) of this
    /// buffer's allocation — what a conforming consumer-side
    /// `vkAllocateMemory(VkImportMemoryFdInfoKHR)` must state, since
    /// OPAQUE_FD has no fd-properties query to derive it from.
    /// `None` for imported buffers, whose allocation lives on the
    /// foreign side; never defaulted, because every index including `0`
    /// is a real value.
    pub fn vma_allocation_memory_type_index(&self) -> Option<u32> {
        let allocation = self.allocation?;
        Some(
            self.vulkan_device
                .allocator()
                .get_allocation_info(allocation)
                .memoryType,
        )
    }

    /// Underlying Vulkan buffer handle.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }
}
