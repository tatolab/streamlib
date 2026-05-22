// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared Vulkan Video device context.
//!
//! Holds the vulkanalia device, instance, physical device, and loaded Vulkan
//! Video extension function pointers. Passed into [`Decoder`](crate::vulkan::video::Decoder)
//! and [`Encoder`](crate::vulkan::video::Encoder) so callers can use their own vulkanalia
//! application's device.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

/// The minimum Vulkan API version required by this library.
///
/// All instance/device creation in this crate must use this version.
/// Vulkan 1.4 is required for video encode/decode extensions and
/// `VK_KHR_video_maintenance1`.

fn create_nv12_ycbcr_conversion(
    device: &vulkanalia::Device,
) -> VideoResult<vk::SamplerYcbcrConversion> {
    let info = vk::SamplerYcbcrConversionCreateInfo::builder()
        .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
        .ycbcr_model(vk::SamplerYcbcrModelConversion::YCBCR_709)
        .ycbcr_range(vk::SamplerYcbcrRange::ITU_NARROW)
        .components(vk::ComponentMapping {
            r: vk::ComponentSwizzle::IDENTITY,
            g: vk::ComponentSwizzle::IDENTITY,
            b: vk::ComponentSwizzle::IDENTITY,
            a: vk::ComponentSwizzle::IDENTITY,
        })
        .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
        .y_chroma_offset(vk::ChromaLocation::MIDPOINT)
        .chroma_filter(vk::Filter::LINEAR)
        .force_explicit_reconstruction(false);

    Ok(unsafe { device.create_sampler_ycbcr_conversion(&info, None)? })
}

/// Shared Vulkan device context for video operations.
///
/// Wraps the host RHI's [`vulkanalia::Device`] and [`vulkanalia::Instance`]
/// for the codec layer. In vulkanalia, extension commands are available
/// directly on Device/Instance via traits (e.g. `KhrVideoQueueExtension`),
/// so no separate function table is needed. Constructed exclusively via
/// [`VideoContext::from_external`] from the host RHI's allocator + queues.
pub struct VideoContext {
    instance: vulkanalia::Instance,
    device: vulkanalia::Device,
    physical_device: vk::PhysicalDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    allocator: Arc<vma::Allocator>,
    nv12_ycbcr_conversion: vk::SamplerYcbcrConversion,
    /// Host RHI handle the codec layer uses to reach the engine's
    /// privileged primitives (video session, future query pool, ...).
    /// Concretely the same `Arc` the `submitter: Arc<dyn RhiQueueSubmitter>`
    /// fields on `SimpleEncoder` / `VkVideoDecoder` were holding pre-#936
    /// (the trait's only implementor was `HostVulkanDevice`); stashed here
    /// so the codec interior can call `HostVulkanVideoSession::new`
    /// without re-routing the FullAccess context all the way down.
    host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
}

/// Errors that can occur during video context or session creation.
#[derive(Debug)]
pub enum VideoError {
    /// A Vulkan API call returned an error.
    Vulkan(vk::Result),
    /// The requested codec is not supported by the device.
    CodecNotSupported(vk::VideoCodecOperationFlagsKHR),
    /// No suitable queue family found.
    NoVideoQueueFamily,
    /// No suitable memory type found.
    NoSuitableMemoryType,
    /// The parser encountered an invalid bitstream.
    BitstreamError(String),
    /// A required video format is not supported.
    FormatNotSupported(vk::Format),
    /// An error surfaced from the engine RHI layer (compute-kernel
    /// construction, descriptor management, etc.).
    Other(String),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Vulkan(r) => write!(f, "Vulkan error: {:?}", r),
            Self::CodecNotSupported(op) => write!(f, "Codec not supported: {:?}", op),
            Self::NoVideoQueueFamily => write!(f, "No video queue family found"),
            Self::NoSuitableMemoryType => write!(f, "No suitable memory type found"),
            Self::BitstreamError(msg) => write!(f, "Bitstream error: {}", msg),
            Self::FormatNotSupported(fmt) => write!(f, "Format not supported: {:?}", fmt),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for VideoError {}

impl From<vk::Result> for VideoError {
    fn from(r: vk::Result) -> Self {
        Self::Vulkan(r)
    }
}

impl From<vk::ErrorCode> for VideoError {
    fn from(e: vk::ErrorCode) -> Self {
        Self::Vulkan(vk::Result::from(e))
    }
}

impl From<crate::core::Error> for VideoError {
    fn from(e: crate::core::Error) -> Self {
        Self::Other(format!("{e}"))
    }
}

pub type VideoResult<T> = Result<T, VideoError>;

impl VideoContext {
    /// Create a new video context from the caller's vulkanalia types.
    ///
    /// In vulkanalia, extension commands are dispatched via traits
    /// (e.g. `KhrVideoQueueExtensionDeviceCommands`), so the caller just
    /// needs to have enabled the relevant extensions when creating the device:
    ///
    /// - `VK_KHR_video_queue`
    /// - `VK_KHR_video_decode_queue` (for decoding)
    /// - `VK_KHR_video_encode_queue` (for encoding)
    /// - Codec-specific extensions (e.g. `VK_KHR_video_decode_h264`)
    /// Create a video context from an externally-owned device and allocator.
    ///
    /// Use this when integrating with a host application (e.g., streamlib)
    /// that already owns the Vulkan device and VMA allocator. No new device
    /// or allocator is created — the caller's handles are shared.
    ///
    /// The caller must ensure the device was created with the required video
    /// encode/decode extensions enabled.
    pub fn from_external(
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
        allocator: Arc<vma::Allocator>,
        host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
    ) -> VideoResult<Self> {
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        let nv12_ycbcr_conversion = create_nv12_ycbcr_conversion(&device)?;

        Ok(Self {
            instance,
            device,
            physical_device,
            memory_properties,
            allocator,
            nv12_ycbcr_conversion,
            host_device,
        })
    }

    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
    }

    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    pub fn memory_properties(&self) -> &vk::PhysicalDeviceMemoryProperties {
        &self.memory_properties
    }

