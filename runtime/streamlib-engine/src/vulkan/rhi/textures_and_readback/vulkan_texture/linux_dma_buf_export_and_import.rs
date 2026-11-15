// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Exporting a Linux texture as a DMA-BUF or OPAQUE_FD, its plane layout, and importing a DMA-BUF.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::TextureFormat;
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;

use super::{HostVkImageMeta, HostVulkanTexture, texture_format_to_vk};

/// Aspect mask `vkGetImageSubresourceLayout` requires to report memory
/// plane `plane_index` of a DMA-BUF-exportable image.
///
/// `VUID-vkGetImageSubresourceLayout-tiling-09433` binds the aspect to the
/// tiling the image was *created* with, never to the modifier the driver
/// picked: `DRM_FORMAT_MOD_LINEAR` is numerically zero, so a modifier-tiled
/// image is indistinguishable from an image created without a modifier by
/// the modifier's value alone.
pub(super) fn dma_buf_plane_layout_query_aspect_mask(
    vk_image_tiling: vk::ImageTiling,
    format: TextureFormat,
    plane_index: usize,
) -> Result<vk::ImageAspectFlags> {
    const MEMORY_PLANE_ASPECTS: [vk::ImageAspectFlags; 4] = [
        vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
        vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
    ];
    const FORMAT_PLANE_ASPECTS: [vk::ImageAspectFlags; 2] =
        [vk::ImageAspectFlags::PLANE_0, vk::ImageAspectFlags::PLANE_1];
    const COLOR_ASPECTS: [vk::ImageAspectFlags; 1] = [vk::ImageAspectFlags::COLOR];

    let (aspects, image_shape) = if vk_image_tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
        (&MEMORY_PLANE_ASPECTS[..], "modifier-tiled")
    } else if vk_image_tiling == vk::ImageTiling::LINEAR {
        if matches!(format, TextureFormat::Nv12) {
            (&FORMAT_PLANE_ASPECTS[..], "linear-tiled NV12")
        } else {
            (&COLOR_ASPECTS[..], "linear-tiled single-plane")
        }
    } else {
        return Err(Error::GpuError(format!(
            "dma_buf_plane_layout: {vk_image_tiling:?} images have driver-opaque layouts — \
             only LINEAR and DRM_FORMAT_MODIFIER_EXT expose queryable plane layouts"
        )));
    };

    aspects.get(plane_index).copied().ok_or_else(|| {
        Error::GpuError(format!(
            "dma_buf_plane_layout: plane index {plane_index} out of range — this RHI names \
             {} plane aspect(s) for a {image_shape} image",
            aspects.len()
        ))
    })
}

impl HostVulkanTexture {
    /// DRM format modifier the driver picked at allocation time.
    ///
    /// Zero for textures created via [`Self::new`] / [`Self::new_device_local`]
    /// or imported via [`Self::from_dma_buf_fd`] — those paths do not go
    /// through `VK_EXT_image_drm_format_modifier`. Render-target textures
    /// allocated via [`Self::new_render_target_dma_buf`] return the modifier
    /// the driver chose from the candidate list, which is zero when that
    /// choice was `DRM_FORMAT_MOD_LINEAR`. Read [`Self::vk_image_tiling`] to
    /// tell those two zeros apart.
    pub fn chosen_drm_format_modifier(&self) -> u64 {
        self.chosen_drm_format_modifier
    }

    /// Whether this is a DMA-BUF image with an explicit, non-LINEAR DRM
    /// format modifier — the memory a display compositor or Chromium
    /// imports and samples on NVIDIA, where a LINEAR import is sampler-only
    /// and shows black.
    pub fn is_a_dma_buf_image_with_a_tiled_drm_format_modifier(&self) -> bool {
        const DRM_FORMAT_MOD_LINEAR: u64 = 0;
        !self.is_opaque_fd_export
            && self.vk_image_meta.vk_image_tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
            && self.chosen_drm_format_modifier != DRM_FORMAT_MOD_LINEAR
    }

