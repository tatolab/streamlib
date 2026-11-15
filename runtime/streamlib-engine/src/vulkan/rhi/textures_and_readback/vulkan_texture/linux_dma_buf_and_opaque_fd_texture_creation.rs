// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Creating the Linux textures that cross a process: render-target DMA-BUFs and OPAQUE_FD export images.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::device::vulkan_device::VmaFrameAllocationSource;
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::DeviceMemoryBudgetAdmission;

use super::{HostVkImageMeta, HostVulkanTexture, texture_format_to_vk, texture_usages_to_vk};

/// The usage an OPAQUE_FD export image of `format` is created with: the
/// request's plus the transfer bits, and for a CUDA-mappable format every
/// usage a CUDA-mapped image carries.
fn vk_usage_of_an_opaque_fd_export_image(
    format: TextureFormat,
    usage: TextureUsages,
) -> vk::ImageUsageFlags {
    let mut usage = usage | TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
    if format.is_cuda_mappable() {
        usage |= TextureUsages::EVERY_USAGE_A_CUDA_MAPPED_IMAGE_CARRIES;
    }
    texture_usages_to_vk(usage)
}

impl HostVulkanTexture {
    /// Create a render-target-capable DMA-BUF exportable texture using
    /// `VK_EXT_image_drm_format_modifier`.
    ///
    /// `modifier_candidates` MUST come from
    /// [`crate::vulkan::rhi::device::drm_modifier_probe::DrmModifierTable::rt_modifiers`]
    /// — every entry has `external_only=FALSE` per the EGL probe, so the
    /// exported FD can be imported on the consumer side as a
    /// `GL_TEXTURE_2D` and bound as an FBO color attachment. The driver
    /// picks one modifier from the list at allocation time; the choice is
    /// available via [`Self::chosen_drm_format_modifier`] after
    /// construction.
    ///
    /// Empty `modifier_candidates` ⇒ `Err` — there is no fallback to
    /// linear at this entry point because linear DMA-BUFs are sampler-only
    /// on NVIDIA Linux (see
    /// NVIDIA EGL's sampler-only linear DMA-BUFs). Callers that
    /// want a linear allocation should use [`Self::new`].
    pub fn new_render_target_dma_buf(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
        modifier_candidates: &[u64],
    ) -> Result<Self> {
        Self::new_render_target_dma_buf_with_device_memory_budget_admission(
            vulkan_device,
            desc,
            modifier_candidates,
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
        )
    }

