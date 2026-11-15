// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Linux buffer that crosses a process: OPAQUE_FD exports, DMA-BUF export and import, and host-cached buffers for CPU reads.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::vulkan_device::VmaFrameAllocationSource;
use crate::vulkan::rhi::vulkan_device::{
    MappedOpaqueFdBufferHostAccessPattern, VmaExportPoolPinnedToOneMemoryType,
};
use crate::vulkan::rhi::vulkan_frame_allocation_device_memory_budget::DeviceMemoryBudgetAdmission;

use super::{
    HostVisibleAllocationIntent, HostVulkanBuffer, HostVulkanBufferMemoryOrigin,
    VulkanImportedPlane,
};

impl HostVulkanBuffer {
    /// The `HostVulkanDevice` this buffer was allocated from — its owning
    /// device. Callers exporting an OPAQUE_FD bind the CUDA context to
    /// this device's `physical_device_uuid()`, never the UUID of some
    /// other (e.g. the current `GpuContext`'s) device.
    pub(crate) fn vulkan_device(&self) -> &Arc<HostVulkanDevice> {
        &self.vulkan_device
    }

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
    pub fn new_opaque_fd_export(vulkan_device: &Arc<HostVulkanDevice>, size: u64) -> Result<Self> {
        let opaque_fd_buffer_pool = vulkan_device
            .opaque_fd_buffer_pool_pinned_to_one_memory_type()
            .ok_or_else(|| {
                Error::GpuError(
                    "OPAQUE_FD buffer pool unavailable — external memory unsupported \
                     or pool construction failed; CUDA / OpenCL interop requires this pool"
                        .into(),
                )
            })?;
        Self::new_opaque_fd_export_from_pool(
            vulkan_device,
            size,
            opaque_fd_buffer_pool,
            MappedOpaqueFdBufferHostAccessPattern::SequentialWrite,
            "HostVulkanBuffer::new_opaque_fd_export",
        )
    }

    /// Create a new OPAQUE_FD-exportable HOST_VISIBLE staging buffer on a
    /// HOST_CACHED memory type, for a consumer that *reads* the mapping.
    ///
    /// Same OPAQUE_FD export semantics as [`Self::new_opaque_fd_export`],
    /// so the check-out and import path is identical; only the memory
    /// properties differ, and with them how fast a CPU reader traverses
    /// the mapping.
    ///
    /// Degrades to [`Self::new_opaque_fd_export`] on a device with no
    /// cached exportable memory type rather than refusing: the plan's
    /// contract is slower there, never unavailable.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(size))]
    pub fn new_opaque_fd_export_host_cached(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
    ) -> Result<Self> {
        match vulkan_device.opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type() {
            Some(opaque_fd_buffer_pool_host_cached) => Self::new_opaque_fd_export_from_pool(
                vulkan_device,
                size,
                opaque_fd_buffer_pool_host_cached,
                MappedOpaqueFdBufferHostAccessPattern::Random,
                "HostVulkanBuffer::new_opaque_fd_export_host_cached",
            ),
            None => Self::new_opaque_fd_export(vulkan_device, size),
        }
    }

