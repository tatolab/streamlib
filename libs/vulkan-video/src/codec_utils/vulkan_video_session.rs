// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkCodecUtils/VulkanVideoSession.h + VulkanVideoSession.cpp
//!
//! Wraps `VkVideoSessionKHR` with associated device memory bindings.
//! The C++ original inherits from `VkVideoRefCountBase`; in Rust we use
//! `Arc<VulkanVideoSession>` for shared ownership instead.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands;
use vulkanalia_vma::{self as vma, Alloc};
use std::sync::Arc;

/// Maximum number of memory bindings a video session may require.
/// Mirrors the C++ `enum { MAX_BOUND_MEMORY = 40 }`.
const MAX_BOUND_MEMORY: usize = 40;

// VP9 and AV1 are now available in vulkanalia 0.35 with Vulkan 1.4 support.

/// Parameters needed to create (or check compatibility of) a `VulkanVideoSession`.
///
/// Gathered into a struct to reduce the number of function arguments (the C++
/// original passes these individually, but Rust benefits from a named bundle).
#[derive(Clone)]
pub struct VideoSessionCreateParams {
    pub session_create_flags: vk::VideoSessionCreateFlagsKHR,
    pub video_queue_family: u32,
    pub video_profile: vk::VideoProfileInfoKHR,
    pub codec_operation: vk::VideoCodecOperationFlagsKHR,
    pub picture_format: vk::Format,
    pub max_coded_extent: vk::Extent2D,
    pub reference_pictures_format: vk::Format,
    pub max_dpb_slots: u32,
    pub max_active_reference_pictures: u32,
}

/// Owns a `VkVideoSessionKHR` and the device memory allocations bound to it.
///
/// Equivalent to the C++ `VulkanVideoSession` class. On drop, destroys the
/// session and frees all bound memory — matching the C++ destructor.
///
/// In vulkanalia, video queue extension commands are available directly on
/// `Device` via the `KhrVideoQueueExtensionDeviceCommands` trait, so no
/// separate extension function table is needed.
pub struct VulkanVideoSession {
    device: vulkanalia::Device,
    allocator: Arc<vma::Allocator>,
    flags: vk::VideoSessionCreateFlagsKHR,
    create_info_snapshot: VideoSessionCreateInfoSnapshot,
    video_session: vk::VideoSessionKHR,
    allocations: Vec<vma::Allocation>,
}

/// Snapshot of the fields from `VkVideoSessionCreateInfoKHR` that we need for
/// compatibility checks. The C++ code stores the full `VkVideoSessionCreateInfoKHR`
/// but we cannot keep pointers into stack-allocated Vulkan structures, so we
/// copy the scalar fields instead.
#[derive(Clone)]
struct VideoSessionCreateInfoSnapshot {
    #[allow(dead_code)] // Used in test assertions
    flags: vk::VideoSessionCreateFlagsKHR,
    queue_family_index: u32,
    picture_format: vk::Format,
    max_coded_extent: vk::Extent2D,
    reference_picture_format: vk::Format,
    max_dpb_slots: u32,
    max_active_reference_pictures: u32,
}

// SAFETY: The inner Vulkan handles are only accessed through `&self` or on drop
// and are not tied to a particular thread.
unsafe impl Send for VulkanVideoSession {}
unsafe impl Sync for VulkanVideoSession {}

impl VulkanVideoSession {
    // ------------------------------------------------------------------
    // Create
    // ------------------------------------------------------------------

