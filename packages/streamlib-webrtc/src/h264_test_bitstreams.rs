// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hand-built H.264 syntax, so a test can state a stream's shape rather than a
//! hex blob nobody can check.

#![cfg(test)]

/// Writes the syntax elements a parameter set is made of.
#[derive(Default)]
pub(crate) struct RawBitstreamWriter {
    bytes: Vec<u8>,
    bits_in_last_byte: u32,
}

impl RawBitstreamWriter {
    pub(crate) fn bit(&mut self, value: u8) -> &mut Self {
        if self.bits_in_last_byte == 0 {
            self.bytes.push(0);
            self.bits_in_last_byte = 8;
        }
        self.bits_in_last_byte -= 1;
        let last = self.bytes.len() - 1;
        self.bytes[last] |= (value & 1) << self.bits_in_last_byte;
        self
    }

    pub(crate) fn bits(&mut self, value: u32, count: u32) -> &mut Self {
        for index in (0..count).rev() {
            self.bit(((value >> index) & 1) as u8);
        }
        self
    }

    /// `ue(v)` — H.264 §9.1.
    pub(crate) fn unsigned_exp_golomb(&mut self, value: u32) -> &mut Self {
        let code_number = value + 1;
        let significant_bits = 32 - code_number.leading_zeros();
        self.bits(0, significant_bits - 1);
        self.bits(code_number, significant_bits)
    }

    /// The `rbsp_trailing_bits()` every NAL unit ends on.
    pub(crate) fn finish(&mut self, nal_unit_header: u8) -> Vec<u8> {
        self.bit(1);
        while self.bits_in_last_byte != 0 {
            self.bit(0);
        }
        let mut nal_unit = vec![nal_unit_header];
        nal_unit.extend_from_slice(&self.bytes);
        nal_unit
    }
}

/// A constrained-baseline SPS for 320x180 4:2:0 progressive: 20 macroblocks
/// across, 12 map units down (192 coded), the bottom 12 rows cropped away.
pub(crate) fn baseline_320x180(vui: impl FnOnce(&mut RawBitstreamWriter)) -> Vec<u8> {
    let mut writer = RawBitstreamWriter::default();
    writer
        .bits(66, 8) // profile_idc — constrained baseline
        .bits(0xE0, 8) // constraint flags
        .bits(30, 8) // level_idc
        .unsigned_exp_golomb(0) // seq_parameter_set_id
        .unsigned_exp_golomb(0) // log2_max_frame_num_minus4
        .unsigned_exp_golomb(0) // pic_order_cnt_type
        .unsigned_exp_golomb(0) // log2_max_pic_order_cnt_lsb_minus4
        .unsigned_exp_golomb(1) // max_num_ref_frames
        .bit(0) // gaps_in_frame_num_value_allowed_flag
        .unsigned_exp_golomb(19) // pic_width_in_mbs_minus1
        .unsigned_exp_golomb(11) // pic_height_in_map_units_minus1
        .bit(1) // frame_mbs_only_flag
        .bit(1) // direct_8x8_inference_flag
        .bit(1) // frame_cropping_flag
        .unsigned_exp_golomb(0) // left
        .unsigned_exp_golomb(0) // right
        .unsigned_exp_golomb(0) // top
        .unsigned_exp_golomb(6); // bottom — 6 crop units of 2 luma rows
    vui(&mut writer);
    writer.finish(0x67)
}

/// No `vui_parameters()` block at all.
pub(crate) fn no_vui(writer: &mut RawBitstreamWriter) {
    writer.bit(0);
}

/// A `vui_parameters()` carrying a full colour description.
pub(crate) fn vui_with_colour(
    primaries: u32,
    transfer: u32,
    matrix: u32,
    full_range: u8,
) -> impl FnOnce(&mut RawBitstreamWriter) {
    move |writer| {
        writer
            .bit(1) // vui_parameters_present_flag
            .bit(0) // aspect_ratio_info_present_flag
            .bit(0) // overscan_info_present_flag
            .bit(1) // video_signal_type_present_flag
            .bits(5, 3) // video_format — unspecified
            .bit(full_range)
            .bit(1) // colour_description_present_flag
            .bits(primaries, 8)
            .bits(transfer, 8)
            .bits(matrix, 8);
    }
}
