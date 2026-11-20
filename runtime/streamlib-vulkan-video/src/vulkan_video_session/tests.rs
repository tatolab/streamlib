// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn ext_name_str(ext: &vk::ExtensionProperties) -> String {
    ext.extension_name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect()
}

#[test]
fn std_header_version_h264_decode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
        .expect("H264 decode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_decode");
    assert_eq!(ext.spec_version, 1 << 22);
}

#[test]
fn std_header_version_h265_decode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H265)
        .expect("H265 decode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_decode");
}

#[test]
fn std_header_version_h264_encode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
        .expect("H264 encode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_encode");
}

#[test]
fn std_header_version_h265_encode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
        .expect("H265 encode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_encode");
}

#[test]
fn std_header_version_av1_decode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_AV1)
        .expect("AV1 decode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_decode");
}

#[test]
fn std_header_version_vp9_decode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_VP9)
        .expect("VP9 decode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_vp9_decode");
}

#[test]
fn std_header_version_av1_encode() {
    let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_AV1)
        .expect("AV1 encode supported");
    assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_encode");
}

#[test]
fn std_header_version_unknown_codec_errors() {
    let result =
        std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::from_bits_truncate(0xDEAD));
    assert!(result.is_err());
}

const EVERY_DECODE_CODEC_OPERATION: [vk::VideoCodecOperationFlagsKHR; 4] = [
    vk::VideoCodecOperationFlagsKHR::DECODE_H264,
    vk::VideoCodecOperationFlagsKHR::DECODE_H265,
    vk::VideoCodecOperationFlagsKHR::DECODE_AV1,
    vk::VideoCodecOperationFlagsKHR::DECODE_VP9,
];

const EVERY_ENCODE_CODEC_OPERATION: [vk::VideoCodecOperationFlagsKHR; 3] = [
    vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
    vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
    vk::VideoCodecOperationFlagsKHR::ENCODE_AV1,
];

const NO_VIDEO: DeviceVideoCodingSupport = DeviceVideoCodingSupport {
    decode: false,
    encode: false,
};
const DECODE_ONLY: DeviceVideoCodingSupport = DeviceVideoCodingSupport {
    decode: true,
    encode: false,
};
const ENCODE_ONLY: DeviceVideoCodingSupport = DeviceVideoCodingSupport {
    decode: false,
    encode: true,
};
const DECODE_AND_ENCODE: DeviceVideoCodingSupport = DeviceVideoCodingSupport {
    decode: true,
    encode: true,
};

fn gpu_error_message(result: VideoResult<VideoCodecOperationDirection>) -> String {
    match result {
        Err(VideoError::Rhi(message)) => message,
        other => panic!("expected Error::GpuError, got {other:?}"),
    }
}

#[test]
fn every_known_codec_operation_is_classified_by_direction() {
    for op in EVERY_DECODE_CODEC_OPERATION {
        assert_eq!(
            VideoCodecOperationDirection::of_codec_operation(op),
            Some(VideoCodecOperationDirection::Decode),
            "{op:?}",
        );
    }
    for op in EVERY_ENCODE_CODEC_OPERATION {
        assert_eq!(
            VideoCodecOperationDirection::of_codec_operation(op),
            Some(VideoCodecOperationDirection::Encode),
            "{op:?}",
        );
    }
}

#[test]
fn combined_same_direction_mask_keeps_its_direction() {
    let decode_h264_and_h265 =
        vk::VideoCodecOperationFlagsKHR::DECODE_H264 | vk::VideoCodecOperationFlagsKHR::DECODE_H265;
    assert_eq!(
        VideoCodecOperationDirection::of_codec_operation(decode_h264_and_h265),
        Some(VideoCodecOperationDirection::Decode),
    );
    let encode_h264_and_av1 =
        vk::VideoCodecOperationFlagsKHR::ENCODE_H264 | vk::VideoCodecOperationFlagsKHR::ENCODE_AV1;
    assert_eq!(
        VideoCodecOperationDirection::of_codec_operation(encode_h264_and_av1),
        Some(VideoCodecOperationDirection::Encode),
    );
}

