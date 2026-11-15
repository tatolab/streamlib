// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A macOS texture over an IOSurface: creating one, adopting one, and handing it across as a Mach port.

use std::sync::{Arc, OnceLock};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{TextureDescriptor, TextureFormat};
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::vulkan_frame_allocation_device_memory_budget::DeviceMemoryBudgetAdmission;
use crate::vulkan::rhi::vulkan_metal_working_set_budget::frame_device_memory_request_for_a_private_iosurface;

use super::{HostVkImageMeta, HostVulkanTexture, texture_format_to_vk, texture_usages_to_vk};

impl HostVulkanTexture {
    /// An image whose storage is a fresh private IOSurface, so it can cross to
    /// a helper process as a Mach port.
    ///
    /// MoltenVK binds the surface when the image is created — there is no
    /// attach after the fact — and checks only its width, height and
    /// bytes-per-element against the format's block size. The image stays
    /// `OPTIMAL`; its rows are the surface's, at the surface's stride. The
    /// binding takes a device-local memory type that is not host-visible:
    /// MoltenVK backs a host-visible binding with a private `MTLBuffer` of its
    /// own for every image.
    pub fn new_iosurface_backed(
        vulkan_device: &Arc<HostVulkanDevice>,
        desc: &TextureDescriptor,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::new_iosurface_backed";
        if desc.format.plane_count() != 1 {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: {:?} is planar; an IOSurface-backed image carries one plane",
                desc.format
            )));
        }
        vulkan_device.allocate_as_the_device_memory_budget_admits(
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget,
            || {
                Ok(frame_device_memory_request_for_a_private_iosurface(
                    desc.width,
                    desc.height,
                    desc.format.bytes_per_pixel(),
                    OPERATION,
                ))
            },
            || {
                let iosurface = crate::apple::iosurface::create_private_iosurface_for_a_gpu_image(
                    desc.width,
                    desc.height,
                    desc.format,
                )?;
                Self::created_over_iosurface(
                    vulkan_device,
                    desc.format,
                    texture_usages_to_vk(desc.usage),
                    iosurface,
                )
            },
        )
    }

    /// An image over an IOSurface another process allocated — a registered
    /// texture's surface, resolved from its Mach port. Refused when the
    /// surface's element size is not `format`'s, or `usage` carries a bit no
    /// `VkImageUsageFlagBits` names.
    pub fn from_iosurface(
        vulkan_device: &Arc<HostVulkanDevice>,
        iosurface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
        format: TextureFormat,
        usage: streamlib_consumer_rhi::VulkanImageUsage,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::from_iosurface";
        if let Some(refusal) =
            streamlib_consumer_rhi::refusal_of_an_iosurface_for_an_image_of_format(
                &iosurface, format,
            )
        {
            return Err(Error::NotSupported(format!("{OPERATION}: {refusal}")));
        }
        let usage_flags = usage
            .as_vk_or_refusal()
            .map_err(|refusal| Error::NotSupported(format!("{OPERATION}: {refusal}")))?;
        Self::created_over_iosurface(vulkan_device, format, usage_flags, iosurface)
    }

    /// The image over `iosurface`, taking its extent from the surface; see
    /// [`streamlib_consumer_rhi::create_image_over_iosurface`] for the
    /// contract.
    fn created_over_iosurface(
        vulkan_device: &Arc<HostVulkanDevice>,
        format: TextureFormat,
        usage_flags: vk::ImageUsageFlags,
        iosurface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
    ) -> Result<Self> {
        const OPERATION: &str = "HostVulkanTexture::created_over_iosurface";
        if !vulkan_device.supports_metal_objects_interop() {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: VK_EXT_metal_objects is not enabled on this device, so an image \
                 cannot be created over an IOSurface"
            )));
        }
        let (width, height) = (iosurface.width() as u32, iosurface.height() as u32);
        let device = vulkan_device.device();
        // SAFETY: the extension is enabled (checked above) and `iosurface` is
        // retained below for the image's whole life.
        let image = unsafe {
            streamlib_consumer_rhi::create_image_over_iosurface(
                device,
                &iosurface,
                width,
                height,
                texture_format_to_vk(format),
                usage_flags,
            )
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "{OPERATION}: the driver refused a {width}x{height} {format:?} image over an \
                 IOSurface: {e}"
            ))
        })?;
        // SAFETY: `image` was just created on this device.
        let memory_requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory = vulkan_device
            .allocate_device_local_memory_for_an_iosurface_backed_image(
                memory_requirements.size,
                memory_requirements.memory_type_bits,
            )
            // SAFETY: nothing else holds the image yet.
            .inspect_err(|_| unsafe { device.destroy_image(image, None) })?;
        // SAFETY: `memory` was allocated for this image's requirements.
        if let Err(e) = unsafe { device.bind_image_memory(image, memory, 0) } {
            // SAFETY: nothing else holds either handle yet.
            unsafe { device.destroy_image(image, None) };
            vulkan_device.free_imported_memory(memory);
            return Err(Error::GpuError(format!(
                "{OPERATION}: binding the IOSurface-backed image's memory failed: {e}"
            )));
        }

        Ok(Self {
            vulkan_device: Some(Arc::clone(vulkan_device)),
            image: Some(image),
            allocation: None,
            imported_memory: Some(memory),
            imported_memory_size: memory_requirements.size,
            cached_image_view: OnceLock::new(),
            backing_iosurface: Some(
                crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
            ),
            width,
            height,
            format,
            vk_image_meta: HostVkImageMeta {
                vk_image_tiling: vk::ImageTiling::OPTIMAL,
                vk_image_usage_flags: usage_flags,
            },
        })
    }

    /// The IOSurface this image's storage is, when it was allocated to cross
    /// to a helper process.
    pub fn backing_iosurface(
        &self,
    ) -> Option<&crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads> {
        self.backing_iosurface.as_ref()
    }

    /// A fresh send right naming this image's IOSurface, for the surface-share
    /// wire. Refused for an image that is not IOSurface-backed.
    pub fn export_iosurface_mach_send_right(
        &self,
    ) -> Result<streamlib_surface_client::OwnedMachSendRight> {
        let iosurface = self.backing_iosurface().ok_or_else(|| {
            Error::NotSupported(
                "export_iosurface_mach_send_right: this image's storage is not an IOSurface".into(),
            )
        })?;
        crate::apple::iosurface::create_iosurface_mach_send_right(iosurface)
    }
}