    /// Allocate a mapped OPAQUE_FD-exportable HOST_VISIBLE buffer from
    /// `pool` under `host_access_pattern`.
    ///
    /// `export_pool` must have been probed with the same `host_access_pattern`
    /// and the same usage flags: a VMA pool is pinned to one memory type
    /// index, and a mismatch between the probe's `memoryTypeBits` and
    /// this buffer's trips VUID-vkBindBufferMemory-memory-01035.
    fn new_opaque_fd_export_from_pool(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: u64,
        export_pool: &VmaExportPoolPinnedToOneMemoryType,
        host_access_pattern: MappedOpaqueFdBufferHostAccessPattern,
        constructor_label: &'static str,
    ) -> Result<Self> {
        if size == 0 {
            return Err(Error::Configuration(format!(
                "{constructor_label}: size must be > 0"
            )));
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
                | host_access_pattern.vma_allocation_create_flags(),
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let (buffer, allocation) = vulkan_device
            .create_frame_buffer_within_the_device_memory_budget(
                DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
                VmaFrameAllocationSource::ExportPool(export_pool),
                &buffer_info,
                &alloc_opts,
                constructor_label,
                |e| {
                    Error::GpuError(format!(
                        "{constructor_label}: failed to create the OPAQUE_FD exportable buffer: {e}"
                    ))
                },
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
            imported_memory: None,
            memory_origin: HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd,
            imported_host_range_owner: None,
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

        let opaque_fd_buffer_pool_device_local = vulkan_device
            .opaque_fd_buffer_pool_device_local_pinned_to_one_memory_type()
            .ok_or_else(|| {
                Error::GpuError(
                    "DEVICE_LOCAL OPAQUE_FD buffer pool unavailable — external memory \
                     unsupported or pool construction failed; GPU-resident CUDA interop \
                     requires this pool"
                        .into(),
                )
            })?;
        let (buffer, allocation) = vulkan_device
            .create_frame_buffer_within_the_device_memory_budget(
                DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
                VmaFrameAllocationSource::ExportPool(opaque_fd_buffer_pool_device_local),
                &buffer_info,
                &alloc_opts,
                "HostVulkanBuffer::new_opaque_fd_export_device_local",
                |e| {
                    Error::GpuError(format!(
                        "Failed to create DEVICE_LOCAL OPAQUE_FD exportable buffer: {e}"
                    ))
                },
            )?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            imported_memory: None,
            memory_origin: HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd,
            imported_host_range_owner: None,
            mapped_ptr: std::ptr::null_mut(),
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
        match self.memory_origin {
            HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd => {}
            HostVulkanBufferMemoryOrigin::VmaAllocation
            | HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf
            | HostVulkanBufferMemoryOrigin::ImportedFromOpaqueFd
            | HostVulkanBufferMemoryOrigin::ImportedFromHostPointer => {
                return Err(Error::GpuError(
                    "HostVulkanBuffer::export_opaque_fd_memory: buffer was not created \
                     with `new_opaque_fd_export`; the underlying memory carries DMA_BUF_EXT \
                     (or no) export flags and OPAQUE_FD export will fail at the driver"
                        .into(),
                ));
            }
        }
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanBuffer::export_opaque_fd_memory: buffer has no VMA allocation".into(),
            )
        })?;
        let alloc_info = self
            .vulkan_device
            .allocator()
            .get_allocation_info(*allocation);
        let memory = alloc_info.deviceMemory;

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { self.vulkan_device.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export OPAQUE_FD memory fd: {e}")))?;
        Ok(fd)
    }

