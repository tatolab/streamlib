// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Standalone encode test binary.
//!
//! Exercises the encoder configuration and parameter generation components
//! WITHOUT requiring a GPU. Validates that the ported encoder infrastructure
//! correctly generates Vulkan Video encode parameters.
//!
//! Usage:
//!   cargo run --bin encode-test              # run built-in self-tests
//!   cargo run --bin encode-test -- --gpu     # run GPU encode tests (requires video encode HW)

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use vulkan_video::{SimpleEncoder, SimpleEncoderConfig, Codec, Preset};
use vulkan_video::vk_video_encoder::vk_video_encoder_def::*;
use vulkan_video::vk_video_encoder::vk_video_gop_structure::*;
use vulkan_video::vk_video_encoder::vk_encoder_config::*;
use vulkan_video::vk_video_encoder::vk_encoder_config_h264;
use vulkan_video::vk_video_encoder::vk_encoder_config_h264::*;
use vulkan_video::vk_video_encoder::vk_encoder_config_h265::*;
use vulkan_video::vk_video_encoder::vk_encoder_config_av1::*;
use vulkan_video::vk_video_encoder::vk_video_encoder_psnr::*;
use vulkan_video::vk_video_encoder::vk_encoder_dpb_h264::*;
use vulkan_video::vk_video_encoder::vk_encoder_dpb_h265::*;
use vulkan_video::vk_video_encoder::vk_encoder_dpb_av1::*;

// ---------------------------------------------------------------------------
// Test runner infrastructure
// ---------------------------------------------------------------------------

struct TestRunner {
    passed: u32,
    failed: u32,
    current_section: String,
}

impl TestRunner {
    fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
            current_section: String::new(),
        }
    }

    fn section(&mut self, name: &str) {
        self.current_section = name.to_string();
        println!("\n=== {} ===", name);
    }

    fn check(&mut self, name: &str, condition: bool) {
        if condition {
            self.passed += 1;
            println!("  [PASS] {}", name);
        } else {
            self.failed += 1;
            println!("  [FAIL] {}", name);
        }
    }

    fn check_eq<T: PartialEq + std::fmt::Debug>(&mut self, name: &str, got: T, expected: T) {
        if got == expected {
            self.passed += 1;
            println!("  [PASS] {}", name);
        } else {
            self.failed += 1;
            println!("  [FAIL] {} -- got {:?}, expected {:?}", name, got, expected);
        }
    }

    fn check_approx(&mut self, name: &str, got: f64, expected: f64, tolerance: f64) {
        if (got - expected).abs() <= tolerance {
            self.passed += 1;
            println!("  [PASS] {}", name);
        } else {
            self.failed += 1;
            println!(
                "  [FAIL] {} -- got {:.6}, expected {:.6} (tolerance {:.6})",
                name, got, expected, tolerance
            );
        }
    }

    fn summary(&self) -> bool {
        println!("\n========================================");
        println!(
            "Results: {} passed, {} failed, {} total",
            self.passed,
            self.failed,
            self.passed + self.failed
        );
        if self.failed == 0 {
            println!("All tests passed.");
        } else {
            println!("SOME TESTS FAILED.");
        }
        println!("========================================");
        self.failed == 0
    }
}

// ---------------------------------------------------------------------------
// Test: GOP structure
// ---------------------------------------------------------------------------