    /// Get a shared reference to the VMA allocator.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        &self.allocator
    }

    /// Host RHI handle. Codec interior reaches engine RHI primitives
    /// (video session, future query pool, future DPB-flavored texture)
    /// through this accessor rather than routing the `FullAccess`
    /// context all the way down.
    pub fn host_device(&self) -> &Arc<crate::vulkan::rhi::HostVulkanDevice> {
        &self.host_device
    }

    /// Shared NV12 sampler Y′CBCR conversion handle for attachment to
    /// multi-planar image views whose parent image includes `SAMPLED` usage
    /// (required by `VUID-VkImageViewCreateInfo-format-06415`).
    pub fn nv12_ycbcr_conversion(&self) -> vk::SamplerYcbcrConversion {
        self.nv12_ycbcr_conversion
    }

    /// Find a queue family that supports the given video codec operation.
    ///
    /// Uses `vkGetPhysicalDeviceQueueFamilyProperties2` with pNext-chained
    /// `VkQueueFamilyVideoPropertiesKHR` to check per-queue-family codec
    /// support. We call the raw function pointer because vulkanalia's
    /// high-level wrapper doesn't propagate pNext chains.
    pub fn find_video_queue_family(
        &self,
        codec_op: vk::VideoCodecOperationFlagsKHR,
        decode: bool,
    ) -> VideoResult<u32> {
        let required_flag = if decode {
            vk::QueueFlags::VIDEO_DECODE_KHR
        } else {
            vk::QueueFlags::VIDEO_ENCODE_KHR
        };

        unsafe {
            // Get the raw function pointer from vulkanalia's command table.
            let fp = InstanceV1_0::commands(&self.instance)
                .get_physical_device_queue_family_properties2;

            // First call: get queue family count.
            let mut count: u32 = 0;
            fp(self.physical_device, &mut count, std::ptr::null_mut());

            let n = count as usize;
            let mut video_props =
                vec![vk::QueueFamilyVideoPropertiesKHR::default(); n];
            let mut props2: Vec<vk::QueueFamilyProperties2> = (0..n)
                .map(|i| {
                    let mut p = vk::QueueFamilyProperties2::default();
                    p.next =
                        &mut video_props[i] as *mut _ as *mut std::ffi::c_void;
                    p
                })
                .collect();

            // Second call: fill results with pNext chains.
            fp(
                self.physical_device,
                &mut count,
                props2.as_mut_ptr(),
            );

            for i in 0..count as usize {
                let flags = props2[i].queue_family_properties.queue_flags;
                if flags.contains(required_flag)
                    && video_props[i].video_codec_operations.contains(codec_op)
                {
                    return Ok(i as u32);
                }
            }
        }
        Err(VideoError::NoVideoQueueFamily)
    }

}

impl Drop for VideoContext {
    fn drop(&mut self) {
        unsafe {
            self.device
                .destroy_sampler_ycbcr_conversion(self.nv12_ycbcr_conversion, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_error_display() {
        let e = VideoError::Vulkan(vk::Result::ERROR_DEVICE_LOST);
        assert!(format!("{}", e).contains("DEVICE_LOST"));

        let e = VideoError::NoVideoQueueFamily;
        assert!(format!("{}", e).contains("queue family"));
    }

}