#[test]
fn empty_or_mixed_direction_mask_has_no_direction() {
    assert_eq!(
        VideoCodecOperationDirection::of_codec_operation(vk::VideoCodecOperationFlagsKHR::NONE),
        None,
    );
    let decode_and_encode =
        vk::VideoCodecOperationFlagsKHR::DECODE_H264 | vk::VideoCodecOperationFlagsKHR::ENCODE_H264;
    assert_eq!(
        VideoCodecOperationDirection::of_codec_operation(decode_and_encode),
        None,
    );
}

#[test]
fn device_without_video_refuses_every_codec_operation_naming_its_direction() {
    for op in EVERY_DECODE_CODEC_OPERATION {
        let message = gpu_error_message(require_device_video_coding_support(op, NO_VIDEO));
        assert!(message.contains("Vulkan Video decode"), "{op:?}: {message}");
    }
    for op in EVERY_ENCODE_CODEC_OPERATION {
        let message = gpu_error_message(require_device_video_coding_support(op, NO_VIDEO));
        assert!(message.contains("Vulkan Video encode"), "{op:?}: {message}");
    }
}

#[test]
fn decode_only_device_serves_decode_and_refuses_encode() {
    for op in EVERY_DECODE_CODEC_OPERATION {
        assert_eq!(
            require_device_video_coding_support(op, DECODE_ONLY).ok(),
            Some(VideoCodecOperationDirection::Decode),
            "{op:?}",
        );
    }
    for op in EVERY_ENCODE_CODEC_OPERATION {
        let message = gpu_error_message(require_device_video_coding_support(op, DECODE_ONLY));
        assert!(message.contains("Vulkan Video encode"), "{op:?}: {message}");
    }
}

#[test]
fn encode_only_device_serves_encode_and_refuses_decode() {
    for op in EVERY_ENCODE_CODEC_OPERATION {
        assert_eq!(
            require_device_video_coding_support(op, ENCODE_ONLY).ok(),
            Some(VideoCodecOperationDirection::Encode),
            "{op:?}",
        );
    }
    for op in EVERY_DECODE_CODEC_OPERATION {
        let message = gpu_error_message(require_device_video_coding_support(op, ENCODE_ONLY));
        assert!(message.contains("Vulkan Video decode"), "{op:?}: {message}");
    }
}

#[test]
fn device_serving_both_directions_admits_every_codec_operation() {
    for op in EVERY_DECODE_CODEC_OPERATION
        .into_iter()
        .chain(EVERY_ENCODE_CODEC_OPERATION)
    {
        assert!(
            require_device_video_coding_support(op, DECODE_AND_ENCODE).is_ok(),
            "{op:?}",
        );
    }
}

#[test]
fn combined_mask_is_refused_unless_its_direction_is_served() {
    let decode_h264_and_h265 =
        vk::VideoCodecOperationFlagsKHR::DECODE_H264 | vk::VideoCodecOperationFlagsKHR::DECODE_H265;
    let message = gpu_error_message(require_device_video_coding_support(
        decode_h264_and_h265,
        ENCODE_ONLY,
    ));
    assert!(message.contains("Vulkan Video decode"), "{message}");

    let decode_and_encode =
        vk::VideoCodecOperationFlagsKHR::DECODE_H264 | vk::VideoCodecOperationFlagsKHR::ENCODE_H264;
    let message = gpu_error_message(require_device_video_coding_support(
        decode_and_encode,
        DECODE_AND_ENCODE,
    ));
    assert!(message.contains("not a single-direction"), "{message}");
}

#[test]
fn max_bound_memory_matches_nvpro_cap() {
    // nvpro-samples `MAX_BOUND_MEMORY = 40`. Anchor here so a
    // future bump is visible.
    assert_eq!(MAX_BOUND_MEMORY, 40);
}

fn baseline_snapshot() -> VideoSessionCreateInfoSnapshot {
    VideoSessionCreateInfoSnapshot {
        queue_family_index: 0,
        picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_coded_extent: vk::Extent2D {
            width: 1920,
            height: 1080,
        },
        reference_picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_dpb_slots: 16,
        max_active_reference_pictures: 16,
    }
}

fn baseline_descriptor() -> VideoSessionDescriptor<'static> {
    VideoSessionDescriptor {
        label: "test/baseline",
        session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
        video_queue_family: 0,
        video_profile: vk::VideoProfileInfoKHR::default(),
        codec_operation: vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_coded_extent: vk::Extent2D {
            width: 1920,
            height: 1080,
        },
        reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_dpb_slots: 16,
        max_active_reference_pictures: 16,
    }
}