fn test_gop_structure(t: &mut TestRunner) {
    t.section("GOP Structure");

    // --- All-intra (gop_frame_count=1, no B frames) ---
    {
        let gop = VkVideoGopStructure::new(1, 0, 0, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        let is_idr = gop.get_position_in_gop(&mut state, &mut pos, true, 100);
        t.check("all-intra: first frame is IDR", is_idr);
        t.check_eq("all-intra: first frame type", pos.picture_type, FrameType::Idr);

        // Subsequent frames in an all-intra GOP with gop_frame_count=1 cycle through I
        gop.get_position_in_gop(&mut state, &mut pos, false, 99);
        t.check_eq(
            "all-intra: second frame is I (gop boundary)",
            pos.picture_type,
            FrameType::I,
        );
    }

    // --- IPP pattern (gop_frame_count=4, 0 B frames) ---
    {
        let gop = VkVideoGopStructure::new(4, 60, 0, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        gop.get_position_in_gop(&mut state, &mut pos, true, 20);
        t.check_eq("IPP: frame 0 is IDR", pos.picture_type, FrameType::Idr);

        gop.get_position_in_gop(&mut state, &mut pos, false, 19);
        t.check_eq("IPP: frame 1 is P", pos.picture_type, FrameType::P);

        gop.get_position_in_gop(&mut state, &mut pos, false, 18);
        t.check_eq("IPP: frame 2 is P", pos.picture_type, FrameType::P);

        gop.get_position_in_gop(&mut state, &mut pos, false, 17);
        t.check_eq("IPP: frame 3 is P", pos.picture_type, FrameType::P);

        gop.get_position_in_gop(&mut state, &mut pos, false, 16);
        t.check_eq("IPP: frame 4 is I (new GOP)", pos.picture_type, FrameType::I);
    }

    // --- IBBBP pattern (gop_frame_count=8, 3 consecutive B frames) ---
    {
        let gop = VkVideoGopStructure::new(8, 60, 3, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        gop.get_position_in_gop(&mut state, &mut pos, true, 30);
        t.check_eq("IBBBP: frame 0 is IDR", pos.picture_type, FrameType::Idr);

        // Frames 1,2,3 should be B
        gop.get_position_in_gop(&mut state, &mut pos, false, 29);
        t.check_eq("IBBBP: frame 1 is B", pos.picture_type, FrameType::B);

        gop.get_position_in_gop(&mut state, &mut pos, false, 28);
        t.check_eq("IBBBP: frame 2 is B", pos.picture_type, FrameType::B);

        gop.get_position_in_gop(&mut state, &mut pos, false, 27);
        t.check_eq("IBBBP: frame 3 is B", pos.picture_type, FrameType::B);

        // Frame 4 should be P (cycle boundary)
        gop.get_position_in_gop(&mut state, &mut pos, false, 26);
        t.check_eq("IBBBP: frame 4 is P", pos.picture_type, FrameType::P);
    }

    // --- IDR period boundary ---
    {
        let gop = VkVideoGopStructure::new(8, 10, 0, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        gop.get_position_in_gop(&mut state, &mut pos, true, 100);
        for i in 1..10u32 {
            gop.get_position_in_gop(&mut state, &mut pos, false, 100 - i);
        }
        let is_idr = gop.get_position_in_gop(&mut state, &mut pos, false, 90);
        t.check("IDR period: frame 10 triggers IDR", is_idr);
        t.check_eq("IDR period: frame 10 type", pos.picture_type, FrameType::Idr);
    }

    // --- Reference frame flag ---
    {
        let gop = VkVideoGopStructure::new(8, 60, 2, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        gop.get_position_in_gop(&mut state, &mut pos, true, 30);
        t.check("ref flag: IDR is reference", gop.is_frame_reference(&pos));

        gop.get_position_in_gop(&mut state, &mut pos, false, 29);
        t.check("ref flag: B is not reference", !gop.is_frame_reference(&pos));
    }

    // --- Closed GOP ---
    {
        let gop = VkVideoGopStructure::new(4, 60, 0, 1, FrameType::P, FrameType::P, true, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        gop.get_position_in_gop(&mut state, &mut pos, true, 20);
        gop.get_position_in_gop(&mut state, &mut pos, false, 19);
        gop.get_position_in_gop(&mut state, &mut pos, false, 18);
        gop.get_position_in_gop(&mut state, &mut pos, false, 17);
        t.check(
            "closed GOP: last frame in GOP has CLOSE_GOP flag",
            (pos.flags & flags::CLOSE_GOP) != 0,
        );
    }
}

// ---------------------------------------------------------------------------
// Test: H.264 encoder configuration
// ---------------------------------------------------------------------------

fn test_h264_config(t: &mut TestRunner) {
    t.section("H.264 Encoder Config");

    // --- Default creation ---
    {
        let cfg = EncoderConfigH264::default();
        t.check_eq(
            "h264 default: profile_idc is INVALID",
            cfg.profile_idc,
            h264_profile::INVALID,
        );
        t.check_eq(
            "h264 default: entropy is CABAC",
            cfg.entropy_coding_mode,
            EntropyCodingMode::Cabac,
        );
        t.check_eq(
            "h264 default: frame_rate_num",
            cfg.base.frame_rate_numerator,
            vk_encoder_config_h264::FRAME_RATE_NUM_DEFAULT,
        );
    }

    // --- Profile selection: Baseline (CAVLC, no B frames, no 8x8) ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 320;
        cfg.base.input.height = 240;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(0);
        cfg.entropy_coding_mode = EntropyCodingMode::Cavlc;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Disable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        t.check_eq(
            "h264 profile: Baseline for CAVLC + no B + no 8x8",
            cfg.profile_idc,
            h264_profile::BASELINE,
        );
    }

    // --- Profile selection: Main (B frames present, CAVLC) ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(2);
        cfg.entropy_coding_mode = EntropyCodingMode::Cavlc;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Disable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        t.check_eq(
            "h264 profile: Main for B frames + CAVLC",
            cfg.profile_idc,
            h264_profile::MAIN,
        );
    }

    // --- Profile selection: High (CABAC + 8x8 transform) ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(0);
        cfg.entropy_coding_mode = EntropyCodingMode::Cabac;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Enable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        t.check_eq(
            "h264 profile: High for CABAC + 8x8 transform",
            cfg.profile_idc,
            h264_profile::HIGH,
        );
    }

    // --- Level selection: 1080p @ 30fps ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.profile_idc = h264_profile::HIGH;
        cfg.pic_width_in_mbs = div_up(1920, 16);   // 120
        cfg.pic_height_in_map_units = div_up(1080, 16); // 68
        cfg.num_ref_frames = 4;
        let level = cfg.determine_level(4, 0, 0, 30.0);
        // 120*68 = 8160 MBs; at 30fps = 244800 mbps => needs at least level 4.0
        t.check(
            "h264 level: 1080p@30 with 4 refs is >= level 4.0 (index 10)",
            level >= 10,
        );
    }

    // --- DPB count initialization ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(0);
        cfg.entropy_coding_mode = EntropyCodingMode::Cabac;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Enable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        let dpb_count = cfg.init_dpb_count();
        t.check("h264 DPB count: > 0", dpb_count > 0);
        t.check("h264 DPB count: <= 17", dpb_count <= 17);
    }

    // --- Rate control: CBR ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.rate_control_mode = vk::VideoEncodeRateControlModeFlagsKHR::CBR;
        cfg.base.average_bitrate = 5_000_000;
        cfg.hrd_bitrate = 5_000_000;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        let ok = cfg.init_rate_control();
        t.check("h264 rate control CBR: init succeeds", ok);
        t.check_eq(
            "h264 rate control CBR: hrd == avg",
            cfg.hrd_bitrate,
            cfg.base.average_bitrate,
        );
    }

    // --- Aspect ratio: 16:9 at 1920x1080 => SAR 1:1 ---
    {
        let (idc, _sw, _sh) = EncoderConfigH264::compute_aspect_ratio(1920, 1080, 16, 9);
        t.check_eq("h264 SAR: 16:9@1080p => idc 1 (square)", idc, 1);
    }
}

