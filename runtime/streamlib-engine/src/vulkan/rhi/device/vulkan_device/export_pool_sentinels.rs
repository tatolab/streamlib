// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};
use crate::vulkan::rhi::device::drm_modifier_probe;

use super::HostVulkanDevice;
use super::export_memory_pools::{
    MappedOpaqueFdBufferHostAccessPattern, VmaExportPoolPinnedToOneMemoryType,
};

/// Internal anti-decay sentinel for an export-capable VMA pool.
///
/// Frees its allocation on drop, so it must drop before the pool it was
/// allocated from: VMA aborts when a pool is destroyed with a live
/// dedicated allocation. Holds the allocator rather than an
/// `Arc<HostVulkanDevice>` so it can be stored on the device itself
/// without creating a reference cycle.
pub(super) struct ExportPoolSentinel {
    allocator: Arc<vma::Allocator>,
    pub(super) resource: ExportPoolSentinelResource,
    pub(super) allocation: vma::Allocation,
    /// Diagnostic label for the pool this sentinel covers.
    pub(super) label: &'static str,
    /// Bytes the sentinel's buffer or image asked for; the driver may round
    /// the allocation up.
    pub(super) size: vk::DeviceSize,
}

/// Width and height of every image-backed [`ExportPoolSentinel`].
const EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS: u32 = 8;

/// Bytes every [`ExportPoolSentinel`] asks for: an 8×8 four-byte-per-pixel
/// image's worth. Kept tiny on purpose: a consumer-sized OPAQUE_FD sentinel
/// deterministically blocked the consumer's own allocation on NVIDIA.
const EXPORT_POOL_SENTINEL_BYTES: vk::DeviceSize = (EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS
    as vk::DeviceSize)
    * (EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS as vk::DeviceSize)
    * 4;

/// How an image-backed [`ExportPoolSentinel`] is tiled — the same tiling the
/// pool's real frames use, so its memory-type bits admit the pool's type.
#[derive(Clone, Copy)]
enum ExportPoolSentinelImageTiling<'a> {
    /// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier.
    Optimal,
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, the driver picking one of
    /// these render-target-capable modifiers.
    DrmFormatModifierFromCandidates(&'a [u64]),
}

/// The Vulkan object an [`ExportPoolSentinel`]'s allocation is bound to.
pub(super) enum ExportPoolSentinelResource {
    Buffer(vk::Buffer),
    Image(vk::Image),
}

impl Drop for ExportPoolSentinel {
    fn drop(&mut self) {
        match self.resource {
            ExportPoolSentinelResource::Buffer(buffer) => unsafe {
                self.allocator.destroy_buffer(buffer, self.allocation);
            },
            ExportPoolSentinelResource::Image(image) => unsafe {
                self.allocator.destroy_image(image, self.allocation);
            },
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
