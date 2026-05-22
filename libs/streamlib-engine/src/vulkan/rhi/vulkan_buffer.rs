// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::{Result, Error};

use super::HostVulkanDevice;

/// Process-global HostVulkanDevice reference for DMA-BUF import.
///
/// Set once during [`GpuDevice::new()`] on Linux. The import trait
/// (`RhiPixelBufferImport::from_external_handle`) is a static method with no
/// device parameter, so this global bridges that gap.
#[cfg(target_os = "linux")]
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

/// Bitstream direction selector for [`HostVulkanBuffer::new_video_bitstream`].
/// Picks `VK_BUFFER_USAGE_VIDEO_ENCODE_DST_BIT_KHR` vs
/// `VK_BUFFER_USAGE_VIDEO_DECODE_SRC_BIT_KHR` — the two roles a
/// HOST_VISIBLE codec bitstream buffer can serve.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoBitstreamDirection {
    /// Encoder output: driver writes compressed NAL bytes; CPU reads
    /// the result via the persistently-mapped pointer for muxing.
    Encode,
    /// Decoder input: CPU writes compressed NAL bytes via the
    /// persistently-mapped pointer; driver reads to drive decode.
    Decode,
}

/// Inputs for [`HostVulkanBuffer::new_video_bitstream`].
///
/// Bitstream buffers are HOST_VISIBLE + HOST_COHERENT + persistently
/// mapped + sequential-write-optimized. The buffer is bound to a
/// codec profile via `VkVideoProfileListInfoKHR` chained on the
/// buffer's `pNext` so the driver knows which codec the bytes serve.
///
/// `size` is the upfront allocation size in bytes. Growth (when a
/// frame doesn't fit) is the codec layer's concern — the codec drops
/// the existing buffer and constructs a fresh, larger one via this
/// same constructor.
#[cfg(target_os = "linux")]
pub struct VideoBitstreamBufferDescriptor<'a> {
    pub label: &'a str,
    pub size: u64,
    pub direction: VideoBitstreamDirection,
    pub video_profile: &'a vk::VideoProfileInfoKHR,
}

/// Generic Vulkan `VkBuffer` allocation primitive — flat bytes with usage flags
/// and (optionally) DMA-BUF / OPAQUE_FD export bookkeeping.
///
/// Role-specific metadata (pixel `width`/`height`/`format`, vertex stride,
/// uniform element size, etc.) lives on the higher-tier wrappers in
/// [`crate::core::rhi`] ([`crate::core::rhi::PixelBuffer`],
/// [`crate::core::rhi::StorageBuffer`], [`crate::core::rhi::UniformBuffer`],
/// [`crate::core::rhi::VertexBuffer`], [`crate::core::rhi::IndexBuffer`]),
/// not on this type. Matches UE5 `FRHIBuffer` / wgpu `Buffer` / Vulkano
/// `Buffer` / Granite `Buffer` shape: the bottom-layer primitive carries
/// only `(size, usage, memory)`; role is a composition concern above.
pub struct HostVulkanBuffer {
    /// HostVulkanDevice reference for tracked allocation/free through the RHI.
    vulkan_device: Arc<HostVulkanDevice>,
    buffer: vk::Buffer,
    /// VMA allocation (HOST_VISIBLE | DEDICATED_MEMORY for DMA-BUF export).
    allocation: Option<vma::Allocation>,
    /// Imported device memory for DMA-BUF import path (VMA cannot import external memory).
    #[cfg(target_os = "linux")]
    imported_memory: Option<vk::DeviceMemory>,
    /// Whether this buffer was imported from a DMA-BUF fd.
    #[cfg(target_os = "linux")]
    imported_from_dma_buf: bool,
    /// Whether this buffer was allocated from the OPAQUE_FD export pool
    /// (vs the DMA_BUF export pool). Determines which `handle_type` is
    /// passed to `vkGetMemoryFdKHR` on export.
    #[cfg(target_os = "linux")]
    is_opaque_fd_export: bool,
    /// Persistently mapped CPU pointer — plane 0 for multi-plane imports.
    /// `null` for DEVICE_LOCAL allocations.
    mapped_ptr: *mut u8,
    /// Planes 1..N for multi-plane DMA-BUF imports. Empty for single-plane
    /// imports and for VMA-allocated buffers.
    #[cfg(target_os = "linux")]
    extra_imported_planes: Vec<VulkanImportedPlane>,
    /// Plane 0 size in bytes.
    size: vk::DeviceSize,
}

