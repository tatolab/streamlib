// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Annex-B NAL unit framing: the one reader both the encoder's parameter-set
//! check and the engine's decoder split a stream with.

use crate::Codec;

/// Why no decoder could enter a stream on these parameter-set bytes, or
/// `None` when they open one.
///
/// The bytes `vkGetEncodedVideoSessionParametersKHR` hands an encoder back
/// are implementation-defined, and an encoder that prepends them to its sync
/// points has made them the stream's only entry point: a decoder that cannot
/// find them configures no session and decodes nothing, however many slices
/// follow. This answers the question with the engine's own NAL reader — the
/// one that will actually be asked to find them — so the check and the reader
/// cannot disagree about what is framed correctly.
///
/// It asks for exactly what this decoder needs and no more. An SPS, because
/// the engine's decoder configures its session from one and refuses without the
/// dimensions it carries; a PPS, because a slice resolves its `pic_parameter_
/// set_id` against the parsed set and fails by name when it is absent. Not a
/// VPS: the H.265 path defaults every field it would have read, so demanding
/// one would refuse an encoder session whose stream this engine decodes.
pub fn why_no_decoder_could_enter_on_these_parameter_sets(
    parameter_set_bytes: &[u8],
    codec: Codec,
) -> Option<String> {
    let required_parameter_sets: &[(u8, &str)] = match codec {
        Codec::H264 => &[(7, "SPS"), (8, "PPS")],
        Codec::H265 => &[(33, "SPS"), (34, "PPS")],
    };
    // The reader's own floor: `feed` skips an H.265 NAL shorter than its
    // two-byte header, so a truncated one must not count as a parameter set
    // here either — the whole point is that this check and the reader agree.
    let nal_header_bytes = match codec {
        Codec::H264 => 1,
        Codec::H265 => 2,
    };
    let nal_unit_types: Vec<u8> = split_annex_b_nal_units(parameter_set_bytes)
        .iter()
        .filter(|nal_unit| nal_unit.len() >= nal_header_bytes)
        .map(|nal_unit| match codec {
            Codec::H264 => nal_unit[0] & 0x1F,
            Codec::H265 => (nal_unit[0] >> 1) & 0x3F,
        })
        .collect();

    let missing: Vec<&str> = required_parameter_sets
        .iter()
        .filter(|(nal_unit_type, _)| !nal_unit_types.contains(nal_unit_type))
        .map(|(_, name)| *name)
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "the engine's own NAL reader finds no {} in the {} byte(s) of parameter sets — it read \
         the Annex-B NAL unit types {:?}",
        missing.join(" and "),
        parameter_set_bytes.len(),
        nal_unit_types,
    ))
}

/// Split Annex-B bytes at their start codes into owned NAL units, each
/// without its trailing zero bytes.
pub fn split_annex_b_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nals = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;

    while i + 2 < data.len() {
        let is_start_code = if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            Some(4)
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            Some(3)
        } else {
            None
        };

        if let Some(sc_len) = is_start_code {
            if let Some(s) = start {
                push_nal_without_trailing_zero_bytes(&mut nals, &data[s..i]);
            }
            start = Some(i + sc_len);
            i += sc_len;
        } else {
            i += 1;
        }
    }

    if let Some(s) = start {
        if s < data.len() {
            push_nal_without_trailing_zero_bytes(&mut nals, &data[s..]);
        }
    }

    nals
}