#[test]
fn snapshot_compatible_when_descriptor_matches_baseline() {
    let snap = baseline_snapshot();
    let desc = baseline_descriptor();
    assert!(snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_compatible_when_descriptor_under_all_caps() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.max_coded_extent = vk::Extent2D {
        width: 1280,
        height: 720,
    };
    desc.max_dpb_slots = 8;
    desc.max_active_reference_pictures = 4;
    assert!(snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_width_exceeds_cap() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.max_coded_extent.width = 1921;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_height_exceeds_cap() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.max_coded_extent.height = 1081;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_dpb_slots_exceed_cap() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.max_dpb_slots = 17;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_active_refs_exceed_cap() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.max_active_reference_pictures = 17;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_picture_format_differs() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.picture_format = vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_reference_format_differs() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.reference_pictures_format = vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_queue_family_differs() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.video_queue_family = 1;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

#[test]
fn snapshot_incompatible_when_session_flags_differ() {
    let snap = baseline_snapshot();
    let mut desc = baseline_descriptor();
    desc.session_create_flags = vk::VideoSessionCreateFlagsKHR::PROTECTED_CONTENT;
    assert!(!snapshot_is_compatible(
        &snap,
        vk::VideoSessionCreateFlagsKHR::empty(),
        &desc
    ));
}

// ----- Hardware-gated --------------------------------------------------

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-vulkan-video/hardware-tests + run with --test-threads=1"
)]
#[test]
fn hardware_construct_h264_encode_session() {
    let Some(device) =
        crate::hardware_test_codec_device::owned_video_encode_device_on_the_first_gpu()
    else {
        return;
    };
    let Some(video_encode_queue) = device.video_encode_queue() else {
        return;
    };
    let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
    let mut video_profile = vk::VideoProfileInfoKHR::default();
    video_profile.video_codec_operation = vk::VideoCodecOperationFlagsKHR::ENCODE_H264;
    video_profile.chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
    video_profile.luma_bit_depth = vk::VideoComponentBitDepthFlagsKHR::_8;
    video_profile.chroma_bit_depth = vk::VideoComponentBitDepthFlagsKHR::_8;
    video_profile.next = &mut *h264_profile as *mut _ as *mut std::ffi::c_void;

    let descriptor = VideoSessionDescriptor {
        label: "hardware/h264-encode-construct",
        session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
        video_queue_family: video_encode_queue.queue_family_index,
        video_profile,
        codec_operation: vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
        picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_coded_extent: vk::Extent2D {
            width: 1920,
            height: 1088,
        },
        reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        max_dpb_slots: 4,
        max_active_reference_pictures: 1,
    };
    match VulkanVideoSession::new(&device, &descriptor) {
        Ok(session) => {
            assert_ne!(
                session.handle(),
                vk::VideoSessionKHR::null(),
                "session handle must be non-null"
            );
            assert!(session.is_compatible(&descriptor));
            let mut smaller = descriptor.clone();
            smaller.max_dpb_slots = 2;
            assert!(session.is_compatible(&smaller));
            let mut larger = descriptor.clone();
            larger.max_coded_extent.width = descriptor.max_coded_extent.width + 1;
            assert!(!session.is_compatible(&larger));
        }
        Err(e) => {
            tracing::warn!(
                "Skipping — driver rejected H.264 encode session at construction (likely insufficient caps on this device): {e:?}"
            );
        }
    }
}

#[test]
fn a_video_session_refused_for_too_many_objects_is_the_session_limit() {
    let refusal = video_session_creation_refusal("encode/session", vk::ErrorCode::TOO_MANY_OBJECTS);
    assert!(
        matches!(
            &refusal,
            VideoError::VideoSessionLimitReached { session_label } if session_label == "encode/session"
        ),
        "{refusal:?}"
    );
}

#[test]
fn a_video_session_refused_for_anything_else_names_the_call() {
    let refusal =
        video_session_creation_refusal("encode/session", vk::ErrorCode::OUT_OF_DEVICE_MEMORY);
    assert!(matches!(refusal, VideoError::Rhi(_)), "{refusal:?}");
    assert!(
        refusal
            .to_string()
            .contains("create_video_session_khr failed"),
        "{refusal}"
    );
}