impl HostVulkanBuffer {
    /// Create a HOST_VISIBLE, DMA-BUF exportable `VkBuffer` with
    /// `STORAGE_BUFFER | TRANSFER_SRC | TRANSFER_DST` usage, allocated
    /// through the device's DMA-BUF export VMA pool.
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
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            size,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            "HostVulkanBuffer::new",
        )
    }

    /// Internal: allocate a HOST_VISIBLE + HOST_COHERENT mapped buffer with
    /// the given usage flags, via the device's `dma_buf_buffer_pool` so it
    /// remains DMA-BUF exportable. Shared spine for every formatless
    /// host-visible buffer constructor on this type.
    fn new_host_visible_with_usage(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_size: u64,
        usage: vk::BufferUsageFlags,
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

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();
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
                Error::GpuError(format!("{constructor_label}: vmaCreateBuffer failed: {e}"))
            })?
        };

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
            #[cfg(target_os = "linux")]
            imported_memory: None,
            #[cfg(target_os = "linux")]
            imported_from_dma_buf: false,
            #[cfg(target_os = "linux")]
            is_opaque_fd_export: false,
            mapped_ptr,
            #[cfg(target_os = "linux")]
            extra_imported_planes: Vec::new(),
            size,
        })
    }

    /// Create a formatless HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    ///
    /// Sibling of [`Self::new`] for callers that have raw bytes rather than
    /// formatted pixel data (V4L2 MMAP frames, audio→GPU compute, ML upload).
    /// Same memory shape — HOST_VISIBLE | HOST_COHERENT, persistently mapped,
    /// DMA-BUF exportable via the device's existing `dma_buf_buffer_pool` —
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

    /// Underlying Vulkan buffer handle.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }
}

impl super::VulkanRhiBuffer for HostVulkanBuffer {
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

#[cfg(target_os = "linux")]
impl HostVulkanBuffer {
    /// Create a new OPAQUE_FD-exportable HOST_VISIBLE staging buffer via
    /// the device's dedicated [`HostVulkanDevice::opaque_fd_buffer_pool`].
    ///
    /// Companion to [`Self::new`]: same memory shape (HOST_VISIBLE +
    /// HOST_COHERENT, persistently mapped), but the export pool's
    /// chained `VkExportMemoryAllocateInfo::handleTypes` carries
    /// `OPAQUE_FD` instead of `DMA_BUF_EXT`. `OPAQUE_FD` is the handle
    /// type CUDA / OpenCL interop expects for `cudaImportExternalMemory`
    /// (and analogous OpenCL APIs).
    ///
    /// Returns `Err` when the device's OPAQUE_FD pool is unavailable
    /// (external memory unsupported, or pool construction failed at
    /// device init). Callers must NOT silently fall back to a
    /// non-exportable allocation — the resulting buffer would be
    /// unusable for CUDA / OpenCL interop and the failure would
    /// surface only at `vkGetMemoryFdKHR` time.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(size))]
    pub fn new_opaque_fd_export(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
    ) -> Result<Self> {
        if size == 0 {
            return Err(Error::Configuration(
                "HostVulkanBuffer::new_opaque_fd_export: size must be > 0".into(),
            ));
        }
        let size = size as vk::DeviceSize;

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
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

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let pool = vulkan_device.opaque_fd_buffer_pool().ok_or_else(|| {
            Error::GpuError(
                "OPAQUE_FD buffer pool unavailable — external memory unsupported \
                 or pool construction failed; CUDA / OpenCL interop requires this pool"
                    .into(),
            )
        })?;
        let (buffer, allocation) = unsafe { pool.create_buffer(buffer_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "Failed to create OPAQUE_FD exportable buffer: {e}"
                ))
            })?;