/// Push one split-out NAL with its Annex-B `trailing_zero_8bits` removed.
/// A four-byte start code absorbs one leading zero, so a zero sitting
/// between a NAL and a `00 00 00 01` would otherwise stay attached to the
/// NAL — asymmetrically with the three-byte case, which absorbs it. Safe
/// to trim unconditionally: a well-formed NAL never ends in a raw `0x00`
/// (its last byte carries the `rbsp_stop_bit`, and `cabac_zero_words`
/// end in `0x03` after emulation prevention).
fn push_nal_without_trailing_zero_bytes(nals: &mut Vec<Vec<u8>>, nal_bytes: &[u8]) {
    let trailing_zero_count = nal_bytes
        .iter()
        .rev()
        .take_while(|byte| **byte == 0)
        .count();
    let payload = &nal_bytes[..nal_bytes.len() - trailing_zero_count];
    if !payload.is_empty() {
        nals.push(payload.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // SimpleDecoder NAL splitting (pure logic, no GPU)
    // ------------------------------------------------------------------

    #[test]
    fn test_split_nal_units_empty() {
        let nals = split_annex_b_nal_units(&[]);
        assert!(nals.is_empty());
    }

    #[test]
    fn test_split_nal_units_single_3byte_sc() {
        // 00 00 01 <NAL data>
        let data = [0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E];
        let nals = split_annex_b_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], &[0x67, 0x42, 0x00, 0x1E]);
    }

    #[test]
    fn test_split_nal_units_single_4byte_sc() {
        let data = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42];
        let nals = split_annex_b_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], &[0x67, 0x42]);
    }

    #[test]
    fn test_split_nal_units_multiple() {
        // SPS + PPS + IDR
        let mut data = Vec::new();
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SC
        data.extend_from_slice(&[0x67, 0x42, 0x00, 0x1E]); // SPS
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SC
        data.extend_from_slice(&[0x68, 0xCE, 0x38, 0x80]); // PPS
        data.extend_from_slice(&[0x00, 0x00, 0x01]); // 3-byte SC
        data.extend_from_slice(&[0x65, 0x88, 0x84]); // IDR

        let nals = split_annex_b_nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0][0] & 0x1F, 7); // SPS
        assert_eq!(nals[1][0] & 0x1F, 8); // PPS
        assert_eq!(nals[2][0] & 0x1F, 5); // IDR
    }

    #[test]
    fn test_split_nal_units_no_start_code() {
        let data = [0x67, 0x42, 0x00, 0x1E];
        let nals = split_annex_b_nal_units(&data);
        assert!(nals.is_empty());
    }

    // ------------------------------------------------------------------
    // The parameter sets a sync point ships with
    // ------------------------------------------------------------------
    //
    // An encoder that prepends `vkGetEncodedVideoSessionParametersKHR` bytes to
    // its sync points has made them the stream's only entry point. If the engine's
    // NAL reader cannot find an SPS and a PPS in them, every slice that follows is
    // skipped and the run ends having decoded nothing. These lock the reader and the check that guards it together.

    /// One Annex-B NAL: `start_code_length` bytes of start code, then `bytes`.
    fn annex_b_nal_unit(start_code_length: usize, bytes: &[u8]) -> Vec<u8> {
        let mut nal_unit = if start_code_length == 4 {
            vec![0x00, 0x00, 0x00, 0x01]
        } else {
            vec![0x00, 0x00, 0x01]
        };
        nal_unit.extend_from_slice(bytes);
        nal_unit
    }

    /// The H.264 parameter sets a driver hands back, Annex-B framed.
    fn h264_annex_b_parameter_sets() -> Vec<u8> {
        let mut parameter_sets = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E]);
        parameter_sets.extend_from_slice(&annex_b_nal_unit(4, &[0x68, 0xCE, 0x38, 0x80]));
        parameter_sets
    }

    #[test]
    fn annex_b_framed_parameter_sets_open_a_decodable_stream() {
        assert_eq!(
            why_no_decoder_could_enter_on_these_parameter_sets(
                &h264_annex_b_parameter_sets(),
                Codec::H264,
            ),
            None
        );
    }

    /// A driver is free to pick either start-code length, and may mix them
    /// within one blob — neither framing may be read as missing parameter sets.
    #[test]
    fn either_start_code_length_frames_parameter_sets_the_reader_accepts() {
        let mut mixed_framing = annex_b_nal_unit(3, &[0x67, 0x42, 0x00, 0x1E]);
        mixed_framing.extend_from_slice(&annex_b_nal_unit(4, &[0x68, 0xCE, 0x38, 0x80]));
        assert_eq!(
            why_no_decoder_could_enter_on_these_parameter_sets(&mixed_framing, Codec::H264,),
            None
        );
    }

    /// Parameter sets that carry no start code are
    /// dropped whole by the reader, because everything before the first start
    /// code is not a NAL unit. Silently shipping those is what produces an
    /// encoder at ~50 frames and a decoder at 0.
    #[test]
    fn parameter_sets_carrying_no_start_code_are_refused_rather_than_silently_dropped() {
        let unframed_parameter_sets = [0x67, 0x42, 0x00, 0x1E, 0x68, 0xCE, 0x38, 0x80];
        assert!(
            split_annex_b_nal_units(&unframed_parameter_sets).is_empty(),
            "the reader finds no NAL unit at all in unframed bytes — which is why they must be \
             refused before they reach a stream"
        );
        let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
            &unframed_parameter_sets,
            Codec::H264,
        )
        .expect("unframed parameter sets must be refused");
        assert!(
            refusal.contains("SPS") && refusal.contains("PPS"),
            "the refusal names what the decoder needed and did not find: {refusal}"
        );
    }

    /// Empty parameter sets are a refusal, not a default: they would make a
    /// headerless stream.
    #[test]
    fn empty_parameter_sets_are_refused_naming_what_a_decoder_needed() {
        let refusal = why_no_decoder_could_enter_on_these_parameter_sets(&[], Codec::H264)
            .expect("empty parameter sets must be refused");
        assert!(
            refusal.contains("SPS") && refusal.contains("PPS"),
            "{refusal}"
        );
    }

    /// Parameter sets that carry only half of what a decoder configures from are
    /// refused naming the half that is missing, not accepted for the half present.
    #[test]
    fn parameter_sets_missing_one_required_set_are_refused_naming_only_that_one() {
        let sps_only = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E]);
        let refusal = why_no_decoder_could_enter_on_these_parameter_sets(&sps_only, Codec::H264)
            .expect("an SPS with no PPS must be refused");
        assert!(refusal.contains("PPS"), "{refusal}");
        assert!(
            !refusal.contains("SPS"),
            "the SPS was there; the refusal must not name it: {refusal}"
        );

        // H.265 reads its NAL types out of a different bit field, so a header
        // framed for H.264 satisfies none of them.
        let h265_refusal = why_no_decoder_could_enter_on_these_parameter_sets(
            &h264_annex_b_parameter_sets(),
            Codec::H265,
        )
        .expect("H.264 parameter sets open no H.265 stream");
        assert!(
            h265_refusal.contains("SPS") && h265_refusal.contains("PPS"),
            "{h265_refusal}"
        );
    }

    /// The check asks for what this engine's decoder needs and no more. Its
    /// H.265 path configures the session from the SPS and defaults every field a
    /// VPS would have carried, so a driver whose parameter-set blob omits the VPS
    /// must still mint — refusing it would reject a stream this engine decodes.
    #[test]
    fn h265_parameter_sets_carrying_no_vps_still_open_a_decodable_stream() {
        let mut sps_and_pps = annex_b_nal_unit(4, &[33 << 1, 0x00, 0x00, 0x03]);
        sps_and_pps.extend_from_slice(&annex_b_nal_unit(4, &[34 << 1, 0x00, 0xC1]));
        assert_eq!(
            why_no_decoder_could_enter_on_these_parameter_sets(&sps_and_pps, Codec::H265,),
            None
        );

        let vps_only = annex_b_nal_unit(4, &[32 << 1, 0x00, 0x0C]);
        let refusal = why_no_decoder_could_enter_on_these_parameter_sets(&vps_only, Codec::H265)
            .expect("a VPS alone opens nothing");
        assert!(
            refusal.contains("SPS") && refusal.contains("PPS") && !refusal.contains("VPS"),
            "{refusal}"
        );
    }

    /// What actually ships: the parameter sets concatenated ahead of the IDR
    /// access unit, in one bag. The reader must recover all three NAL units from
    /// it, so the session configures on the same bag it then decodes.
    #[test]
    fn a_sync_point_access_unit_reads_back_as_its_parameter_sets_then_its_idr() {
        let mut sync_point_access_unit = h264_annex_b_parameter_sets();
        sync_point_access_unit.extend_from_slice(&annex_b_nal_unit(4, &[0x65, 0x88, 0x84, 0x00]));

        let nal_unit_types: Vec<u8> = split_annex_b_nal_units(&sync_point_access_unit)
            .iter()
            .map(|nal_unit| nal_unit[0] & 0x1F)
            .collect();
        assert_eq!(nal_unit_types, vec![7, 8, 5]);
    }

    /// H.264 permits trailing zero bytes after a NAL unit's payload, and a
    /// driver's parameter sets may carry them. Those bytes belong to no NAL
    /// unit: the reader must not hand them on as payload, or the parameter set
    /// the decoder parses is not the one the encoder wrote. Both start-code
    /// widths, because a four-byte code absorbs one leading zero itself.
    #[test]
    fn trailing_zero_bytes_between_nal_units_stay_out_of_the_payload() {
        for next_start_code_length in [3usize, 4] {
            let mut with_trailing_zero = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E, 0x00]);
            with_trailing_zero.extend_from_slice(&annex_b_nal_unit(
                next_start_code_length,
                &[0x68, 0xCE, 0x38, 0x80],
            ));

            let nal_units = split_annex_b_nal_units(&with_trailing_zero);
            assert_eq!(
                nal_units.len(),
                2,
                "next start code {next_start_code_length} bytes"
            );
            assert_eq!(nal_units[0], vec![0x67, 0x42, 0x00, 0x1E]);
            assert_eq!(nal_units[1], vec![0x68, 0xCE, 0x38, 0x80]);
        }

        // Trailing zeros ahead of no further start code — the buffer's end —
        // are filler too, and a run that is nothing but zeros is no NAL at all.
        let trailing_at_end = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E, 0x00, 0x00]);
        assert_eq!(
            split_annex_b_nal_units(&trailing_at_end),
            vec![vec![0x67, 0x42, 0x00, 0x1E]]
        );
    }

    /// The reader skips an H.265 NAL shorter than its two-byte header, so a
    /// truncated one must not satisfy the parameter-set check either — the
    /// check exists to agree with the reader.
    #[test]
    fn a_truncated_h265_nal_header_is_not_counted_as_a_parameter_set() {
        let mut one_byte_parameter_sets = annex_b_nal_unit(4, &[33 << 1]);
        one_byte_parameter_sets.extend_from_slice(&annex_b_nal_unit(4, &[34 << 1]));
        let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
            &one_byte_parameter_sets,
            Codec::H265,
        )
        .expect("one-byte NAL headers configure nothing");
        assert!(
            refusal.contains("SPS") && refusal.contains("PPS"),
            "{refusal}"
        );
    }
}