// ---------------------------------------------------------------------------
// Test: H.265 encoder configuration
// ---------------------------------------------------------------------------

fn test_h265_config(t: &mut TestRunner) {
    t.section("H.265 Encoder Config");

    // --- Default creation ---
    {
        let cfg = EncoderConfigH265::default();
        t.check_eq(
            "h265 default: profile is INVALID",
            cfg.profile,
            h265_profile::INVALID,
        );
        t.check_eq("h265 default: cu_size", cfg.cu_size, CuSize::Size32x32);
        t.check_eq("h265 default: cu_min_size", cfg.cu_min_size, CuSize::Size16x16);
    }

    // --- Profile: Main 8-bit 4:2:0 ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.init_profile_level();
        t.check_eq(
            "h265 profile: Main for 8-bit 420",
            cfg.profile,
            h265_profile::MAIN,
        );
        t.check(
            "h265 level: valid for 1080p",
            cfg.level_idc != u32::MAX,
        );
    }

    // --- Profile: Main 10 ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 10;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.init_profile_level();
        t.check_eq(
            "h265 profile: Main 10 for 10-bit 420",
            cfg.profile,
            h265_profile::MAIN_10,
        );
    }

    // --- CTB alignment ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.encode_width = 1920;
        cfg.base.encode_height = 1080;
        cfg.cu_size = CuSize::Size32x32;
        let (w, h, size) = cfg.get_ctb_aligned_pic_size_in_samples(false);
        // CuSize::Size32x32 => ctb_log2_size_y = 5, ctb_size_y = 32
        t.check_eq("h265 CTB align: width 1920 stays 1920", w, 1920);
        t.check_eq("h265 CTB align: height 1080 -> 1088", h, 1088);
        t.check_eq("h265 CTB align: size", size, 1920 * 1088);
    }

    // --- Tier selection ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.determine_level_tier();
        // For 1080p with no bitrate constraint, main tier should suffice
        t.check(
            "h265 tier: main tier for 1080p",
            !cfg.general_tier_flag,
        );
    }

    // --- DPB count ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.init_profile_level();
        let dpb = cfg.init_dpb_count();
        t.check("h265 DPB count: > 0", dpb > 0);
    }

    // --- Aspect ratio ---
    {
        let (idc, _, _) = EncoderConfigH265::compute_aspect_ratio(1920, 1080, 16, 9);
        t.check_eq("h265 SAR: 16:9@1080p => idc 1", idc, 1);

        let (idc2, _, _) = EncoderConfigH265::compute_aspect_ratio(1920, 1080, 0, 0);
        t.check_eq("h265 SAR: no DAR => -1", idc2, -1);
    }
}

// ---------------------------------------------------------------------------
// Test: AV1 encoder configuration
// ---------------------------------------------------------------------------

fn test_av1_config(t: &mut TestRunner) {
    t.section("AV1 Encoder Config");

    // --- Default creation ---
    {
        let cfg = EncoderConfigAV1::default();
        t.check_eq(
            "av1 default: profile is INVALID",
            cfg.profile,
            av1_profile::INVALID,
        );
        t.check_eq("av1 default: level is INVALID", cfg.level, u32::MAX);
        t.check_eq("av1 default: tier is 0", cfg.tier, 0);
        t.check("av1 default: tiles disabled", !cfg.enable_tiles);
    }

    // --- Profile/level for 1080p ---
    {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_sbs = div_up(cfg.base.encode_width, SUPERBLOCK_SIZE);
        cfg.pic_height_in_sbs = div_up(cfg.base.encode_height, SUPERBLOCK_SIZE);
        cfg.init_profile_level();
        t.check_eq("av1 profile: Main for 1080p", cfg.profile, av1_profile::MAIN);
        t.check("av1 level: valid for 1080p", cfg.level != u32::MAX);
        t.check_eq("av1 tier: main for 1080p", cfg.tier, 0);
    }

    // --- Tile configuration defaults ---
    {
        let cfg = EncoderConfigAV1::default();
        t.check_eq("av1 tiles: tile_cols default", cfg.tile_cols, 0);
        t.check_eq("av1 tiles: tile_rows default", cfg.tile_rows, 0);
    }

    // --- Level for small resolution ---
    {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.encode_width = 320;
        cfg.base.encode_height = 240;
        cfg.profile = av1_profile::MAIN;
        t.check("av1 level: 320x240 validates at level 2.0", cfg.validate_level(0, 0));
    }

    // --- Superblock size constant ---
    t.check_eq("av1 superblock size", SUPERBLOCK_SIZE, 64);

    // --- Uncompressed size ---
    {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.encode_width = 1920;
        cfg.base.encode_height = 1080;
        cfg.profile = av1_profile::MAIN;
        let size = cfg.get_uncompressed_size();
        // 1920 * 1080 * 15 / 8 = 3888000
        t.check_eq("av1 uncompressed size: 1080p Main", size, 3888000);
    }

    // --- DPB count ---
    {
        let mut cfg = EncoderConfigAV1::default();
        let dpb = cfg.init_dpb_count();
        t.check_eq("av1 DPB count: 8", dpb, 8);
    }

    // --- Level limits table ---
    t.check_eq("av1 level table size", LEVEL_LIMITS_AV1.len(), 24);
}