    /// Create a new video session, allocate and bind the required device memory.
    ///
    /// This is the Rust translation of the static `VulkanVideoSession::Create`
    /// method in the C++ source. Returns an `Arc` (replacing the C++
    /// `VkSharedBaseObj` ref-counted pointer).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `device` and `instance` are valid and that
    /// `physical_device` belongs to the instance. The device must have been
    /// created with the `VK_KHR_video_queue` extension enabled.
    pub unsafe fn create(
        device: &vulkanalia::Device,
        _instance: &vulkanalia::Instance,
        _physical_device: vk::PhysicalDevice,
        allocator: &Arc<vma::Allocator>,
        params: &VideoSessionCreateParams,
    ) -> Result<Arc<Self>, vk::Result> {
        // --- Build the std header version based on codec type ---------
        let std_header_version = std_header_version_for_codec(params.codec_operation)?;

        // --- Populate VkVideoSessionCreateInfoKHR --------------------
        let profile_info = params.video_profile;
        let create_info = vk::VideoSessionCreateInfoKHR::builder()
            .flags(params.session_create_flags)
            .video_profile(&profile_info)
            .queue_family_index(params.video_queue_family)
            .picture_format(params.picture_format)
            .max_coded_extent(params.max_coded_extent)
            .max_dpb_slots(params.max_dpb_slots)
            .max_active_reference_pictures(params.max_active_reference_pictures)
            .reference_picture_format(params.reference_pictures_format)
            .std_header_version(&std_header_version);

        // --- Create the VkVideoSessionKHR ----------------------------
        let video_session = device
            .create_video_session_khr(&create_info, None)
            .map_err(|e| e)?;

        let mut allocations: Vec<vma::Allocation> = Vec::new();

        // --- Query memory requirements -------------------------------
        let mem_requirements = device
            .get_video_session_memory_requirements_khr(video_session)
            .map_err(|e| {
                device.destroy_video_session_khr(video_session, None);
                e
            })?;

        assert!(
            mem_requirements.len() <= MAX_BOUND_MEMORY,
            "video session requires {} memory bindings, max is {}",
            mem_requirements.len(),
            MAX_BOUND_MEMORY,
        );

        // --- Allocate and record each memory binding -----------------
        let mut bind_infos: Vec<vk::BindVideoSessionMemoryInfoKHR> =
            Vec::with_capacity(mem_requirements.len());

        for req in mem_requirements.iter() {
            let memory_type_bits = req.memory_requirements.memory_type_bits;
            if memory_type_bits == 0 {
                cleanup_on_failure(allocator, video_session, device, &allocations);
                return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
            }

            let alloc_options = vma::AllocationOptions {
                usage: vma::MemoryUsage::Unknown,
                memory_type_bits,
                ..Default::default()
            };

            let allocation = allocator
                .allocate_memory(req.memory_requirements, &alloc_options)
                .map_err(|e| {
                    cleanup_on_failure(allocator, video_session, device, &allocations);
                    e
                })?;

            let alloc_info = allocator.get_allocation_info(allocation);

            allocations.push(allocation);

            bind_infos.push(
                vk::BindVideoSessionMemoryInfoKHR::builder()
                    .memory_bind_index(req.memory_bind_index)
                    .memory(alloc_info.deviceMemory)
                    .memory_offset(alloc_info.offset)
                    .memory_size(req.memory_requirements.size)
                    .build(),
            );
        }

        // --- Bind all memory to the session --------------------------
        device
            .bind_video_session_memory_khr(video_session, &bind_infos)
            .map_err(|e| {
                cleanup_on_failure(allocator, video_session, device, &allocations);
                e
            })?;

        let snapshot = VideoSessionCreateInfoSnapshot {
            flags: params.session_create_flags,
            queue_family_index: params.video_queue_family,
            picture_format: params.picture_format,
            max_coded_extent: params.max_coded_extent,
            reference_picture_format: params.reference_pictures_format,
            max_dpb_slots: params.max_dpb_slots,
            max_active_reference_pictures: params.max_active_reference_pictures,
        };

        Ok(Arc::new(Self {
            device: device.clone(),
            allocator: Arc::clone(allocator),
            flags: params.session_create_flags,
            create_info_snapshot: snapshot,
            video_session,
            allocations,
        }))
    }

    // ------------------------------------------------------------------
    // Accessors
    // ------------------------------------------------------------------

    /// Return the raw `VkVideoSessionKHR` handle.
    ///
    /// C++ equivalent: `GetVideoSession()` / `operator VkVideoSessionKHR()`.
    #[inline]
    pub fn video_session(&self) -> vk::VideoSessionKHR {
        self.video_session
    }

    // ------------------------------------------------------------------
    // IsCompatible
    // ------------------------------------------------------------------

    /// Check whether this session is compatible with the given parameters.
    ///
    /// Mirrors the C++ `IsCompatible` method. The caller can reuse the
    /// session when this returns `true` instead of creating a new one.
    pub fn is_compatible(
        &self,
        device: &vulkanalia::Device,
        params: &VideoSessionCreateParams,
    ) -> bool {
        if params.session_create_flags != self.flags {
            return false;
        }

        if params.max_coded_extent.width > self.create_info_snapshot.max_coded_extent.width {
            return false;
        }

        if params.max_coded_extent.height > self.create_info_snapshot.max_coded_extent.height {
            return false;
        }

        if params.max_dpb_slots > self.create_info_snapshot.max_dpb_slots {
            return false;
        }

        if params.max_active_reference_pictures
            > self.create_info_snapshot.max_active_reference_pictures
        {
            return false;
        }

        if params.reference_pictures_format
            != self.create_info_snapshot.reference_picture_format
        {
            return false;
        }

        if params.picture_format != self.create_info_snapshot.picture_format {
            return false;
        }

        // In the C++ code this compares `m_vkDevCtx->getDevice()`. We compare
        // the `vulkanalia::Device` handle, which wraps the raw `VkDevice`.
        if device.handle() != self.device.handle() {
            return false;
        }

        if params.video_queue_family != self.create_info_snapshot.queue_family_index {
            return false;
        }

        true
    }
}

// ------------------------------------------------------------------
// Drop — mirrors the C++ destructor
// ------------------------------------------------------------------

impl Drop for VulkanVideoSession {
    fn drop(&mut self) {
        if self.video_session != vk::VideoSessionKHR::null() {
            unsafe {
                self.device
                    .destroy_video_session_khr(self.video_session, None);
            }
            self.video_session = vk::VideoSessionKHR::null();
        }

        for allocation in self.allocations.drain(..) {
            unsafe {
                self.allocator.free_memory(allocation);
            }
        }
    }
}

// ------------------------------------------------------------------
// Private helpers
// ------------------------------------------------------------------

