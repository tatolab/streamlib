// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the SPS a stream carries says about its pictures.
//!
//! RTP carries neither extent nor colour, so the sequence parameter set is the
//! only place a WHEP player can learn them — which is why this parses the
//! bitstream rather than taking either from config.

use crate::error::{Result, WebRtcExtensionError};

/// The extent and colour one SPS declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceParameterSet {
    /// Coded width before the conformance crop — the codec-aligned extent,
    /// which is what the encoded-video wire contract's `width` means and what
    /// the engine's own encoder writes. A player that published the cropped
    /// display extent would disagree with `H264Encoder` about the same stream,
    /// and `Mp4Sink` copies these straight into `tkhd`.
    pub width: u32,
    /// Coded height before the conformance crop.
    pub height: u32,
    /// Absent when the VUI described no colour axis at all, which is distinct
    /// from a VUI that described one and left the rest unspecified.
    pub color: Option<ColorDescription>,
}

/// The bag's `color` sub-map, in the wire's own string spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ColorDescription {
    pub primaries: Option<&'static str>,
    pub transfer: Option<&'static str>,
    pub matrix: Option<&'static str>,
    pub range: Option<&'static str>,
}

impl ColorDescription {
    fn describes_nothing(&self) -> bool {
        self.primaries.is_none()
            && self.transfer.is_none()
            && self.matrix.is_none()
            && self.range.is_none()
    }
}

