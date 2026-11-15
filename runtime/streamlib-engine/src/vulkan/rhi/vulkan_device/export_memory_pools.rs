// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use vma::Alloc as _;
#[cfg(target_os = "linux")]
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
#[cfg(target_os = "linux")]
use vulkanalia_vma as vma;

use crate::core::{Error, Result};
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::drm_modifier_probe;

use super::HostVulkanDevice;
#[cfg(target_os = "linux")]
use super::memory_type_index_is_host_cached;
#[cfg(target_os = "linux")]
use super::{
    EXPORT_POOL_SENTINEL_BYTES, EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS, ExportPoolSentinel,
    ExportPoolSentinelImageTiling, ExportPoolSentinelResource, VmaExportPoolPinnedToOneMemoryType,
};

/// How a mapped OPAQUE_FD buffer allocation is reached from the host.
///
/// One choice rather than two flags: VMA asserts when both HOST_ACCESS
/// bits are set on one allocation. A pool and every allocation it serves
/// must carry the same one, or the probe's `memoryTypeBits` stop matching
/// the real buffer's and the bind trips
/// VUID-vkBindBufferMemory-memory-01035.
#[cfg(target_os = "linux")]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MappedOpaqueFdBufferHostAccessPattern {
    /// Producer-write memory — the shared OPAQUE_FD pool's hint.
    SequentialWrite,
    /// CPU-read memory; steers VMA to prefer a HOST_CACHED type.
    Random,
}

#[cfg(target_os = "linux")]
impl MappedOpaqueFdBufferHostAccessPattern {
    /// `MAPPED` plus this pattern's single HOST_ACCESS bit.
    pub(crate) fn vma_allocation_create_flags(self) -> vma::AllocationCreateFlags {
        vma::AllocationCreateFlags::MAPPED
            | match self {
                Self::SequentialWrite => vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                Self::Random => vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            }
    }
}

/// Allocate one [`EXPORT_POOL_SENTINEL_BYTES`] buffer exportable as
/// `handle_type` out of `pinned_pool`, which must be one of `device`'s pools.
///
/// Bypasses [`HostVulkanBuffer`](crate::vulkan::rhi::HostVulkanBuffer), whose `Arc<HostVulkanDevice>` would
/// be a reference cycle once the sentinel is stored on the device itself.
/// `required_flags` and `allocation_create_flags` (on top of
/// `DEDICATED_MEMORY`) must be the pool's real frames', or the sentinel's
/// memory-type bits can miss the type the pool is pinned to.
#[cfg(target_os = "linux")]
pub(super) fn make_export_pool_buffer_sentinel(
    device: &HostVulkanDevice,
    pinned_pool: &VmaExportPoolPinnedToOneMemoryType,
    label: &'static str,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
    required_flags: vk::MemoryPropertyFlags,
    allocation_create_flags: vma::AllocationCreateFlags,
) -> Result<ExportPoolSentinel> {
    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(handle_type)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(EXPORT_POOL_SENTINEL_BYTES)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY | allocation_create_flags,
        required_flags,
        ..Default::default()
    };

    let (buffer, allocation) = unsafe { pinned_pool.pool.create_buffer(buffer_info, &alloc_opts) }
        .map_err(|e| {
            Error::GpuError(format!(
                "export pool '{label}' sentinel buffer allocation failed \
                 (size={EXPORT_POOL_SENTINEL_BYTES}): {e}"
            ))
        })?;

    Ok(ExportPoolSentinel {
        allocator: Arc::clone(device.allocator()),
        resource: ExportPoolSentinelResource::Buffer(buffer),
        allocation,
        label,
        size: EXPORT_POOL_SENTINEL_BYTES,
    })
}