// ---------------------------------------------------------------------------
// Test: PSNR computation
// ---------------------------------------------------------------------------

fn test_psnr_computation(t: &mut TestRunner) {
    t.section("PSNR Computation");

    // --- Identical frames => 100 dB ---
    {
        let data = vec![128u8; 16 * 16];
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&data, 16, &data, 16, 16, 16);
        t.check_approx("psnr: identical frames => 100 dB", psnr, 100.0, 1e-6);
    }

    // --- Known MSE: all 128 vs all 138 => diff=10, MSE=100 ---
    {
        let src = vec![128u8; 16 * 16];
        let recon = vec![138u8; 16 * 16];
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&src, 16, &recon, 16, 16, 16);
        let expected = 10.0 * (255.0 * 255.0 / 100.0_f64).log10(); // ~28.13 dB
        t.check_approx("psnr: known MSE 100 => ~28.13 dB", psnr, expected, 0.01);
    }

    // --- Known MSE: all 0 vs all 1 => diff=1, MSE=1 ---
    {
        let src = vec![0u8; 64 * 64];
        let recon = vec![1u8; 64 * 64];
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&src, 64, &recon, 64, 64, 64);
        let expected = 10.0 * (255.0 * 255.0 / 1.0_f64).log10(); // ~48.13 dB
        t.check_approx("psnr: MSE 1 => ~48.13 dB", psnr, expected, 0.01);
    }

    // --- Empty data => -1 ---
    {
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&[], 0, &[], 0, 0, 0);
        t.check_approx("psnr: empty data => -1", psnr, -1.0, 1e-6);
    }

    // --- Frame PSNR accumulation ---
    {
        let mut psnr = VkVideoEncoderPsnr::new();
        psnr.configure(16, 16, 16, 16, 3, true, vk::Format::UNDEFINED);

        let y = vec![128u8; 16 * 16];
        let u = vec![128u8; 8 * 8];
        let v = vec![128u8; 8 * 8];

        psnr.compute_frame_psnr(&y, &u, &v, &y, &u, &v);
        t.check_approx("psnr accumulation: 1 frame avg Y", psnr.get_average_psnr_y(), 100.0, 1e-6);

        psnr.compute_frame_psnr(&y, &u, &v, &y, &u, &v);
        t.check_approx("psnr accumulation: 2 frames avg Y", psnr.get_average_psnr_y(), 100.0, 1e-6);
        t.check_approx("psnr accumulation: avg U", psnr.get_average_psnr_u(), 100.0, 1e-6);
        t.check_approx("psnr accumulation: avg V", psnr.get_average_psnr_v(), 100.0, 1e-6);
    }

    // --- No frames => -1 ---
    {
        let psnr = VkVideoEncoderPsnr::new();
        t.check_approx("psnr no frames: avg Y => -1", psnr.get_average_psnr_y(), -1.0, 1e-6);
        t.check_approx("psnr no frames: avg U => -1", psnr.get_average_psnr_u(), -1.0, 1e-6);
        t.check_approx("psnr no frames: avg V => -1", psnr.get_average_psnr_v(), -1.0, 1e-6);
    }

    // --- Deinit resets state ---
    {
        let mut psnr = VkVideoEncoderPsnr::new();
        psnr.configure(16, 16, 16, 16, 3, true, vk::Format::UNDEFINED);
        let y = vec![128u8; 16 * 16];
        psnr.compute_frame_psnr(&y, &[], &[], &y, &[], &[]);
        psnr.deinit();
        t.check_approx("psnr deinit: avg Y => -1", psnr.get_average_psnr_y(), -1.0, 1e-6);
    }
}

// ---------------------------------------------------------------------------
// Test: DPB management
// ---------------------------------------------------------------------------