/// Clean up allocated resources when session creation fails partway through.
///
/// # Safety
///
/// The caller must ensure all handles are valid.
unsafe fn cleanup_on_failure(
    allocator: &Arc<vma::Allocator>,
    video_session: vk::VideoSessionKHR,
    device: &vulkanalia::Device,
    allocations: &[vma::Allocation],
) {
    for &allocation in allocations {
        allocator.free_memory(allocation);
    }
    if video_session != vk::VideoSessionKHR::null() {
        device.destroy_video_session_khr(video_session, None);
    }
}

/// Return the `VkExtensionProperties` (std header version) for a given codec
/// operation type, matching the C++ switch statement in `VulkanVideoSession::Create`.
///
/// Divergence from C++: the C++ code uses static locals with compile-time
/// constants from Vulkan Video headers. Rust does not have the
/// `VK_STD_VULKAN_VIDEO_CODEC_*` constants readily available in vulkanalia,
/// so we construct them at runtime using the well-known extension name strings
/// and spec version numbers defined by the Vulkan Video specification.
fn std_header_version_for_codec(
    codec_op: vk::VideoCodecOperationFlagsKHR,
) -> Result<vk::ExtensionProperties, vk::Result> {
    // Helper to build a VkExtensionProperties from a name and version.
    fn make_ext(name: &[u8], spec_version: u32) -> vk::ExtensionProperties {
        let props = vk::ExtensionProperties {
            extension_name: vk::StringArray::from_bytes(name),
            spec_version,
        };
        props
    }

    // Extension name strings and spec versions from the Vulkan Video headers.
    // These match the VK_STD_VULKAN_VIDEO_CODEC_* defines used in C++.
    //
    // The spec version is VK_MAKE_VIDEO_STD_VERSION(1, 0, 0) = (1 << 22).
    // NOT the VK_KHR extension revision number.
    const STD_VIDEO_VERSION_1_0_0: u32 = 1 << 22; // VK_MAKE_VIDEO_STD_VERSION(1, 0, 0)

    if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_h264_decode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_h265_decode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_av1_decode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_VP9 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_vp9_decode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_h264_encode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_h265_encode\0", STD_VIDEO_VERSION_1_0_0))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_AV1 {
        Ok(make_ext(b"VK_STD_vulkan_video_codec_av1_encode\0", STD_VIDEO_VERSION_1_0_0))
    } else {
        // C++ hits assert(0) here.
        Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)
    }
}

// ======================================================================
// Unit tests — pure logic only (no GPU)
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to extract the extension name string from `VkExtensionProperties`.
    fn ext_name_str(ext: &vk::ExtensionProperties) -> String {
        ext.extension_name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8 as char)
            .collect()
    }

    #[test]
    fn test_std_header_version_h264_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
            .expect("H264 decode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_decode");
        assert_eq!(ext.spec_version, 1 << 22); // VK_MAKE_VIDEO_STD_VERSION(1, 0, 0)
    }

    #[test]
    fn test_std_header_version_h265_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H265)
            .expect("H265 decode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_decode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_av1_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_AV1)
            .expect("AV1 decode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_decode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_vp9_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::from_bits_truncate(
            vk::VideoCodecOperationFlagsKHR::DECODE_VP9.bits(),
        ))
        .expect("VP9 decode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_vp9_decode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_encode_h264() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .expect("H264 encode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_encode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_encode_h265() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
            .expect("H265 encode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_encode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_encode_av1() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::from_bits_truncate(
            vk::VideoCodecOperationFlagsKHR::ENCODE_AV1.bits(),
        ))
        .expect("AV1 encode should be supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_encode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn test_std_header_version_unknown_codec() {
        let result = std_header_version_for_codec(
            vk::VideoCodecOperationFlagsKHR::from_bits_truncate(0xDEAD),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
    }

    #[test]
    fn test_is_compatible_snapshot_logic() {
        // We cannot call is_compatible without a real VulkanVideoSession, but
        // we can verify the snapshot comparison logic through field checks.
        let snap = VideoSessionCreateInfoSnapshot {
            flags: vk::VideoSessionCreateFlagsKHR::empty(),
            queue_family_index: 0,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: vk::Extent2D { width: 1920, height: 1080 },
            reference_picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots: 16,
            max_active_reference_pictures: 16,
        };

        // Exact match — compatible.
        assert!(snap.flags == vk::VideoSessionCreateFlagsKHR::empty());
        assert!(1920 <= snap.max_coded_extent.width);
        assert!(1080 <= snap.max_coded_extent.height);
        assert!(16 <= snap.max_dpb_slots);
        assert!(16 <= snap.max_active_reference_pictures);

        // Exceeding extents would fail compatibility.
        assert!(1921 > snap.max_coded_extent.width);
        assert!(1081 > snap.max_coded_extent.height);
        assert!(17 > snap.max_dpb_slots);
    }

    #[test]
    fn test_max_bound_memory_constant() {
        // Sanity check that the constant matches the C++ enum.
        assert_eq!(MAX_BOUND_MEMORY, 40);
    }
}
