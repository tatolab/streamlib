// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unit tests for the encoder module — pure logic only (no GPU).

use super::config::*;
use vulkanalia::vk;

// -- FrameType --

#[test]
fn test_frame_type_name() {
    assert_eq!(FrameType::Idr.name(), "IDR");
    assert_eq!(FrameType::I.name(), "I");
    assert_eq!(FrameType::P.name(), "P");
    assert_eq!(FrameType::B.name(), "B");
}

#[test]
fn test_frame_type_default() {
    assert_eq!(FrameType::default(), FrameType::P);
}

#[test]
fn test_frame_type_equality() {
    assert_eq!(FrameType::Idr, FrameType::Idr);
    assert_ne!(FrameType::Idr, FrameType::I);
    assert_ne!(FrameType::P, FrameType::B);
}

// -- RateControlMode --

#[test]
fn test_rate_control_default() {
    assert_eq!(RateControlMode::default(), RateControlMode::Default);
}

#[test]
fn test_rate_control_to_vk_flags() {
    assert_eq!(
        RateControlMode::Default.to_vk_flags(),
        vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT
    );
    assert_eq!(
        RateControlMode::Cbr.to_vk_flags(),
        vk::VideoEncodeRateControlModeFlagsKHR::CBR
    );
    assert_eq!(
        RateControlMode::Vbr.to_vk_flags(),
        vk::VideoEncodeRateControlModeFlagsKHR::VBR
    );
    assert_eq!(
        RateControlMode::Cqp.to_vk_flags(),
        vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
    );
}

// -- EncodeConfig validation --

#[test]
fn test_config_validate_ok() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_zero_width() {
    let cfg = EncodeConfig {
        width: 0,
        height: 1080,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_zero_height() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_zero_framerate() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        framerate_numerator: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_dpb_slots_zero() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        max_dpb_slots: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_dpb_slots_too_many() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        max_dpb_slots: 17,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

// -- Alignment --

#[test]
fn test_aligned_width_already_aligned() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    assert_eq!(cfg.aligned_width(), 1920);
}

#[test]
fn test_aligned_width_not_aligned() {
    let cfg = EncodeConfig {
        width: 1921,
        height: 1080,
        ..Default::default()
    };
    assert_eq!(cfg.aligned_width(), 1936);
}

#[test]
fn test_aligned_height_already_aligned() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1088,
        ..Default::default()
    };
    assert_eq!(cfg.aligned_height(), 1088);
}

#[test]
fn test_aligned_height_not_aligned() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    assert_eq!(cfg.aligned_height(), 1088);
}

// -- Bitstream buffer size --

#[test]
fn test_effective_bitstream_buffer_size_default() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        bitstream_buffer_size: 0,
        ..Default::default()
    };
    let size = cfg.effective_bitstream_buffer_size();
    assert!(size >= 2 * 1024 * 1024);
    // 1920*1080 = 2073600 < 2MiB (2097152), so 2MiB wins
    assert_eq!(size, 2 * 1024 * 1024);
}

#[test]
fn test_effective_bitstream_buffer_size_explicit() {
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        bitstream_buffer_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    assert_eq!(cfg.effective_bitstream_buffer_size(), 4 * 1024 * 1024);
}

#[test]
fn test_effective_bitstream_buffer_size_small_resolution() {
    let cfg = EncodeConfig {
        width: 320,
        height: 240,
        bitstream_buffer_size: 0,
        ..Default::default()
    };
    assert_eq!(cfg.effective_bitstream_buffer_size(), 2 * 1024 * 1024);
}

// -- EncodeConfig defaults --

#[test]
fn test_config_defaults() {
    let cfg = EncodeConfig::default();
    assert_eq!(cfg.width, 0);
    assert_eq!(cfg.height, 0);
    assert_eq!(cfg.framerate_numerator, 30);
    assert_eq!(cfg.framerate_denominator, 1);
    assert_eq!(cfg.codec, vk::VideoCodecOperationFlagsKHR::ENCODE_H264);
    assert_eq!(cfg.rate_control_mode, RateControlMode::Default);
    assert_eq!(cfg.average_bitrate, 5_000_000);
    assert_eq!(cfg.gop_size, 30);
    assert_eq!(cfg.idr_period, 60);
    assert_eq!(cfg.num_b_frames, 0);
    assert_eq!(cfg.max_dpb_slots, 16);
    assert_eq!(cfg.const_qp_intra, 26);
    assert_eq!(cfg.const_qp_inter_p, 28);
    assert_eq!(cfg.const_qp_inter_b, 30);
}

// -- EncodedOutput --

#[test]
fn test_encoded_output_clone() {
    let output = EncodedOutput {
        data: vec![0x00, 0x00, 0x00, 0x01, 0x65],
        frame_type: FrameType::Idr,
        pts: 0,
        encode_order: 0,
        bitstream_offset: 0,
        bitstream_size: 5,
    };
    let cloned = output.clone();
    assert_eq!(cloned.data, output.data);
    assert_eq!(cloned.frame_type, output.frame_type);
    assert_eq!(cloned.pts, output.pts);
    assert_eq!(cloned.bitstream_size, 5);
}

// -- Encoder construction (no GPU) --

#[test]
fn test_encoder_config_integration() {
    // Verify the type system works end-to-end without a GPU.
    let cfg = EncodeConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
    assert_eq!(cfg.aligned_width(), 1920);
    assert_eq!(cfg.aligned_height(), 1088);
}

// ===================================================================
// SimpleEncoderConfig validation tests
// ===================================================================

