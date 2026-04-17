// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Patch VUI/timing fields in driver-generated SPS/VPS NAL units.
//!
//! The NVIDIA Vulkan Video encoder emits SPS/VPS NAL units with broken timing
//! info (time_scale=0 for H.264, wrong r_frame_rate for H.265). This module
//! parses the cached header bytes, locates the VUI/timing section, and rewrites
//! it with correct values derived from the encoder config's framerate.
//!
//! Approach: parse the driver-generated NAL to the VUI/timing bit offset, copy
//! all preceding bits verbatim, then write a corrected VUI/timing section
//! followed by RBSP trailing bits. This avoids re-serializing the entire
//! parameter set.

use vulkanalia::vk;

// ---------------------------------------------------------------------------
// Bitstream reader (minimal, for finding VUI/timing offsets)
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.pos)
    }

    fn u(&mut self, n: u32) -> u32 {
        let mut val = 0u32;
        for _ in 0..n {
            let byte_idx = self.pos / 8;
            let bit_idx = 7 - (self.pos % 8);
            if byte_idx < self.data.len() {
                val = (val << 1) | (((self.data[byte_idx] >> bit_idx) & 1) as u32);
            } else {
                val <<= 1;
            }
            self.pos += 1;
        }
        val
    }

    fn flag(&mut self) -> bool {
        self.u(1) != 0
    }

    fn ue(&mut self) -> u32 {
        let mut lz = 0u32;
        while self.remaining() > 0 && self.u(1) == 0 {
            lz += 1;
            if lz > 31 {
                return u32::MAX;
            }
        }
        if lz == 0 {
            return 0;
        }
        let suffix = self.u(lz);
        (1u32 << lz) - 1 + suffix
    }

    fn se(&mut self) -> i32 {
        let code = self.ue();
        if code == 0 {
            return 0;
        }
        let val = ((code + 1) / 2) as i32;
        if code % 2 == 0 {
            -val
        } else {
            val
        }
    }
}

// ---------------------------------------------------------------------------
// Bitstream writer
// ---------------------------------------------------------------------------