    /// Export the buffer's memory as a DMA-BUF file descriptor.
    ///
    /// Refused by name for memory that carries no DMA-BUF export: an
    /// OPAQUE_FD allocation or import, an imported host range, and any
    /// allocation on a device that exports no DMA-BUF.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        let device_memory = match self.memory_origin {
            HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd
            | HostVulkanBufferMemoryOrigin::ImportedFromOpaqueFd => {
                return Err(Error::NotSupported(format!(
                    "HostVulkanBuffer::export_dma_buf_fd: this buffer's memory on {} is an \
                     OPAQUE_FD allocation, which exports no DMA-BUF; its handle is the OPAQUE_FD",
                    self.vulkan_device.name()
                )));
            }
            HostVulkanBufferMemoryOrigin::ImportedFromHostPointer => {
                return Err(Error::NotSupported(format!(
                    "HostVulkanBuffer::export_dma_buf_fd: this buffer's memory on {} is an \
                     imported host range, which exports no DMA-BUF",
                    self.vulkan_device.name()
                )));
            }
            HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf => {
                self.imported_memory.ok_or_else(|| {
                    Error::GpuError(
                        "HostVulkanBuffer::export_dma_buf_fd: a DMA-BUF import holds no \
                         imported memory"
                            .into(),
                    )
                })?
            }
            HostVulkanBufferMemoryOrigin::VmaAllocation => {
                if !self.vulkan_device.supports_dma_buf_export() {
                    return Err(Error::NotSupported(format!(
                        "HostVulkanBuffer::export_dma_buf_fd: {} exports no DMA-BUF",
                        self.vulkan_device.name()
                    )));
                }
                let allocation = self.allocation.as_ref().ok_or_else(|| {
                    Error::GpuError(
                        "HostVulkanBuffer::export_dma_buf_fd: buffer has no VMA allocation".into(),
                    )
                })?;
                self.vulkan_device
                    .allocator()
                    .get_allocation_info(*allocation)
                    .deviceMemory
            }
        };

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { self.vulkan_device.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export DMA-BUF fd: {e}")))?;

        Ok(fd)
    }

    /// Export this buffer as the canonical [`crate::core::rhi::RhiExternalHandle`]
    /// for its allocation flavor.
    ///
    /// - Buffers from [`Self::new_opaque_fd_export`] yield
    ///   [`crate::core::rhi::RhiExternalHandle::OpaqueFd`], stating the
    ///   allocation's memory type index.
    /// - All other buffers yield
    ///   [`crate::core::rhi::RhiExternalHandle::DmaBuf`].
    ///
    /// Callers that don't care which handle type they get use this method;
    /// callers that need a specific type call [`Self::export_dma_buf_fd`] or
    /// [`Self::export_opaque_fd_memory`] directly. The returned `fd` carries
    /// the same ownership semantics as the underlying export — caller
    /// closes it (or transfers ownership via SCM_RIGHTS).
    #[tracing::instrument(level = "trace", skip(self), fields(size = self.size, memory_origin = ?self.memory_origin))]
    pub fn export_external_handle(&self) -> Result<crate::core::rhi::RhiExternalHandle> {
        let size = self.size() as usize;
        match self.memory_origin {
            HostVulkanBufferMemoryOrigin::VmaAllocationExportableAsOpaqueFd => {
                let exporting_allocation_memory_type_index =
                    self.vma_allocation_memory_type_index().ok_or_else(|| {
                        Error::GpuError(
                            "HostVulkanBuffer::export_external_handle: an OPAQUE_FD export has \
                             no VMA allocation to state a memory type index from"
                                .into(),
                        )
                    })?;
                let fd = self.export_opaque_fd_memory()?;
                Ok(crate::core::rhi::RhiExternalHandle::OpaqueFd {
                    fd,
                    size,
                    exporting_allocation_memory_type_index,
                })
            }
            HostVulkanBufferMemoryOrigin::VmaAllocation
            | HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf
            | HostVulkanBufferMemoryOrigin::ImportedFromOpaqueFd
            | HostVulkanBufferMemoryOrigin::ImportedFromHostPointer => {
                let fd = self.export_dma_buf_fd()?;
                Ok(crate::core::rhi::RhiExternalHandle::DmaBuf { fd, size })
            }
        }
    }

    /// Import a buffer from a single-plane DMA-BUF file descriptor.
    ///
    /// Thin wrapper over [`Self::from_dma_buf_fds`] with the same fd
    /// ownership contract.
    #[tracing::instrument(
        level = "trace",
        skip(vulkan_device, dma_buf_fd),
        fields(allocation_size)
    )]
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<HostVulkanDevice>,
        dma_buf_fd: std::os::fd::OwnedFd,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        Self::from_dma_buf_fds(vulkan_device, vec![dma_buf_fd], &[allocation_size])
    }

    /// Import one `vk::Buffer` + `vk::DeviceMemory` pair per plane from
    /// the given DMA-BUF fds. `plane_sizes[i]` must be the non-zero
    /// allocation size of plane `i`.
    ///
    /// Takes the fds by value because the caller holds nothing after this
    /// returns: each fd is either handed to the driver at its plane's
    /// `vkAllocateMemory` — the driver's from then on, whatever that call
    /// returned (see [`HostVulkanDevice::import_dma_buf_memory`]) — or
    /// closed here on an exit before that point. If plane N fails, planes
    /// 0..N are torn down with their memory and the fds of the planes
    /// after N are closed here.
    #[tracing::instrument(level = "trace", skip(vulkan_device, dma_buf_fds, plane_sizes), fields(plane_count = dma_buf_fds.len()))]
    pub fn from_dma_buf_fds(
        vulkan_device: &Arc<HostVulkanDevice>,
        dma_buf_fds: Vec<std::os::fd::OwnedFd>,
        plane_sizes: &[vk::DeviceSize],
    ) -> Result<Self> {
        if dma_buf_fds.is_empty() {
            return Err(Error::Configuration(
                "DMA-BUF import: fd vec must be non-empty".into(),
            ));
        }
        if dma_buf_fds.len() != plane_sizes.len() {
            return Err(Error::Configuration(format!(
                "DMA-BUF import: plane_sizes length ({}) must match fds length ({})",
                plane_sizes.len(),
                dma_buf_fds.len()
            )));
        }
        if dma_buf_fds.len() > streamlib_surface_client::MAX_DMA_BUF_PLANES {
            return Err(Error::Configuration(format!(
                "DMA-BUF import: plane count {} exceeds MAX_DMA_BUF_PLANES ({})",
                dma_buf_fds.len(),
                streamlib_surface_client::MAX_DMA_BUF_PLANES
            )));
        }

        // Import every plane. Stash each successful import in a vec so we
        // can unwind on partial failure; the fds not yet reached stay in
        // the iterator and close with it on the way out.
        let mut imported: Vec<VulkanImportedPlane> = Vec::with_capacity(dma_buf_fds.len());
        for (idx, (fd, &plane_size)) in dma_buf_fds.into_iter().zip(plane_sizes.iter()).enumerate()
        {
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
            memory_origin: HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf,
            imported_host_range_owner: None,
            mapped_ptr: plane0.mapped_ptr,
            extra_imported_planes: imported,
            size: plane0.size,
        })
    }

    /// Import a single-plane OPAQUE_FD as a mapped HOST_VISIBLE `VkBuffer`,
    /// binding the memory type index the exporter allocated from.
    ///
    /// `stated_memory_type_index` is the registration's
    /// `vk_memory_type_index`. Refused by name before any Vulkan call on a
    /// device without external memory, and before the driver sees the fd when
    /// the buffer cannot bind that type or the type is not HOST_VISIBLE and
    /// HOST_COHERENT, which a mapped read needs. Same fd ownership as
    /// [`HostVulkanDevice::import_opaque_fd_memory_at_stated_memory_type_index`]:
    /// closed here on any exit before `vkAllocateMemory`, the driver's from it on.
    #[tracing::instrument(
        level = "trace",
        skip(vulkan_device, opaque_fd),
        fields(allocation_size)
    )]
    pub fn from_opaque_fd_at_stated_memory_type_index(
        vulkan_device: &Arc<HostVulkanDevice>,
        opaque_fd: std::os::fd::OwnedFd,
        allocation_size: vk::DeviceSize,
        stated_memory_type_index: u32,
    ) -> Result<Self> {
        const CONSTRUCTOR_LABEL: &str =
            "HostVulkanBuffer::from_opaque_fd_at_stated_memory_type_index";
        if !vulkan_device.supports_external_memory() {
            return Err(Error::NotSupported(format!(
                "{CONSTRUCTOR_LABEL}: {} did not enable VK_KHR_external_memory_fd, so no \
                 OPAQUE_FD can be imported",
                vulkan_device.name()
            )));
        }
        if allocation_size == 0 {
            return Err(Error::Configuration(format!(
                "{CONSTRUCTOR_LABEL}: allocation_size must be > 0"
            )));
        }
        let plane = create_bind_and_map_imported_plane(
            vulkan_device,
            allocation_size,
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
            move |requirements, imported_allocation_size| {
                // No flush or invalidate exists on this path, so the mapped
                // read needs coherent memory as well as host-visible memory.
                let mapped_read_property_flags =
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
                match vulkan_device.property_flags_of_memory_type_index(stated_memory_type_index) {
                    Some(property_flags) if property_flags.contains(mapped_read_property_flags) => {
                    }
                    Some(property_flags) => {
                        return Err(Error::Configuration(format!(
                            "{CONSTRUCTOR_LABEL}: memory type {stated_memory_type_index} on {} is \
                             {property_flags:?}, not HOST_VISIBLE | HOST_COHERENT, so the import \
                             could not be mapped for reading",
                            vulkan_device.name()
                        )));
                    }
                    None => {
                        return Err(Error::Configuration(format!(
                            "{CONSTRUCTOR_LABEL}: {} has no memory type {stated_memory_type_index}",
                            vulkan_device.name()
                        )));
                    }
                }
                let stated_memory_type_bit =
                    1u32.checked_shl(stated_memory_type_index).unwrap_or(0);
                if requirements.memory_type_bits & stated_memory_type_bit == 0 {
                    return Err(Error::Configuration(format!(
                        "{CONSTRUCTOR_LABEL}: the imported buffer cannot bind memory type \
                         {stated_memory_type_index} on {} (memoryTypeBits {:#x})",
                        vulkan_device.name(),
                        requirements.memory_type_bits
                    )));
                }
                vulkan_device.import_opaque_fd_memory_at_stated_memory_type_index(
                    opaque_fd,
                    imported_allocation_size,
                    stated_memory_type_index,
                )
            },
        )?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            allocation: None,
            imported_memory: Some(plane.memory),
            memory_origin: HostVulkanBufferMemoryOrigin::ImportedFromOpaqueFd,
            imported_host_range_owner: None,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            size: plane.size,
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
    /// Same fd ownership as [`Self::from_dma_buf_fds`]: the driver's from
    /// its `vkAllocateMemory` on, closed here on any exit before that.
    #[tracing::instrument(level = "trace", skip(vulkan_device, dma_buf_fd), fields(size))]
    pub fn from_dma_buf_fd_as_storage_buffer(
        vulkan_device: &Arc<HostVulkanDevice>,
        dma_buf_fd: std::os::fd::OwnedFd,
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
        let plane = import_single_plane(vulkan_device, dma_buf_fd, effective_size)?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            allocation: None,
            imported_memory: Some(plane.memory),
            memory_origin: HostVulkanBufferMemoryOrigin::ImportedFromDmaBuf,
            imported_host_range_owner: None,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            size: plane.size,
        })
    }

    /// Allocate a `STORAGE_BUFFER` the CPU reads back after the GPU writes
    /// it — HOST_VISIBLE, persistently mapped, and on a HOST_CACHED type
    /// wherever the device has one. Never the sequential-write allocation
    /// [`Self::new_storage_buffer_host_visible`] hands out: reading
    /// write-combined memory with `memcpy` runs at a few hundred MB/s.
    ///
    /// Not exportable — this is private staging, never a shared surface.
    pub fn new_storage_buffer_host_cached_for_cpu_reads(
        vulkan_device: &Arc<HostVulkanDevice>,
        byte_len: u64,
    ) -> Result<Self> {
        Self::new_host_visible_with_usage(
            vulkan_device,
            byte_len,
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            HostVisibleAllocationIntent::RandomReadPrivate,
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
            "HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads",
        )
    }
}