    /// Per-plane DMA-BUF layout for this texture, in plane index order.
    ///
    /// Each entry is `(offset_bytes, row_pitch_bytes)` from
    /// `vkGetImageSubresourceLayout` — the values consumer-side EGL imports
    /// must pass in `EGL_DMA_BUF_PLANE{N}_OFFSET_EXT` /
    /// `EGL_DMA_BUF_PLANE{N}_PITCH_EXT`.
    ///
    /// For single-plane formats (BGRA/RGBA) returns one entry. NV12 returns
    /// two (Y plane, then UV). Returns `Err` for textures without a backing
    /// image or when the format's plane count isn't supported by this RHI
    /// build.
    pub fn dma_buf_plane_layout(&self) -> Result<Vec<(u64, u64)>> {
        let vk_dev = self
            .vulkan_device
            .as_ref()
            .ok_or_else(|| Error::GpuError("dma_buf_plane_layout: no HostVulkanDevice".into()))?;
        let image = self
            .image
            .ok_or_else(|| Error::GpuError("dma_buf_plane_layout: no image".into()))?;

        // `vkGetImageSubresourceLayout` (the call below) requires LINEAR
        // or DRM_FORMAT_MODIFIER tiling per VUID-vkGetImageSubresourceLayout-image-07790.
        // OPTIMAL-tiled images have driver-opaque layouts; their plane
        // strides aren't meaningful for DMA-BUF export (those textures
        // aren't DMA-BUF importable anyway). Refuse the query so the
        // caller (`surface_store::register_texture`) takes the fallback
        // `vec![(0, 0)]` path it already tolerates.
        if self.vk_image_meta.vk_image_tiling == vk::ImageTiling::OPTIMAL {
            return Err(Error::GpuError(
                "dma_buf_plane_layout: not applicable for VK_IMAGE_TILING_OPTIMAL textures \
                 (only LINEAR or DRM_FORMAT_MODIFIER tilings expose queryable plane layouts)"
                    .into(),
            ));
        }

        let plane_count = match self.format {
            TextureFormat::Nv12 => 2,
            _ => 1,
        };

        let mut planes = Vec::with_capacity(plane_count);
        for plane_idx in 0..plane_count {
            let aspect_mask = dma_buf_plane_layout_query_aspect_mask(
                self.vk_image_meta.vk_image_tiling,
                self.format,
                plane_idx,
            )?;

            let subres = vk::ImageSubresource::builder()
                .aspect_mask(aspect_mask)
                .mip_level(0)
                .array_layer(0)
                .build();
            let layout = unsafe { vk_dev.device().get_image_subresource_layout(image, &subres) };
            planes.push((layout.offset, layout.row_pitch));
        }

        Ok(planes)
    }

    /// True iff this texture was allocated via
    /// [`Self::new_opaque_fd_export`]. Gates the OPAQUE_FD export
    /// accessor.
    pub fn is_opaque_fd_export(&self) -> bool {
        self.is_opaque_fd_export
    }

    /// Allocation size in bytes from VMA, used to thread the
    /// `allocation_size` argument across to
    /// [`streamlib_consumer_rhi::ConsumerVulkanTexture::from_opaque_fd`]
    /// on the consumer side. Returns 0 for placeholders / imported
    /// images (where the allocation lives on the foreign side).
    pub fn vma_allocation_size(&self) -> vk::DeviceSize {
        let Some(allocation) = self.allocation.as_ref() else {
            return 0;
        };
        let Some(vk_dev) = self.vulkan_device.as_ref() else {
            return 0;
        };
        vk_dev.allocator().get_allocation_info(*allocation).size as vk::DeviceSize
    }

    /// Memory type index (`vmaGetAllocationInfo().memoryType`) of this
    /// texture's allocation — what a conforming consumer-side
    /// `vkAllocateMemory(VkImportMemoryFdInfoKHR)` must state. `None` for
    /// placeholders / imported images (where the allocation lives on the
    /// foreign side); never defaulted, because every index including `0`
    /// is a real value.
    pub fn vma_allocation_memory_type_index(&self) -> Option<u32> {
        let allocation = self.allocation.as_ref()?;
        let vk_dev = self.vulkan_device.as_ref()?;
        Some(
            vk_dev
                .allocator()
                .get_allocation_info(*allocation)
                .memoryType,
        )
    }

    /// The owning device's `VkPhysicalDeviceIDProperties::deviceUUID` —
    /// the device-binding contract for an OPAQUE_FD export (importing on
    /// the wrong GPU of a multi-GPU rig corrupts silently). Sourced from
    /// this texture's own `HostVulkanDevice`, never the current
    /// `GpuContext`'s, per the buffer-export precedent. `None` for
    /// placeholders / imported images with no stored device.
    pub fn exporting_physical_device_uuid(&self) -> Option<[u8; 16]> {
        Some(self.vulkan_device.as_ref()?.physical_device_uuid())
    }