struct BitWriter {
    buf: Vec<u8>,
    bit_pos: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            bit_pos: 0,
        }
    }

    fn put_bit(&mut self, b: u32) {
        let byte_idx = self.bit_pos / 8;
        let bit_idx = 7 - (self.bit_pos % 8);
        if byte_idx >= self.buf.len() {
            self.buf.push(0);
        }
        if b & 1 != 0 {
            self.buf[byte_idx] |= 1 << bit_idx;
        }
        self.bit_pos += 1;
    }

    fn put_bits(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            self.put_bit((val >> i) & 1);
        }
    }

    fn put_ue(&mut self, val: u32) {
        if val == 0 {
            self.put_bit(1);
            return;
        }
        let code = val + 1;
        let lz = 31 - code.leading_zeros();
        for _ in 0..lz {
            self.put_bit(0);
        }
        self.put_bits(code, lz + 1);
    }

    fn trailing_bits(&mut self) {
        self.put_bit(1); // RBSP stop bit
        while self.bit_pos % 8 != 0 {
            self.put_bit(0);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Copy the first `n` bits from `src` into `writer`.
fn copy_bits(src: &[u8], writer: &mut BitWriter, n: usize) {
    for i in 0..n {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        let bit = if byte_idx < src.len() {
            (src[byte_idx] >> bit_idx) & 1
        } else {
            0
        };
        writer.put_bit(bit as u32);
    }
}

// ---------------------------------------------------------------------------
// Emulation Prevention Byte (EPB) handling
// ---------------------------------------------------------------------------

/// Remove emulation prevention bytes: `00 00 03 xx` -> `00 00 xx`
/// where xx is 00, 01, 02, or 03.
fn remove_epb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + 2 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 3
        {
            // Check if this is a valid EPB (next byte must be 00, 01, 02, or 03)
            if i + 3 < data.len() && data[i + 3] <= 3 {
                out.push(0);
                out.push(0);
                i += 3; // skip the 03 emulation prevention byte
                continue;
            }
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

/// Add emulation prevention bytes to RBSP data.
/// Inserts 03 before `00 00 00`, `00 00 01`, `00 00 02`, `00 00 03`.
fn add_epb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 64);
    let mut zero_count = 0u32;
    for &b in data {
        if zero_count >= 2 && b <= 3 {
            out.push(3);
            zero_count = 0;
        }
        out.push(b);
        if b == 0 {
            zero_count += 1;
        } else {
            zero_count = 0;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// NAL unit splitting / reassembly
// ---------------------------------------------------------------------------

/// Split Annex-B byte stream into (start_code_length, nal_bytes) pairs.
/// `nal_bytes` includes the NAL header but excludes the start code.
fn split_nals(data: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let mut nals = Vec::new();
    let mut i = 0;
    while i < data.len() {
        // Look for start code
        let sc_len;
        if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            sc_len = 4;
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            sc_len = 3;
        } else {
            i += 1;
            continue;
        }
        let nal_start = i + sc_len;
        i = nal_start;

        // Find next start code or end of data
        let mut nal_end = data.len();
        while i + 2 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 {
                if (i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1)
                    || data[i + 2] == 1
                {
                    nal_end = i;
                    break;
                }
            }
            i += 1;
        }
        if i + 2 >= data.len() {
            i = data.len();
        }

        if nal_start < nal_end {
            nals.push((sc_len, data[nal_start..nal_end].to_vec()));
        }
    }
    nals
}

/// Reassemble NAL units with Annex-B start codes.
fn reassemble_nals(nals: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let total: usize = nals.iter().map(|(sc, d)| sc + d.len()).sum();
    let mut out = Vec::with_capacity(total);
    for (sc_len, nal_data) in nals {
        if *sc_len == 4 {
            out.extend_from_slice(&[0, 0, 0, 1]);
        } else {
            out.extend_from_slice(&[0, 0, 1]);
        }
        out.extend_from_slice(nal_data);
    }
    out
}

// ---------------------------------------------------------------------------
// H.264: find vui_parameters_present_flag bit offset in SPS RBSP
// ---------------------------------------------------------------------------

/// Advance the reader past an H.264 scaling list of the given size.
/// Reads `scaling_list_present_flag` and, if set, up to `size` delta values.
fn skip_h264_scaling_list(r: &mut BitReader, size: usize) {
    if r.flag() {
        let mut last_scale: i32 = 8;
        let mut next_scale: i32 = 8;
        for _j in 0..size {
            if next_scale != 0 {
                let delta = r.se();
                next_scale = (last_scale + delta) & 0xff;
            }
            let scale = if next_scale == 0 {
                last_scale
            } else {
                next_scale
            };
            last_scale = scale;
        }
    }
}

/// Parse H.264 SPS RBSP (after NAL header byte) to find the bit offset of
/// `vui_parameters_present_flag`. Returns `None` if parsing fails (data too
/// short or unexpected structure).
fn find_h264_sps_vui_bit_offset(rbsp: &[u8]) -> Option<usize> {
    let mut r = BitReader::new(rbsp);
    if r.remaining() < 24 {
        return None;
    }

    let profile_idc = r.u(8) as u8;
    r.u(8); // constraint_set_flags
    r.u(8); // level_idc
    r.ue(); // seq_parameter_set_id

    // High profile extensions
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma_format_idc = r.ue();
        if chroma_format_idc == 3 {
            r.flag(); // separate_colour_plane_flag
        }
        r.ue(); // bit_depth_luma_minus8
        r.ue(); // bit_depth_chroma_minus8
        r.flag(); // qpprime_y_zero_transform_bypass_flag
        if r.flag() {
            // seq_scaling_matrix_present_flag
            for i in 0..8u32 {
                let size = if i < 6 { 16 } else { 64 };
                skip_h264_scaling_list(&mut r, size);
            }
        }
    }

    r.ue(); // log2_max_frame_num_minus4
    let poc_type = r.ue();
    if poc_type == 0 {
        r.ue(); // log2_max_pic_order_cnt_lsb_minus4
    } else if poc_type == 1 {
        r.flag(); // delta_pic_order_always_zero_flag
        r.se(); // offset_for_non_ref_pic
        r.se(); // offset_for_top_to_bottom_field
        let n = r.ue();
        for _ in 0..n {
            r.se(); // offset_for_ref_frame[i]
        }
    }

    r.ue(); // max_num_ref_frames
    r.flag(); // gaps_in_frame_num_value_allowed_flag
    r.ue(); // pic_width_in_mbs_minus1
    r.ue(); // pic_height_in_map_units_minus1
    let frame_mbs_only = r.flag();
    if !frame_mbs_only {
        r.flag(); // mb_adaptive_frame_field_flag
    }
    r.flag(); // direct_8x8_inference_flag
    if r.flag() {
        // frame_cropping_flag
        r.ue(); // crop_left
        r.ue(); // crop_right
        r.ue(); // crop_top
        r.ue(); // crop_bottom
    }

    if r.remaining() < 1 {
        return None;
    }
    Some(r.pos())
}

// ---------------------------------------------------------------------------
// H.265: find timing_info_present_flag bit offset in VPS RBSP
// ---------------------------------------------------------------------------

/// Skip H.265 profile_tier_level() in the bitstream.
fn skip_h265_profile_tier_level(r: &mut BitReader, max_sub_layers_minus1: u8) {
    r.u(2); // general_profile_space
    r.u(1); // general_tier_flag
    r.u(5); // general_profile_idc
    r.u(32); // general_profile_compatibility_flags
    r.u(4); // constraint flags
    r.u(32); // reserved bits (first 32)
    r.u(12); // reserved bits (last 12)
    r.u(8); // general_level_idc

    let mut sub_profile_present = [false; 8];
    let mut sub_level_present = [false; 8];
    for i in 0..max_sub_layers_minus1 as usize {
        sub_profile_present[i] = r.flag();
        sub_level_present[i] = r.flag();
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.u(2); // reserved_zero_2bits
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if sub_profile_present[i] {
            // sub_layer profile: 2+1+5+32+4+32+12 = 88 bits
            r.u(32);
            r.u(32);
            r.u(24);
        }
        if sub_level_present[i] {
            r.u(8); // sub_layer_level_idc
        }
    }
}

/// Parse H.265 VPS RBSP (after 2-byte NAL header) to find the bit offset of
/// `vps_timing_info_present_flag`.
fn find_h265_vps_timing_bit_offset(rbsp: &[u8]) -> Option<usize> {
    let mut r = BitReader::new(rbsp);
    if r.remaining() < 32 {
        return None;
    }

    r.u(4); // vps_video_parameter_set_id
    r.u(1); // vps_base_layer_internal_flag
    r.u(1); // vps_base_layer_available_flag
    r.u(6); // vps_max_layers_minus1
    let max_sub_layers_minus1 = r.u(3) as u8;
    r.u(1); // vps_temporal_id_nesting_flag
    r.u(16); // vps_reserved_0xffff_16bits

    skip_h265_profile_tier_level(&mut r, max_sub_layers_minus1);

    let sub_layer_ordering_present = r.flag();
    let start = if sub_layer_ordering_present {
        0
    } else {
        max_sub_layers_minus1 as u32
    };
    for _ in start..=max_sub_layers_minus1 as u32 {
        r.ue(); // max_dec_pic_buffering_minus1
        r.ue(); // max_num_reorder_pics
        r.ue(); // max_latency_increase_plus1
    }

    let max_layer_id = r.u(6);
    let num_layer_sets = r.ue() + 1;
    for _ in 1..num_layer_sets {
        for _ in 0..=max_layer_id {
            r.u(1); // layer_id_included_flag
        }
    }

    if r.remaining() < 1 {
        return None;
    }
    Some(r.pos())
}

// ---------------------------------------------------------------------------
// H.264 SPS NAL patching
// ---------------------------------------------------------------------------

/// Patch an H.264 SPS NAL unit to include correct VUI timing.
/// `nal_bytes` includes the 1-byte NAL header.
fn patch_h264_sps_nal(nal_bytes: &[u8], fps_num: u32, fps_den: u32) -> Vec<u8> {
    if nal_bytes.len() < 2 {
        return nal_bytes.to_vec();
    }

    let nal_header = nal_bytes[0];
    let rbsp_with_epb = &nal_bytes[1..];
    let rbsp = remove_epb(rbsp_with_epb);

    let vui_offset = match find_h264_sps_vui_bit_offset(&rbsp) {
        Some(off) => off,
        None => {
            tracing::warn!("Failed to find VUI offset in H.264 SPS, skipping timing patch");
            return nal_bytes.to_vec();
        }
    };

    let mut w = BitWriter::new();

    // Copy all bits before vui_parameters_present_flag
    copy_bits(&rbsp, &mut w, vui_offset);

    // Write vui_parameters_present_flag = 1
    w.put_bit(1);

    // Write minimal VUI with correct timing info.
    // All flags before timing are set to 0 (no aspect ratio, overscan,
    // video signal type, or chroma loc info).
    w.put_bit(0); // aspect_ratio_info_present_flag
    w.put_bit(0); // overscan_info_present_flag
    w.put_bit(0); // video_signal_type_present_flag
    w.put_bit(0); // chroma_loc_info_present_flag

    // Timing info
    w.put_bit(1); // timing_info_present_flag
    w.put_bits(fps_den, 32); // num_units_in_tick
    w.put_bits(fps_num * 2, 32); // time_scale (field-based: 2 ticks per frame)
    w.put_bit(1); // fixed_frame_rate_flag

    // HRD and remaining VUI flags
    w.put_bit(0); // nal_hrd_parameters_present_flag
    w.put_bit(0); // vcl_hrd_parameters_present_flag
    // (no low_delay_hrd_flag since neither HRD present)
    w.put_bit(0); // pic_struct_present_flag
    w.put_bit(0); // bitstream_restriction_flag

    // RBSP trailing bits
    w.trailing_bits();

    // Re-add EPBs and prepend NAL header
    let patched_rbsp = w.into_bytes();
    let with_epb = add_epb(&patched_rbsp);
    let mut result = Vec::with_capacity(1 + with_epb.len());
    result.push(nal_header);
    result.extend_from_slice(&with_epb);
    result
}

// ---------------------------------------------------------------------------
// H.265 VPS NAL patching
// ---------------------------------------------------------------------------

/// Patch an H.265 VPS NAL unit to include correct timing info.
/// `nal_bytes` includes the 2-byte NAL header.
fn patch_h265_vps_nal(nal_bytes: &[u8], fps_num: u32, fps_den: u32) -> Vec<u8> {
    if nal_bytes.len() < 3 {
        return nal_bytes.to_vec();
    }

    let nal_header = &nal_bytes[..2];
    let rbsp_with_epb = &nal_bytes[2..];
    let rbsp = remove_epb(rbsp_with_epb);

    let timing_offset = match find_h265_vps_timing_bit_offset(&rbsp) {
        Some(off) => off,
        None => {
            tracing::warn!("Failed to find timing offset in H.265 VPS, skipping timing patch");
            return nal_bytes.to_vec();
        }
    };

    let mut w = BitWriter::new();

    // Copy all bits before vps_timing_info_present_flag
    copy_bits(&rbsp, &mut w, timing_offset);

    // Write timing info
    w.put_bit(1); // vps_timing_info_present_flag
    w.put_bits(fps_den, 32); // vps_num_units_in_tick
    w.put_bits(fps_num, 32); // vps_time_scale
    w.put_bit(0); // vps_poc_proportional_to_timing_flag
    w.put_ue(0); // vps_num_hrd_parameters = 0

    // vps_extension_flag = 0
    w.put_bit(0);

    // RBSP trailing bits
    w.trailing_bits();

    // Re-add EPBs and prepend NAL header
    let patched_rbsp = w.into_bytes();
    let with_epb = add_epb(&patched_rbsp);
    let mut result = Vec::with_capacity(2 + with_epb.len());
    result.extend_from_slice(nal_header);
    result.extend_from_slice(&with_epb);
    result
}

// ---------------------------------------------------------------------------
// Top-level: patch entire cached header
// ---------------------------------------------------------------------------

/// Patch the timing info in the cached SPS/VPS header bytes.
///
/// For H.264: rewrites the SPS VUI to include correct `num_units_in_tick` and
/// `time_scale` derived from `fps_num` / `fps_den`.
///
/// For H.265: rewrites the VPS to include correct `vps_num_units_in_tick` and
/// `vps_time_scale`.
///
/// PPS NAL units are passed through unchanged.
pub(crate) fn patch_header_timing(
    header: &[u8],
    codec_flag: vk::VideoCodecOperationFlagsKHR,
    fps_num: u32,
    fps_den: u32,
) -> Vec<u8> {
    if header.is_empty() || fps_num == 0 || fps_den == 0 {
        return header.to_vec();
    }

    let nals = split_nals(header);
    if nals.is_empty() {
        return header.to_vec();
    }

    let mut patched: Vec<(usize, Vec<u8>)> = Vec::with_capacity(nals.len());

    for (sc_len, nal_data) in &nals {
        if nal_data.is_empty() {
            patched.push((*sc_len, nal_data.clone()));
            continue;
        }

        if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            let nal_type = nal_data[0] & 0x1F;
            if nal_type == 7 {
                // SPS
                tracing::debug!(
                    original_len = nal_data.len(),
                    fps_num,
                    fps_den,
                    "Patching H.264 SPS VUI timing"
                );
                patched.push((*sc_len, patch_h264_sps_nal(nal_data, fps_num, fps_den)));
            } else {
                patched.push((*sc_len, nal_data.clone()));
            }
        } else if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            // H.265 NAL type is bits [1:6] of the first byte
            let nal_type = (nal_data[0] >> 1) & 0x3F;
            if nal_type == 32 {
                // VPS
                tracing::debug!(
                    original_len = nal_data.len(),
                    fps_num,
                    fps_den,
                    "Patching H.265 VPS timing"
                );
                patched.push((*sc_len, patch_h265_vps_nal(nal_data, fps_num, fps_den)));
            } else {
                patched.push((*sc_len, nal_data.clone()));
            }
        } else {
            patched.push((*sc_len, nal_data.clone()));
        }
    }

    reassemble_nals(&patched)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_writer_single_bits() {
        let mut w = BitWriter::new();
        w.put_bit(1);
        w.put_bit(0);
        w.put_bit(1);
        w.put_bit(0);
        w.put_bit(1);
        w.put_bit(0);
        w.put_bit(1);
        w.put_bit(0);
        assert_eq!(w.into_bytes(), vec![0b10101010]);
    }

    #[test]
    fn test_bit_writer_put_bits() {
        let mut w = BitWriter::new();
        w.put_bits(0xFF, 8);
        assert_eq!(w.into_bytes(), vec![0xFF]);

        let mut w = BitWriter::new();
        w.put_bits(0b1010, 4);
        w.put_bits(0b0101, 4);
        assert_eq!(w.into_bytes(), vec![0b10100101]);
    }

    #[test]
    fn test_bit_writer_ue() {
        // ue(0) = "1" -> 0x80 (1 bit + 7 padding)
        let mut w = BitWriter::new();
        w.put_ue(0);
        w.trailing_bits();
        // "1" + stop "1" + 6 zeros = 0b11000000 = 0xC0
        assert_eq!(w.into_bytes(), vec![0b11000000]);

        // ue(1) = "010" (3 bits)
        let mut w = BitWriter::new();
        w.put_ue(1);
        w.trailing_bits();
        // "010" + stop "1" + 4 zeros = 0b01010000 = 0x50
        assert_eq!(w.into_bytes(), vec![0b01010000]);

        // ue(2) = "011"
        let mut w = BitWriter::new();
        w.put_ue(2);
        w.trailing_bits();
        assert_eq!(w.into_bytes(), vec![0b01110000]);

        // ue(3) = "00100"
        let mut w = BitWriter::new();
        w.put_ue(3);
        w.trailing_bits();
        // "00100" + stop "1" + 2 zeros = 0b00100100
        assert_eq!(w.into_bytes(), vec![0b00100100]);
    }

    #[test]
    fn test_bit_reader_ue_roundtrip() {
        for val in [0u32, 1, 2, 3, 7, 8, 15, 100, 255, 1000] {
            let mut w = BitWriter::new();
            w.put_ue(val);
            w.trailing_bits();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.ue(), val, "ue roundtrip failed for {val}");
        }
    }

    #[test]
    fn test_epb_remove() {
        // 00 00 03 00 -> 00 00 00
        assert_eq!(remove_epb(&[0, 0, 3, 0]), vec![0, 0, 0]);
        // 00 00 03 01 -> 00 00 01
        assert_eq!(remove_epb(&[0, 0, 3, 1]), vec![0, 0, 1]);
        // 00 00 03 02 -> 00 00 02
        assert_eq!(remove_epb(&[0, 0, 3, 2]), vec![0, 0, 2]);
        // 00 00 03 03 -> 00 00 03
        assert_eq!(remove_epb(&[0, 0, 3, 3]), vec![0, 0, 3]);
        // No EPB: regular data
        assert_eq!(remove_epb(&[1, 2, 3, 4]), vec![1, 2, 3, 4]);
        // 00 00 03 04 -> no removal (04 > 03)
        assert_eq!(remove_epb(&[0, 0, 3, 4]), vec![0, 0, 3, 4]);
    }

    #[test]
    fn test_epb_add() {
        // 00 00 00 -> 00 00 03 00
        assert_eq!(add_epb(&[0, 0, 0]), vec![0, 0, 3, 0]);
        // 00 00 01 -> 00 00 03 01
        assert_eq!(add_epb(&[0, 0, 1]), vec![0, 0, 3, 1]);
        // Regular data: no change
        assert_eq!(add_epb(&[1, 2, 3, 4]), vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_epb_roundtrip() {
        let data: Vec<u8> = vec![0, 0, 0, 1, 0, 0, 2, 0, 0, 3, 5, 10];
        let with_epb = add_epb(&data);
        let restored = remove_epb(&with_epb);
        assert_eq!(data, restored);
    }

    #[test]
    fn test_split_nals() {
        // Two NAL units with 4-byte start codes
        let data = vec![
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, // SPS NAL
            0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS NAL
        ];
        let nals = split_nals(&data);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].0, 4);
        assert_eq!(nals[0].1, vec![0x67, 0x42, 0x00, 0x1E]);
        assert_eq!(nals[1].0, 4);
        assert_eq!(nals[1].1, vec![0x68, 0xCE, 0x38, 0x80]);
    }

    #[test]
    fn test_copy_bits() {
        let src = vec![0b10110100, 0b11001010];
        let mut w = BitWriter::new();
        copy_bits(&src, &mut w, 12);
        w.trailing_bits();
        let bytes = w.into_bytes();
        // First 12 bits: 1011 0100 1100
        // Trailing: 1 + 3 zeros = 1000
        // = 0b10110100 0b11001000
        assert_eq!(bytes, vec![0b10110100, 0b11001000]);
    }

    #[test]
    fn test_h264_sps_vui_offset_basic() {
        // Build a minimal Baseline profile SPS RBSP (after NAL header byte):
        // profile_idc=66 (Baseline, NOT high profile), constraint=0, level=30
        // sps_id=0, log2_max_frame_num=0, poc_type=0, log2_max_poc_lsb=0,
        // max_ref=1, gaps=0, width=7 (128px), height=5 (96px),
        // frame_mbs_only=1, direct_8x8=1, no cropping
        let mut w = BitWriter::new();
        w.put_bits(66, 8); // profile_idc = Baseline
        w.put_bits(0, 8); // constraint_set_flags
        w.put_bits(30, 8); // level_idc
        w.put_ue(0); // sps_id
        // (no high profile extension)
        w.put_ue(0); // log2_max_frame_num_minus4
        w.put_ue(0); // pic_order_cnt_type = 0
        w.put_ue(0); // log2_max_pic_order_cnt_lsb_minus4
        w.put_ue(1); // max_num_ref_frames
        w.put_bit(0); // gaps_in_frame_num
        w.put_ue(7); // pic_width_in_mbs_minus1
        w.put_ue(5); // pic_height_in_map_units_minus1
        w.put_bit(1); // frame_mbs_only_flag
        w.put_bit(1); // direct_8x8_inference_flag
        w.put_bit(0); // frame_cropping_flag
        // NEXT BIT: vui_parameters_present_flag
        let expected_offset = w.bit_pos;
        w.put_bit(0); // vui_parameters_present_flag = 0
        w.trailing_bits();

        let rbsp = w.into_bytes();
        let offset = find_h264_sps_vui_bit_offset(&rbsp).unwrap();
        assert_eq!(offset, expected_offset);
    }

    #[test]
    fn test_patch_h264_sps_roundtrip() {
        // Build a minimal SPS NAL and patch it
        let mut w = BitWriter::new();
        w.put_bits(66, 8); // profile_idc
        w.put_bits(0, 8); // constraint
        w.put_bits(30, 8); // level
        w.put_ue(0); // sps_id
        w.put_ue(0); // log2_max_frame_num
        w.put_ue(0); // poc_type
        w.put_ue(0); // log2_max_poc_lsb
        w.put_ue(1); // max_ref
        w.put_bit(0); // gaps
        w.put_ue(7); // width
        w.put_ue(5); // height
        w.put_bit(1); // frame_mbs_only
        w.put_bit(1); // direct_8x8
        w.put_bit(0); // cropping
        w.put_bit(0); // vui_present=0
        w.trailing_bits();
        let rbsp = w.into_bytes();

        // Add NAL header (SPS, nal_ref_idc=3)
        let mut nal = vec![0x67]; // 0110 0111 = ref_idc=3, type=7
        nal.extend_from_slice(&add_epb(&rbsp));

        let patched = patch_h264_sps_nal(&nal, 30, 1);

        // Parse the patched NAL to verify VUI timing
        let patched_rbsp = remove_epb(&patched[1..]);
        let vui_offset = find_h264_sps_vui_bit_offset(&patched_rbsp).unwrap();
        let mut r = BitReader::new(&patched_rbsp);
        r.u(vui_offset as u32); // skip to VUI flag
        assert!(r.flag()); // vui_parameters_present_flag = 1
        assert!(!r.flag()); // aspect_ratio_info_present_flag = 0
        assert!(!r.flag()); // overscan_info_present_flag = 0
        assert!(!r.flag()); // video_signal_type_present_flag = 0
        assert!(!r.flag()); // chroma_loc_info_present_flag = 0
        assert!(r.flag()); // timing_info_present_flag = 1
        let num_units = r.u(32);
        let time_scale = r.u(32);
        let fixed = r.flag();
        assert_eq!(num_units, 1); // fps_den
        assert_eq!(time_scale, 60); // fps_num * 2
        assert!(fixed); // fixed_frame_rate_flag
    }

    #[test]
    fn test_patch_header_h264() {
        // Build a complete Annex-B header with SPS + PPS
        let mut sps_w = BitWriter::new();
        sps_w.put_bits(66, 8);
        sps_w.put_bits(0, 8);
        sps_w.put_bits(30, 8);
        sps_w.put_ue(0);
        sps_w.put_ue(0);
        sps_w.put_ue(0);
        sps_w.put_ue(0);
        sps_w.put_ue(1);
        sps_w.put_bit(0);
        sps_w.put_ue(7);
        sps_w.put_ue(5);
        sps_w.put_bit(1);
        sps_w.put_bit(1);
        sps_w.put_bit(0);
        sps_w.put_bit(0); // no VUI
        sps_w.trailing_bits();
        let sps_rbsp = sps_w.into_bytes();

        let mut header = Vec::new();
        // SPS with 4-byte start code
        header.extend_from_slice(&[0, 0, 0, 1, 0x67]);
        header.extend_from_slice(&add_epb(&sps_rbsp));
        // PPS with 4-byte start code
        header.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80]);

        let patched = patch_header_timing(
            &header,
            vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            30,
            1,
        );

        // The patched header should still have 2 NALs
        let nals = split_nals(&patched);
        assert_eq!(nals.len(), 2);
        // SPS should be patched (different from original)
        assert_ne!(nals[0].1, {
            let mut v = vec![0x67];
            v.extend_from_slice(&add_epb(&sps_rbsp));
            v
        });
        // PPS should be unchanged
        assert_eq!(nals[1].1, vec![0x68, 0xCE, 0x38, 0x80]);
    }
}
