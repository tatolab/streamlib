// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Creating a texture: the default exportable allocation, and the device-local one.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::device::vulkan_device::VmaFrameAllocationSource;
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::DeviceMemoryBudgetAdmission;

use super::{HostVkImageMeta, HostVulkanTexture, texture_format_to_vk, texture_usages_to_vk};

impl HostVulkanTexture {
    /// Create a Vulkan texture — DMA-BUF exportable through the device's
    /// dedicated VMA export pool on a device that exports DMA-BUF, a local
    /// allocation on one that does not.
    ///
    /// The export pool is configured with `pMemoryAllocateNext` set to
    /// `VkExportMemoryAllocateInfo::DMA_BUF_EXT`, isolating exportable
    /// allocations from the default VMA pool. This avoids NVIDIA driver
    /// failures where global export configuration causes OOM after swapchain
    /// creation.
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, desc: &TextureDescriptor) -> Result<Self> {
        Self::new_with_device_memory_budget_admission(
            vulkan_device,
            desc,
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
        )
    }

    /// [`Self::new`], weighed against the device-memory budget as `admission` says.
    pub(crate) fn new_with_device_memory_budget_admission(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
        admission: DeviceMemoryBudgetAdmission,
    ) -> Result<Self> {
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // Declare DMA-BUF handle type at image creation — required by Vulkan spec
        // (VUID-vkBindImageMemory-memory-02728) when memory will be allocated with
        // VkExportMemoryAllocateInfo. Omitted on a device that exports no
        // DMA-BUF: there no DMA-BUF pool exists, and a driver without the
        // handle type refuses the image outright with FEATURE_NOT_PRESENT.
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image_info = if vulkan_device.supports_dma_buf_export() {
            image_info.push_next(&mut external_image_info)
        } else {
            image_info
        };

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        // Prefer the DMA-BUF image pool; fall back to default allocator (no export)
        // if the pool isn't available (a device that exports no DMA-BUF).
        #[cfg(target_os = "linux")]
        let allocation_source = VmaFrameAllocationSource::export_pool_or_the_default_allocator(
            vulkan_device.dma_buf_image_pool_pinned_to_one_memory_type(),
        );
        #[cfg(not(target_os = "linux"))]
        let allocation_source = VmaFrameAllocationSource::DefaultAllocator;
        let (image, allocation) = vulkan_device
            .create_frame_image_within_the_device_memory_budget(
                admission,
                allocation_source,
                &image_info,
                &alloc_opts,
                "HostVulkanTexture::new",
                |e| Error::GpuError(format!("Failed to create exportable image: {e}")),
            )?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
            #[cfg(target_os = "linux")]
            is_opaque_fd_export: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Create a non-exportable DEVICE_LOCAL texture via the default VMA allocator.
    ///
    /// Unlike [`new`] which uses the DMA-BUF export pool, this uses the default
    /// VMA allocator with no external memory info. For same-process textures that
    /// don't need cross-process sharing.
    pub fn new_device_local(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let alloc_opts = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let (image, allocation) = vulkan_device
            .create_frame_image_within_the_device_memory_budget(
                DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
                VmaFrameAllocationSource::DefaultAllocator,
                &image_info,
                &alloc_opts,
                "HostVulkanTexture::new_device_local",
                |e| Error::GpuError(format!("Failed to create device-local image: {e}")),
            )?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
            #[cfg(target_os = "linux")]
            is_opaque_fd_export: false,
            #[cfg(target_os = "linux")]
            chosen_drm_format_modifier: 0,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }
}
