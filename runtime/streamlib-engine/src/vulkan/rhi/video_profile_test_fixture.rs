// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Real H.264 video profiles for the RHI's hardware-gated video fixtures.
//!
//! `HostVulkanTexture::new_video_dpb` and
//! `HostVulkanBuffer::new_video_bitstream` chain the caller's profile onto
//! `VkVideoProfileListInfoKHR`, and the driver reads every field of it during
//! `vkCreateImage` / `vkCreateBuffer`. A default-constructed profile is all
//! zeroes, which is not a legal `VkVideoProfileInfoKHR` — the allocation is
//! refused and the validation layer reports each zeroed field.

#![cfg(all(test, target_os = "linux"))]

use vulkanalia::vk;

use super::{HostVulkanDevice, VideoBitstreamDirection, VideoDpbDirection};

/// A `VkVideoProfileInfoKHR` that owns the codec-specific structs its `pNext`
/// chain points at.
///
/// The chain members are boxed so their addresses survive a move of this
/// value; a profile chained to locals and returned by value would hand the
/// driver pointers into a dead stack frame.
pub(super) struct VideoProfileWithOwnedCodecExtensionChain {
    video_profile_info: vk::VideoProfileInfoKHR,
    _owned_codec_extension_chain: OwnedCodecExtensionChain,
}

/// Ownership-only. The fields are never read — the raw `pNext` pointers in
/// [`VideoProfileWithOwnedCodecExtensionChain::video_profile_info`] are what
/// the driver follows, and these boxes are what keep those pointers valid.
enum OwnedCodecExtensionChain {
    DecodeH264 {
        _decode_h264_profile_info: Box<vk::VideoDecodeH264ProfileInfoKHR>,
    },
    EncodeH264 {
        _encode_h264_profile_info: Box<vk::VideoEncodeH264ProfileInfoKHR>,
        _encode_usage_info: Box<vk::VideoEncodeUsageInfoKHR>,
    },
}

impl VideoProfileWithOwnedCodecExtensionChain {
    /// 8-bit 4:2:0 progressive H.264 decode — the profile
    /// `VkVideoDecoder::start_video_sequence` builds for its DPB and
    /// bitstream allocations.
    pub(super) fn h264_decode_420_8bit() -> Self {
        let decode_h264_profile_info = Box::new(vk::VideoDecodeH264ProfileInfoKHR {
            s_type: vk::StructureType::VIDEO_DECODE_H264_PROFILE_INFO_KHR,
            next: std::ptr::null(),
            std_profile_idc: vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
            picture_layout: vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        });

        Self {
            video_profile_info: video_profile_info_420_8bit(
                vk::VideoCodecOperationFlagsKHR::DECODE_H264,
                &*decode_h264_profile_info as *const _ as *const std::ffi::c_void,
            ),
            _owned_codec_extension_chain: OwnedCodecExtensionChain::DecodeH264 {
                _decode_h264_profile_info: decode_h264_profile_info,
            },
        }
    }

    /// 8-bit 4:2:0 H.264 encode. `VkVideoEncodeUsageInfoKHR` is chained behind
    /// the codec profile the way `VkVideoCoreProfile` chains it for every
    /// encode session.
    pub(super) fn h264_encode_420_8bit() -> Self {
        let encode_usage_info = Box::new(vk::VideoEncodeUsageInfoKHR {
            s_type: vk::StructureType::VIDEO_ENCODE_USAGE_INFO_KHR,
            next: std::ptr::null(),
            video_usage_hints: vk::VideoEncodeUsageFlagsKHR::DEFAULT,
            video_content_hints: vk::VideoEncodeContentFlagsKHR::DEFAULT,
            tuning_mode: vk::VideoEncodeTuningModeKHR::DEFAULT,
        });
        let encode_h264_profile_info = Box::new(vk::VideoEncodeH264ProfileInfoKHR {
            s_type: vk::StructureType::VIDEO_ENCODE_H264_PROFILE_INFO_KHR,
            next: &*encode_usage_info as *const _ as *const std::ffi::c_void,
            std_profile_idc: vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN,
        });

        Self {
            video_profile_info: video_profile_info_420_8bit(
                vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
                &*encode_h264_profile_info as *const _ as *const std::ffi::c_void,
            ),
            _owned_codec_extension_chain: OwnedCodecExtensionChain::EncodeH264 {
                _encode_h264_profile_info: encode_h264_profile_info,
                _encode_usage_info: encode_usage_info,
            },
        }
    }

    /// The profile a DPB image of `direction` must be bound to.
    pub(super) fn h264_for_dpb_direction(direction: VideoDpbDirection) -> Self {
        match direction {
            VideoDpbDirection::Decode => Self::h264_decode_420_8bit(),
            VideoDpbDirection::Encode => Self::h264_encode_420_8bit(),
        }
    }

    /// The profile a bitstream buffer of `direction` must be bound to.
    pub(super) fn h264_for_bitstream_direction(direction: VideoBitstreamDirection) -> Self {
        match direction {
            VideoBitstreamDirection::Decode => Self::h264_decode_420_8bit(),
            VideoBitstreamDirection::Encode => Self::h264_encode_420_8bit(),
        }
    }

    /// The profile to hand a descriptor. The borrow is what ties the `pNext`
    /// chain's validity to `self` — the driver follows those pointers during
    /// the call, so `self` must outlive it.
    pub(super) fn video_profile_info(&self) -> &vk::VideoProfileInfoKHR {
        &self.video_profile_info
    }
}

/// Whether `device` can run H.264 in the direction a DPB image is built for.
///
/// `supports_video_{decode,encode}` is the enabled-extension answer, not
/// `video_{decode,encode}_queue_family_index` — the latter is a bare queue-flag
/// scan that reports `Some` even when `VK_KHR_video_*_h264` was never enabled,
/// and a profile naming a codec whose extension is off gets refused.
pub(super) fn device_supports_h264_for_dpb_direction(
    device: &HostVulkanDevice,
    direction: VideoDpbDirection,
) -> bool {
    match direction {
        VideoDpbDirection::Decode => device.supports_video_decode(),
        VideoDpbDirection::Encode => device.supports_video_encode(),
    }
}

/// Whether `device` can run H.264 in the direction a bitstream buffer is built
/// for. Same predicate as [`device_supports_h264_for_dpb_direction`].
pub(super) fn device_supports_h264_for_bitstream_direction(
    device: &HostVulkanDevice,
    direction: VideoBitstreamDirection,
) -> bool {
    match direction {
        VideoBitstreamDirection::Decode => device.supports_video_decode(),
        VideoBitstreamDirection::Encode => device.supports_video_encode(),
    }
}

fn video_profile_info_420_8bit(
    video_codec_operation: vk::VideoCodecOperationFlagsKHR,
    codec_extension_chain: *const std::ffi::c_void,
) -> vk::VideoProfileInfoKHR {
    vk::VideoProfileInfoKHR {
        s_type: vk::StructureType::VIDEO_PROFILE_INFO_KHR,
        next: codec_extension_chain,
        video_codec_operation,
        chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::_420,
        luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::_8,
        chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::_8,
    }
}