/// Allocate one 8×8 four-byte-per-pixel image exportable as `handle_type`
/// out of `pinned_pool`, which must be one of `device`'s pools.
///
/// The image-backed counterpart of [`make_export_pool_buffer_sentinel`].
/// `format`, `tiling` and `usage` must be the pool's real frames', or the
/// sentinel's memory-type bits can miss the type the pool is pinned to.
#[cfg(target_os = "linux")]
fn make_export_pool_image_sentinel(
    device: &HostVulkanDevice,
    pinned_pool: &VmaExportPoolPinnedToOneMemoryType,
    label: &'static str,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
    format: vk::Format,
    tiling: ExportPoolSentinelImageTiling<'_>,
    usage: vk::ImageUsageFlags,
) -> Result<ExportPoolSentinel> {
    let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(handle_type)
        .build();
    let drm_format_modifier_candidates: &[u64] = match tiling {
        ExportPoolSentinelImageTiling::Optimal => &[],
        ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(candidates) => candidates,
    };
    let mut drm_format_modifier_list_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
        .drm_format_modifiers(drm_format_modifier_candidates)
        .build();

    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS,
            height: EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external_image_info);
    let image_info = match tiling {
        ExportPoolSentinelImageTiling::Optimal => image_info.tiling(vk::ImageTiling::OPTIMAL),
        ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(_) => image_info
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .push_next(&mut drm_format_modifier_list_info),
    };

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };

    let (image, allocation) = unsafe { pinned_pool.pool.create_image(image_info, &alloc_opts) }
        .map_err(|e| {
            Error::GpuError(format!(
                "export pool '{label}' sentinel image allocation failed \
                 ({EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS}x\
                 {EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS} {format:?}): {e}"
            ))
        })?;

    Ok(ExportPoolSentinel {
        allocator: Arc::clone(device.allocator()),
        resource: ExportPoolSentinelResource::Image(image),
        allocation,
        label,
        size: EXPORT_POOL_SENTINEL_BYTES,
    })
}

/// Pick a DEVICE_LOCAL memory type for a tiled DMA-BUF VkImage.
///
/// VMA's `find_memory_type_index_for_image_info` cannot probe images with
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it asserts on the missing
/// plane aspect for multi-plane modifiers), so we create a real probe
/// image, query its memory requirements, and find a matching DEVICE_LOCAL
/// type ourselves. The probe image is destroyed before the function returns.
#[cfg(target_os = "linux")]
fn probe_tiled_dma_buf_memory_type(
    device: &vulkanalia::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    tiled_modifiers: &[u64],
) -> Result<u32> {
    let mut probe_modifier_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
        .drm_format_modifiers(tiled_modifiers);
    let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let probe_image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut probe_modifier_info)
        .push_next(&mut probe_external_info);

    let probe_image = unsafe { device.create_image(&probe_image_info, None) }
        .map_err(|e| Error::GpuError(format!("tiled-DMA-BUF probe vkCreateImage failed: {e}")))?;

    let mem_reqs = unsafe { device.get_image_memory_requirements(probe_image) };
    unsafe { device.destroy_image(probe_image, None) };

    // First DEVICE_LOCAL type that satisfies memory_type_bits, preferring
    // one without HOST_VISIBLE (main VRAM heap) on discrete GPUs.
    for pass in 0..2 {
        for i in 0..memory_properties.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let flags = memory_properties.memory_types[i as usize].property_flags;
            if !flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                continue;
            }
            if pass == 0 && flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                continue;
            }
            return Ok(i);
        }
    }
    Err(Error::GpuError(format!(
        "no DEVICE_LOCAL memory type accepts tiled DMA-BUF probe image (memory_type_bits=0x{:x})",
        mem_reqs.memory_type_bits
    )))
}