        let allocator = vulkan_device.allocator();
        let alloc_info = allocator.get_allocation_info(allocation);
        let mapped_ptr = alloc_info.pMappedData.cast::<u8>();
        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(buffer, allocation) };
            return Err(Error::GpuError(
                "VMA OPAQUE_FD staging buffer mapped pointer is null — expected persistent mapping".into(),
            ));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            imported_memory: None,
            imported_from_dma_buf: false,
            is_opaque_fd_export: true,
            mapped_ptr,
            extra_imported_planes: Vec::new(),
            size,
        })
    }

    /// Allocate an OPAQUE_FD-exportable, **DEVICE_LOCAL** `VkBuffer` from
    /// the device's [`HostVulkanDevice::opaque_fd_buffer_pool_device_local`].
    ///
    /// GPU-resident sibling of [`Self::new_opaque_fd_export`]: same
    /// OPAQUE_FD export semantics, same usage flags, but the underlying
    /// memory lives in VRAM rather than pinned host. The returned buffer's
    /// [`Self::mapped_ptr`] is `null` — callers must populate it via
    /// `vkCmdCopyImageToBuffer` / `vkCmdBlitImage` from a host-side
    /// pipeline step before signaling the consumer's timeline.
    ///
    /// Use this for hot-path camera→cuda flows where pinned-host PCIe
    /// bandwidth is the bottleneck. CUDA classifies the imported memory
    /// as `cudaMemoryTypeDevice` (DLPack `kDLCUDA`) automatically via
    /// `cudaPointerGetAttributes` on `cudaExternalMemoryGetMappedBuffer`'s
    /// returned pointer, so no separate handle-type wire flag is needed.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(size))]
    pub fn new_opaque_fd_export_device_local(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
    ) -> Result<Self> {
        if size == 0 {
            return Err(Error::Configuration(
                "HostVulkanBuffer::new_opaque_fd_export_device_local: size must be > 0".into(),
            ));
        }
        let size = size as vk::DeviceSize;

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
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

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let pool = vulkan_device
            .opaque_fd_buffer_pool_device_local()
            .ok_or_else(|| {
                Error::GpuError(
                    "DEVICE_LOCAL OPAQUE_FD buffer pool unavailable — external memory \
                     unsupported or pool construction failed; GPU-resident CUDA interop \
                     requires this pool"
                        .into(),
                )
            })?;
        let (buffer, allocation) = unsafe { pool.create_buffer(buffer_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "Failed to create DEVICE_LOCAL OPAQUE_FD exportable buffer: {e}"
                ))
            })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            imported_memory: None,
            imported_from_dma_buf: false,
            is_opaque_fd_export: true,
            mapped_ptr: std::ptr::null_mut(),
            extra_imported_planes: Vec::new(),
            size,
        })
    }

    /// Allocate a HOST_VISIBLE + HOST_COHERENT video bitstream buffer
    /// bound to the codec profile in `descriptor.video_profile`.
    ///
    /// Direction (encode / decode) drives the required
    /// `VkBufferUsageFlags` bit:
    /// - [`VideoBitstreamDirection::Encode`] →
    ///   `VIDEO_ENCODE_DST_BIT_KHR` — driver writes encoded NAL bytes
    ///   into the buffer; CPU reads via [`Self::mapped_ptr`] for muxing.
    /// - [`VideoBitstreamDirection::Decode`] →
    ///   `VIDEO_DECODE_SRC_BIT_KHR` — CPU writes input NAL bytes into
    ///   the buffer; driver reads to drive decode.
    ///
    /// The buffer is allocated with `MAPPED` + `HOST_ACCESS_SEQUENTIAL_WRITE`
    /// and is NOT DMA-BUF exportable (codec stays host-side). The call
    /// is wrapped in [`HostVulkanDevice::lock_device`] so concurrent
    /// processor submissions can't race the allocation on NVIDIA Linux
    /// (the original motivator for
    /// `RhiQueueSubmitter::with_device_resource_lock` — see repo
    /// history for issue #278).
    ///
    /// Growth is the codec layer's concern: when a frame doesn't fit
    /// the codec drops this buffer and constructs a fresh, larger one.
    /// Engine-tier `resize()` is deliberately absent — every modern
    /// explicit-API engine surveyed (wgpu, Bevy, Dawn, UE5, Granite)
    /// and every Vulkan-Video reference codec (FFmpeg, NVIDIA
    /// `vk_video_samples`) puts growth at the caller or in a pool
    /// one layer above the primitive.
    #[tracing::instrument(level = "trace", skip(vulkan_device, descriptor), fields(label = descriptor.label, size = descriptor.size, dir = ?descriptor.direction))]
    pub fn new_video_bitstream(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &VideoBitstreamBufferDescriptor<'_>,
    ) -> Result<Self> {
        if descriptor.size == 0 {
            return Err(Error::Configuration(format!(
                "HostVulkanBuffer::new_video_bitstream ({}): size must be > 0",
                descriptor.label,
            )));
        }
        let size = descriptor.size as vk::DeviceSize;

        let usage = match descriptor.direction {
            VideoBitstreamDirection::Encode => vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR,
            VideoBitstreamDirection::Decode => vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR,
        };

        let mut profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(descriptor.video_profile));

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut profile_list);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();

        // Same threading discipline the codec previously got via
        // `RhiQueueSubmitter::with_device_resource_lock` (see #278).
        let _device_lock = vulkan_device.lock_device();

        let (buffer, allocation) = unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "HostVulkanBuffer::new_video_bitstream ({}): vmaCreateBuffer failed: {e}",
                    descriptor.label,
                ))
            })?;

        let alloc_info = allocator.get_allocation_info(allocation);
        let mapped_ptr = alloc_info.pMappedData.cast::<u8>();
        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(buffer, allocation) };
            return Err(Error::GpuError(format!(
                "HostVulkanBuffer::new_video_bitstream ({}): VMA mapped pointer is null \
                 — expected persistent mapping",
                descriptor.label,
            )));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            imported_memory: None,
            imported_from_dma_buf: false,
            is_opaque_fd_export: false,
            mapped_ptr,
            extra_imported_planes: Vec::new(),
            size,
        })
    }

    /// Export the buffer's memory as an OPAQUE_FD file descriptor.
    ///
    /// Only valid for buffers created via [`Self::new_opaque_fd_export`];
    /// returns `Err` for DMA-BUF-flavored allocations (call
    /// [`Self::export_dma_buf_fd`] instead). Each call returns a fresh
    /// kernel fd (the driver dups internally) — the caller owns it and
    /// is responsible for closing it (or for transferring ownership via
    /// SCM_RIGHTS / `cudaImportExternalMemory` etc., both of which
    /// `dup` again on receipt).
    pub fn export_opaque_fd_memory(&self) -> Result<std::os::unix::io::RawFd> {
        if !self.is_opaque_fd_export {
            return Err(Error::GpuError(
                "HostVulkanBuffer::export_opaque_fd_memory: buffer was not created \
                 with `new_opaque_fd_export`; the underlying memory carries DMA_BUF_EXT \
                 (or no) export flags and OPAQUE_FD export will fail at the driver"
                    .into(),
            ));
        }
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanBuffer::export_opaque_fd_memory: buffer has no VMA allocation"
                    .into(),
            )
        })?;
        let alloc_info = self.vulkan_device.allocator().get_allocation_info(*allocation);
        let memory = alloc_info.deviceMemory;

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { self.vulkan_device.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| {
                Error::GpuError(format!("Failed to export OPAQUE_FD memory fd: {e}"))
            })?;
        Ok(fd)
    }

    /// Export the buffer's memory as a DMA-BUF file descriptor.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        // Determine which DeviceMemory to export from
        let device_memory = if let Some(allocation) = &self.allocation {
            let alloc_info = self.vulkan_device.allocator().get_allocation_info(*allocation);
            alloc_info.deviceMemory
        } else if let Some(memory) = self.imported_memory {
            memory
        } else {
            return Err(Error::GpuError(
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
            .map_err(|e| Error::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        Ok(fd)
    }

    /// Export this buffer as the canonical [`crate::core::rhi::RhiExternalHandle`]
    /// for its allocation flavor.
    ///
    /// - Buffers from [`Self::new_opaque_fd_export`] yield
    ///   [`crate::core::rhi::RhiExternalHandle::OpaqueFd`].
    /// - All other buffers yield
    ///   [`crate::core::rhi::RhiExternalHandle::DmaBuf`].
    ///
    /// Callers that don't care which handle type they get use this method;
    /// callers that need a specific type call [`Self::export_dma_buf_fd`] or
    /// [`Self::export_opaque_fd_memory`] directly. The returned `fd` carries
    /// the same ownership semantics as the underlying export — caller
    /// closes it (or transfers ownership via SCM_RIGHTS).
    #[tracing::instrument(level = "trace", skip(self), fields(size = self.size, opaque_fd = self.is_opaque_fd_export))]
    pub fn export_external_handle(&self) -> Result<crate::core::rhi::RhiExternalHandle> {
        let size = self.size() as usize;
        if self.is_opaque_fd_export {
            let fd = self.export_opaque_fd_memory()?;
            Ok(crate::core::rhi::RhiExternalHandle::OpaqueFd { fd, size })
        } else {
            let fd = self.export_dma_buf_fd()?;
            Ok(crate::core::rhi::RhiExternalHandle::DmaBuf { fd, size })
        }
    }

    /// Import a buffer from a single-plane DMA-BUF file descriptor.
    ///
    /// Thin wrapper over [`Self::from_dma_buf_fds`] for back-compat with
    /// existing single-plane callers — new code should prefer the
    /// multi-plane signature even when there is only one plane.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, allocation_size))]
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<HostVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        Self::from_dma_buf_fds(vulkan_device, &[fd], &[allocation_size])
    }

    /// Import one `vk::Buffer` + `vk::DeviceMemory` pair per plane from
    /// the given DMA-BUF fds. `plane_sizes[i]` must be the non-zero
    /// allocation size of plane `i`.
    ///
    /// Partial-failure semantics: if plane N fails to import, every
    /// plane 0..N that already succeeded is torn down (buffer destroyed,
    /// memory unmapped + freed) before the error is returned. The
    /// caller retains ownership of the fds — each fd is consumed by
    /// `vkAllocateMemory` only on success.
    #[tracing::instrument(level = "trace", skip(vulkan_device, fds, plane_sizes), fields(plane_count = fds.len()))]
    pub fn from_dma_buf_fds(
        vulkan_device: &Arc<HostVulkanDevice>,
        fds: &[std::os::unix::io::RawFd],
        plane_sizes: &[vk::DeviceSize],
    ) -> Result<Self> {
        if fds.is_empty() {
            return Err(Error::Configuration(
                "DMA-BUF import: fd vec must be non-empty".into(),
            ));
        }
        if fds.len() != plane_sizes.len() {
            return Err(Error::Configuration(format!(
                "DMA-BUF import: plane_sizes length ({}) must match fds length ({})",
                plane_sizes.len(),
                fds.len()
            )));
        }
        if fds.len() > streamlib_surface_client::MAX_DMA_BUF_PLANES {
            return Err(Error::Configuration(format!(
                "DMA-BUF import: plane count {} exceeds MAX_DMA_BUF_PLANES ({})",
                fds.len(),
                streamlib_surface_client::MAX_DMA_BUF_PLANES
            )));
        }

        // Import every plane. Stash each successful import in a vec so we
        // can unwind on partial failure.
        let mut imported: Vec<VulkanImportedPlane> = Vec::with_capacity(fds.len());
        for (idx, (&fd, &plane_size)) in fds.iter().zip(plane_sizes.iter()).enumerate() {
            if plane_size == 0 {
                for plane in imported.into_iter() {
                    teardown_imported_plane(vulkan_device, plane);
                }
                return Err(Error::Configuration(format!(
                    "DMA-BUF import: plane {idx} has size=0 — caller must supply each \
                     plane's allocation size (pixel-shape derivation no longer lives on \
                     the bottom-layer primitive)"
                )));
            }

            match import_single_plane(vulkan_device, fd, plane_size) {
                Ok(plane) => imported.push(plane),
                Err(e) => {
                    for plane in imported.into_iter() {
                        teardown_imported_plane(vulkan_device, plane);
                    }
                    return Err(e);
                }
            }
        }

        // Split plane 0 out; the rest become `extra_imported_planes`.
        let plane0 = imported.remove(0);
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane0.buffer,
            allocation: None,
            imported_memory: Some(plane0.memory),
            imported_from_dma_buf: true,
            is_opaque_fd_export: false,
            mapped_ptr: plane0.mapped_ptr,
            extra_imported_planes: imported,
            size: plane0.size,
        })
    }

    /// Import a DMA-BUF fd as a STORAGE_BUFFER-usage `VkBuffer` bound to
    /// imported memory.
    ///
    /// Sibling of [`Self::from_dma_buf_fd`] for V4L2-shape capture paths
    /// that hand the kernel a flat DMA-BUF fd (e.g. V4L2 `M_DMABUF` MMAP
    /// frames) rather than formatted pixel data. Same plumbing — calls
    /// [`HostVulkanDevice::import_dma_buf_memory`] under the hood — but
    /// the `byte_size` is taken flat and the wrapping
    /// [`crate::core::rhi::PixelBuffer`] receives synthetic dimensions
    /// (see [`Self::new_storage_buffer_host_visible`] for the convention).
    ///
    /// Caller retains ownership of `fd` only when import fails *before*
    /// `vkAllocateMemory` consumes the descriptor (parameter validation
    /// rejections, `vkCreateBuffer` failure). On successful return the
    /// driver owns the fd — caller must NOT `close()` it. If
    /// `vkAllocateMemory` succeeds but a later step (`vkBindBufferMemory`,
    /// `vkMapMemory`) fails, the fd is still consumed (the import path
    /// frees the imported memory + destroys the buffer, but the kernel-
    /// side fd transfer is irreversible).
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, size))]
    pub fn from_dma_buf_fd_as_storage_buffer(
        vulkan_device: &Arc<HostVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        size: u64,
    ) -> Result<Self> {
        if size == 0 {
            return Err(Error::Configuration(
                "HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer: size must be > 0".into(),
            ));
        }
        if size > u32::MAX as u64 {
            return Err(Error::Configuration(format!(
                "HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer: size {size} \
                 exceeds 4 GB synthetic-width cap"
            )));
        }
        let effective_size = size as vk::DeviceSize;
        let plane = import_single_plane(vulkan_device, fd, effective_size)?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            allocation: None,
            imported_memory: Some(plane.memory),
            imported_from_dma_buf: true,
            is_opaque_fd_export: false,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            size: plane.size,
        })
    }

}

