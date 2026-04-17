// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared Vulkan Video device context.
//!
//! Holds the vulkanalia device, instance, physical device, and loaded Vulkan
//! Video extension function pointers. Passed into [`Decoder`](crate::Decoder)
//! and [`Encoder`](crate::Encoder) so callers can use their own vulkanalia
//! application's device.

use std::ffi::CStr;
use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

/// The minimum Vulkan API version required by this library.
///
/// All instance/device creation in this crate must use this version.
/// Vulkan 1.4 is required for video encode/decode extensions and
/// `VK_KHR_video_maintenance1`.
pub const REQUIRED_VULKAN_API_VERSION: u32 = vk::make_version(1, 4, 0);

/// Reject software renderers (llvmpipe, lavapipe, swiftshader, etc.).
///
/// Video encode/decode requires dedicated hardware queue families that
/// software renderers do not provide. Returns `Err` with a descriptive
/// message if the device is a software renderer.
pub fn reject_software_renderer(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Result<(), VideoError> {
    let props = unsafe { instance.get_physical_device_properties(physical_device) };

    if props.device_type == vk::PhysicalDeviceType::CPU {
        let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
            .to_string_lossy();
        return Err(VideoError::BitstreamError(format!(
            "Software renderer detected: {:?}. \
             Video encode/decode requires a discrete or integrated GPU.",
            name
        )));
    }

    let name_lower = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
        .to_string_lossy()
        .to_ascii_lowercase();
    for sw in &["llvmpipe", "lavapipe", "swiftshader", "softpipe"] {
        if name_lower.contains(sw) {
            return Err(VideoError::BitstreamError(format!(
                "Software renderer detected: {:?}. \
                 Video encode/decode requires a discrete or integrated GPU.",
                name_lower
            )));
        }
    }

    Ok(()
    )
}

/// Shared Vulkan device context for video operations.
///
/// Wraps the caller's [`vulkanalia::Device`] and [`vulkanalia::Instance`] along
/// with the loaded Vulkan Video extension function pointers. In vulkanalia,
/// extension commands are available directly on Device/Instance via traits
/// (e.g. `KhrVideoQueueExtension`), so no separate function table is needed.
///
/// # Example
///
/// ```ignore
/// let ctx = VideoContext::new(
///     instance.clone(),
///     device.clone(),
///     physical_device,
/// )?;
/// ```
pub struct VideoContext {
    instance: vulkanalia::Instance,
    device: vulkanalia::Device,
    physical_device: vk::PhysicalDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    allocator: Arc<vma::Allocator>,
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
    /// Create a new video context from the caller's vulkanalia types.
    ///
    /// Creates a VMA allocator internally for all memory management.
    pub fn new(
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
    ) -> VideoResult<Self> {
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        let mut alloc_options = vma::AllocatorOptions::new(&instance, &device, physical_device);
        alloc_options.version = vulkanalia::Version::new(
            vk::version_major(REQUIRED_VULKAN_API_VERSION) as u32,
            vk::version_minor(REQUIRED_VULKAN_API_VERSION) as u32,
            0,
        );

        let allocator = Arc::new(unsafe {
            vma::Allocator::new(&alloc_options)
                .map_err(|e| VideoError::Vulkan(vk::Result::from(e)))?
        });

        Ok(Self {
            instance,
            device,
            physical_device,
            memory_properties,
            allocator,
        })
    }

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
    ) -> VideoResult<Self> {
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        Ok(Self {
            instance,
            device,
            physical_device,
            memory_properties,
            allocator,
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