/// Profiles whose SPS carries the chroma format and scaling lists that the
/// baseline profiles omit — H.264 §7.3.2.1.1.
const PROFILES_CARRYING_CHROMA_FORMAT: [u8; 13] =
    [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

/// Parse one SPS NAL unit, header byte included.
pub(crate) fn parse_sequence_parameter_set(nal_unit: &[u8]) -> Result<SequenceParameterSet> {
    let Some((&header, payload)) = nal_unit.split_first() else {
        return Err(WebRtcExtensionError::MalformedBitstream {
            what: "an empty NAL unit is not a sequence parameter set".to_owned(),
        });
    };
    if header & 0x1F != crate::h264_rtp_depacketiser::NAL_TYPE_SEQUENCE_PARAMETER_SET {
        return Err(WebRtcExtensionError::MalformedBitstream {
            what: format!(
                "NAL unit type {} is not a sequence parameter set",
                header & 0x1F
            ),
        });
    }

    let raw_byte_sequence = strip_emulation_prevention_bytes(payload);
    let mut bits = RawBitstreamReader::new(&raw_byte_sequence);

    let profile_idc = bits.read_bits(8)? as u8;
    bits.skip_bits(8)?; // constraint flags and reserved bits
    bits.skip_bits(8)?; // level_idc
    bits.read_unsigned_exp_golomb()?; // seq_parameter_set_id

    // Absent means 4:2:0 — H.264 §7.4.2.1.1.
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    if PROFILES_CARRYING_CHROMA_FORMAT.contains(&profile_idc) {
        chroma_format_idc = bits.read_unsigned_exp_golomb()?;
        if chroma_format_idc == 3 {
            separate_colour_plane = bits.read_flag()?;
        }
        bits.read_unsigned_exp_golomb()?; // bit_depth_luma_minus8
        bits.read_unsigned_exp_golomb()?; // bit_depth_chroma_minus8
        bits.skip_bits(1)?; // qpprime_y_zero_transform_bypass_flag
        if bits.read_flag()? {
            let list_count = if chroma_format_idc == 3 { 12 } else { 8 };
            for list_index in 0..list_count {
                if bits.read_flag()? {
                    let coefficient_count = if list_index < 6 { 16 } else { 64 };
                    skip_scaling_list(&mut bits, coefficient_count)?;
                }
            }
        }
    }

    bits.read_unsigned_exp_golomb()?; // log2_max_frame_num_minus4
    match bits.read_unsigned_exp_golomb()? {
        0 => {
            bits.read_unsigned_exp_golomb()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            bits.skip_bits(1)?; // delta_pic_order_always_zero_flag
            bits.read_signed_exp_golomb()?; // offset_for_non_ref_pic
            bits.read_signed_exp_golomb()?; // offset_for_top_to_bottom_field
            let cycle_length = bits.read_unsigned_exp_golomb()?;
            for _ in 0..cycle_length {
                bits.read_signed_exp_golomb()?;
            }
        }
        _ => {}
    }

    bits.read_unsigned_exp_golomb()?; // max_num_ref_frames
    bits.skip_bits(1)?; // gaps_in_frame_num_value_allowed_flag

    let width_in_macroblocks = one_more_than(bits.read_unsigned_exp_golomb()?)?;
    let height_in_map_units = one_more_than(bits.read_unsigned_exp_golomb()?)?;
    let frame_macroblocks_only = bits.read_flag()?;
    if !frame_macroblocks_only {
        bits.skip_bits(1)?; // mb_adaptive_frame_field_flag
    }
    bits.skip_bits(1)?; // direct_8x8_inference_flag

    let mut crop_left = 0;
    let mut crop_right = 0;
    let mut crop_top = 0;
    let mut crop_bottom = 0;
    if bits.read_flag()? {
        crop_left = bits.read_unsigned_exp_golomb()?;
        crop_right = bits.read_unsigned_exp_golomb()?;
        crop_top = bits.read_unsigned_exp_golomb()?;
        crop_bottom = bits.read_unsigned_exp_golomb()?;
    }

    let color = if bits.read_flag()? {
        parse_video_usability_information(&mut bits)?
    } else {
        None
    };

    // H.264 §7.4.2.1.1: a field-coded stream stores half a frame per map unit,
    // so the map units count twice.
    let field_multiplier = if frame_macroblocks_only { 1 } else { 2 };
    // Every one of these is an Exp-Golomb value off the wire, so a crafted SPS
    // reaches them with anything up to `u32::MAX`. Overflow checks are on in
    // the profile this wheel is built with, so unchecked arithmetic here is a
    // panic in library code reading network input.
    let coded_width = macroblocks_to_luma_samples(width_in_macroblocks, 1)?;
    let coded_height = macroblocks_to_luma_samples(height_in_map_units, field_multiplier)?;

    let chroma_array_type = if separate_colour_plane {
        0
    } else {
        chroma_format_idc
    };
    // §7.4.2.1.1 again: the crop offsets count in chroma samples, so a 4:2:0
    // stream's offsets are halved relative to luma in both axes.
    let (crop_unit_width, crop_unit_height): (u32, u32) = match chroma_array_type {
        0 => (1, field_multiplier),
        1 => (2, 2 * field_multiplier),
        2 => (2, field_multiplier),
        _ => (1, field_multiplier),
    };

    // The crop is parsed but not applied: the bag carries the coded extent. It
    // is still read, because a crop that removes the whole picture is a
    // bitstream to refuse rather than one to publish an extent for.
    let cropped_width = crop_unit_width.saturating_mul(crop_left.saturating_add(crop_right));
    let cropped_height = crop_unit_height.saturating_mul(crop_top.saturating_add(crop_bottom));
    if cropped_width >= coded_width || cropped_height >= coded_height {
        return Err(WebRtcExtensionError::MalformedBitstream {
            what: format!(
                "the frame cropping offsets remove the whole picture: \
                 {coded_width}x{coded_height} coded, {cropped_width}x{cropped_height} cropped"
            ),
        });
    }

    Ok(SequenceParameterSet {
        width: coded_width,
        height: coded_height,
        color,
    })
}

fn one_more_than(value: u32) -> Result<u32> {
    value
        .checked_add(1)
        .ok_or_else(|| WebRtcExtensionError::MalformedBitstream {
            what: format!("a macroblock count of {value} overflows its own increment"),
        })
}

fn macroblocks_to_luma_samples(macroblocks: u32, field_multiplier: u32) -> Result<u32> {
    macroblocks
        .checked_mul(16)
        .and_then(|samples| samples.checked_mul(field_multiplier))
        .ok_or_else(|| WebRtcExtensionError::MalformedBitstream {
            what: format!("{macroblocks} macroblocks is not a picture extent this can express"),
        })
}

/// H.264 §E.1.1. Read only as far as the colour description — everything after
/// it is timing and bitstream-restriction data no bag key needs.
fn parse_video_usability_information(
    bits: &mut RawBitstreamReader<'_>,
) -> Result<Option<ColorDescription>> {
    if bits.read_flag()? {
        // Extended_SAR — §E.2.1 spells the aspect ratio out inline.
        const EXTENDED_SAMPLE_ASPECT_RATIO: u32 = 255;
        if bits.read_bits(8)? == EXTENDED_SAMPLE_ASPECT_RATIO {
            bits.skip_bits(32)?;
        }
    }
    if bits.read_flag()? {
        bits.skip_bits(1)?; // overscan_appropriate_flag
    }
    if !bits.read_flag()? {
        return Ok(None);
    }

    bits.skip_bits(3)?; // video_format
    let full_range = bits.read_flag()?;
    let mut color = ColorDescription {
        range: Some(if full_range { "full" } else { "limited" }),
        ..Default::default()
    };

    if bits.read_flag()? {
        color.primaries = primaries_from_h273_byte(bits.read_bits(8)? as u8);
        color.transfer = transfer_from_h273_byte(bits.read_bits(8)? as u8);
        color.matrix = matrix_from_h273_byte(bits.read_bits(8)? as u8);
    }

    Ok((!color.describes_nothing()).then_some(color))
}

fn skip_scaling_list(bits: &mut RawBitstreamReader<'_>, coefficient_count: usize) -> Result<()> {
    let mut last_scale: i64 = 8;
    let mut next_scale: i64 = 8;
    for _ in 0..coefficient_count {
        if next_scale != 0 {
            let delta = bits.read_signed_exp_golomb()?;
            next_scale = (last_scale + delta as i64 + 256) % 256;
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }
    Ok(())
}

/// H.264 §7.4.1.1: a `0x03` inserted after two zero bytes keeps a payload from
/// forming a start code, and is not part of the syntax the bit reader sees.
fn strip_emulation_prevention_bytes(payload: &[u8]) -> Vec<u8> {
    let mut raw_byte_sequence = Vec::with_capacity(payload.len());
    let mut zero_run = 0;
    for &byte in payload {
        if zero_run == 2 && byte == 0x03 {
            zero_run = 0;
            continue;
        }
        zero_run = if byte == 0 { zero_run + 1 } else { 0 };
        raw_byte_sequence.push(byte);
    }
    raw_byte_sequence
}

/// A big-endian bit reader over an RBSP, refusing a read past the end rather
/// than returning zeroes a parser would read as real syntax.
struct RawBitstreamReader<'a> {
    bytes: &'a [u8],
    bit_position: usize,
}

impl<'a> RawBitstreamReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    fn read_bits(&mut self, count: usize) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_one_bit()?);
        }
        Ok(value)
    }

    fn skip_bits(&mut self, count: usize) -> Result<()> {
        for _ in 0..count {
            self.read_one_bit()?;
        }
        Ok(())
    }

    fn read_flag(&mut self) -> Result<bool> {
        Ok(self.read_one_bit()? == 1)
    }

    fn read_one_bit(&mut self) -> Result<u8> {
        let byte = self.bytes.get(self.bit_position / 8).ok_or_else(|| {
            WebRtcExtensionError::MalformedBitstream {
                what: format!(
                    "the sequence parameter set ends after {} bits, mid-syntax",
                    self.bytes.len() * 8
                ),
            }
        })?;
        let bit = (byte >> (7 - self.bit_position % 8)) & 1;
        self.bit_position += 1;
        Ok(bit)
    }

    /// `ue(v)` — H.264 §9.1.
    fn read_unsigned_exp_golomb(&mut self) -> Result<u32> {
        let mut leading_zeroes = 0;
        while self.read_one_bit()? == 0 {
            leading_zeroes += 1;
            // 32 zeroes would overflow the u32 the syntax elements fit in, so
            // a longer run is corruption rather than a very large value.
            if leading_zeroes >= 32 {
                return Err(WebRtcExtensionError::MalformedBitstream {
                    what: "an Exp-Golomb code longer than 32 bits".to_owned(),
                });
            }
        }
        if leading_zeroes == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(leading_zeroes)?;
        Ok((1u32 << leading_zeroes) - 1 + suffix)
    }

    /// `se(v)` — H.264 §9.1.1.
    fn read_signed_exp_golomb(&mut self) -> Result<i32> {
        let unsigned = self.read_unsigned_exp_golomb()?;
        let magnitude = unsigned.div_ceil(2) as i32;
        Ok(if unsigned % 2 == 0 {
            -magnitude
        } else {
            magnitude
        })
    }
}