    /// [`Self::new_render_target_dma_buf`], weighed against the device-memory
    /// budget as `admission` says.
    pub(crate) fn new_render_target_dma_buf_with_device_memory_budget_admission(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
        modifier_candidates: &[u64],
        admission: DeviceMemoryBudgetAdmission,
    ) -> Result<Self> {
        if modifier_candidates.is_empty() {
            return Err(Error::GpuError(
                "new_render_target_dma_buf: empty modifier list — EGL did not advertise an external_only=FALSE modifier for this format. Linear DMA-BUF is sampler-only on NVIDIA; refusing to allocate.".into(),
            ));
        }

        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = texture_usages_to_vk(desc.usage);

        // VK_EXT_image_drm_format_modifier requires the modifier list to
        // outlive the ImageCreateInfo. Hold the slice in a local — the
        // builder borrows from it via the pNext chain pointer, and the
        // chain is consumed by create_image before this function returns.
        let mut modifier_list_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
            .drm_format_modifiers(modifier_candidates);

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

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
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut modifier_list_info)
            .push_next(&mut external_image_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        // Prefer the dedicated tiled DMA-BUF image pool; fall back to the
        // default allocator (still with the export-info pNext chain) only
        // when external memory isn't supported. The pool's underlying
        // `VkDeviceMemory` block is pre-warmed at `HostVulkanDevice::new()`
        // (see `nvidia-dma-buf-after-swapchain.md`), so the post-swapchain
        // NVIDIA cap doesn't apply to the pooled path.
        let (image, allocation) = vulkan_device
            .create_frame_image_within_the_device_memory_budget(
                admission,
                VmaFrameAllocationSource::export_pool_or_the_default_allocator(
                    vulkan_device.dma_buf_image_pool_tiled_pinned_to_one_memory_type(),
                ),
                &image_info,
                &alloc_opts,
                "HostVulkanTexture::new_render_target_dma_buf",
                |e| {
                    Error::GpuError(format!(
                        "Failed to create render-target DMA-BUF image (modifiers={:?}): {e}",
                        modifier_candidates
                    ))
                },
            )?;

        // Read back which modifier the driver actually chose.
        let chosen = {
            use vulkanalia::vk::ExtImageDrmFormatModifierExtensionDeviceCommands;
            let mut props = vk::ImageDrmFormatModifierPropertiesEXT::default();
            let device = vulkan_device.device();
            unsafe { device.get_image_drm_format_modifier_properties_ext(image, &mut props) }
                .map_err(|e| {
                    // Image leaks on this branch — the allocator owns it. We
                    // destroy it explicitly so the caller doesn't need to.
                    unsafe { vulkan_device.allocator().destroy_image(image, allocation) };
                    Error::GpuError(format!(
                        "vkGetImageDrmFormatModifierPropertiesEXT failed: {e}"
                    ))
                })?;
            props.drm_format_modifier
        };

        if !modifier_candidates.contains(&chosen) {
            unsafe { vulkan_device.allocator().destroy_image(image, allocation) };
            return Err(Error::GpuError(format!(
                "Driver picked modifier 0x{:016x} that wasn't in our candidate list {:?} — VUID violation",
                chosen, modifier_candidates
            )));
        }

        tracing::info!(
            "HostVulkanTexture render-target DMA-BUF: {}x{} {:?} → modifier 0x{:016x}",
            desc.width,
            desc.height,
            desc.format,
            chosen
        );

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            imported_memory: None,
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
            chosen_drm_format_modifier: chosen,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Whether `vulkan_device` reports it can export the image
    /// [`Self::new_opaque_fd_export`] creates for `format` and `usage` as an
    /// OPAQUE_FD — asked of the device, never assumed from its driver.
    pub fn device_exports_an_opaque_fd_image_of_format_and_usage(
        vulkan_device: &HostVulkanDevice,
        format: TextureFormat,
        usage: TextureUsages,
    ) -> bool {
        if format.plane_count() != 1 {
            return false;
        }
        let mut external_image_format_info = vk::PhysicalDeviceExternalImageFormatInfo::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .build();
        let image_format_info = vk::PhysicalDeviceImageFormatInfo2::builder()
            .format(texture_format_to_vk(format))
            .type_(vk::ImageType::_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk_usage_of_an_opaque_fd_export_image(format, usage))
            .push_next(&mut external_image_format_info)
            .build();
        let mut external_image_format_properties = vk::ExternalImageFormatProperties::default();
        let mut image_format_properties = vk::ImageFormatProperties2::builder()
            .push_next(&mut external_image_format_properties)
            .build();
        // VK_ERROR_FORMAT_NOT_SUPPORTED is the device's "no" for the
        // combination as a whole.
        let queried = unsafe {
            vulkan_device
                .instance()
                .get_physical_device_image_format_properties2(
                    vulkan_device.physical_device(),
                    &image_format_info,
                    &mut image_format_properties,
                )
        };
        queried.is_ok()
            && external_image_format_properties
                .external_memory_properties
                .external_memory_features
                .contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
    }

    /// Allocate an OPAQUE_FD-exportable DEVICE_LOCAL OPTIMAL `VkImage` from
    /// [`HostVulkanDevice::opaque_fd_image_pool`] — the cross-process image
    /// flavor of a device without DMA-BUF, and the Vulkan → CUDA
    /// `cudaExternalMemoryGetMappedMipmappedArray` interop flavor of one with.
    ///
    /// Pairs with [`Self::export_opaque_fd_memory`] and
    /// [`streamlib_consumer_rhi::ConsumerVulkanTexture::from_opaque_fd`].
    ///
    /// - **Format** is any single-plane format; keeping to the ones CUDA
    ///   maps ([`TextureFormat::is_cuda_mappable`]) is the caller's.
    /// - **Tiling** is OPTIMAL with no DRM modifier chain: LINEAR returns
    ///   `CUDA_ERROR_INVALID_VALUE` from the mipmapped-array import, and
    ///   OPAQUE_FD + `DRM_FORMAT_MODIFIER_EXT` is not valid on NVIDIA.
    /// - **Usage** is the descriptor's plus `TRANSFER_SRC | TRANSFER_DST`;
    ///   a CUDA-mappable format also always carries `SAMPLED | STORAGE`, the
    ///   texture- and surface-object backings CUDA binds.
    /// - **Memory** is `DEDICATED_MEMORY`: the import takes the whole
    ///   `VkDeviceMemory` block.
    ///
    /// Returns `Err` when the pool is unavailable, the format is
    /// multi-plane, or the device does not report the format and usage as
    /// OPAQUE_FD-exportable. Callers must not fall back to a non-exportable
    /// allocation — the failure would surface only at `vkGetMemoryFdKHR`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(width = desc.width, height = desc.height, format = ?desc.format))]
    pub fn new_opaque_fd_export(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        if desc.format.plane_count() != 1 {
            return Err(Error::Configuration(format!(
                "HostVulkanTexture::new_opaque_fd_export: format {:?} is multi-plane; an \
                 OPAQUE_FD image crosses as one memory block no import can split into planes",
                desc.format
            )));
        }
        if desc.width == 0 || desc.height == 0 {
            return Err(Error::Configuration(
                "HostVulkanTexture::new_opaque_fd_export: width and height must be > 0".into(),
            ));
        }

        if !Self::device_exports_an_opaque_fd_image_of_format_and_usage(
            vulkan_device,
            desc.format,
            desc.usage,
        ) {
            return Err(Error::NotSupported(format!(
                "HostVulkanTexture::new_opaque_fd_export: {} cannot export an optimal {} \
                 image with usage {} as OPAQUE_FD",
                vulkan_device.name(),
                desc.format.wire_name(),
                crate::core::rhi::texture_usages_to_wire(desc.usage).join(" | "),
            )));
        }

        let vk_format = texture_format_to_vk(desc.format);
        let usage_flags = vk_usage_of_an_opaque_fd_export_image(desc.format, desc.usage);

        // Declare OPAQUE_FD handle type on the image create info; the
        // matching ExportMemoryAllocateInfo lives on the pool's
        // pNext chain.
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
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
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_image_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        let opaque_fd_image_pool = vulkan_device
            .opaque_fd_image_pool_pinned_to_one_memory_type()
            .ok_or_else(|| {
                Error::GpuError(
                    "OPAQUE_FD image pool unavailable — external memory unsupported \
                     or pool construction failed; CUDA `VkImage` interop requires this pool"
                        .into(),
                )
            })?;
        let (image, allocation) = vulkan_device
            .create_frame_image_within_the_device_memory_budget(
                DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
                VmaFrameAllocationSource::ExportPool(opaque_fd_image_pool),
                &image_info,
                &alloc_opts,
                "HostVulkanTexture::new_opaque_fd_export",
                |e| {
                    Error::GpuError(format!(
                        "Failed to create OPAQUE_FD exportable image ({}x{} {:?}): {e}",
                        desc.width, desc.height, desc.format,
                    ))
                },
            )?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: Some(allocation),
            imported_memory: None,
            imported_memory_size: 0,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: true,
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