#[test]
fn test_simple_config_validate_ok() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 1080,
        fps: 30,
        codec: Codec::H264,
        preset: Preset::Medium,
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_simple_config_validate_zero_width() {
    let cfg = SimpleEncoderConfig {
        width: 0,
        height: 1080,
        fps: 30,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_simple_config_validate_zero_height() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 0,
        fps: 30,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_simple_config_validate_odd_width() {
    let cfg = SimpleEncoderConfig {
        width: 1921,
        height: 1080,
        fps: 30,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("even"));
}

#[test]
fn test_simple_config_validate_odd_height() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 1081,
        fps: 30,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("even"));
}

#[test]
fn test_simple_config_validate_zero_fps() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 1080,
        fps: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_simple_config_validate_qp_out_of_range() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        qp: Some(52),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());

    let cfg2 = SimpleEncoderConfig {
        qp: Some(-1),
        ..cfg
    };
    assert!(cfg2.validate().is_err());
}

#[test]
fn test_simple_config_validate_qp_valid() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        qp: Some(26),
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_simple_config_validate_zero_bitrate() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        bitrate_bps: Some(0),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_simple_config_validate_streaming_zero_idr_interval() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        streaming: true,
        idr_interval_secs: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn test_simple_config_validate_streaming_ok() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_simple_config_to_encode_config_preset_fast() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 1080,
        fps: 30,
        preset: Preset::Fast,
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    assert_eq!(enc.rate_control_mode, RateControlMode::Cqp);
    assert_eq!(enc.const_qp_intra, 20);
    assert_eq!(enc.gop_size, 30);
    assert_eq!(enc.num_b_frames, 0);
}

#[test]
fn test_simple_config_to_encode_config_preset_quality() {
    let cfg = SimpleEncoderConfig {
        width: 1920,
        height: 1080,
        fps: 30,
        preset: Preset::Quality,
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    assert_eq!(enc.const_qp_intra, 15);
    assert_eq!(enc.gop_size, 60);
}

#[test]
fn test_simple_config_to_encode_config_explicit_qp() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        qp: Some(20),
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    assert_eq!(enc.rate_control_mode, RateControlMode::Cqp);
    assert_eq!(enc.const_qp_intra, 20);
    assert_eq!(enc.const_qp_inter_p, 22);
}

#[test]
fn test_simple_config_to_encode_config_explicit_bitrate() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        bitrate_bps: Some(5_000_000),
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    assert_eq!(enc.rate_control_mode, RateControlMode::Vbr);
    assert_eq!(enc.average_bitrate, 5_000_000);
    assert_eq!(enc.max_bitrate, 10_000_000);
}

#[test]
fn test_simple_config_to_encode_config_streaming() {
    let cfg = SimpleEncoderConfig {
        width: 1280,
        height: 720,
        fps: 30,
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    // IDR period = 2 * 30 = 60
    assert_eq!(enc.idr_period, 60);
    assert_eq!(enc.gop_size, 60);
    assert_eq!(enc.num_b_frames, 0);
}

#[test]
fn test_simple_config_effective_prepend_header() {
    let mut cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        ..Default::default()
    };
    // Non-streaming: default false
    assert!(!cfg.effective_prepend_header());

    // Streaming: default true
    cfg.streaming = true;
    cfg.idr_interval_secs = 2;
    assert!(cfg.effective_prepend_header());

    // Explicit override
    cfg.prepend_header_to_idr = Some(false);
    assert!(!cfg.effective_prepend_header());
}

#[test]
fn test_simple_config_codec_h265() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        codec: Codec::H265,
        ..Default::default()
    };
    let enc = cfg.to_encode_config();
    assert_eq!(enc.codec, vk::VideoCodecOperationFlagsKHR::ENCODE_H265);
}

#[test]
fn test_simple_config_default() {
    let cfg = SimpleEncoderConfig::default();
    assert_eq!(cfg.width, 0);
    assert_eq!(cfg.height, 0);
    assert_eq!(cfg.fps, 30);
    assert_eq!(cfg.codec, Codec::H264);
    assert_eq!(cfg.preset, Preset::Medium);
    assert!(cfg.qp.is_none());
    assert!(cfg.bitrate_bps.is_none());
    assert!(!cfg.streaming);
    assert_eq!(cfg.idr_interval_secs, 2);
    assert!(cfg.prepend_header_to_idr.is_none());
}

#[test]
fn test_encode_packet_clone() {
    let pkt = EncodePacket {
        data: vec![0x00, 0x00, 0x00, 0x01, 0x65],
        frame_type: FrameType::Idr,
        pts: 42,
        is_keyframe: true,
        timestamp_ns: Some(1_000_000_000),
    };
    let c = pkt.clone();
    assert_eq!(c.data, pkt.data);
    assert_eq!(c.pts, 42);
    assert!(c.is_keyframe);
}

#[test]
fn test_gop_structure_from_config() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        preset: Preset::Fast,
        ..Default::default()
    };
    let gop = cfg.to_gop_structure();
    assert_eq!(gop.consecutive_b_frame_count(), 0);
    assert_eq!(gop.idr_period(), 60);
    assert_eq!(gop.gop_frame_count(), 30);
}

#[test]
fn test_gop_structure_streaming() {
    let cfg = SimpleEncoderConfig {
        width: 640,
        height: 480,
        fps: 30,
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };
    let gop = cfg.to_gop_structure();
    assert_eq!(gop.consecutive_b_frame_count(), 0);
    assert_eq!(gop.idr_period(), 60);
}