/// H.273 §8.1 → the bag's spelling. An enumerant the bag vocabulary does not
/// model reads as absent, which is the decode-direction posture the engine's
/// own VUI translation takes rather than fabricating a variant.
fn primaries_from_h273_byte(byte: u8) -> Option<&'static str> {
    Some(match byte {
        1 => "bt709",
        4 => "bt470_m",
        5 => "bt470_bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "film",
        9 => "bt2020",
        10 => "smpte428",
        11 => "smpte431",
        12 => "smpte432",
        22 => "ebu3213",
        _ => return None,
    })
}

/// H.273 §8.2 → the bag's spelling.
fn transfer_from_h273_byte(byte: u8) -> Option<&'static str> {
    Some(match byte {
        1 => "bt709",
        4 => "gamma22",
        5 => "gamma28",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "linear",
        9 => "log100",
        10 => "log100_sqrt10",
        11 => "xvycc",
        12 => "bt1361",
        13 => "srgb",
        14 => "bt2020_ten_bit",
        15 => "bt2020_twelve_bit",
        16 => "smpte2084",
        17 => "smpte428",
        18 => "arib_std_b67",
        _ => return None,
    })
}

/// H.273 §8.3 → the bag's spelling.
fn matrix_from_h273_byte(byte: u8) -> Option<&'static str> {
    Some(match byte {
        0 => "identity",
        1 => "bt709",
        4 => "fcc",
        5 => "bt470_bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "ycgco",
        9 => "bt2020_ncl",
        10 => "bt2020_cl",
        11 => "smpte2085",
        12 => "chroma_ncl",
        13 => "chroma_cl",
        14 => "ictcp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264_test_bitstreams::{
        RawBitstreamWriter, baseline_320x180, no_vui, vui_with_colour,
    };

    #[test]
    fn the_extent_is_the_coded_one_the_wire_contract_asks_for() {
        // 320x180 displayed, coded at 320x192 with the bottom 12 rows cropped.
        // The bag carries the coded extent, which is what `H264Encoder` writes
        // for the same stream and what `Mp4Sink` copies into `tkhd`.
        let parsed = parse_sequence_parameter_set(&baseline_320x180(no_vui)).unwrap();

        assert_eq!((parsed.width, parsed.height), (320, 192));
        assert_eq!(parsed.color, None);
    }

    #[test]
    fn a_colour_description_lands_in_the_bags_own_spelling() {
        let sps = baseline_320x180(vui_with_colour(1, 13, 1, 0));

        let parsed = parse_sequence_parameter_set(&sps).unwrap();

        assert_eq!(
            parsed.color,
            Some(ColorDescription {
                primaries: Some("bt709"),
                transfer: Some("srgb"),
                matrix: Some("bt709"),
                range: Some("limited"),
            })
        );
    }

    #[test]
    fn a_full_range_flag_alone_still_describes_the_range() {
        let sps = baseline_320x180(|writer| {
            writer
                .bit(1) // vui_parameters_present_flag
                .bit(0) // aspect_ratio_info_present_flag
                .bit(0) // overscan_info_present_flag
                .bit(1) // video_signal_type_present_flag
                .bits(5, 3)
                .bit(1) // video_full_range_flag
                .bit(0); // colour_description_present_flag
        });

        let parsed = parse_sequence_parameter_set(&sps).unwrap();

        assert_eq!(
            parsed.color,
            Some(ColorDescription {
                range: Some("full"),
                ..Default::default()
            })
        );
    }

    #[test]
    fn an_enumerant_the_bag_does_not_model_reads_as_absent_not_as_a_guess() {
        // 23 is past EBU3213, the last primaries value the vocabulary carries.
        let sps = baseline_320x180(vui_with_colour(23, 13, 1, 0));

        let parsed = parse_sequence_parameter_set(&sps).unwrap();

        let color = parsed.color.unwrap();
        assert_eq!(color.primaries, None);
        assert_eq!(color.transfer, Some("srgb"));
    }

    #[test]
    fn a_high_profile_sps_reads_past_the_chroma_format_it_carries() {
        let mut writer = RawBitstreamWriter::default();
        writer
            .bits(100, 8) // profile_idc — High, which carries chroma_format_idc
            .bits(0, 8)
            .bits(31, 8)
            .unsigned_exp_golomb(0) // seq_parameter_set_id
            .unsigned_exp_golomb(1) // chroma_format_idc — 4:2:0
            .unsigned_exp_golomb(0) // bit_depth_luma_minus8
            .unsigned_exp_golomb(0) // bit_depth_chroma_minus8
            .bit(0) // qpprime_y_zero_transform_bypass_flag
            .bit(0) // seq_scaling_matrix_present_flag
            .unsigned_exp_golomb(0) // log2_max_frame_num_minus4
            .unsigned_exp_golomb(2) // pic_order_cnt_type — neither branch reads more
            .unsigned_exp_golomb(1) // max_num_ref_frames
            .bit(0)
            .unsigned_exp_golomb(119) // 1920 across
            .unsigned_exp_golomb(67) // 1088 down
            .bit(1) // frame_mbs_only_flag
            .bit(1) // direct_8x8_inference_flag
            .bit(1) // frame_cropping_flag
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(4) // 8 luma rows off the bottom
            .bit(0); // vui_parameters_present_flag
        let sps = writer.finish(0x67);

        let parsed = parse_sequence_parameter_set(&sps).unwrap();

        // Coded at 1088 with 8 rows cropped for display; the bag says 1088.
        assert_eq!((parsed.width, parsed.height), (1920, 1088));
    }

    #[test]
    fn an_interlaced_stream_counts_its_map_units_twice() {
        let mut writer = RawBitstreamWriter::default();
        writer
            .bits(66, 8)
            .bits(0, 8)
            .bits(30, 8)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(2)
            .unsigned_exp_golomb(1)
            .bit(0)
            .unsigned_exp_golomb(39) // 640 across
            .unsigned_exp_golomb(14) // 15 map units, doubled to 480 rows
            .bit(0) // frame_mbs_only_flag — field coded
            .bit(0) // mb_adaptive_frame_field_flag
            .bit(1)
            .bit(0) // frame_cropping_flag
            .bit(0);
        let sps = writer.finish(0x67);

        let parsed = parse_sequence_parameter_set(&sps).unwrap();

        assert_eq!((parsed.width, parsed.height), (640, 480));
    }

    #[test]
    fn a_nal_unit_that_is_not_a_sequence_parameter_set_is_refused_by_type() {
        let refusal = parse_sequence_parameter_set(&[0x68, 0xCE, 0x3C, 0x80]).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedBitstream { .. }
        ));
    }

    #[test]
    fn a_truncated_sequence_parameter_set_is_refused_rather_than_read_as_zeroes() {
        let truncated = &baseline_320x180(no_vui)[..4];

        let refusal = parse_sequence_parameter_set(truncated).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedBitstream { .. }
        ));
    }

    #[test]
    fn a_crafted_macroblock_count_is_refused_rather_than_overflowing() {
        let mut writer = RawBitstreamWriter::default();
        writer
            .bits(66, 8)
            .bits(0, 8)
            .bits(30, 8)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(2)
            .unsigned_exp_golomb(1)
            .bit(0)
            .unsigned_exp_golomb(u32::MAX - 1) // pic_width_in_mbs_minus1
            .unsigned_exp_golomb(0)
            .bit(1)
            .bit(1)
            .bit(0)
            .bit(0);
        let sps = writer.finish(0x67);

        let refusal = parse_sequence_parameter_set(&sps).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedBitstream { .. }
        ));
    }

    #[test]
    fn cropping_that_removes_the_whole_picture_is_refused() {
        let mut writer = RawBitstreamWriter::default();
        writer
            .bits(66, 8)
            .bits(0, 8)
            .bits(30, 8)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(2)
            .unsigned_exp_golomb(1)
            .bit(0)
            .unsigned_exp_golomb(0) // one macroblock across
            .unsigned_exp_golomb(0) // one map unit down
            .bit(1)
            .bit(1)
            .bit(1) // frame_cropping_flag
            .unsigned_exp_golomb(8) // 16 luma columns off a 16-wide picture
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .unsigned_exp_golomb(0)
            .bit(0);
        let sps = writer.finish(0x67);

        let refusal = parse_sequence_parameter_set(&sps).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedBitstream { .. }
        ));
    }

    #[test]
    fn an_emulation_prevention_byte_is_not_part_of_the_syntax() {
        assert_eq!(
            strip_emulation_prevention_bytes(&[0x00, 0x00, 0x03, 0x01]),
            vec![0x00, 0x00, 0x01]
        );
        // Only after exactly two zeroes: a `03` anywhere else is real payload.
        assert_eq!(
            strip_emulation_prevention_bytes(&[0x00, 0x03, 0x00, 0x03]),
            vec![0x00, 0x03, 0x00, 0x03]
        );
        // The run restarts after each escape, so three zeroes and an escape
        // survive as three zeroes.
        assert_eq!(
            strip_emulation_prevention_bytes(&[0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x02]),
            vec![0x00, 0x00, 0x00, 0x00, 0x02]
        );
    }

    #[test]
    fn an_escaped_sequence_parameter_set_parses_to_the_same_extent() {
        let sps = baseline_320x180(no_vui);
        let mut escaped = vec![sps[0]];
        // Re-insert the escapes a real encoder would have written.
        let mut zero_run = 0;
        for &byte in &sps[1..] {
            if zero_run == 2 && byte <= 0x03 {
                escaped.push(0x03);
                zero_run = 0;
            }
            zero_run = if byte == 0 { zero_run + 1 } else { 0 };
            escaped.push(byte);
        }

        assert_eq!(
            parse_sequence_parameter_set(&escaped).unwrap(),
            parse_sequence_parameter_set(&sps).unwrap()
        );
    }
}