    /// Export the texture's OPAQUE_FD memory as a file descriptor.
    ///
    /// Only valid for textures created via [`Self::new_opaque_fd_export`];
    /// returns `Err` for DMA-BUF-flavored allocations (call
    /// [`Self::export_dma_buf_fd`] instead). Each call returns a fresh
    /// kernel fd (the driver dups internally) — the caller owns it and
    /// is responsible for closing it (or for transferring ownership via
    /// SCM_RIGHTS / `cudaImportExternalMemory`, both of which `dup`
    /// again on receipt).
    ///
    /// Mirrors `HostVulkanBuffer::export_opaque_fd_memory` for images.
    pub fn export_opaque_fd_memory(&self) -> Result<std::os::unix::io::RawFd> {
        if !self.is_opaque_fd_export {
            return Err(Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: texture was not created \
                 with `new_opaque_fd_export`; the underlying memory carries DMA_BUF_EXT \
                 (or no) export flags and OPAQUE_FD export will fail at the driver"
                    .into(),
            ));
        }
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: no HostVulkanDevice stored".into(),
            )
        })?;
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            Error::GpuError(
                "HostVulkanTexture::export_opaque_fd_memory: texture has no VMA allocation".into(),
            )
        })?;
        let alloc_info = vk_dev.allocator().get_allocation_info(*allocation);
        let memory = alloc_info.deviceMemory;

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        let fd = unsafe { vk_dev.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export OPAQUE_FD memory fd: {e}")))?;
        Ok(fd)
    }

    /// Export the texture's memory as a DMA-BUF file descriptor.
    ///
    /// Each call returns a fresh kernel fd (the driver dups internally) —
    /// the caller owns it and is responsible for closing it (or for
    /// transferring ownership via SCM_RIGHTS / an external-memory import,
    /// both of which `dup` again on receipt). The texture keeps no copy and
    /// will never close it.
    ///
    /// Refused by name for an allocation that carries no DMA-BUF export: an
    /// OPAQUE_FD image, and any image this engine allocated on a device that
    /// exports no DMA-BUF.
    pub fn export_dma_buf_fd(&self) -> Result<std::os::unix::io::RawFd> {
        let vk_dev = self.vulkan_device.as_ref().ok_or_else(|| {
            Error::GpuError("Cannot export DMA-BUF: no HostVulkanDevice stored".into())
        })?;
        if self.imported_memory.is_none() && self.is_opaque_fd_export {
            return Err(Error::NotSupported(format!(
                "HostVulkanTexture::export_dma_buf_fd: this texture's memory on {} is an \
                 OPAQUE_FD allocation, which exports no DMA-BUF",
                vk_dev.name()
            )));
        }
        if self.imported_memory.is_none() && !vk_dev.supports_dma_buf_export() {
            return Err(Error::NotSupported(format!(
                "HostVulkanTexture::export_dma_buf_fd: {} exports no DMA-BUF",
                vk_dev.name()
            )));
        }

        // Get DeviceMemory from raw allocation (export/import path) or VMA allocation
        let device_memory = if let Some(memory) = self.imported_memory {
            memory
        } else if let Some(allocation) = self.allocation.as_ref() {
            let alloc_info = vk_dev.allocator().get_allocation_info(*allocation);
            alloc_info.deviceMemory
        } else {
            return Err(Error::GpuError(
                "Cannot export DMA-BUF from texture without memory".into(),
            ));
        };

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(device_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;
        unsafe { vk_dev.device().get_memory_fd_khr(&get_fd_info) }
            .map_err(|e| Error::GpuError(format!("Failed to export DMA-BUF fd: {e}")))
    }

    /// Import a DMA-BUF image laid out by a tiled DRM format modifier, with
    /// the plane offsets and strides its exporter reported, as an image of
    /// `usage`.
    ///
    /// Consumes every plane fd: plane 0 is handed to the driver at
    /// `vkAllocateMemory`, whatever that call returns (see
    /// [`HostVulkanDevice::import_dma_buf_memory`]), and the rest close
    /// here — multi-plane binding is not wired yet.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one property of the exported image the import must match"
    )]
    pub fn import_dma_buf_with_drm_format_modifier(
        vulkan_device: &Arc<HostVulkanDevice>,
        plane_fds: Vec<std::os::fd::OwnedFd>,
        plane_offsets: &[u64],
        plane_strides: &[u64],
        drm_format_modifier: u64,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
        usage: streamlib_consumer_rhi::VulkanImageUsage,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::import_dma_buf_with_drm_format_modifier";
        if plane_fds.is_empty() {
            return Err(Error::GpuError(format!("{OPERATION}: no plane fd")));
        }
        if plane_offsets.len() != plane_fds.len() || plane_strides.len() != plane_fds.len() {
            return Err(Error::GpuError(format!(
                "{OPERATION}: plane arrays length mismatch — fds={} offsets={} strides={}",
                plane_fds.len(),
                plane_offsets.len(),
                plane_strides.len()
            )));
        }
        if drm_format_modifier == 0 {
            return Err(Error::GpuError(format!(
                "{OPERATION}: the linear modifier is no tiled layout; a linear DMA-BUF imports \
                 through HostVulkanTexture::from_dma_buf_fd"
            )));
        }
        let usage_flags = usage
            .as_vk_or_refusal()
            .map_err(|refusal| Error::GpuError(format!("{OPERATION}: {refusal}")))?;

        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);

        let plane_layouts: Vec<vk::SubresourceLayout> = plane_offsets
            .iter()
            .zip(plane_strides.iter())
            .map(|(off, stride)| vk::SubresourceLayout {
                offset: *off,
                size: 0,
                row_pitch: *stride,
                array_pitch: 0,
                depth_pitch: 0,
            })
            .collect();

        let mut explicit_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::builder()
            .drm_format_modifier(drm_format_modifier)
            .plane_layouts(&plane_layouts);

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut explicit_modifier_info)
            .push_next(&mut external_image_info);

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "{OPERATION}: create_image failed (modifier=0x{:016x}): {e}",
                drm_format_modifier
            ))
        })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

        // A single-plane BGRA or RGBA image binds plane 0's memory alone.
        let mut plane_fds = plane_fds.into_iter();
        let plane_0_fd = plane_fds.next().expect("checked non-empty above");
        drop(plane_fds);
        let memory = vulkan_device
            .import_dma_buf_memory(
                plane_0_fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .inspect_err(|_e| {
                unsafe { device.destroy_image(image, None) };
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_image(image, None) };
            Error::GpuError(format!("{OPERATION}: bind_image_memory failed: {e}"))
        })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
            chosen_drm_format_modifier: drm_format_modifier,
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// Import a texture from a DMA-BUF file descriptor.
    ///
    /// Takes the fd by value because the caller holds nothing after this
    /// returns: the fd is handed to the driver at `vkAllocateMemory` — the
    /// driver's from then on, whatever that call returned (see
    /// [`HostVulkanDevice::import_dma_buf_memory`]) — or closed here on an
    /// exit before that point.
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<HostVulkanDevice>,
        dma_buf_fd: std::os::fd::OwnedFd,
        width: u32,
        height: u32,
        format: TextureFormat,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let vk_format = texture_format_to_vk(format);
        let usage_flags = vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::SAMPLED;

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(usage_flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        let image = unsafe { device.create_image(&image_info, None) }.map_err(|e| {
            Error::GpuError(format!("Failed to create image for DMA-BUF import: {e}"))
        })?;

        let mem_requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc_size = allocation_size.max(mem_requirements.size);

        // VMA cannot import external memory — use raw import path in the RHI
        let memory = vulkan_device
            .import_dma_buf_memory(
                dma_buf_fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .inspect_err(|_e| {
                unsafe { device.destroy_image(image, None) };
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }
            .map(|_| ())
            .map_err(|e| {
                vulkan_device.free_imported_memory(memory);
                unsafe { device.destroy_image(image, None) };
                Error::GpuError(format!("Failed to bind imported memory: {e}"))
            })?;

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: alloc_size,
            cached_image_view: OnceLock::new(),
            is_opaque_fd_export: false,
            chosen_drm_format_modifier: 0,
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::LINEAR,
                vk_image_usage_flags: usage_flags,
            },
        })
    }
}