/// Import one DMA-BUF plane as a mapped HOST_VISIBLE | HOST_COHERENT buffer.
fn import_single_plane(
    vulkan_device: &Arc<HostVulkanDevice>,
    dma_buf_fd: std::os::fd::OwnedFd,
    effective_size: vk::DeviceSize,
) -> Result<VulkanImportedPlane> {
    create_bind_and_map_imported_plane(
        vulkan_device,
        effective_size,
        vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        move |requirements, alloc_size| {
            vulkan_device.import_dma_buf_memory(
                dma_buf_fd,
                alloc_size,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
        },
    )
}

/// Create a `VkBuffer` of `effective_size` for external memory of
/// `vk_handle_type`, bind the memory `import_memory` imports for it, and map
/// it. `import_memory` is handed the buffer's requirements and the
/// allocation size — `effective_size` or the requirements' size, whichever
/// is larger. Every failure unwinds what was created before it; an fd the
/// closure owns closes with it when `vkCreateBuffer` fails before it runs.
fn create_bind_and_map_imported_plane(
    vulkan_device: &Arc<HostVulkanDevice>,
    effective_size: vk::DeviceSize,
    vk_handle_type: vk::ExternalMemoryHandleTypeFlags,
    import_memory: impl FnOnce(&vk::MemoryRequirements, vk::DeviceSize) -> Result<vk::DeviceMemory>,
) -> Result<VulkanImportedPlane> {
    let device = vulkan_device.device();
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
        Error::GpuError(format!(
            "HostVulkanBuffer: vkCreateBuffer for an imported {vk_handle_type:?} plane failed: {e}"
        ))
    })?;
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let alloc_size = effective_size.max(requirements.size);

    let memory = import_memory(&requirements, alloc_size)
        .inspect_err(|_| unsafe { device.destroy_buffer(buffer, None) })?;

    if let Err(e) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        vulkan_device.free_imported_memory(memory);
        unsafe { device.destroy_buffer(buffer, None) };
        return Err(Error::GpuError(format!(
            "HostVulkanBuffer: vkBindBufferMemory for an imported {vk_handle_type:?} plane \
             failed: {e}"
        )));
    }

    let mapped_ptr = vulkan_device
        .map_imported_memory(memory, effective_size)
        .inspect_err(|_| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
        })?;

    Ok(VulkanImportedPlane {
        buffer,
        memory,
        mapped_ptr,
        size: effective_size,
    })
}

/// Partial-unwind helper for [`HostVulkanBuffer::from_dma_buf_fds`] —
/// tears down one already-imported plane when a later plane fails.
pub(super) fn teardown_imported_plane(
    vulkan_device: &Arc<HostVulkanDevice>,
    plane: VulkanImportedPlane,
) {
    unsafe {
        vulkan_device.device().destroy_buffer(plane.buffer, None);
    }
    vulkan_device.unmap_imported_memory(plane.memory);
    vulkan_device.free_imported_memory(plane.memory);
}