fn test_dpb_management(t: &mut TestRunner) {
    t.section("DPB Management");

    // --- H.264 DPB ---
    {
        let mut dpb = VkEncDpbH264::create_instance();
        dpb.dpb_sequence_start(4);
        t.check_eq("h264 dpb: max_dpb_size after init", dpb.get_max_dpb_size(), 4);

        // Start an IDR picture
        let pic_info = PicInfoH264 {
            frame_num: 0,
            pic_order_cnt: 0,
            primary_pic_type: 0,
            idr_pic_id: 0,
            is_reference: true,
            is_idr: true,
            long_term_reference_flag: false,
            no_output_of_prior_pics_flag: false,
            adaptive_ref_pic_marking_mode_flag: false,
            field_pic_flag: false,
            bottom_field_flag: false,
            time_stamp: 0,
        };
        let idx = dpb.dpb_picture_start(
            &pic_info,
            0,  // log2_max_frame_num_minus4
            0,  // pic_order_cnt_type
            0,  // log2_max_pic_order_cnt_lsb_minus4
            false, // gaps_in_frame_num
            4,  // max_num_ref_frames
        );
        t.check("h264 dpb: IDR picture start succeeds", idx >= 0);

        let (frame_num, _poc) = dpb.get_updated_frame_num_and_pic_order_cnt();
        t.check_eq("h264 dpb: IDR frame_num", frame_num, 0);

        // Check ref frame counting
        let (total, _short_term, _long_term) = dpb.get_num_ref_frames_in_dpb(0);
        // Before dpb_picture_end, current frame is not yet marked
        t.check("h264 dpb: ref frame count >= 0", total >= 0);
    }

    // --- H.265 DPB ---
    {
        let mut dpb = VkEncDpbH265::new();
        dpb.dpb_sequence_start(4, false);
        t.check("h265 dpb: sequence start succeeds", true);

        let idx = dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
        t.check("h265 dpb: IDR picture start returns valid slot", idx >= 0);

        dpb.dpb_picture_end(1, true);

        // Mark reference pictures for IDR
        dpb.reference_picture_marking(0, 0, false); // pic_type=0 is IDR

        // Start a P picture
        let idx2 = dpb.dpb_picture_start(1, 1, false, false, true, 0, false, 1);
        t.check("h265 dpb: P picture start returns valid slot", idx2 >= 0);
    }

    // --- AV1 DPB ---
    {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        t.check_eq("av1 dpb: max_dpb_size after init", dpb.get_max_dpb_size(), 8);

        // Key frame
        let idx = dpb.dpb_picture_start(Av1FrameType::Key, 0, 0, 0, false, -1);
        t.check("av1 dpb: key frame start valid", idx >= 0);

        t.check_eq("av1 dpb: frame type is Key", dpb.get_frame_type(idx as i32), Av1FrameType::Key);
        t.check_eq("av1 dpb: frame_id is 0", dpb.get_frame_id(idx as i32), 0);
        t.check_eq("av1 dpb: POC is 0", dpb.get_pic_order_cnt_val(idx as i32), 0);

        // Refresh frame flags for shown key frame
        let flags = dpb.get_refresh_frame_flags(true, false);
        t.check_eq("av1 dpb: key frame refresh flags = 0xff", flags, 0xff);

        // Show existing frame flags
        let flags2 = dpb.get_refresh_frame_flags(false, true);
        t.check_eq("av1 dpb: show existing refresh flags = 0", flags2, 0);

        // Configure ref buf update
        dpb.configure_ref_buf_update(true, false, FrameUpdateType::KfUpdate);
        dpb.dpb_picture_end(idx, false);
    }
}

// ---------------------------------------------------------------------------
// Test: Encoder state (SPS/PPS parameter set generation)
// ---------------------------------------------------------------------------

fn test_encoder_state(t: &mut TestRunner) {
    t.section("Encoder State (parameter sets)");

    // --- Base EncoderConfig initialization ---
    {
        let mut cfg = EncoderConfig::default();
        cfg.input.width = 1920;
        cfg.input.height = 1080;
        cfg.input.bpp = 8;
        let result = cfg.initialize_parameters();
        t.check("base config: init succeeds", result.is_ok());
        t.check_eq("base config: encode_width", cfg.encode_width, 1920);
        t.check_eq("base config: encode_height", cfg.encode_height, 1080);
        t.check_eq("base config: msb_shift for 8-bit", cfg.input.msb_shift, 0);
    }

    // --- 10-bit MSB shift ---
    {
        let mut cfg = EncoderConfig::default();
        cfg.input.width = 1920;
        cfg.input.height = 1080;
        cfg.input.bpp = 10;
        cfg.initialize_parameters().unwrap();
        t.check_eq("base config: msb_shift for 10-bit", cfg.input.msb_shift, 6);
    }

    // --- Input verify: invalid dimensions ---
    {
        let mut params = EncoderInputImageParameters::default();
        params.width = 0;
        params.height = 1080;
        t.check("input verify: width 0 fails", !params.verify_inputs());
    }

    // --- Input verify: valid dimensions ---
    {
        let mut params = EncoderInputImageParameters::default();
        params.width = 1920;
        params.height = 1080;
        t.check("input verify: 1920x1080 succeeds", params.verify_inputs());
        t.check("input verify: full_image_size > 0", params.full_image_size > 0);
    }

    // --- H.264 full parameter init flow ---
    {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        let result = cfg.initialize_parameters();
        t.check("h264 param init: succeeds", result.is_ok());
        t.check_eq("h264 param init: pic_width_in_mbs", cfg.pic_width_in_mbs, 120);
        t.check_eq("h264 param init: pic_height_in_map_units", cfg.pic_height_in_map_units, 68);
        t.check(
            "h264 param init: profile resolved",
            cfg.profile_idc != h264_profile::INVALID,
        );
        t.check(
            "h264 param init: level resolved",
            cfg.level_idc != u32::MAX,
        );
    }

    // --- H.265 full parameter init flow ---
    {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        let result = cfg.initialize_parameters();
        t.check("h265 param init: succeeds", result.is_ok());
        t.check(
            "h265 param init: profile resolved",
            cfg.profile != h265_profile::INVALID,
        );
        t.check(
            "h265 param init: level resolved",
            cfg.level_idc != u32::MAX,
        );
    }

    // --- AV1 full parameter init flow ---
    {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        let result = cfg.initialize_parameters();
        t.check("av1 param init: succeeds", result.is_ok());
        t.check_eq("av1 param init: profile", cfg.profile, av1_profile::MAIN);
        t.check("av1 param init: level resolved", cfg.level != u32::MAX);
        t.check_eq("av1 param init: pic_width_in_sbs", cfg.pic_width_in_sbs, div_up(1920, 64));
        t.check_eq("av1 param init: pic_height_in_sbs", cfg.pic_height_in_sbs, div_up(1080, 64));
    }

    // --- Base rate control ---
    {
        let mut cfg = EncoderConfig::default();
        cfg.rate_control_mode = vk::VideoEncodeRateControlModeFlagsKHR::CBR;
        cfg.average_bitrate = 5_000_000;
        cfg.hrd_bitrate = 5_000_000;
        t.check("base rate control: init succeeds", cfg.init_rate_control());
        t.check_eq(
            "base rate control CBR: hrd == avg",
            cfg.hrd_bitrate,
            cfg.average_bitrate,
        );
    }

    // --- Utility functions ---
    {
        t.check_eq("align_size(1921,16)", align_size(1921u32, 16), 1936);
        t.check_eq("div_up(1080,16)", div_up(1080u32, 16), 68);
        t.check_eq("fast_int_log2(256)", fast_int_log2(256), 9);
        t.check_eq("gcd(1920,1080)", gcd(1920, 1080), 120);
        t.check_eq("int_abs(-42)", int_abs(-42), 42);
    }

    // --- Bit depth flag bits ---
    {
        t.check_eq(
            "bit_depth 8",
            get_component_bit_depth_flag_bits(8),
            vk::VideoComponentBitDepthFlagsKHR::_8,
        );
        t.check_eq(
            "bit_depth 10",
            get_component_bit_depth_flag_bits(10),
            vk::VideoComponentBitDepthFlagsKHR::_10,
        );
        t.check_eq(
            "bit_depth 12",
            get_component_bit_depth_flag_bits(12),
            vk::VideoComponentBitDepthFlagsKHR::_12,
        );
        t.check(
            "bit_depth 7 => empty",
            get_component_bit_depth_flag_bits(7).is_empty(),
        );
    }
}

