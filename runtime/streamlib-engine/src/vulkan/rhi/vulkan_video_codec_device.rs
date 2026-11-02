// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's device as the codec layer's [`VulkanVideoCodecDevice`].

use std::sync::{Arc, MutexGuard};

use streamlib_vulkan_video::{
    VideoError, VideoResult, VulkanVideoCodecDevice, VulkanVideoCodecQueue,
};
use vulkanalia::vk;
#[cfg(test)]
use vulkanalia::vk::Handle as _;
use vulkanalia_vma as vma;

use super::HostVulkanDevice;

impl VulkanVideoCodecDevice for HostVulkanDevice {
    fn instance(&self) -> &vulkanalia::Instance {
        HostVulkanDevice::instance(self)
    }

    fn device(&self) -> &vulkanalia::Device {
        HostVulkanDevice::device(self)
    }

    fn physical_device(&self) -> vk::PhysicalDevice {
        HostVulkanDevice::physical_device(self)
    }

    fn allocator(&self) -> &Arc<vma::Allocator> {
        HostVulkanDevice::allocator(self)
    }

    fn video_encode_queue(&self) -> Option<VulkanVideoCodecQueue> {
        if !self.supports_video_encode() {
            return None;
        }
        Some(VulkanVideoCodecQueue {
            queue: HostVulkanDevice::video_encode_queue(self)?,
            queue_family_index: self.video_encode_queue_family_index()?,
        })
    }

    fn video_decode_queue(&self) -> Option<VulkanVideoCodecQueue> {
        if !self.supports_video_decode() {
            return None;
        }
        Some(VulkanVideoCodecQueue {
            queue: HostVulkanDevice::video_decode_queue(self)?,
            queue_family_index: self.video_decode_queue_family_index()?,
        })
    }

    fn compute_queue(&self) -> VulkanVideoCodecQueue {
        match (
            HostVulkanDevice::compute_queue(self),
            self.compute_queue_family_index(),
        ) {
            (Some(queue), Some(queue_family_index)) => VulkanVideoCodecQueue {
                queue,
                queue_family_index,
            },
            _ => VulkanVideoCodecQueue {
                queue: self.queue(),
                queue_family_index: self.queue_family_index(),
            },
        }
    }

    fn transfer_queue(&self) -> VulkanVideoCodecQueue {
        VulkanVideoCodecQueue {
            queue: HostVulkanDevice::transfer_queue(self),
            queue_family_index: self.transfer_queue_family_index(),
        }
    }

    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> VideoResult<()> {
        unsafe { HostVulkanDevice::submit_to_queue(self, queue, submits, fence) }
            .map_err(VideoError::from)
    }

