// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Patch VPS timing fields in driver-generated H.265 VPS NAL units.
//!
//! The NVIDIA Vulkan Video encoder emits an H.265 VPS NAL with broken timing
//! (`vps_time_scale = 0` / wrong `vps_num_units_in_tick`). The VPS is a
//! separate NAL from the SPS, so its timing block cannot be set through the
//! standard `pSequenceParameterSetVui` chain — this module parses the cached
//! VPS bytes, locates the timing section, and rewrites it with correct
//! values derived from the encoder config's framerate.
//!
//! H.264 timing rides in the SPS VUI, which the session-parameter creation
//! path now chains directly via `pSequenceParameterSetVui` (see
//! `session.rs::create_session_parameters`). No post-write H.264 patching
//! is required.

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
        if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
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
                if (i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1) || data[i + 2] == 1
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

/// Patch the timing info in the cached H.265 VPS header bytes.
///
/// Only the H.265 VPS is rewritten — `vps_num_units_in_tick` /
/// `vps_time_scale`. H.264 SPS VUI timing is now produced directly by the
/// session-parameter creation path (`session.rs::create_session_parameters`
/// chains `pSequenceParameterSetVui`), so calls with
/// `codec_flag = ENCODE_H264` are a no-op and return the input unchanged.
///
/// All other NAL units (SPS, PPS, slice data) are passed through unchanged.
pub(crate) fn patch_header_timing(
    header: &[u8],
    codec_flag: vk::VideoCodecOperationFlagsKHR,
    fps_num: u32,
    fps_den: u32,
) -> Vec<u8> {
    if header.is_empty() || fps_num == 0 || fps_den == 0 {
        return header.to_vec();
    }
    if codec_flag != vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        // H.264 timing now rides in the SPS VUI chain; nothing to patch.
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
    fn h264_patch_header_timing_is_no_op() {
        // H.264 timing rides in the SPS VUI chain now; patch_header_timing
        // should pass the bytes through unchanged for H.264.
        let header = vec![
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, // SPS NAL (Baseline)
            0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS NAL
        ];
        let patched =
            patch_header_timing(&header, vk::VideoCodecOperationFlagsKHR::ENCODE_H264, 30, 1);
        assert_eq!(patched, header);
    }
}
