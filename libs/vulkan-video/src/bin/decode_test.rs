// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Standalone decode test binary.
//!
//! Exercises the ported bitstream parser components WITHOUT requiring a GPU.
//!
//! Usage:
//!   cargo run --bin decode-test                    # run built-in self-tests
//!   cargo run --bin decode-test -- /path/to.h264   # parse an H.264 file's headers
//!   cargo run --bin decode-test -- /path/to.h265   # parse an H.265 file's headers
//!   cargo run --bin decode-test -- /path/to.ivf    # parse an AV1 IVF file's OBUs
//!   cargo run --bin decode-test -- --pipeline       # parse /tmp test streams, write report
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::Path;
use std::process;

use vulkan_video::nv_video_parser::byte_stream_parser::{
    BitstreamPacket, ByteStreamParser, StartCodeFinder, remove_emulation_prevention_bytes,
};
use vulkan_video::nv_video_parser::vulkan_h264_decoder::{
    BitstreamReader, NalUnitType as H264NalUnitType, VulkanH264Decoder,
};
use vulkan_video::nv_video_parser::vulkan_h265_decoder::{
    BitstreamReader as H265BitstreamReader,
    NalUnitType as H265NalUnitType, VulkanH265Decoder,
};
use vulkan_video::nv_video_parser::nv_vulkan_h264_scaling_list::{
    self, ScalingListH264, ScalingListType, FLAT_4X4_16, FLAT_8X8_16,
    DEFAULT_4X4_INTRA, DEFAULT_4X4_INTER, DEFAULT_8X8_INTRA, DEFAULT_8X8_INTER,
};
use vulkan_video::nv_video_parser::nv_vulkan_h265_scaling_list::{
    self, ScalingList as H265ScalingList, DEFAULT_SCALING_LIST_8X8,
};
use vulkan_video::nv_video_parser::vulkan_av1_decoder::{
    Av1ObuType, VulkanAv1Decoder,
};

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

struct TestRunner {
    passed: u32,
    failed: u32,
}

impl TestRunner {
    fn new() -> Self {
        Self { passed: 0, failed: 0 }
    }

    fn run(&mut self, name: &str, f: impl FnOnce() -> Result<(), String>) {
        print!("  {name} ... ");
        match f() {
            Ok(()) => {
                println!("PASS");
                self.passed += 1;
            }
            Err(e) => {
                println!("FAIL: {e}");
                self.failed += 1;
            }
        }
    }

    fn summary(&self) -> bool {
        println!();
        println!(
            "Results: {} passed, {} failed, {} total",
            self.passed,
            self.failed,
            self.passed + self.failed,
        );
        self.failed == 0
    }
}

// ---------------------------------------------------------------------------
// Exp-Golomb tests
// ---------------------------------------------------------------------------