    fn lock_device(&self) -> MutexGuard<'_, ()> {
        HostVulkanDevice::lock_device(self)
    }

    fn wait_idle(&self) -> VideoResult<()> {
        HostVulkanDevice::wait_idle(self).map_err(VideoError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::rig_only_capability_skip::{
        RigOnlyCapability, skip_unless_this_machine_has,
    };
    use crate::vulkan::rhi::video_profile_test_fixture::{
        VideoProfileWithOwnedCodecExtensionChain, device_supports_h264_for_bitstream_direction,
        device_supports_h264_for_dpb_direction,
    };
    use streamlib_vulkan_video::{
        VideoBitstreamBufferDescriptor, VideoBitstreamDirection, VideoDpbDirection,
        VideoDpbImageDescriptor, VulkanVideoBitstreamBuffer, VulkanVideoDpbImage,
    };

    fn host_vulkan_device_or_skip() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(host_device) => Some(host_device),
            Err(error) => {
                eprintln!("Skipping — no Vulkan device available: {error}");
                None
            }
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_dpb_rejects_zero_dimensions() {
        let Some(host_device) = host_vulkan_device_or_skip() else {
            return;
        };
        let device: Arc<dyn VulkanVideoCodecDevice> = host_device.clone();
        let profile = VideoProfileWithOwnedCodecExtensionChain::h264_decode_420_8bit();
        for (w, h, layers) in [(0, 64, 4), (64, 0, 4), (64, 64, 0), (0, 0, 0)] {
            let descriptor = VideoDpbImageDescriptor {
                label: "test/zero-dim",
                width: w,
                height: h,
                format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                array_layers: layers,
                direction: VideoDpbDirection::Decode,
                additional_usage: vk::ImageUsageFlags::empty(),
                sharing_queue_families: &[],
                video_profile: profile.video_profile_info(),
            };
            match VulkanVideoDpbImage::new(&device, &descriptor) {
                Err(VideoError::Configuration(msg)) => {
                    assert!(
                        msg.contains("must be > 0"),
                        "rejection must explain the constraint, got: {msg}"
                    );
                    assert!(
                        msg.contains("test/zero-dim"),
                        "rejection must carry the descriptor label, got: {msg}"
                    );
                }
                Err(e) => panic!("{w}x{h}x{layers}: expected Configuration error, got {e}"),
                Ok(_) => {
                    panic!("{w}x{h}x{layers}: zero-component descriptor must be rejected, got Ok")
                }
            }
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_dpb_succeeds_for_both_directions() {
        let Some(host_device) = host_vulkan_device_or_skip() else {
            return;
        };
        let device: Arc<dyn VulkanVideoCodecDevice> = host_device.clone();
        for direction in [VideoDpbDirection::Decode, VideoDpbDirection::Encode] {
            if skip_unless_this_machine_has(
                RigOnlyCapability::VulkanVideo,
                device_supports_h264_for_dpb_direction(&host_device, direction),
            ) {
                continue;
            }
            let additional_usage = match direction {
                VideoDpbDirection::Decode => {
                    vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::SAMPLED
                }
                VideoDpbDirection::Encode => vk::ImageUsageFlags::empty(),
            };
            let profile =
                VideoProfileWithOwnedCodecExtensionChain::h264_for_dpb_direction(direction);
            let descriptor = VideoDpbImageDescriptor {
                label: "test/dpb-positive",
                width: 1920,
                height: 1088,
                format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                array_layers: 4,
                direction,
                additional_usage,
                sharing_queue_families: &[],
                video_profile: profile.video_profile_info(),
            };
            let texture = VulkanVideoDpbImage::new(&device, &descriptor)
                .unwrap_or_else(|e| panic!("{direction:?}: DPB image allocation failed: {e}"));
            assert_ne!(
                texture.image(),
                vk::Image::null(),
                "{direction:?}: VkImage handle must be non-null"
            );
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_bitstream_rejects_zero_size() {
        let Some(host_device) = host_vulkan_device_or_skip() else {
            return;
        };
        let device: Arc<dyn VulkanVideoCodecDevice> = host_device.clone();
        for direction in [
            VideoBitstreamDirection::Encode,
            VideoBitstreamDirection::Decode,
        ] {
            let profile =
                VideoProfileWithOwnedCodecExtensionChain::h264_for_bitstream_direction(direction);
            let descriptor = VideoBitstreamBufferDescriptor {
                label: "test/zero-size",
                size: 0,
                direction,
                video_profile: profile.video_profile_info(),
            };
            match VulkanVideoBitstreamBuffer::new(&device, &descriptor) {
                Err(VideoError::Configuration(msg)) => {
                    assert!(
                        msg.contains("size must be > 0"),
                        "zero-size error must explain the constraint, got: {msg}"
                    );
                    assert!(
                        msg.contains("test/zero-size"),
                        "zero-size error must carry the label, got: {msg}"
                    );
                }
                Err(e) => {
                    panic!("zero size ({direction:?}): expected Configuration error, got {e}")
                }
                Ok(_) => panic!("zero size ({direction:?}): expected rejection, got Ok"),
            }
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_video_bitstream_succeeds_for_both_directions() {
        let Some(host_device) = host_vulkan_device_or_skip() else {
            return;
        };
        let device: Arc<dyn VulkanVideoCodecDevice> = host_device.clone();
        const SIZE: u64 = 1 << 20; // 1 MiB
        for direction in [
            VideoBitstreamDirection::Encode,
            VideoBitstreamDirection::Decode,
        ] {
            if skip_unless_this_machine_has(
                RigOnlyCapability::VulkanVideo,
                device_supports_h264_for_bitstream_direction(&host_device, direction),
            ) {
                continue;
            }
            let profile =
                VideoProfileWithOwnedCodecExtensionChain::h264_for_bitstream_direction(direction);
            let descriptor = VideoBitstreamBufferDescriptor {
                label: "test/bitstream-positive",
                size: SIZE,
                direction,
                video_profile: profile.video_profile_info(),
            };
            let bitstream_buffer = VulkanVideoBitstreamBuffer::new(&device, &descriptor)
                .unwrap_or_else(|e| panic!("{direction:?}: bitstream allocation failed: {e}"));
            assert_eq!(bitstream_buffer.size(), SIZE, "size must match descriptor");
            assert!(
                !bitstream_buffer.mapped_ptr().is_null(),
                "{direction:?} bitstream buffer must be persistently mapped"
            );
            assert_ne!(
                bitstream_buffer.buffer(),
                vk::Buffer::null(),
                "VkBuffer handle must be non-null"
            );
        }
    }
}