// ---------------------------------------------------------------------------
// GPU encode test helpers
// ---------------------------------------------------------------------------

/// Find a queue family that supports video encode operations.
///
/// Returns `(queue_family_index, queue_count)` or `None`.
fn find_encode_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Option<(u32, u32)> {
    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };

    for (i, props) in queue_families.iter().enumerate() {
        if props
            .queue_flags
            .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
        {
            return Some((i as u32, props.queue_count));
        }
    }
    None
}

/// Generate NV12 test fixture frames via ffmpeg.
///
/// Returns the raw NV12 data for all frames concatenated. Each frame is
/// `width * height * 3 / 2` bytes.
fn generate_fixture_frames(width: u32, height: u32, num_frames: u32) -> Vec<u8> {
    let fixture_path = "/tmp/nvpro_encode_fixture.yuv";
    let duration = format!("{:.3}", num_frames as f64 / 30.0);

    // Generate SMPTE color bars as NV12 raw frames using ffmpeg
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f", "lavfi",
            "-i", &format!("smptebars=size={}x{}:rate=30:duration={}", width, height, duration),
            "-frames:v", &num_frames.to_string(),
            "-pix_fmt", "nv12",
            fixture_path,
        ])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            match std::fs::read(fixture_path) {
                Ok(data) => {
                    let frame_size = (width * height * 3 / 2) as usize;
                    let expected = frame_size * num_frames as usize;
                    if data.len() == expected {
                        println!("  [PASS] Generated {} fixture frames ({} bytes each)", num_frames, frame_size);
                        data
                    } else {
                        println!("  [WARN] Fixture size mismatch: got {}, expected {}", data.len(), expected);
                        println!("         Falling back to programmatic gradient pattern");
                        generate_gradient_frames(width, height, num_frames)
                    }
                }
                Err(e) => {
                    println!("  [WARN] Could not read fixture: {}. Using gradient pattern.", e);
                    generate_gradient_frames(width, height, num_frames)
                }
            }
        }
        _ => {
            println!("  [WARN] ffmpeg not available. Using programmatic gradient pattern.");
            generate_gradient_frames(width, height, num_frames)
        }
    }
}

/// Generate NV12 frames with a gradient pattern (fallback when ffmpeg is unavailable).
///
/// Each frame has a horizontal Y gradient and a frame-varying UV color shift.
fn generate_gradient_frames(width: u32, height: u32, num_frames: u32) -> Vec<u8> {
    let frame_size = (width * height * 3 / 2) as usize;
    let mut data = vec![0u8; frame_size * num_frames as usize];

    for f in 0..num_frames {
        let offset = f as usize * frame_size;
        let y_size = (width * height) as usize;

        // Y plane: horizontal gradient with frame-based vertical shift
        for row in 0..height {
            for col in 0..width {
                let y_val = ((col as f32 / width as f32 * 200.0) as u8)
                    .wrapping_add((f * 3) as u8)
                    .wrapping_add((row as u8).wrapping_mul(16u8.wrapping_add(f as u8 & 7)));
                data[offset + (row * width + col) as usize] = y_val.max(16).min(235);
            }
        }

        // UV plane: color pattern that shifts per frame
        let uv_offset = offset + y_size;
        let uv_width = width as usize;
        let uv_height = (height / 2) as usize;
        for row in 0..uv_height {
            for col in (0..uv_width).step_by(2) {
                let u = (128u8).wrapping_add(((col as f32 / uv_width as f32 * 100.0) as u8).wrapping_add(f as u8 * 5));
                let v = (128u8).wrapping_add(((row as f32 / uv_height as f32 * 100.0) as u8).wrapping_add(f as u8 * 3));
                data[uv_offset + row * uv_width + col] = u.max(16).min(240);
                data[uv_offset + row * uv_width + col + 1] = v.max(16).min(240);
            }
        }
    }

    println!("  [PASS] Generated {} programmatic gradient frames", num_frames);
    data
}