fn test_exp_golomb(runner: &mut TestRunner) {
    println!("[Exp-Golomb]");

    runner.run("ue(0) = single 1-bit", || {
        // ue(0) encoding: 1 -> codeNum 0
        let data = [0b1000_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.ue();
        if val != 0 {
            return Err(format!("expected 0, got {val}"));
        }
        Ok(())
    });

    runner.run("ue(1) = 010", || {
        // ue(1): 010 -> codeNum 1
        let data = [0b0100_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.ue();
        if val != 1 {
            return Err(format!("expected 1, got {val}"));
        }
        Ok(())
    });

    runner.run("ue(2) = 011", || {
        // ue(2): 011
        let data = [0b0110_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.ue();
        if val != 2 {
            return Err(format!("expected 2, got {val}"));
        }
        Ok(())
    });

    runner.run("ue(3) = 00100", || {
        // ue(3): 00100
        let data = [0b0010_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.ue();
        if val != 3 {
            return Err(format!("expected 3, got {val}"));
        }
        Ok(())
    });

    runner.run("ue(7) = 0001000", || {
        // ue(7): 0001000
        let data = [0b0001_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.ue();
        if val != 7 {
            return Err(format!("expected 7, got {val}"));
        }
        Ok(())
    });

    runner.run("se(0) from ue(0)", || {
        let data = [0b1000_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.se();
        if val != 0 {
            return Err(format!("expected 0, got {val}"));
        }
        Ok(())
    });

    runner.run("se(+1) from ue(1) = 010", || {
        // ue(1) -> se mapping: codeNum=1 -> +1
        let data = [0b0100_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.se();
        if val != 1 {
            return Err(format!("expected 1, got {val}"));
        }
        Ok(())
    });

    runner.run("se(-1) from ue(2) = 011", || {
        // ue(2) -> se mapping: codeNum=2 -> -1
        let data = [0b0110_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.se();
        if val != -1 {
            return Err(format!("expected -1, got {val}"));
        }
        Ok(())
    });

    runner.run("se(+2) from ue(3) = 00100", || {
        // codeNum=3 -> +2
        let data = [0b0010_0000];
        let mut r = BitstreamReader::new(&data);
        let val = r.se();
        if val != 2 {
            return Err(format!("expected 2, got {val}"));
        }
        Ok(())
    });

    runner.run("se(-2) from ue(4) = 00101", || {
        // codeNum=4 -> -2
        let data = [0b0010_1000];
        let mut r = BitstreamReader::new(&data);
        let val = r.se();
        if val != -2 {
            return Err(format!("expected -2, got {val}"));
        }
        Ok(())
    });

    runner.run("sequential ue reads", || {
        // Pack ue(0)=1, ue(1)=010, ue(2)=011 into bits: 1 010 011 0
        let data = [0b1010_0110];
        let mut r = BitstreamReader::new(&data);
        let v0 = r.ue();
        let v1 = r.ue();
        let v2 = r.ue();
        if v0 != 0 || v1 != 1 || v2 != 2 {
            return Err(format!("expected (0,1,2), got ({v0},{v1},{v2})"));
        }
        Ok(())
    });

    runner.run("fixed-width u(8) read", || {
        let data = [0xAB, 0xCD];
        let mut r = BitstreamReader::new(&data);
        let v = r.u(8);
        if v != 0xAB {
            return Err(format!("expected 0xAB, got 0x{v:02X}"));
        }
        let v2 = r.u(8);
        if v2 != 0xCD {
            return Err(format!("expected 0xCD, got 0x{v2:02X}"));
        }
        Ok(())
    });

    runner.run("u(3) partial byte read", || {
        // 0b101_00000 -> first 3 bits = 5
        let data = [0b1010_0000];
        let mut r = BitstreamReader::new(&data);
        let v = r.u(3);
        if v != 5 {
            return Err(format!("expected 5, got {v}"));
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Byte stream parser / start code tests
// ---------------------------------------------------------------------------

fn test_byte_stream_parser(runner: &mut TestRunner) {
    println!("[ByteStreamParser]");

    runner.run("StartCodeFinder basic detection", || {
        let mut finder = StartCodeFinder::new();
        let data = [0x00, 0x00, 0x01, 0x65];
        let res = finder.next_start_code(&data);
        if !res.found {
            return Err("start code not found".into());
        }
        if res.bytes_consumed != 3 {
            return Err(format!("expected 3 consumed, got {}", res.bytes_consumed));
        }
        Ok(())
    });

    runner.run("StartCodeFinder no start code", || {
        let mut finder = StartCodeFinder::new();
        let data = [0xAA, 0xBB, 0xCC, 0xDD];
        let res = finder.next_start_code(&data);
        if res.found {
            return Err("false positive start code".into());
        }
        if res.bytes_consumed != 4 {
            return Err(format!("expected 4 consumed, got {}", res.bytes_consumed));
        }
        Ok(())
    });

    runner.run("StartCodeFinder split across calls", || {
        let mut finder = StartCodeFinder::new();
        let part1 = [0xFF, 0x00, 0x00];
        let res1 = finder.next_start_code(&part1);
        if res1.found {
            return Err("premature start code in part1".into());
        }
        let part2 = [0x01, 0x65];
        let res2 = finder.next_start_code(&part2);
        if !res2.found {
            return Err("start code not found across split".into());
        }
        if res2.bytes_consumed != 1 {
            return Err(format!("expected 1 consumed in part2, got {}", res2.bytes_consumed));
        }
        Ok(())
    });

    runner.run("StartCodeFinder 4-byte start code", || {
        let mut finder = StartCodeFinder::new();
        let data = [0x00, 0x00, 0x00, 0x01, 0x65];
        let res = finder.next_start_code(&data);
        if !res.found {
            return Err("4-byte start code not found".into());
        }
        if res.bytes_consumed != 4 {
            return Err(format!("expected 4 consumed, got {}", res.bytes_consumed));
        }
        Ok(())
    });

    runner.run("StartCodeFinder reset", || {
        let mut finder = StartCodeFinder::new();
        finder.next_start_code(&[0x00, 0x00]);
        finder.reset();
        if finder.bit_bfr() != !0u32 {
            return Err("reset did not restore initial state".into());
        }
        Ok(())
    });

    runner.run("ByteStreamParser single NAL", || {
        let mut parser = ByteStreamParser::new(1024);
        // start_code | 0x65 0xAA 0xBB | start_code
        let data = [0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0x00, 0x00, 0x01];
        let pck = BitstreamPacket {
            data: &data,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;
        if parser.completed_nalus().len() != 1 {
            return Err(format!(
                "expected 1 NALU, got {}",
                parser.completed_nalus().len()
            ));
        }
        if parser.completed_nalus()[0] != [0x65, 0xAA, 0xBB] {
            return Err(format!(
                "unexpected NALU content: {:?}",
                parser.completed_nalus()[0]
            ));
        }
        Ok(())
    });

    runner.run("ByteStreamParser two NALs with EOP", || {
        let mut parser = ByteStreamParser::new(1024);
        let data = [
            0x00, 0x00, 0x01, // start code 1
            0x67, 0x42,       // NAL 1 (SPS-like)
            0x00, 0x00, 0x01, // start code 2
            0x68, 0xCE,       // NAL 2 (PPS-like)
        ];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;
        if parser.completed_nalus().len() != 2 {
            return Err(format!(
                "expected 2 NALUs, got {}",
                parser.completed_nalus().len()
            ));
        }
        if parser.completed_nalus()[0] != [0x67, 0x42] {
            return Err(format!("NALU 0 mismatch: {:?}", parser.completed_nalus()[0]));
        }
        if parser.completed_nalus()[1] != [0x68, 0xCE] {
            return Err(format!("NALU 1 mismatch: {:?}", parser.completed_nalus()[1]));
        }
        Ok(())
    });

    runner.run("ByteStreamParser EOP flushes trailing NAL", || {
        let mut parser = ByteStreamParser::new(1024);
        let data = [0x00, 0x00, 0x01, 0x65, 0xAA];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;
        if parser.completed_nalus().len() != 1 {
            return Err(format!(
                "expected 1 NALU, got {}",
                parser.completed_nalus().len()
            ));
        }
        if parser.completed_nalus()[0] != [0x65, 0xAA] {
            return Err(format!("NALU mismatch: {:?}", parser.completed_nalus()[0]));
        }
        Ok(())
    });

    runner.run("emulation prevention byte removal", || {
        let input = [0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0xBB];
        let rbsp = remove_emulation_prevention_bytes(&input);
        let expected = [0x00, 0x00, 0x01, 0x00, 0x00, 0xBB];
        if rbsp != expected {
            return Err(format!("expected {:?}, got {:?}", expected, rbsp));
        }
        Ok(())
    });

    runner.run("emulation prevention: no 03 present", || {
        let input = [0xAA, 0xBB, 0xCC];
        let rbsp = remove_emulation_prevention_bytes(&input);
        if rbsp != input {
            return Err(format!("expected {:?}, got {:?}", input, rbsp));
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// H.264 parser tests
// ---------------------------------------------------------------------------

/// Build a minimal synthetic H.264 SPS NAL unit (Baseline profile, 176x144).
///
/// This is a manually constructed bitstream that `parse_sps` can consume.
fn build_h264_sps_nalu() -> Vec<u8> {
    // We construct a valid SPS RBSP for:
    //   profile_idc = 66 (Baseline)
    //   constraint_set_flags = 0xC0 (constraint_set0=1, constraint_set1=1)
    //   level_idc = 30 (Level 3.0)
    //   sps_id = 0 (ue=0 -> 1-bit: 1)
    //   log2_max_frame_num_minus4 = 0 (ue=0)
    //   pic_order_cnt_type = 0 (ue=0)
    //   log2_max_pic_order_cnt_lsb_minus4 = 0 (ue=0)
    //   max_num_ref_frames = 1 (ue=1 -> 010)
    //   gaps_in_frame_num_value_allowed_flag = 0 (1 bit)
    //   pic_width_in_mbs_minus1 = 10 (ue=10 -> 0001011, 176/16-1=10)
    //   pic_height_in_map_units_minus1 = 8 (ue=8 -> 0001001, 144/16-1=8)
    //   frame_mbs_only_flag = 1 (1 bit)
    //   direct_8x8_inference_flag = 0 (1 bit) -- skipped because frame_mbs_only=1
    //   frame_cropping_flag = 0 (1 bit)
    //   vui_parameters_present_flag = 0 (1 bit)
    //   RBSP stop bit
    //
    // Bit layout:
    //   profile_idc: 01000010 (0x42 = 66)
    //   constraint:  11000000 (0xC0)
    //   level_idc:   00011110 (0x1E = 30)
    //   sps_id:      1         (ue=0)
    //   log2_max_frame_num_minus4: 1 (ue=0)
    //   pic_order_cnt_type:        1 (ue=0)
    //   log2_max_pic_order_cnt_lsb_minus4: 1 (ue=0)
    //   max_num_ref_frames:        010 (ue=1)
    //   gaps_in_frame_num:         0
    //   pic_width_in_mbs_minus1:   0001011 (ue=10)
    //   pic_height_in_map_units_minus1: 0001001 (ue=8)
    //   frame_mbs_only_flag:       1
    //   frame_cropping_flag:       0
    //   vui_parameters_present:    0
    //   rbsp stop bit:             1
    //
    // Concatenated bits (after the 3-byte header):
    //   1111 010 0 0001011 0001001 1 0 0 1 (+ padding)
    //
    // Let's encode byte by byte:
    // Byte 0-2: profile_idc, constraint, level = 0x42, 0xC0, 0x1E
    // After those 3 bytes, the remaining bits:
    //   bit 0: 1 (sps_id ue=0)
    //   bit 1: 1 (log2_max_frame_num_minus4 ue=0)
    //   bit 2: 1 (pic_order_cnt_type ue=0)
    //   bit 3: 1 (log2_max_pic_order_cnt_lsb_minus4 ue=0)
    //   bits 4-6: 010 (max_num_ref_frames ue=1)
    //   bit 7: 0 (gaps_in_frame_num)
    //   bits 8-14: 0001011 (pic_width_in_mbs_minus1 ue=10)
    //   bits 15-21: 0001001 (pic_height ue=8)
    //   bit 22: 1 (frame_mbs_only)
    //   bit 23: 0 (frame_cropping)
    //   bit 24: 0 (vui_parameters_present)
    //   bit 25: 1 (stop bit)
    //
    // Bytes 3..6:
    //   byte 3: 1111_0100 = 0xF4
    //   byte 4: 0001_0110 = 0x16
    //   byte 5: 0010_0110 = 0x26
    //   byte 6: 0100_0000 = 0x40
    vec![0x42, 0xC0, 0x1E, 0xF4, 0x16, 0x26, 0x40]
}

/// Build a minimal synthetic H.264 PPS NAL unit (pps_id=0, sps_id=0).
fn build_h264_pps_nalu() -> Vec<u8> {
    // PPS RBSP:
    //   pps_id = 0 (ue=0 -> 1)
    //   sps_id = 0 (ue=0 -> 1)
    //   entropy_coding_mode_flag = 0 (1 bit, CAVLC)
    //   bottom_field_pic_order_in_frame_present_flag = 0 (1 bit)
    //   num_slice_groups_minus1 = 0 (ue=0 -> 1)
    //   num_ref_idx_l0_default_active_minus1 = 0 (ue=0 -> 1)
    //   num_ref_idx_l1_default_active_minus1 = 0 (ue=0 -> 1)
    //   weighted_pred_flag = 0 (1 bit)
    //   weighted_bipred_idc = 0 (2 bits)
    //   pic_init_qp_minus26 = 0 (se=0 -> 1)
    //   pic_init_qs_minus26 = 0 (se=0 -> 1)
    //   chroma_qp_index_offset = 0 (se=0 -> 1)
    //   deblocking_filter_control_present_flag = 1 (1 bit)
    //   constrained_intra_pred_flag = 0 (1 bit)
    //   redundant_pic_cnt_present_flag = 0 (1 bit)
    //   stop bit = 1
    //
    // Bits: 1 1 0 0 1 1 1 0 00 1 1 1 1 0 0 1
    // Byte 0: 1100_1110 = 0xCE
    // Byte 1: 0111_1001 = 0x79
    // Need padding at end
    vec![0xCE, 0x79, 0x00]
}

fn test_h264_parser(runner: &mut TestRunner) {
    println!("[H.264 Parser]");

    runner.run("H264 SPS parse (Baseline 176x144)", || {
        let sps_rbsp = build_h264_sps_nalu();
        let mut decoder = VulkanH264Decoder::new();
        let mut reader = BitstreamReader::new(&sps_rbsp);
        let result = decoder.parse_sps(&mut reader);
        match result {
            Some(sps_id) => {
                if sps_id != 0 {
                    return Err(format!("expected sps_id=0, got {sps_id}"));
                }
                // Verify the SPS was stored
                let sps = decoder.spss[0].as_ref()
                    .ok_or("SPS 0 not stored after parse")?;
                if sps.profile_idc != 66 {
                    return Err(format!("profile_idc: expected 66, got {}", sps.profile_idc));
                }
                // Width = (pic_width_in_mbs_minus1 + 1) * 16 = 11 * 16 = 176
                let width = (sps.pic_width_in_mbs_minus1 + 1) * 16;
                if width != 176 {
                    return Err(format!("width: expected 176, got {width}"));
                }
                // Height = (pic_height_in_map_units_minus1 + 1) * 16 = 9 * 16 = 144
                let height = (sps.pic_height_in_map_units_minus1 + 1) * 16;
                if height != 144 {
                    return Err(format!("height: expected 144, got {height}"));
                }
                Ok(())
            }
            None => Err("parse_sps returned None".into()),
        }
    });

    runner.run("H264 PPS parse (pps_id=0, sps_id=0)", || {
        // Must parse SPS first so PPS can reference it
        let sps_rbsp = build_h264_sps_nalu();
        let mut decoder = VulkanH264Decoder::new();
        let mut reader = BitstreamReader::new(&sps_rbsp);
        decoder.parse_sps(&mut reader)
            .ok_or("SPS parse failed")?;

        let pps_rbsp = build_h264_pps_nalu();
        let mut reader = BitstreamReader::new(&pps_rbsp);
        let ok = decoder.parse_pps(&mut reader);
        if !ok {
            return Err("parse_pps returned false".into());
        }
        // Verify PPS was stored
        let pps = decoder.ppss[0].as_ref()
            .ok_or("PPS 0 not stored after parse")?;
        if pps.seq_parameter_set_id != 0 {
            return Err(format!(
                "PPS sps_id: expected 0, got {}",
                pps.seq_parameter_set_id
            ));
        }
        Ok(())
    });

    runner.run("H264 NAL type identification", || {
        // Verify H264NalUnitType::from_u8 for key types
        let cases: &[(u8, H264NalUnitType)] = &[
            (1, H264NalUnitType::CodedSlice),
            (5, H264NalUnitType::CodedSliceIdr),
            (7, H264NalUnitType::Sps),
            (8, H264NalUnitType::Pps),
            (6, H264NalUnitType::Sei),
            (9, H264NalUnitType::AccessUnitDelimiter),
        ];
        for &(raw, expected) in cases {
            match H264NalUnitType::from_u8(raw) {
                Some(t) if t == expected => {}
                Some(t) => {
                    return Err(format!(
                        "NAL type {raw}: expected {expected:?}, got {t:?}"
                    ));
                }
                None => {
                    return Err(format!("NAL type {raw}: from_u8 returned None"));
                }
            }
        }
        Ok(())
    });

    runner.run("H264 full NAL stream (SPS+PPS via ByteStreamParser)", || {
        // Build a byte stream: start_code + SPS_NAL + start_code + PPS_NAL + EOP
        let sps_nalu = build_h264_sps_nalu();
        let pps_nalu = build_h264_pps_nalu();

        let mut stream = Vec::new();
        // NAL header byte for SPS: forbidden_zero_bit=0, nal_ref_idc=3, nal_unit_type=7
        // = 0b0_11_00111 = 0x67
        stream.extend_from_slice(&[0x00, 0x00, 0x01, 0x67]);
        stream.extend_from_slice(&sps_nalu);
        // NAL header byte for PPS: nal_ref_idc=3, nal_unit_type=8 = 0x68
        stream.extend_from_slice(&[0x00, 0x00, 0x01, 0x68]);
        stream.extend_from_slice(&pps_nalu);

        let mut parser = ByteStreamParser::new(4096);
        let pck = BitstreamPacket {
            data: &stream,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;

        if parser.completed_nalus().len() != 2 {
            return Err(format!(
                "expected 2 NALUs, got {}",
                parser.completed_nalus().len()
            ));
        }

        // Parse extracted NALUs through the H264 decoder
        let mut decoder = VulkanH264Decoder::new();

        // First NALU: SPS (header byte 0x67 + SPS RBSP)
        let nalu0 = &parser.completed_nalus()[0];
        if nalu0.is_empty() {
            return Err("NALU 0 is empty".into());
        }
        let nal_type = nalu0[0] & 0x1F;
        if nal_type != 7 {
            return Err(format!("expected NAL type 7 (SPS), got {nal_type}"));
        }
        let mut reader = BitstreamReader::new(&nalu0[1..]);
        decoder.parse_sps(&mut reader)
            .ok_or("SPS parse from stream failed")?;

        // Second NALU: PPS (header byte 0x68 + PPS RBSP)
        let nalu1 = &parser.completed_nalus()[1];
        if nalu1.is_empty() {
            return Err("NALU 1 is empty".into());
        }
        let nal_type = nalu1[0] & 0x1F;
        if nal_type != 8 {
            return Err(format!("expected NAL type 8 (PPS), got {nal_type}"));
        }
        let mut reader = BitstreamReader::new(&nalu1[1..]);
        if !decoder.parse_pps(&mut reader) {
            return Err("PPS parse from stream failed".into());
        }

        // Verify decoder state
        if decoder.spss[0].is_none() {
            return Err("SPS 0 missing after stream parse".into());
        }
        if decoder.ppss[0].is_none() {
            return Err("PPS 0 missing after stream parse".into());
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// H.265 parser tests
// ---------------------------------------------------------------------------

fn test_h265_parser(runner: &mut TestRunner) {
    println!("[H.265 Parser]");

    runner.run("H265 NAL type identification", || {
        let cases: &[(u8, H265NalUnitType)] = &[
            (0, H265NalUnitType::TrailN),
            (1, H265NalUnitType::TrailR),
            (19, H265NalUnitType::IdrWRadl),
            (20, H265NalUnitType::IdrNLp),
            (21, H265NalUnitType::CraNut),
            (32, H265NalUnitType::VpsNut),
            (33, H265NalUnitType::SpsNut),
            (34, H265NalUnitType::PpsNut),
        ];
        for &(raw, expected) in cases {
            match H265NalUnitType::from_raw(raw) {
                Some(t) if t == expected => {}
                Some(t) => {
                    return Err(format!(
                        "H265 NAL type {raw}: expected {expected:?}, got {t:?}"
                    ));
                }
                None => {
                    return Err(format!("H265 NAL type {raw}: from_raw returned None"));
                }
            }
        }
        Ok(())
    });

    runner.run("H265 NAL type is_idr", || {
        if !H265NalUnitType::IdrWRadl.is_idr() {
            return Err("IdrWRadl should be IDR".into());
        }
        if !H265NalUnitType::IdrNLp.is_idr() {
            return Err("IdrNLp should be IDR".into());
        }
        if H265NalUnitType::CraNut.is_idr() {
            return Err("CraNut should not be IDR".into());
        }
        if H265NalUnitType::TrailR.is_idr() {
            return Err("TrailR should not be IDR".into());
        }
        Ok(())
    });

    runner.run("H265 NAL type is_irap", || {
        if !H265NalUnitType::IdrWRadl.is_irap() {
            return Err("IdrWRadl should be IRAP".into());
        }
        if !H265NalUnitType::CraNut.is_irap() {
            return Err("CraNut should be IRAP".into());
        }
        if !H265NalUnitType::BlaWLp.is_irap() {
            return Err("BlaWLp should be IRAP".into());
        }
        if H265NalUnitType::TrailR.is_irap() {
            return Err("TrailR should not be IRAP".into());
        }
        Ok(())
    });

    runner.run("H265 NAL type is_slice", || {
        if !H265NalUnitType::TrailR.is_slice() {
            return Err("TrailR should be slice".into());
        }
        if !H265NalUnitType::IdrWRadl.is_slice() {
            return Err("IdrWRadl should be slice".into());
        }
        if H265NalUnitType::VpsNut.is_slice() {
            return Err("VpsNut should not be slice".into());
        }
        if H265NalUnitType::SpsNut.is_slice() {
            return Err("SpsNut should not be slice".into());
        }
        Ok(())
    });

    runner.run("VulkanH265Decoder instantiation", || {
        let decoder = VulkanH265Decoder::new();
        if decoder.max_dpb_size != 0 {
            return Err(format!("initial max_dpb_size: expected 0, got {}", decoder.max_dpb_size));
        }
        if decoder.picture_started {
            return Err("picture_started should be false initially".into());
        }
        // All parameter set stores should be empty
        for i in 0..16 {
            if decoder.spss[i].is_some() {
                return Err(format!("spss[{i}] should be None"));
            }
        }
        for i in 0..16 {
            if decoder.vpss[i].is_some() {
                return Err(format!("vpss[{i}] should be None"));
            }
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Scaling list tests
// ---------------------------------------------------------------------------

fn test_scaling_lists(runner: &mut TestRunner) {
    println!("[Scaling Lists]");

    runner.run("H264 SPS flat scaling (no scaling_matrix_present)", || {
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let present = nv_vulkan_h264_scaling_list::set_sps_scaling_lists_h264(
            None, &mut w4, &mut w8,
        );
        if present {
            return Err("expected false (no scaling list present)".into());
        }
        // All 4x4 should be flat (16)
        for i in 0..6 {
            if w4[i] != FLAT_4X4_16 {
                return Err(format!("4x4 matrix {i} is not flat"));
            }
        }
        // All 8x8 should be flat (16)
        for i in 0..2 {
            if w8[i] != FLAT_8X8_16 {
                return Err(format!("8x8 matrix {i} is not flat"));
            }
        }
        Ok(())
    });

    runner.run("H264 SPS UseDefault scaling lists", || {
        let mut sl = ScalingListH264::default();
        sl.scaling_matrix_present_flag = true;
        // Set all to UseDefault
        for i in 0..8 {
            sl.scaling_list_type[i] = ScalingListType::UseDefault;
        }
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let present = nv_vulkan_h264_scaling_list::set_sps_scaling_lists_h264(
            Some(&sl), &mut w4, &mut w8,
        );
        if !present {
            return Err("expected true (scaling list present)".into());
        }
        // Lists 0..2 should be DEFAULT_4X4_INTRA, 3..5 DEFAULT_4X4_INTER
        for i in 0..3 {
            if w4[i] != DEFAULT_4X4_INTRA {
                return Err(format!("4x4 matrix {i}: expected intra default"));
            }
        }
        for i in 3..6 {
            if w4[i] != DEFAULT_4X4_INTER {
                return Err(format!("4x4 matrix {i}: expected inter default"));
            }
        }
        // 8x8: index 6 -> intra, index 7 -> inter
        if w8[0] != DEFAULT_8X8_INTRA {
            return Err("8x8 matrix 0: expected intra default".into());
        }
        if w8[1] != DEFAULT_8X8_INTER {
            return Err("8x8 matrix 1: expected inter default".into());
        }
        Ok(())
    });

    runner.run("H265 default 4x4 scaling (all 16)", || {
        let scl = H265ScalingList::default();
        let mut factors = [0u8; 4 * 4 * 6];
        nv_vulkan_h265_scaling_list::init_4x4_scaling_lists_h265(&mut factors, &scl);
        for (i, &v) in factors.iter().enumerate() {
            if v != 16 {
                return Err(format!("factor[{i}]: expected 16, got {v}"));
            }
        }
        Ok(())
    });

    runner.run("H265 default 8x8 scaling (size_id=1)", || {
        let scl = H265ScalingList::default();
        let mut factors = [0u8; 8 * 8 * 6];
        let mut dc = [0u8; 6];
        nv_vulkan_h265_scaling_list::init_8x8_scaling_lists_h265(
            &mut factors, &mut dc, &scl, 1,
        );
        // Matrices 0..2 use intra defaults, 3..5 use inter defaults
        for matrix_id in 0..6 {
            let list_idx = if matrix_id >= 3 { 1 } else { 0 };
            let offset = 8 * 8 * matrix_id;
            for k in 0..64 {
                if factors[offset + k] != DEFAULT_SCALING_LIST_8X8[list_idx][k] {
                    return Err(format!(
                        "matrix {matrix_id}, k={k}: expected {}, got {}",
                        DEFAULT_SCALING_LIST_8X8[list_idx][k],
                        factors[offset + k],
                    ));
                }
            }
        }
        Ok(())
    });

    runner.run("H265 default 32x32 scaling (size_id=3, 2 matrices)", || {
        let scl = H265ScalingList::default();
        let mut factors = [0u8; 8 * 8 * 2];
        let mut dc = [0u8; 2];
        nv_vulkan_h265_scaling_list::init_8x8_scaling_lists_h265(
            &mut factors, &mut dc, &scl, 3,
        );
        // matrix 0 -> intra, matrix 1 -> inter
        for matrix_id in 0..2 {
            let list_idx = if matrix_id >= 1 { 1 } else { 0 };
            let offset = 8 * 8 * matrix_id;
            for k in 0..64 {
                if factors[offset + k] != DEFAULT_SCALING_LIST_8X8[list_idx][k] {
                    return Err(format!(
                        "32x32 matrix {matrix_id}, k={k}: expected {}, got {}",
                        DEFAULT_SCALING_LIST_8X8[list_idx][k],
                        factors[offset + k],
                    ));
                }
            }
            if dc[matrix_id] != DEFAULT_SCALING_LIST_8X8[list_idx][0] {
                return Err(format!(
                    "DC[{matrix_id}]: expected {}, got {}",
                    DEFAULT_SCALING_LIST_8X8[list_idx][0],
                    dc[matrix_id],
                ));
            }
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// File parsing mode
// ---------------------------------------------------------------------------

/// Detect whether a sequence of NAL units is H.265 by inspecting the first few headers.
///
/// H.265 NAL headers are 2 bytes. The nal_unit_type field is bits [1:6] of byte 0.
/// H.265 streams typically start with VPS (32), SPS (33), PPS (34).
/// H.264 NAL headers are 1 byte with nal_unit_type in bits [0:4], range 0-31.
/// Since H.265 VPS/SPS/PPS types (32-34) exceed the H.264 type range, if we see
/// any of those values in the first few NALUs, it must be H.265.
fn detect_h265(nalus: &[Vec<u8>]) -> bool {
    // Check up to the first 10 non-empty NALUs
    for nalu in nalus.iter().filter(|n| !n.is_empty()).take(10) {
        let h265_type = (nalu[0] >> 1) & 0x3F;
        // VPS=32, SPS=33, PPS=34 are unambiguously H.265
        if h265_type >= 32 && h265_type <= 34 {
            return true;
        }
        // H.264 SPS=7, PPS=8 are unambiguously H.264
        let h264_type = nalu[0] & 0x1F;
        if h264_type == 7 || h264_type == 8 {
            return false;
        }
    }
    // Default: assume H.264 if no clear signal
    false
}

fn parse_file(path: &str) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    println!("File: {path}");
    println!("Size: {} bytes", data.len());

    if data.len() < 4 {
        return Err("file too small to contain any units".into());
    }

    // Check for IVF container (AV1)
    if is_ivf(&data) {
        println!("Detected format: IVF container");
        parse_file_av1(&data);
        return Ok(());
    }

    // Parse NAL units from the byte stream (Annex-B: H.264/H.265)
    let mut parser = ByteStreamParser::new(data.len() + 1024);
    let pck = BitstreamPacket {
        data: &data,
        eop: true,
        ..Default::default()
    };
    parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;

    let nalus = parser.completed_nalus();
    println!("NAL units found: {}", nalus.len());

    if nalus.is_empty() {
        println!("No NAL units detected. The file may not be an Annex-B byte stream.");
        return Ok(());
    }

    // Try to detect codec from first NAL unit header.
    // Scan through NAL units to find the first non-empty one and use its header
    // for codec detection. Some parsers may emit an empty or padding NALU before
    // the real stream starts.
    let first_nalu = match nalus.iter().find(|n| !n.is_empty()) {
        Some(n) => n,
        None => {
            println!("All NAL units are empty.");
            return Ok(());
        }
    };

    println!(
        "First NAL unit: {} bytes, header={:#04x} {:#04x}",
        first_nalu.len(),
        first_nalu[0],
        if first_nalu.len() > 1 { first_nalu[1] } else { 0 },
    );

    // Detect codec using a multi-NALU heuristic that checks for unambiguous
    // H.265 VPS/SPS/PPS types (32-34) vs H.264 SPS/PPS types (7-8).
    let is_h265 = detect_h265(nalus);

    if is_h265 {
        println!("Detected codec: H.265/HEVC");
        parse_file_h265(nalus);
    } else {
        println!("Detected codec: H.264/AVC (assuming from NAL header)");
        parse_file_h264(nalus);
    }

    Ok(())
}

fn parse_file_h264(nalus: &[Vec<u8>]) {
    let mut decoder = VulkanH264Decoder::new();
    let mut sps_count = 0u32;
    let mut pps_count = 0u32;
    let mut idr_count = 0u32;
    let mut slice_count = 0u32;
    let mut sei_count = 0u32;
    let mut other_count = 0u32;

    for nalu in nalus {
        if nalu.is_empty() {
            continue;
        }
        let nal_type_raw = nalu[0] & 0x1F;
        let rbsp = remove_emulation_prevention_bytes(&nalu[1..]);
        match H264NalUnitType::from_u8(nal_type_raw) {
            Some(H264NalUnitType::Sps) => {
                let mut reader = BitstreamReader::new(&rbsp);
                match decoder.parse_sps(&mut reader) {
                    Some(id) => {
                        sps_count += 1;
                        if let Some(sps) = &decoder.spss[id as usize] {
                            let w = (sps.pic_width_in_mbs_minus1 + 1) * 16;
                            let h = (sps.pic_height_in_map_units_minus1 + 1) * 16;
                            println!(
                                "  SPS id={id}: profile={}, level={:?}, {}x{}",
                                sps.profile_idc, sps.level_idc, w, h,
                            );
                        }
                    }
                    None => println!("  SPS: parse failed"),
                }
            }
            Some(H264NalUnitType::Pps) => {
                let mut reader = BitstreamReader::new(&rbsp);
                if decoder.parse_pps(&mut reader) {
                    pps_count += 1;
                } else {
                    println!("  PPS: parse failed");
                }
            }
            Some(H264NalUnitType::CodedSliceIdr) => idr_count += 1,
            Some(H264NalUnitType::CodedSlice) => slice_count += 1,
            Some(H264NalUnitType::Sei) => sei_count += 1,
            _ => other_count += 1,
        }
    }

    println!();
    println!("Summary:");
    println!("  SPS: {sps_count}");
    println!("  PPS: {pps_count}");
    println!("  IDR slices: {idr_count}");
    println!("  Non-IDR slices: {slice_count}");
    println!("  SEI: {sei_count}");
    println!("  Other: {other_count}");
}

/// Return a human-readable name for an H.265 NAL unit type.
fn h265_nal_type_name(nal_type: H265NalUnitType) -> &'static str {
    match nal_type {
        H265NalUnitType::TrailN => "TRAIL_N",
        H265NalUnitType::TrailR => "TRAIL_R",
        H265NalUnitType::TsaN => "TSA_N",
        H265NalUnitType::TsaR => "TSA_R",
        H265NalUnitType::StsaN => "STSA_N",
        H265NalUnitType::StsaR => "STSA_R",
        H265NalUnitType::RadlN => "RADL_N",
        H265NalUnitType::RadlR => "RADL_R",
        H265NalUnitType::RaslN => "RASL_N",
        H265NalUnitType::RaslR => "RASL_R",
        H265NalUnitType::BlaWLp => "BLA_W_LP",
        H265NalUnitType::BlaWRadl => "BLA_W_RADL",
        H265NalUnitType::BlaNLp => "BLA_N_LP",
        H265NalUnitType::IdrWRadl => "IDR_W_RADL",
        H265NalUnitType::IdrNLp => "IDR_N_LP",
        H265NalUnitType::CraNut => "CRA_NUT",
        H265NalUnitType::VpsNut => "VPS_NUT",
        H265NalUnitType::SpsNut => "SPS_NUT",
        H265NalUnitType::PpsNut => "PPS_NUT",
        H265NalUnitType::AudNut => "AUD_NUT",
        H265NalUnitType::EosNut => "EOS_NUT",
        H265NalUnitType::EobNut => "EOB_NUT",
        H265NalUnitType::FdNut => "FD_NUT",
        H265NalUnitType::PrefixSeiNut => "PREFIX_SEI_NUT",
        H265NalUnitType::SuffixSeiNut => "SUFFIX_SEI_NUT",
    }
}

/// Attempt a lightweight parse of an H.265 SPS RBSP to extract key fields.
///
/// This parses just enough of the SPS header to get resolution and format info,
/// using the H.265 BitstreamReader. The full SPS parse lives in VulkanH265Decoder
/// but may not yet be wired up; this standalone extraction is sufficient for the
/// test binary's reporting needs.
fn parse_h265_sps_summary(rbsp: &[u8]) -> Option<H265SpsSummary> {
    let mut r = H265BitstreamReader::new(rbsp);

    let sps_video_parameter_set_id = r.u(4)? as u8;
    let sps_max_sub_layers_minus1 = r.u(3)? as u8;
    let _sps_temporal_id_nesting_flag = r.u(1)?;

    // profile_tier_level( 1, sps_max_sub_layers_minus1 )
    // general_profile_space(2), general_tier_flag(1), general_profile_idc(5)
    let _general_profile_space = r.u(2)?;
    let _general_tier_flag = r.u(1)?;
    let _general_profile_idc = r.u(5)?;
    // general_profile_compatibility_flag[32]
    for _ in 0..32 {
        r.u(1)?;
    }
    // general_progressive_source_flag .. general_reserved_zero_44bits
    // 48 bits of constraint flags
    for _ in 0..48 {
        r.u(1)?;
    }
    // general_level_idc
    let _general_level_idc = r.u(8)?;

    // sub_layer_profile_present_flag[i], sub_layer_level_present_flag[i]
    let mut sub_layer_profile_present = [false; 8];
    let mut sub_layer_level_present = [false; 8];
    for i in 0..sps_max_sub_layers_minus1 as usize {
        sub_layer_profile_present[i] = r.u(1)? != 0;
        sub_layer_level_present[i] = r.u(1)? != 0;
    }
    if sps_max_sub_layers_minus1 > 0 {
        for _ in sps_max_sub_layers_minus1..8 {
            r.u(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..sps_max_sub_layers_minus1 as usize {
        if sub_layer_profile_present[i] {
            // sub_layer_profile_space(2), tier_flag(1), profile_idc(5),
            // compatibility_flag[32], 48 constraint bits
            r.skip_bits(2 + 1 + 5 + 32 + 48);
        }
        if sub_layer_level_present[i] {
            r.u(8)?; // sub_layer_level_idc
        }
    }

    let sps_seq_parameter_set_id = r.ue()? as u8;
    let chroma_format_idc = r.ue()? as u8;

    if chroma_format_idc == 3 {
        let _separate_colour_plane_flag = r.u(1)?;
    }

    let pic_width_in_luma_samples = r.ue()?;
    let pic_height_in_luma_samples = r.ue()?;

    Some(H265SpsSummary {
        sps_video_parameter_set_id,
        sps_seq_parameter_set_id,
        chroma_format_idc,
        pic_width_in_luma_samples,
        pic_height_in_luma_samples,
    })
}

struct H265SpsSummary {
    sps_video_parameter_set_id: u8,
    sps_seq_parameter_set_id: u8,
    chroma_format_idc: u8,
    pic_width_in_luma_samples: u32,
    pic_height_in_luma_samples: u32,
}

fn parse_file_h265(nalus: &[Vec<u8>]) {
    let mut vps_count = 0u32;
    let mut sps_count = 0u32;
    let mut pps_count = 0u32;
    let mut idr_count = 0u32;
    let mut slice_count = 0u32;
    let mut sei_count = 0u32;
    let mut other_count = 0u32;

    for nalu in nalus {
        if nalu.len() < 2 {
            continue;
        }
        // H.265 NAL header: 2 bytes
        let nal_type_raw = (nalu[0] >> 1) & 0x3F;
        match H265NalUnitType::from_raw(nal_type_raw) {
            Some(H265NalUnitType::VpsNut) => {
                vps_count += 1;
                println!("  VPS found (type={})", h265_nal_type_name(H265NalUnitType::VpsNut));
            }
            Some(H265NalUnitType::SpsNut) => {
                sps_count += 1;
                // Skip 2-byte NAL header, then remove emulation prevention bytes
                let rbsp = remove_emulation_prevention_bytes(&nalu[2..]);
                match parse_h265_sps_summary(&rbsp) {
                    Some(sps) => {
                        println!(
                            "  SPS id={}: vps_id={}, chroma_format={}, {}x{}",
                            sps.sps_seq_parameter_set_id,
                            sps.sps_video_parameter_set_id,
                            sps.chroma_format_idc,
                            sps.pic_width_in_luma_samples,
                            sps.pic_height_in_luma_samples,
                        );
                    }
                    None => {
                        println!("  SPS found (parse failed)");
                    }
                }
            }
            Some(H265NalUnitType::PpsNut) => {
                pps_count += 1;
                println!("  PPS found (type={})", h265_nal_type_name(H265NalUnitType::PpsNut));
            }
            Some(t) if t.is_idr() => {
                idr_count += 1;
            }
            Some(t) if t.is_slice() => {
                slice_count += 1;
            }
            Some(H265NalUnitType::PrefixSeiNut | H265NalUnitType::SuffixSeiNut) => {
                sei_count += 1;
            }
            Some(t) => {
                other_count += 1;
                if other_count <= 5 {
                    println!("  Other NAL: {} (type={})", h265_nal_type_name(t), nal_type_raw);
                }
            }
            None => {
                other_count += 1;
            }
        }
    }

    println!();
    println!("Summary:");
    println!("  VPS: {vps_count}");
    println!("  SPS: {sps_count}");
    println!("  PPS: {pps_count}");
    println!("  IDR slices: {idr_count}");
    println!("  Non-IDR slices: {slice_count}");
    println!("  SEI: {sei_count}");
    println!("  Other: {other_count}");
}

// ---------------------------------------------------------------------------
// IVF container / AV1 OBU parsing
// ---------------------------------------------------------------------------

/// IVF file header (32 bytes).
struct IvfFileHeader {
    fourcc: [u8; 4],
    width: u16,
    height: u16,
    timebase_den: u32,
    timebase_num: u32,
    num_frames: u32,
}

/// Returns true if the data starts with the "DKIF" IVF signature.
fn is_ivf(data: &[u8]) -> bool {
    data.len() >= 32 && &data[0..4] == b"DKIF"
}

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn parse_ivf_header(data: &[u8]) -> Option<IvfFileHeader> {
    if data.len() < 32 || &data[0..4] != b"DKIF" {
        return None;
    }
    Some(IvfFileHeader {
        fourcc: [data[8], data[9], data[10], data[11]],
        width: read_u16_le(data, 12),
        height: read_u16_le(data, 14),
        timebase_den: read_u32_le(data, 16),
        timebase_num: read_u32_le(data, 20),
        num_frames: read_u32_le(data, 24),
    })
}

/// Extract IVF frame payloads from the data (skipping the 32-byte file header).
fn extract_ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut offset = 32; // skip file header
    while offset + 12 <= data.len() {
        let frame_size = read_u32_le(data, offset) as usize;
        // skip 4-byte size + 8-byte timestamp
        let payload_start = offset + 12;
        let payload_end = payload_start + frame_size;
        if payload_end > data.len() {
            break;
        }
        frames.push(&data[payload_start..payload_end]);
        offset = payload_end;
    }
    frames
}

/// Return a human-readable name for an AV1 OBU type.
fn av1_obu_type_name(obu_type: Av1ObuType) -> &'static str {
    match obu_type {
        Av1ObuType::SequenceHeader => "SEQUENCE_HEADER",
        Av1ObuType::TemporalDelimiter => "TEMPORAL_DELIMITER",
        Av1ObuType::FrameHeader => "FRAME_HEADER",
        Av1ObuType::TileGroup => "TILE_GROUP",
        Av1ObuType::Metadata => "METADATA",
        Av1ObuType::Frame => "FRAME",
        Av1ObuType::RedundantFrameHeader => "REDUNDANT_FRAME_HEADER",
        Av1ObuType::TileList => "TILE_LIST",
        Av1ObuType::Padding => "PADDING",
    }
}

/// Parse OBUs from a single IVF frame payload and return type counts.
fn count_obus_in_frame(frame: &[u8], counts: &mut [u32; 16]) {
    let mut pos = 0;
    while pos < frame.len() {
        let remaining = &frame[pos..];
        let hdr = match VulkanAv1Decoder::read_obu_header(remaining) {
            Some(h) => h,
            None => break,
        };

        let header_size = hdr.header_size as usize;

        // Read the OBU payload size (LEB128) after the header
        let size_data = &remaining[header_size..];
        let (payload_size, size_len) = match VulkanAv1Decoder::read_obu_size(size_data) {
            Some(v) if hdr.has_size_field => v,
            _ => {
                // No size field or parse failure — consume rest of frame
                if let Some(t) = hdr.obu_type {
                    counts[t as usize] += 1;
                }
                break;
            }
        };

        if let Some(t) = hdr.obu_type {
            counts[t as usize] += 1;
        }

        pos += header_size + size_len as usize + payload_size as usize;
    }
}

fn parse_file_av1(data: &[u8]) {
    let header = match parse_ivf_header(data) {
        Some(h) => h,
        None => {
            println!("Failed to parse IVF header.");
            return;
        }
    };

    let fourcc_str = std::str::from_utf8(&header.fourcc).unwrap_or("????");
    println!("IVF header:");
    println!("  FourCC: {fourcc_str}");
    println!("  Resolution: {}x{}", header.width, header.height);
    println!("  Timebase: {}/{}", header.timebase_num, header.timebase_den);
    println!("  Num frames (IVF): {}", header.num_frames);
    println!();

    let frames = extract_ivf_frames(data);
    println!("IVF frames extracted: {}", frames.len());

    // Count OBU types across all frames
    let mut obu_counts = [0u32; 16];
    for frame in &frames {
        count_obus_in_frame(frame, &mut obu_counts);
    }

    println!();
    println!("OBU Summary:");
    for i in 0..16u8 {
        if obu_counts[i as usize] == 0 {
            continue;
        }
        let name = match Av1ObuType::from_u8(i) {
            Some(t) => av1_obu_type_name(t),
            None => "UNKNOWN",
        };
        println!("  {name} (type={i}): {}", obu_counts[i as usize]);
    }
}

fn parse_file_av1_to_summary(data: &[u8]) -> String {
    let mut out = String::new();
    let header = match parse_ivf_header(data) {
        Some(h) => h,
        None => {
            let _ = writeln!(out, "Failed to parse IVF header.");
            return out;
        }
    };

    let fourcc_str = std::str::from_utf8(&header.fourcc).unwrap_or("????");
    let _ = writeln!(out, "Detected codec: AV1 (IVF container)");
    let _ = writeln!(out, "  FourCC: {fourcc_str}");
    let _ = writeln!(out, "  Resolution: {}x{}", header.width, header.height);
    let _ = writeln!(out, "  Timebase: {}/{}", header.timebase_num, header.timebase_den);
    let _ = writeln!(out, "  Num frames (IVF): {}", header.num_frames);

    let frames = extract_ivf_frames(data);
    let _ = writeln!(out, "  IVF frames extracted: {}", frames.len());

    let mut obu_counts = [0u32; 16];
    for frame in &frames {
        count_obus_in_frame(frame, &mut obu_counts);
    }

    for i in 0..16u8 {
        if obu_counts[i as usize] == 0 {
            continue;
        }
        let name = match Av1ObuType::from_u8(i) {
            Some(t) => av1_obu_type_name(t),
            None => "UNKNOWN",
        };
        let _ = writeln!(out, "  {name} (type={i}): {}", obu_counts[i as usize]);
    }
    out
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pipeline mode: parse H.264, H.265, and AV1 test streams, write report
// ---------------------------------------------------------------------------

fn parse_file_to_summary(path: &str) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let mut out = String::new();
    let _ = writeln!(out, "File: {path}");
    let _ = writeln!(out, "Size: {} bytes", data.len());

    if data.len() < 4 {
        let _ = writeln!(out, "File too small to contain any units.");
        return Ok(out);
    }

    // Check for IVF container (AV1)
    if is_ivf(&data) {
        out.push_str(&parse_file_av1_to_summary(&data));
        return Ok(out);
    }

    let mut parser = ByteStreamParser::new(data.len() + 1024);
    let pck = BitstreamPacket {
        data: &data,
        eop: true,
        ..Default::default()
    };
    parser.parse_byte_stream(&pck).map_err(|e| format!("{e:?}"))?;

    let nalus = parser.completed_nalus();
    let _ = writeln!(out, "NAL units found: {}", nalus.len());

    if nalus.is_empty() {
        let _ = writeln!(out, "No NAL units detected.");
        return Ok(out);
    }

    let is_h265 = detect_h265(nalus);

    if is_h265 {
        let _ = writeln!(out, "Detected codec: H.265/HEVC");
        let (vps, sps, pps, idr, slice, sei, other) = count_h265_nalus(nalus);
        let _ = writeln!(out, "  VPS: {vps}");
        let _ = writeln!(out, "  SPS: {sps}");
        let _ = writeln!(out, "  PPS: {pps}");
        let _ = writeln!(out, "  IDR slices: {idr}");
        let _ = writeln!(out, "  Non-IDR slices: {slice}");
        let _ = writeln!(out, "  SEI: {sei}");
        let _ = writeln!(out, "  Other: {other}");

        // Try to extract SPS info for the first SPS NAL
        for nalu in nalus {
            if nalu.len() < 2 {
                continue;
            }
            let nt = (nalu[0] >> 1) & 0x3F;
            if nt == 33 {
                let rbsp = remove_emulation_prevention_bytes(&nalu[2..]);
                if let Some(sps_info) = parse_h265_sps_summary(&rbsp) {
                    let _ = writeln!(
                        out,
                        "  SPS detail: id={}, vps_id={}, chroma={}, {}x{}",
                        sps_info.sps_seq_parameter_set_id,
                        sps_info.sps_video_parameter_set_id,
                        sps_info.chroma_format_idc,
                        sps_info.pic_width_in_luma_samples,
                        sps_info.pic_height_in_luma_samples,
                    );
                }
                break;
            }
        }
    } else {
        let _ = writeln!(out, "Detected codec: H.264/AVC");
        let (sps, pps, idr, slice, sei, other) = count_h264_nalus(nalus);
        let _ = writeln!(out, "  SPS: {sps}");
        let _ = writeln!(out, "  PPS: {pps}");
        let _ = writeln!(out, "  IDR slices: {idr}");
        let _ = writeln!(out, "  Non-IDR slices: {slice}");
        let _ = writeln!(out, "  SEI: {sei}");
        let _ = writeln!(out, "  Other: {other}");
    }

    Ok(out)
}

fn count_h264_nalus(nalus: &[Vec<u8>]) -> (u32, u32, u32, u32, u32, u32) {
    let (mut sps, mut pps, mut idr, mut slice, mut sei, mut other) = (0, 0, 0, 0, 0, 0);
    for nalu in nalus {
        if nalu.is_empty() {
            continue;
        }
        let nal_type_raw = nalu[0] & 0x1F;
        match H264NalUnitType::from_u8(nal_type_raw) {
            Some(H264NalUnitType::Sps) => sps += 1,
            Some(H264NalUnitType::Pps) => pps += 1,
            Some(H264NalUnitType::CodedSliceIdr) => idr += 1,
            Some(H264NalUnitType::CodedSlice) => slice += 1,
            Some(H264NalUnitType::Sei) => sei += 1,
            _ => other += 1,
        }
    }
    (sps, pps, idr, slice, sei, other)
}

fn count_h265_nalus(nalus: &[Vec<u8>]) -> (u32, u32, u32, u32, u32, u32, u32) {
    let (mut vps, mut sps, mut pps, mut idr, mut slice, mut sei, mut other) =
        (0, 0, 0, 0, 0, 0, 0);
    for nalu in nalus {
        if nalu.len() < 2 {
            continue;
        }
        let nal_type_raw = (nalu[0] >> 1) & 0x3F;
        match H265NalUnitType::from_raw(nal_type_raw) {
            Some(H265NalUnitType::VpsNut) => vps += 1,
            Some(H265NalUnitType::SpsNut) => sps += 1,
            Some(H265NalUnitType::PpsNut) => pps += 1,
            Some(t) if t.is_idr() => idr += 1,
            Some(t) if t.is_slice() => slice += 1,
            Some(H265NalUnitType::PrefixSeiNut | H265NalUnitType::SuffixSeiNut) => sei += 1,
            _ => other += 1,
        }
    }
    (vps, sps, pps, idr, slice, sei, other)
}

fn run_pipeline() -> Result<(), String> {
    let h264_path = "/tmp/test_h264_stream.h264";
    let h265_path = "/tmp/test_h265_stream.h265";
    let av1_path = "/tmp/test_av1_large.ivf";
    let report_path = "/tmp/decode_results.txt";

    let mut report = String::new();
    let _ = writeln!(report, "=== nvpro-vulkan-video decode pipeline report ===");
    let _ = writeln!(report);

    let mut any_parsed = false;

    if Path::new(h264_path).exists() {
        any_parsed = true;
        let _ = writeln!(report, "--- H.264 Stream ---");
        match parse_file_to_summary(h264_path) {
            Ok(s) => report.push_str(&s),
            Err(e) => { let _ = writeln!(report, "Error: {e}"); }
        }
        let _ = writeln!(report);
    } else {
        let _ = writeln!(report, "H.264 test stream not found at {h264_path}");
        let _ = writeln!(report);
    }

    if Path::new(h265_path).exists() {
        any_parsed = true;
        let _ = writeln!(report, "--- H.265 Stream ---");
        match parse_file_to_summary(h265_path) {
            Ok(s) => report.push_str(&s),
            Err(e) => { let _ = writeln!(report, "Error: {e}"); }
        }
        let _ = writeln!(report);
    } else {
        let _ = writeln!(report, "H.265 test stream not found at {h265_path}");
        let _ = writeln!(report);
    }

    if Path::new(av1_path).exists() {
        any_parsed = true;
        let _ = writeln!(report, "--- AV1 Stream ---");
        match parse_file_to_summary(av1_path) {
            Ok(s) => report.push_str(&s),
            Err(e) => { let _ = writeln!(report, "Error: {e}"); }
        }
        let _ = writeln!(report);
    } else {
        let _ = writeln!(report, "AV1 test stream not found at {av1_path}");
        let _ = writeln!(report);
    }

    if !any_parsed {
        return Err(format!(
            "No test streams found. Place files at {h264_path}, {h265_path}, and/or {av1_path}"
        ));
    }

    print!("{report}");

    fs::write(report_path, &report)
        .map_err(|e| format!("failed to write report to {report_path}: {e}"))?;
    println!("Report written to {report_path}");

    Ok(())
}



// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    // Note: tracing-subscriber is a dev-dependency and may not be available
    // in the binary. Parser tracing output goes to the default subscriber
    // if one is installed by the caller.

    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        let arg = &args[1];

        // Pipeline mode
        if arg == "--pipeline" {
            println!("=== nvpro-vulkan-video decode-test (pipeline mode) ===");
            match run_pipeline() {
                Ok(()) => process::exit(0),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }

        // File parsing mode
        println!("=== nvpro-vulkan-video decode-test (file mode) ===");
        match parse_file(arg) {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        }
    }

    // Self-test mode
    println!("=== nvpro-vulkan-video decode-test (self-test mode) ===");
    println!();

    let mut runner = TestRunner::new();

    test_exp_golomb(&mut runner);
    println!();
    test_byte_stream_parser(&mut runner);
    println!();
    test_h264_parser(&mut runner);
    println!();
    test_h265_parser(&mut runner);
    println!();
    test_scaling_lists(&mut runner);
    println!();

    if runner.summary() {
        process::exit(0);
    } else {
        process::exit(1);
    }
}