impl HostVulkanDevice {
    /// Allocate one tiny sentinel out of every export pool the device
    /// created, **before any swapchain is bound**, for the device to hold
    /// for its life.
    ///
    /// On NVIDIA Linux a post-swapchain allocation of an exportable handle
    /// type succeeds only while a live allocation of that type anchors the
    /// driver's per-handle-type state, and nothing outside the engine is
    /// such an anchor: the shared pools free every frame of a flavour once
    /// it goes unused. The sentinels are that anchor, on every driver. They
    /// stay tiny because a consumer-sized OPAQUE_FD sentinel
    /// deterministically blocked the consumer's own allocation (#637).
    /// Pools the device did not create are skipped.
    #[cfg(target_os = "linux")]
    pub(super) fn allocate_one_sentinel_per_export_pool(
        device: &Self,
    ) -> Result<Vec<ExportPoolSentinel>> {
        let bgra_sampled_copyable_image_usage = vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST;

        let mut sentinels: Vec<ExportPoolSentinel> = Vec::new();

        if let Some(pinned_pool) = device.dma_buf_buffer_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "dma_buf_host_visible",
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            )?);
        }

        if let Some(pinned_pool) = device.dma_buf_image_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_image_sentinel(
                device,
                pinned_pool,
                "dma_buf_image",
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                vk::Format::B8G8R8A8_UNORM,
                ExportPoolSentinelImageTiling::Optimal,
                bgra_sampled_copyable_image_usage,
            )?);
        }

        // The tiled pool exists only when this same modifier list is non-empty.
        if let Some(pinned_pool) = device.dma_buf_image_pool_tiled_pinned_to_one_memory_type() {
            let render_target_modifiers = device
                .drm_modifier_table()
                .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            if !render_target_modifiers.is_empty() {
                sentinels.push(make_export_pool_image_sentinel(
                    device,
                    pinned_pool,
                    "dma_buf_image_tiled",
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    vk::Format::B8G8R8A8_UNORM,
                    ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(
                        render_target_modifiers,
                    ),
                    bgra_sampled_copyable_image_usage | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                )?);
            }
        }

        if let Some(pinned_pool) = device.opaque_fd_buffer_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_host_visible",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                MappedOpaqueFdBufferHostAccessPattern::SequentialWrite
                    .vma_allocation_create_flags(),
            )?);
        }

        // Its memory type differs from the write-combined pool's wherever the
        // device has a cached exportable type, so it is anchored separately.
        if let Some(pinned_pool) =
            device.opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type()
        {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_host_cached",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                MappedOpaqueFdBufferHostAccessPattern::Random.vma_allocation_create_flags(),
            )?);
        }

        if let Some(pinned_pool) =
            device.opaque_fd_buffer_pool_device_local_pinned_to_one_memory_type()
        {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_device_local",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                vma::AllocationCreateFlags::empty(),
            )?);
        }

        // R8G8B8A8 is the CUDA-mappable layout the pool's real images use, so
        // the sentinel's memory-type bits match theirs. No in-tree consumer
        // allocates OPAQUE_FD images after a swapchain yet, so this anchor is
        // extrapolated from the buffer evidence rather than reproduced.
        if let Some(pinned_pool) = device.opaque_fd_image_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_image_sentinel(
                device,
                pinned_pool,
                "opaque_fd_image",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::Format::R8G8B8A8_UNORM,
                ExportPoolSentinelImageTiling::Optimal,
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )?);
        }

        for sentinel in &sentinels {
            tracing::info!(
                "HostVulkanDevice export pool sentinel retained: {} ({} bytes)",
                sentinel.label,
                sentinel.size,
            );
        }
        Ok(sentinels)
    }
}