/// Create one `VkBuffer` + bind one imported `VkDeviceMemory` + map it.
/// Used by [`HostVulkanBuffer::from_dma_buf_fds`] once per plane.
#[cfg(target_os = "linux")]
fn import_single_plane(
    vulkan_device: &Arc<HostVulkanDevice>,
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
        Error::GpuError(format!("Failed to create buffer for DMA-BUF import: {e}"))
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
        Error::GpuError(format!("Failed to bind imported memory: {e}"))
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

/// Partial-unwind helper for [`HostVulkanBuffer::from_dma_buf_fds`] —
/// tears down one already-imported plane when a later plane fails.
#[cfg(target_os = "linux")]
fn teardown_imported_plane(vulkan_device: &Arc<HostVulkanDevice>, plane: VulkanImportedPlane) {
    unsafe {
        vulkan_device.device().destroy_buffer(plane.buffer, None);
    }
    vulkan_device.unmap_imported_memory(plane.memory);
    vulkan_device.free_imported_memory(plane.memory);
}

impl Drop for HostVulkanBuffer {
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
unsafe impl Send for HostVulkanBuffer {}
unsafe impl Sync for HostVulkanBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_pool_buffer_creation_1920x1080_bgra32() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        const W: u64 = 1920;
        const H: u64 = 1080;
        const BPP: u64 = 4;
        let size = W * H * BPP;
        let buf = HostVulkanBuffer::new(&device, size).expect("buffer creation failed");

        assert_eq!(buf.size(), size);
        assert!(!buf.mapped_ptr().is_null());
        assert_ne!(buf.buffer(), vk::Buffer::null());

        println!("Pool buffer created: {} bytes ({W}x{H}x{BPP})", buf.size());
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_buffer_write_and_readback() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = HostVulkanBuffer::new(&device, 64 * 64 * 4).expect("buffer creation failed");

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

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_dma_buf_export() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
            .expect("buffer creation failed");

        let fd = buf.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

        println!("DMA-BUF exported: fd={fd}");
        // fd ownership is caller's — close it
        unsafe { libc::close(fd) };
    }

    /// `new_opaque_fd_export` allocates from the OPAQUE_FD pool;
    /// `export_opaque_fd_memory` returns a valid kernel fd; cross-flavor
    /// export is rejected (calling `export_opaque_fd_memory` on a DMA-BUF
    /// buffer produces an error).
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn opaque_fd_export_round_trip_and_cross_flavor_rejection() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        if device.opaque_fd_buffer_pool().is_none() {
            println!("Skipping - OPAQUE_FD buffer pool unavailable on this driver");
            return;
        }

        // Positive: OPAQUE_FD-allocated buffer exports an OPAQUE_FD fd.
        let buf = HostVulkanBuffer::new_opaque_fd_export(&device, (128 as u64) * (128 as u64) * (4 as u64))
        .expect("new_opaque_fd_export failed");
        assert!(!buf.mapped_ptr().is_null(), "mapped pointer should be non-null");
        assert_eq!(buf.size(), (128 * 128 * 4) as vk::DeviceSize);
        let fd = buf
            .export_opaque_fd_memory()
            .expect("export_opaque_fd_memory failed");
        assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
        unsafe { libc::close(fd) };

        // Negative: DMA-BUF-allocated buffer rejects OPAQUE_FD export.
        let dma_buf = HostVulkanBuffer::new(&device, (64 as u64) * (64 as u64) * (4 as u64))
            .expect("dma-buf buffer failed");
        match dma_buf.export_opaque_fd_memory() {
            Err(crate::core::Error::GpuError(msg)) => {
                assert!(
                    msg.contains("not created with `new_opaque_fd_export`"),
                    "error must call out the cross-flavor mismatch, got: {msg}"
                );
            }
            other => panic!(
                "expected cross-flavor rejection on DMA-BUF buffer, got {other:?}"
            ),
        }
    }

    /// `new_opaque_fd_export_device_local` allocates from the dedicated
    /// DEVICE_LOCAL pool, returns a non-mappable buffer (mapped_ptr null
    /// by construction), and exports an OPAQUE_FD fd. Sibling of
    /// `opaque_fd_export_round_trip_and_cross_flavor_rejection` covering
    /// the GPU-resident path.
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn opaque_fd_device_local_export_round_trip() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        if device.opaque_fd_buffer_pool_device_local().is_none() {
            println!(
                "Skipping - DEVICE_LOCAL OPAQUE_FD buffer pool unavailable on this driver"
            );
            return;
        }

        let buf = HostVulkanBuffer::new_opaque_fd_export_device_local(&device, (128 as u64) * (128 as u64) * (4 as u64))
        .expect("new_opaque_fd_export_device_local failed");
        // DEVICE_LOCAL allocations are not host-mapped.
        assert!(
            buf.mapped_ptr().is_null(),
            "DEVICE_LOCAL OPAQUE_FD buffers must not expose a host pointer; \
             callers populate via vkCmdCopyImageToBuffer / vkCmdBlitImage"
        );
        assert_eq!(buf.size(), (128 * 128 * 4) as vk::DeviceSize);
        let fd = buf
            .export_opaque_fd_memory()
            .expect("export_opaque_fd_memory failed");
        assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
        unsafe { libc::close(fd) };
    }

    /// `export_external_handle` dispatches to the correct
    /// `RhiExternalHandle` variant for each allocation flavor.
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn export_external_handle_dispatches_on_allocation_flavor() {
        use crate::core::rhi::RhiExternalHandle;

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // DMA-BUF flavor → DmaBuf variant.
        let dma_buf =
            HostVulkanBuffer::new(&device, (64 as u64) * (64 as u64) * (4 as u64))
                .expect("dma-buf buffer failed");
        match dma_buf.export_external_handle() {
            Ok(RhiExternalHandle::DmaBuf { fd, size }) => {
                assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
                assert_eq!(size, (64 * 64 * 4) as usize);
                unsafe { libc::close(fd) };
            }
            other => panic!("expected DmaBuf, got: {other:?}"),
        }

        // OPAQUE_FD flavor → OpaqueFd variant. Skip if the pool isn't
        // available on this driver (already tested separately above).
        if device.opaque_fd_buffer_pool().is_some() {
            let opaque_buf = HostVulkanBuffer::new_opaque_fd_export(&device, (64 as u64) * (64 as u64) * (4 as u64))
            .expect("new_opaque_fd_export failed");
            match opaque_buf.export_external_handle() {
                Ok(RhiExternalHandle::OpaqueFd { fd, size }) => {
                    assert!(fd >= 0, "OPAQUE_FD fd must be non-negative, got {fd}");
                    assert_eq!(size, (64 * 64 * 4) as usize);
                    unsafe { libc::close(fd) };
                }
                other => panic!("expected OpaqueFd, got: {other:?}"),
            }
        }
    }

    /// `physical_device_uuid()` returns 16 bytes that look like a real
    /// UUID — neither all-zero (would indicate `vkGetPhysicalDeviceProperties2`
    /// never ran) nor all-the-same-byte (would catch a constant-write bug
    /// like `[0u8; 16].fill(1)`). Real GPU UUIDs from NVIDIA / AMD / Intel
    /// have many distinct byte values; a threshold of >= 4 distinct bytes
    /// is comfortably below every observed real-device UUID and well
    /// above any plausible constant-write bug.
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn physical_device_uuid_is_populated() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let uuid = device.physical_device_uuid();
        assert!(
            uuid.iter().any(|b| *b != 0),
            "physical_device_uuid should not be all-zero — got {uuid:?}"
        );
        let distinct: std::collections::HashSet<u8> = uuid.iter().copied().collect();
        assert!(
            distinct.len() >= 4,
            "physical_device_uuid should have >= 4 distinct byte values \
             (real GPU UUIDs do; a constant-write bug wouldn't) — got {uuid:02x?} \
             with {} distinct values",
            distinct.len()
        );

        // Cheap idempotence check: the accessor is a copy of a stored
        // array; calling it twice must yield identical bytes.
        assert_eq!(
            uuid,
            device.physical_device_uuid(),
            "physical_device_uuid must be stable across calls"
        );

        println!("physical_device_uuid: {uuid:02x?}");
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_multiple_buffers_coexist() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let b0 = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
            .expect("buffer 0 failed");
        let b1 = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
            .expect("buffer 1 failed");
        let b2 = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
            .expect("buffer 2 failed");
        let b3 = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
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

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_drop_frees_without_panic() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let buf = HostVulkanBuffer::new(&device, (1920 as u64) * (1080 as u64) * (4 as u64))
            .expect("buffer creation failed");
        drop(buf);

        println!("Buffer drop completed without panic");
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_dma_buf_import_round_trip() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // Create source buffer and write a known pattern
        let width = 64u32;
        let height = 64u32;
        let bpp = 4u32;
        let src = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
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
        let imported = HostVulkanBuffer::from_dma_buf_fd(&device, fd, src.size())
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
    /// `HostVulkanBuffer`, confirm `plane_count()` reports 2, and each
    /// plane's bytes survive intact. Mirrors the symmetry the polyglot
    /// Python and Deno shims provide via `*_gpu_surface_plane_{count,size,mmap,base_address}`.
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_dma_buf_import_multi_plane_round_trip() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
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
        let src0 = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
            .expect("source plane 0 creation failed");
        let src1 = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
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
        let imported = HostVulkanBuffer::from_dma_buf_fds(&device, &[fd0, fd1], &[plane_size, plane_size])
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
    /// more planes than the surface-share `MAX_DMA_BUF_PLANES` cap (4 today).
    /// Covers the Rust half of the consistency the wire helpers already
    /// enforce on sends/receives.
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn test_dma_buf_import_rejects_oversize_plane_vec() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // fds/sizes vecs with one more entry than the cap. Negative fds
        // are fine — we expect the length check to fire before any
        // syscall touches them.
        let fds: Vec<std::os::unix::io::RawFd> =
            (0..=streamlib_surface_client::MAX_DMA_BUF_PLANES as i32)
                .map(|_| -1i32)
                .collect();
        let sizes: Vec<vk::DeviceSize> = vec![1024; fds.len()];

        let result = HostVulkanBuffer::from_dma_buf_fds(&device, &fds, &sizes);
        match result {
            Ok(_) => panic!("oversize plane vec must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("MAX_DMA_BUF_PLANES"),
                "error should name the cap, got: {e}"
            ),
        }
    }

    /// `new_storage_buffer_host_visible` allocates a HOST_VISIBLE
    /// STORAGE_BUFFER-usage VkBuffer: mapped pointer is non-null, size
    /// matches the requested byte count, write→readback round-trips
    /// through the persistent mapping.
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn storage_buffer_host_visible_write_readback() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let byte_size: u64 = 4096;
        let buf = HostVulkanBuffer::new_storage_buffer_host_visible(&device, byte_size)
            .expect("storage buffer allocation failed");

        assert_eq!(buf.size(), byte_size as vk::DeviceSize);
        assert!(!buf.mapped_ptr().is_null(), "mapped pointer must be non-null");
        assert_ne!(buf.buffer(), vk::Buffer::null());

        // Write a counter pattern through the mapped pointer and read it back.
        let ptr = buf.mapped_ptr();
        unsafe {
            for i in 0..byte_size as usize {
                std::ptr::write(ptr.add(i), (i & 0xFF) as u8);
            }
            for i in 0..byte_size as usize {
                let v = std::ptr::read(ptr.add(i));
                assert_eq!(v, (i & 0xFF) as u8, "mismatch at offset {i}");
            }
        }

        println!(
            "storage buffer round-trip verified: {} bytes",
            byte_size
        );
    }

    /// `new_storage_buffer_host_visible` rejects byte_size = 0 and
    /// byte_size > u32::MAX with a `Configuration` error. No device touch
    /// for the validation paths, so this runs without hardware.
    #[test]
    fn storage_buffer_host_visible_rejects_invalid_sizes() {
        // Validation errors fire before any device interaction. Build a
        // device lazily — if it fails (no GPU), skip; the validation
        // path itself doesn't depend on the device but the function
        // signature requires one.
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        match HostVulkanBuffer::new_storage_buffer_host_visible(&device, 0) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("byte_size must be > 0"), "got: {msg}");
            }
            Err(other) => panic!("expected Configuration error for zero size, got {other:?}"),
            Ok(_) => panic!("expected zero-size rejection, got Ok"),
        }

        let oversized = (u32::MAX as u64) + 1;
        match HostVulkanBuffer::new_storage_buffer_host_visible(&device, oversized) {
            Err(Error::Configuration(msg)) => {
                assert!(
                    msg.contains("exceeds 4 GB synthetic-width cap"),
                    "got: {msg}"
                );
            }
            Err(other) => panic!("expected Configuration error for oversized, got {other:?}"),
            Ok(_) => panic!("expected oversized rejection, got Ok"),
        }
    }

    /// `from_dma_buf_fd_as_storage_buffer` imports a DMA-BUF fd as a
    /// STORAGE_BUFFER-usage VkBuffer with mapped memory. Round-trip:
    /// allocate a source HOST_VISIBLE SSBO, export its DMA-BUF fd,
    /// import via `from_dma_buf_fd_as_storage_buffer`, verify the
    /// imported mapping carries the same bytes the source wrote.
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn storage_buffer_from_dma_buf_fd_round_trip() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let byte_size: u64 = 8192;
        let src = HostVulkanBuffer::new_storage_buffer_host_visible(&device, byte_size)
            .expect("source SSBO allocation failed");

        let pattern: [u8; 4] = [0x55, 0xAA, 0xCC, 0x33];
        let size_usize = byte_size as usize;
        unsafe {
            for i in (0..size_usize).step_by(4) {
                std::ptr::copy_nonoverlapping(pattern.as_ptr(), src.mapped_ptr().add(i), 4);
            }
        }

        let fd = src.export_dma_buf_fd().expect("DMA-BUF export failed");
        assert!(fd >= 0);

        let imported =
            HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(&device, fd, byte_size)
                .expect("DMA-BUF SSBO import failed");

        assert_eq!(imported.size(), byte_size as vk::DeviceSize);
        assert!(!imported.mapped_ptr().is_null());

        unsafe {
            for i in (0..size_usize).step_by(4) {
                let b = std::ptr::read(imported.mapped_ptr().add(i));
                let g = std::ptr::read(imported.mapped_ptr().add(i + 1));
                let r = std::ptr::read(imported.mapped_ptr().add(i + 2));
                let a = std::ptr::read(imported.mapped_ptr().add(i + 3));
                assert_eq!([b, g, r, a], pattern, "imported mismatch at offset {i}");
            }
        }

        println!(
            "DMA-BUF SSBO round-trip verified: {} bytes, fd={fd}",
            byte_size
        );
    }

    /// `from_dma_buf_fd_as_storage_buffer` returns a typed error for
    /// invalid sizes (zero, > u32::MAX) before touching the fd.
    #[cfg(target_os = "linux")]
    #[test]
    fn storage_buffer_from_dma_buf_fd_rejects_invalid_sizes() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // fd = -1 would fail at import-time; but size validation runs
        // first so the fd is never touched here.
        match HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(&device, -1, 0) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("size must be > 0"), "got: {msg}");
            }
            Err(other) => panic!("expected Configuration error for zero size, got {other:?}"),
            Ok(_) => panic!("expected zero-size rejection, got Ok"),
        }

        let oversized = (u32::MAX as u64) + 1;
        match HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(&device, -1, oversized) {
            Err(Error::Configuration(msg)) => {
                assert!(
                    msg.contains("exceeds 4 GB synthetic-width cap"),
                    "got: {msg}"
                );
            }
            Err(other) => panic!("expected Configuration error for oversized, got {other:?}"),
            Ok(_) => panic!("expected oversized rejection, got Ok"),
        }
    }

    /// `from_dma_buf_fd_as_storage_buffer` cleans up on import failure —
    /// an undersized DMA-BUF import (small fd, larger requested size)
    /// must return `Err` and not leak `VkBuffer` / `VkDeviceMemory`.
    ///
    /// Choice of failure mode: undersized fd is the only failure path
    /// every modern Linux Vulkan driver (NVIDIA proprietary, Mesa
    /// iris/radeonsi) is empirically known to reject — closed fds, `-1`,
    /// and `/dev/null` fds are tolerated by NVIDIA proprietary at
    /// `vkAllocateMemory` time despite the spec requiring rejection
    /// (driver returns success with phantom memory). Asking the driver
    /// to import a 4 KB-backed DMA-BUF as 16 MB hits the kernel's
    /// `dma_buf_attach`-time size check, which NVIDIA does honor.
    ///
    /// Source DMA-BUF is itself allocated through the engine so we
    /// don't depend on external producers. Import counter is verified
    /// to stay flat across the failed call (allocator + cleanup paths
    /// must be balanced).
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn storage_buffer_from_dma_buf_fd_drops_on_failure() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // Allocate a small source DMA-BUF (4 KB). Its kernel-side
        // backing is one page; asking to import it at 16 MB must
        // be rejected per spec (the dma_buf cannot back the
        // requested size).
        let source_size: u64 = 4096;
        let source = HostVulkanBuffer::new_storage_buffer_host_visible(
            &device,
            source_size,
        )
        .expect("source SSBO allocation failed");
        let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");

        let oversized: u64 = 16 * 1024 * 1024;
        let before = device.live_import_allocation_count();
        let result = HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
            &device,
            fd,
            oversized,
        );
        let after = device.live_import_allocation_count();

        // Strict: the driver MUST reject. If it accepts, either the
        // driver is silently allocating ordinary memory (NVIDIA-style
        // tolerance generalizing beyond closed fds), or our test
        // setup is wrong. Either way the leak invariant isn't being
        // exercised and the test should fail rather than silently no-op.
        let err = match result {
            Ok(_) => {
                panic!(
                    "expected DMA-BUF import to reject oversized request \
                     (source={source_size} bytes, requested={oversized} bytes) — \
                     driver accepted; leak invariant unverified"
                );
            }
            Err(e) => e,
        };
        let _ = err;

        assert_eq!(
            before, after,
            "failed import must not leak VkDeviceMemory — live count: before={before}, after={after}"
        );
        // fd ownership stayed with the caller because allocate failed;
        // close it.
        unsafe { libc::close(fd) };
    }
}