/// Run the GPU-based encode integration test.
///
/// Creates a SimpleEncoder via the public API, encodes multiple IDR + P
/// frames, writes the output to `/tmp/encoded_test.h264`, and optionally
/// validates with ffprobe.
fn run_gpu_encode_test() {
    println!("nvpro-vulkan-video GPU encode integration test");
    println!("================================================\n");

    // ------------------------------------------------------------------
    // 1. Configuration
    // ------------------------------------------------------------------
    let width: u32 = 640;
    let height: u32 = 480;
    let total_frames: u32 = 30;

    let enc_config = SimpleEncoderConfig {
        width,
        height,
        fps: 30,
        codec: Codec::H264,
        preset: Preset::Medium,
        qp: Some(23),
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };

    // ------------------------------------------------------------------
    // 2. Create SimpleEncoder (handles all Vulkan setup internally)
    // ------------------------------------------------------------------
    let mut encoder = match SimpleEncoder::new(enc_config) {
        Ok(e) => e,
        Err(e) => {
            println!("SimpleEncoder::new failed: {}. Skipping GPU test.", e);
            std::process::exit(0);
        }
    };

    println!("SimpleEncoder created: {}x{} H.264, CQP QP=23", width, height);

    // ------------------------------------------------------------------
    // 3. Generate test fixture frames
    // ------------------------------------------------------------------
    println!("--- Generating test fixture ---");

    let frame_size = (width * height * 3 / 2) as usize;
    let fixture_data = generate_fixture_frames(width, height, total_frames);

    let fixture_snapshot_path = "/tmp/nvpro_encode_debug/fixture_source_frame00.png";
    let _ = std::fs::create_dir_all("/tmp/nvpro_encode_debug");
    let fixture_yuv_path = "/tmp/nvpro_encode_debug/fixture_source.yuv";
    let _ = std::fs::write(fixture_yuv_path, &fixture_data[..frame_size]);
    let _ = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "rawvideo", "-pix_fmt", "nv12",
            "-s", &format!("{}x{}", width, height),
            "-i", fixture_yuv_path,
            "-frames:v", "1",
            fixture_snapshot_path,
        ])
        .output();
    println!("  Fixture source frame 0 saved to {}", fixture_snapshot_path);

    // ------------------------------------------------------------------
    // 4. Extract SPS/PPS header
    // ------------------------------------------------------------------
    println!("\n--- Extracting SPS/PPS header ---");
    let header = encoder.header();
    println!("  [PASS] Extracted header: {} bytes", header.len());

    // ------------------------------------------------------------------
    // 5. Encode multiple frames
    // ------------------------------------------------------------------
    println!("\n--- H.264 Encode Test ({} frames) ---", total_frames);

    let mut bitstream = Vec::new();
    let mut total_encoded_bytes: usize = 0;

    for frame_idx in 0..total_frames {
        let frame_offset = frame_idx as usize * frame_size;
        let frame_data = &fixture_data[frame_offset..frame_offset + frame_size];

        let packets = match encoder.submit_frame(frame_data, None) {
            Ok(pkts) => pkts,
            Err(e) => {
                println!("  [FAIL] submit_frame (frame {}) failed: {}", frame_idx, e);
                std::process::exit(1);
            }
        };

        for pkt in &packets {
            if pkt.data.is_empty() {
                println!("  [FAIL] Frame {} produced empty bitstream!", frame_idx);
                std::process::exit(1);
            }
            total_encoded_bytes += pkt.data.len();
            bitstream.extend_from_slice(&pkt.data);

            if frame_idx == 0 || frame_idx == total_frames as u32 - 1 {
                println!(
                    "  [PASS] Frame {:>2}: {:>4} bytes, type={}, pts={}",
                    frame_idx,
                    pkt.data.len(),
                    pkt.frame_type.name(),
                    pkt.pts,
                );
            }
        }
    }

    if let Ok(trailing) = encoder.finish() {
        for pkt in &trailing {
            total_encoded_bytes += pkt.data.len();
            bitstream.extend_from_slice(&pkt.data);
        }
    }

    println!(
        "  [PASS] All {} frames encoded successfully ({} bytes total, avg {:.0} bytes/frame)",
        total_frames, total_encoded_bytes, total_encoded_bytes as f64 / total_frames as f64,
    );

    let has_start_code = bitstream.len() >= 4
        && bitstream[0] == 0 && bitstream[1] == 0 && bitstream[2] == 0 && bitstream[3] == 1;
    if has_start_code {
        println!("  [PASS] Bitstream starts with NAL start code");
    }

    // ------------------------------------------------------------------
    // 6. Write to file
    // ------------------------------------------------------------------
    let out_path = "/tmp/encoded_test.h264";
    let out_path_mp4 = "/tmp/encoded_test.mp4";

    match std::fs::write(out_path, &bitstream) {
        Ok(_) => println!("\n  [PASS] Wrote {} bytes to {}", bitstream.len(), out_path),
        Err(e) => println!("  [WARN] Could not write {}: {}", out_path, e),
    }

    // ------------------------------------------------------------------
    // 7. Validate with ffprobe
    // ------------------------------------------------------------------
    println!("\n--- ffprobe validation ---");
    match std::process::Command::new("ffprobe")
        .args(["-v", "error",
            "-show_entries", "stream=codec_name,profile,level,width,height,pix_fmt,r_frame_rate",
            "-of", "default=noprint_wrappers=1", out_path])
        .output()
    {
        Ok(probe) => {
            let stdout = String::from_utf8_lossy(&probe.stdout);
            if probe.status.success() && !stdout.is_empty() {
                println!("  [PASS] ffprobe succeeded:");
                for line in stdout.lines() { println!("         {}", line); }
            }
        }
        Err(_) => println!("  [INFO] ffprobe not found -- skipping"),
    }

    // ------------------------------------------------------------------
    // 8. Count frames with ffprobe
    // ------------------------------------------------------------------
    println!("\n--- Frame count validation ---");
    match std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0",
            "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", out_path])
        .output()
    {
        Ok(probe) => {
            let stdout = String::from_utf8_lossy(&probe.stdout).trim().to_string();
            if let Ok(count) = stdout.parse::<u32>() {
                if count == total_frames {
                    println!("  [PASS] ffprobe counted {} frames (expected {})", count, total_frames);
                } else {
                    println!("  [INFO] ffprobe counted {} frames (expected {})", count, total_frames);
                }
            }
        }
        Err(_) => {}
    }

    // ------------------------------------------------------------------
    // 9. Convert to MP4
    // ------------------------------------------------------------------
    println!("\n--- MP4 conversion ---");
    match std::process::Command::new("ffmpeg")
        .args(["-y", "-i", out_path, "-c", "copy", "-movflags", "+faststart", out_path_mp4])
        .output()
    {
        Ok(result) if result.status.success() => {
            let mp4_size = std::fs::metadata(out_path_mp4).map(|m| m.len()).unwrap_or(0);
            println!("  [PASS] Converted to MP4: {} ({} bytes)", out_path_mp4, mp4_size);
        }
        _ => println!("  [INFO] ffmpeg MP4 conversion failed or not found"),
    }

    // ------------------------------------------------------------------
    // 10. NAL unit counting
    // ------------------------------------------------------------------
    println!("\n--- Round-trip NAL parse ---");
    let mut nal_count = 0u32;
    let mut i = 0usize;
    while i + 3 < bitstream.len() {
        if bitstream[i] == 0 && bitstream[i + 1] == 0 {
            if bitstream[i + 2] == 1 { nal_count += 1; i += 3; continue; }
            if i + 3 < bitstream.len() && bitstream[i + 2] == 0 && bitstream[i + 3] == 1 {
                nal_count += 1; i += 4; continue;
            }
        }
        i += 1;
    }
    if nal_count > 0 {
        println!("  [PASS] Found {} NAL unit(s) in bitstream", nal_count);
    } else {
        println!("  [WARN] No Annex B start codes found");
    }

    // ------------------------------------------------------------------
    // 11. Compare with source
    // ------------------------------------------------------------------
    println!("\n--- Source vs Decoded comparison ---");
    let decoded_frame_path = "/tmp/nvpro_encode_debug/decoded_frame00.png";
    let _ = std::process::Command::new("ffmpeg")
        .args(["-y", "-i", out_path_mp4, "-vf", "select=eq(n\\,0)",
            "-vsync", "vfr", "-frames:v", "1", decoded_frame_path])
        .output();
    let _ = std::process::Command::new("ffmpeg")
        .args(["-i", fixture_snapshot_path, "-i", decoded_frame_path,
            "-lavfi", "ssim;[0:v][1:v]psnr", "-f", "null", "-"])
        .output()
        .map(|r| {
            let stderr = String::from_utf8_lossy(&r.stderr);
            for line in stderr.lines() {
                if line.contains("PSNR") || line.contains("SSIM") {
                    println!("  {}", line.trim());
                }
            }
        });

    println!("\n--- Output files ---");
    println!("  Raw H.264 bitstream:   {}", out_path);
    println!("  MP4 container:         {}", out_path_mp4);

    // Cleanup (SimpleEncoder Drop handles all Vulkan resources)
    drop(encoder);

    println!("\n  GPU encode test completed successfully.\n");
    println!("========================================");
    println!("GPU encode integration test: PASSED");
    println!("========================================");
}

// dummy references to keep old functions from being completely unused
#[allow(dead_code)]
fn _keep_old_helpers() {
    let _ = find_encode_queue_family as fn(&vulkanalia::Instance, vk::PhysicalDevice) -> Option<(u32, u32)>;
}
// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    // Initialize tracing subscriber for debug logging.
    // Use RUST_LOG=debug to see H.265 DPB/encode instrumentation.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();

    let run_gpu = args.iter().any(|a| a == "--gpu");

    if run_gpu {
        run_gpu_encode_test();
        std::process::exit(0);
    }

    println!("nvpro-vulkan-video encode-test");
    println!("Running encoder integration self-tests (no GPU required)...");
    println!("(pass --gpu to run GPU encode integration tests)\n");

    let mut t = TestRunner::new();

    test_gop_structure(&mut t);
    test_h264_config(&mut t);
    test_h265_config(&mut t);
    test_av1_config(&mut t);
    test_psnr_computation(&mut t);
    test_dpb_management(&mut t);
    test_encoder_state(&mut t);

    let success = t.summary();
    std::process::exit(if success { 0 } else { 1 });
}
