// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_is_valid_codec() {
    assert!(VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264
    ));
    assert!(VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::DECODE_H265
    ));
    assert!(VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::DECODE_AV1
    ));
    assert!(VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::ENCODE_H264
    ));
    assert!(VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::ENCODE_H265
    ));
    assert!(VkVideoCoreProfile::is_valid_codec(CODEC_OP_DECODE_VP9));
    assert!(VkVideoCoreProfile::is_valid_codec(CODEC_OP_ENCODE_AV1));
    assert!(!VkVideoCoreProfile::is_valid_codec(
        vk::VideoCodecOperationFlagsKHR::NONE
    ));
}

#[test]
fn test_is_decode_codec_type() {
    let profile = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(profile.is_decode_codec_type());
    assert!(!profile.is_encode_codec_type());
}

#[test]
fn test_is_encode_codec_type() {
    let profile = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(profile.is_encode_codec_type());
    assert!(!profile.is_decode_codec_type());
}

#[test]
fn test_profile_equality() {
    let a = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    let b = a.clone();
    assert_eq!(a, b);

    let c = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H265,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert_ne!(a, c);
}

#[test]
fn test_profile_inequality_chroma() {
    let a = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    let b = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_444,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert_ne!(a, b);
}

#[test]
fn test_get_color_subsampling_generic() {
    let profile = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_422,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert_eq!(
        profile.get_color_subsampling_generic(),
        StdChromaFormatIdc::Chroma422
    );
}

#[test]
fn test_luma_chroma_bit_depth() {
    let profile = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H265,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN_10.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert_eq!(profile.get_luma_bit_depth_minus8(), 2);
    assert_eq!(profile.get_chroma_bit_depth_minus8(), 2);
    assert!(profile.is_16bit_format());
}

#[test]
fn test_8bit_not_16bit() {
    let profile = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(!profile.is_16bit_format());
}

#[test]
fn test_codec_get_vk_format_420_8bit() {
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            true,
        ),
        vk::Format::G8_B8R8_2PLANE_420_UNORM
    );
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            false,
        ),
        vk::Format::G8_B8_R8_3PLANE_420_UNORM
    );
}

#[test]
fn test_codec_get_vk_format_monochrome() {
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            true,
        ),
        vk::Format::R8_UNORM
    );
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            false,
        ),
        vk::Format::R10X6_UNORM_PACK16
    );
}

#[test]
fn test_get_video_chroma_format_from_vk_format() {
    assert_eq!(
        VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
            vk::Format::G8_B8R8_2PLANE_420_UNORM
        ),
        StdChromaFormatIdc::Chroma420
    );
    assert_eq!(
        VkVideoCoreProfile::get_video_chroma_format_from_vk_format(vk::Format::R8_UNORM),
        StdChromaFormatIdc::Monochrome
    );
    assert_eq!(
        VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
            vk::Format::G8_B8_R8_3PLANE_444_UNORM
        ),
        StdChromaFormatIdc::Chroma444
    );
    assert_eq!(
        VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
            vk::Format::G8_B8R8_2PLANE_422_UNORM
        ),
        StdChromaFormatIdc::Chroma422
    );
}

#[test]
fn test_codec_to_name() {
    assert_eq!(
        VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::DECODE_H264),
        "decode h.264"
    );
    assert_eq!(
        VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::ENCODE_H265),
        "encode h.265"
    );
    assert_eq!(
        VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
        "decode av1"
    );
    assert_eq!(
        VkVideoCoreProfile::codec_to_name(CODEC_OP_DECODE_VP9),
        "decode vp9"
    );
}

#[test]
fn test_matrix_coefficients() {
    let bt709 = VkVideoCoreProfile::codec_get_matrix_coefficients(1);
    assert!((bt709.kb - 0.0722).abs() < 1e-6);
    assert!((bt709.kr - 0.2126).abs() < 1e-6);

    let bt2020 = VkVideoCoreProfile::codec_get_matrix_coefficients(9);
    assert!((bt2020.kb - 0.0593).abs() < 1e-6);
    assert!((bt2020.kr - 0.2627).abs() < 1e-6);

    let unknown = VkVideoCoreProfile::codec_get_matrix_coefficients(255);
    assert!((unknown.kb - 1.0).abs() < 1e-6);
    assert!((unknown.kr - 1.0).abs() < 1e-6);
}

#[test]
fn test_get_codec_specific_profiles() {
    let decode_h264 = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(decode_h264.get_decode_h264_profile().is_some());
    assert!(decode_h264.get_decode_h265_profile().is_none());
    assert!(decode_h264.get_encode_h264_profile().is_none());

    let encode_h265 = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::VideoComponentBitDepthFlagsKHR::_8,
        vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(encode_h265.get_encode_h265_profile().is_some());
    assert!(encode_h265.get_decode_h265_profile().is_none());
}

#[test]
fn test_default_is_invalid() {
    let profile = VkVideoCoreProfile::new_default();
    assert!(!profile.is_valid());
    // get_profile() returns Some because s_type is set, but the profile
    // has NONE codec operation and INVALID subsampling/bit-depth.
    let p = profile.get_profile().unwrap();
    assert_eq!(
        p.video_codec_operation,
        vk::VideoCodecOperationFlagsKHR::NONE
    );
}

#[test]
fn test_clone_and_copy() {
    let original = VkVideoCoreProfile::new(
        vk::VideoCodecOperationFlagsKHR::DECODE_AV1,
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::VideoComponentBitDepthFlagsKHR::_10,
        vk::video::STD_VIDEO_AV1_PROFILE_MAIN.0 as u32,
        vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
        None,
    );
    assert!(original.is_valid());
    assert!(original.get_decode_av1_profile().is_some());

    let cloned = original.clone();
    assert!(cloned.is_valid());
    assert_eq!(original, cloned);
    assert!(cloned.get_decode_av1_profile().is_some());
}

#[test]
fn test_chroma_format_idc_values() {
    // Verify our enum values match the native C constants.
    assert_eq!(
        StdChromaFormatIdc::Monochrome as u32,
        vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME.0 as u32,
    );
    assert_eq!(
        StdChromaFormatIdc::Chroma420 as u32,
        vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420.0 as u32,
    );
    assert_eq!(
        StdChromaFormatIdc::Chroma422 as u32,
        vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_422.0 as u32,
    );
    assert_eq!(
        StdChromaFormatIdc::Chroma444 as u32,
        vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_444.0 as u32,
    );
}

#[test]
fn test_codec_get_vk_format_all_subsampling_depths() {
    // 422 / 10-bit
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::_422,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            true,
        ),
        vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
    );
    // 444 / 12-bit / 3-plane
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::_444,
            vk::VideoComponentBitDepthFlagsKHR::_12,
            false,
        ),
        vk::Format::G12X4_B12X4_R12X4_3PLANE_444_UNORM_3PACK16
    );
    // monochrome / 12-bit
    assert_eq!(
        VkVideoCoreProfile::codec_get_vk_format(
            vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
            vk::VideoComponentBitDepthFlagsKHR::_12,
            true,
        ),
        vk::Format::R12X4_UNORM_PACK16
    );
}