impl HostVulkanDevice {
    /// Build VMA pools dedicated to DMA-BUF exportable allocations.
    ///
    /// Each pool is pinned to a memory type that supports the relevant property
    /// flags and carries VkExportMemoryAllocateInfo::DMA_BUF_EXT via
    /// pMemoryAllocateNext. The export info structs are heap-boxed and returned
    /// alongside the pools — the caller must keep them alive for the pool's
    /// lifetime (VMA stores raw pointers to them).
    ///
    /// The third pool (tiled) is conditional on `tiled_modifiers` being
    /// non-empty — we need at least one render-target-capable DRM modifier to
    /// create the probe image. When the EGL probe returned no modifiers (no
    /// libEGL, no display, extension missing) the tiled pool is `None` and
    /// callers fall back to refusing render-target allocations.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_dma_buf_pools(
        allocator: &Arc<vma::Allocator>,
        device: &vulkanalia::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        tiled_modifiers: &[u64],
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        VmaExportPoolPinnedToOneMemoryType,
        Option<VmaExportPoolPinnedToOneMemoryType>,
        Box<vk::ExportMemoryAllocateInfo>,
        Box<vk::ExportMemoryAllocateInfo>,
        Option<Box<vk::ExportMemoryAllocateInfo>>,
    )> {
        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        // The probe must mirror the real buffer create info used by
        // `HostVulkanBuffer::new`, including the `ExternalMemoryBufferCreateInfo`
        // pNext chain — DMA-BUF external buffers have a narrower
        // `memoryTypeBits` than plain buffers, and omitting the chain lets VMA
        // pick a memory type the real buffer won't accept at bind time
        // (VUID-vkBindBufferMemory-memory-01035).
        let mut probe_buffer_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_buffer_external_info);
        let probe_buffer_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let buffer_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_buffer_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF buffer pool: {e}")))?;

        // ── Find memory type for DEVICE_LOCAL DMA-BUF exportable images ──
        // Same rationale: the real image (`HostVulkanTexture::new`) carries
        // `ExternalMemoryImageCreateInfo::DMA_BUF_EXT` which can narrow
        // `memoryTypeBits`.
        let mut probe_image_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_image_external_info);
        let probe_image_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let image_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_image_info(probe_image_info, &probe_image_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF image pool: {e}")))?;

        // ── Box the VkExportMemoryAllocateInfo structs (need stable pointers) ──
        let mut buffer_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );
        let mut image_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );

        // ── Create the pools ──────────────────────────────────────────────────
        // VMA's pMemoryAllocateNext stores a raw pointer to the export info.
        // Box gives a stable heap address — we keep the Boxes alive by returning
        // them alongside the pools. Drop order (handled by HostVulkanDevice::drop):
        // pool → Box, so the pointer is valid for the pool's entire lifetime.
        let mut buffer_pool_options = vma::PoolOptions::default();
        buffer_pool_options = buffer_pool_options.push_next(buffer_export_info.as_mut());
        buffer_pool_options.memory_type_index = buffer_mem_type_idx;
        let buffer_pool = allocator
            .create_pool(&buffer_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF buffer pool: {e}")))?;

        let mut image_pool_options = vma::PoolOptions::default();
        image_pool_options = image_pool_options.push_next(image_export_info.as_mut());
        image_pool_options.memory_type_index = image_mem_type_idx;
        let image_pool = allocator
            .create_pool(&image_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF image pool: {e}")))?;

        // ── Tiled (DRM_FORMAT_MODIFIER) image pool ────────────────────────
        // Only created when the EGL probe returned at least one
        // render-target-capable modifier. VMA's
        // `find_memory_type_index_for_image_info` asserts on
        // `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it can't infer the plane
        // aspect for multi-plane modifiers), so we probe via raw
        // `vkCreateImage` + `vkGetImageMemoryRequirements` and pick a
        // DEVICE_LOCAL type ourselves. The probe image is destroyed before
        // the pool is created — VMA only needs the memory type index.
        let (tiled_pool, tiled_export_info) = if tiled_modifiers.is_empty() {
            (None, None)
        } else {
            match probe_tiled_dma_buf_memory_type(device, memory_properties, tiled_modifiers) {
                Ok(tiled_mem_type_idx) => {
                    let mut tiled_export_info = Box::new(
                        vk::ExportMemoryAllocateInfo::builder()
                            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                            .build(),
                    );
                    let mut tiled_pool_options = vma::PoolOptions::default();
                    tiled_pool_options = tiled_pool_options.push_next(tiled_export_info.as_mut());
                    tiled_pool_options.memory_type_index = tiled_mem_type_idx;
                    match allocator.create_pool(&tiled_pool_options) {
                        Ok(tiled_pool) => {
                            tracing::info!(
                                "DMA-BUF VMA tiled image pool created — mem_type={}, modifiers_probed={}",
                                tiled_mem_type_idx,
                                tiled_modifiers.len(),
                            );
                            (
                                Some(VmaExportPoolPinnedToOneMemoryType {
                                    pool: tiled_pool,
                                    memory_type_index: tiled_mem_type_idx,
                                }),
                                Some(tiled_export_info),
                            )
                        }
                        Err(e) => {
                            tracing::warn!(
                                "DMA-BUF VMA tiled image pool creation failed (mem_type={}): {e} — render-target DMA-BUF allocations will be unavailable",
                                tiled_mem_type_idx,
                            );
                            (None, None)
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Tiled DMA-BUF memory-type probe failed: {e} — render-target DMA-BUF allocations will be unavailable",
                    );
                    (None, None)
                }
            }
        };

        tracing::info!(
            "DMA-BUF VMA pools created — buffer mem_type={}, image mem_type={}, tiled={}",
            buffer_mem_type_idx,
            image_mem_type_idx,
            tiled_pool.is_some(),
        );

        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool: buffer_pool,
                memory_type_index: buffer_mem_type_idx,
            },
            VmaExportPoolPinnedToOneMemoryType {
                pool: image_pool,
                memory_type_index: image_mem_type_idx,
            },
            tiled_pool,
            buffer_export_info,
            image_export_info,
            tiled_export_info,
        ))
    }

    /// Probe the memory type index an OPAQUE_FD-exportable HOST_VISIBLE
    /// buffer pool must pin itself to under `host_access_pattern`.
    ///
    /// The probe shape mirrors `HostVulkanBuffer`'s real allocations —
    /// HOST_VISIBLE | HOST_COHERENT, TRANSFER_SRC | TRANSFER_DST |
    /// STORAGE_BUFFER, chained `VkExternalMemoryBufferCreateInfo::OPAQUE_FD`.
    /// The probe's `memoryTypeBits` must match the real buffer's or the
    /// bind fails with VUID-vkBindBufferMemory-memory-01035.
    #[cfg(target_os = "linux")]
    fn probe_opaque_fd_host_visible_buffer_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        host_access_pattern: MappedOpaqueFdBufferHostAccessPattern,
    ) -> Result<u32> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | host_access_pattern.vma_allocation_create_flags(),
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD buffer pool: {e}")))
    }

    /// Pin a VMA pool chaining `VkExportMemoryAllocateInfo::OPAQUE_FD` to
    /// `memory_type_index`.
    ///
    /// The export info `Box` is returned alongside the pool — callers
    /// must keep it alive for the pool's entire lifetime (VMA stores
    /// raw pointers via `pMemoryAllocateNext`).
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        memory_type_index: u32,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = memory_type_index;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD buffer pool: {e}")))?;
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Used by CUDA / OpenCL interop: the host allocates from this pool,
    /// `vkGetMemoryFdKHR` with `OPAQUE_FD` produces a kernel fd, and the
    /// remote API (`cudaImportExternalMemory` etc.) imports the same
    /// underlying memory by fd. Sibling of [`Self::create_dma_buf_pools`];
    /// kept separate because a VMA pool can chain only one
    /// `VkExportMemoryAllocateInfo` per pool, and OPAQUE_FD vs DMA_BUF_EXT
    /// are mutually-exclusive handle types.
    ///
    /// `HOST_ACCESS_SEQUENTIAL_WRITE` is the right hint for this pool's
    /// producer-write consumers; the CPU-read sibling is
    /// [`Self::create_opaque_fd_buffer_pool_host_cached`].
    #[cfg(target_os = "linux")]
    pub(super) fn create_opaque_fd_buffer_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::SequentialWrite,
        )?;
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool created — mem_type={}",
            memory_type_index
        );
        Ok((pinned_pool, export_info))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE
    /// buffers on a HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::create_opaque_fd_buffer_pool`], whose write-combined memory
    /// the CPU reads roughly an order of magnitude slower.
    ///
    /// `Ok(None)` is the device having no cached exportable type, not a
    /// failure: `HOST_ACCESS_RANDOM` makes VMA *prefer* HOST_CACHED and
    /// `vk_mem_alloc.h` is explicit that it "cannot require it", so the
    /// probe succeeds on a cache-less device and hands back an uncached
    /// index. Reading the probed index's flags is therefore the detection,
    /// not a probe error.
    ///
    /// HOST_COHERENT stays required: the child-side OPAQUE_FD import binds
    /// the memory as HOST_VISIBLE | HOST_COHERENT and no flush or
    /// invalidate exists anywhere on this path.
    #[cfg(target_os = "linux")]
    pub(super) fn create_opaque_fd_buffer_pool_host_cached(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<
        Option<(
            VmaExportPoolPinnedToOneMemoryType,
            Box<vk::ExportMemoryAllocateInfo>,
        )>,
    > {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::Random,
        )?;
        if !memory_type_index_is_host_cached(allocator.get_memory_properties(), memory_type_index) {
            return Ok(None);
        }
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (HOST_CACHED) created — mem_type={}",
            memory_type_index
        );
        Ok(Some((pinned_pool, export_info)))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// buffers (GPU-resident sibling of [`Self::create_opaque_fd_buffer_pool`]).
    ///
    /// Same `VkExportMemoryAllocateInfo::handleTypes = OPAQUE_FD`, but the
    /// memory-type probe excludes HOST_VISIBLE so VMA picks a true VRAM
    /// type instead of the BAR aperture or pinned-host fallback. Used by
    /// hot-path camera→cuda flows where pinned-host PCIe bandwidth is a
    /// real bottleneck (~16-32 GB/s vs ~600 GB/s on VRAM, RTX 3090).
    /// Pool storage is single-memory-type-index by VMA contract — that's
    /// why this is a separate pool from the HOST_VISIBLE one.
    #[cfg(target_os = "linux")]
    pub(super) fn create_opaque_fd_buffer_pool_device_local(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "find memory type for DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"
            ))
        })?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator.create_pool(&pool_options).map_err(|e| {
            Error::GpuError(format!("create DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"))
        })?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (DEVICE_LOCAL) created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// **images** — image-flavored sibling of
    /// [`Self::create_opaque_fd_buffer_pool_device_local`].
    ///
    /// Probe shape matches [`HostVulkanTexture::new_opaque_fd_export`](crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export)
    /// exactly so the pool's `memoryTypeBits` matches the real
    /// allocations' bind requirements (VUID-vkBindImageMemory-memory-01047):
    ///
    /// - `vk::Format::R8G8B8A8_UNORM` — the canonical CUDA-mappable
    ///   8-bit RGBA format. Other CUDA-mappable variants (`R16G16B16A16_SFLOAT`,
    ///   `R32G32B32A32_SFLOAT`) share the same DEVICE_LOCAL memory-type
    ///   bits on every Vulkan driver shipping today.
    /// - `vk::ImageTiling::OPTIMAL` — LINEAR returns
    ///   `CUDA_ERROR_INVALID_VALUE` from `cuExternalMemoryGetMappedMipmappedArray`;
    ///   no DRM modifier (OPAQUE_FD + `DRM_FORMAT_MODIFIER_EXT` is not
    ///   a valid combination on NVIDIA).
    /// - `VkExternalMemoryImageCreateInfo::OPAQUE_FD` — the export handle
    ///   type that pairs with `cudaImportExternalMemory(OPAQUE_FD)` on
    ///   the consumer side.
    /// - `DEDICATED_MEMORY` allocation flag — CUDA's
    ///   `cudaExternalMemoryGetMappedMipmappedArray` imports the whole
    ///   `VkDeviceMemory` block.
    ///
    /// VMA's `find_memory_type_index_for_image_info` works for OPTIMAL
    /// tiling (unlike `DRM_FORMAT_MODIFIER_EXT`, which requires the
    /// raw `vkCreateImage` + `vkGetImageMemoryRequirements` probe used
    /// in `probe_tiled_dma_buf_memory_type`).
    #[cfg(target_os = "linux")]
    pub(super) fn create_opaque_fd_image_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_image_info(probe_image_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD image pool: {e}")))?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD image pool: {e}")))?;

        tracing::info!(
            "OPAQUE_FD VMA image pool created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }

    /// Find a memory type that satisfies both the type filter and required properties.
    ///
    /// Used internally for DMA-BUF import (raw `vkAllocateMemory` path).
    /// VMA handles memory type selection for all non-import allocations.
    pub(super) fn find_memory_type(
        &self,
        type_filter: u32,
        required_properties: vk::MemoryPropertyFlags,
    ) -> Result<u32> {
        // When DEVICE_LOCAL is requested, prefer types NOT also HOST_VISIBLE
        // (main VRAM heap) over types that are HOST_VISIBLE (BAR aperture).
        if required_properties.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            // Pass 1: DEVICE_LOCAL without HOST_VISIBLE (main VRAM)
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                {
                    return Ok(i);
                }
            }
            // Pass 2: any DEVICE_LOCAL (including BAR aperture)
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                {
                    return Ok(i);
                }
            }
        }

        // Standard path: find first matching type
        for i in 0..self.memory_properties.memory_type_count {
            let type_supported = (type_filter & (1 << i)) != 0;
            let properties_supported = self.memory_properties.memory_types[i as usize]
                .property_flags
                .contains(required_properties);
            if type_supported && properties_supported {
                return Ok(i);
            }
        }
        Err(Error::GpuError(format!(
            "No suitable memory type found (filter: 0x{:x}, required: {:?})",
            type_filter, required_properties
        )))
    }
}
