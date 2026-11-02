// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The WebCodecs / RFC 6381 codec string an H.264 access unit's SPS spells.

/// `nal_unit_type` of an H.264 sequence parameter set.
const H264_SEQUENCE_PARAMETER_SET_NAL_UNIT_TYPE: u8 = 7;

/// `avc1.PPCCLL` for the first SPS in `annex_b_access_unit`: profile_idc,
/// the constraint-set flags and level_idc, the bytes that open the SPS's
/// payload. `None` when the access unit carries no readable SPS.
pub fn avc1_codec_string_of_the_sequence_parameter_set(
    annex_b_access_unit: &[u8],
) -> Option<String> {
    let sequence_parameter_set = annex_b_nal_units(annex_b_access_unit).find(|nal_unit| {
        nal_unit
            .first()
            .is_some_and(|header| header & 0x1F == H264_SEQUENCE_PARAMETER_SET_NAL_UNIT_TYPE)
    })?;
    let [profile_idc, constraint_set_flags, level_idc] = *sequence_parameter_set.get(1..4)? else {
        return None;
    };
    Some(format!(
        "avc1.{profile_idc:02x}{constraint_set_flags:02x}{level_idc:02x}"
    ))
}

/// The NAL units of an Annex-B byte stream, start codes removed.
fn annex_b_nal_units(annex_b_bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start_code_ends = Vec::new();
    let mut index = 0;
    while index + 3 <= annex_b_bytes.len() {
        if annex_b_bytes[index] == 0
            && annex_b_bytes[index + 1] == 0
            && annex_b_bytes[index + 2] == 1
        {
            start_code_ends.push((index, index + 3));
            index += 3;
        } else {
            index += 1;
        }
    }
    let next_starts: Vec<usize> = start_code_ends
        .iter()
        .skip(1)
        .map(|&(start_code_begin, _)| {
            // A four-byte start code's leading zero belongs to it, not to the
            // NAL unit before.
            if start_code_begin > 0 && annex_b_bytes[start_code_begin - 1] == 0 {
                start_code_begin - 1
            } else {
                start_code_begin
            }
        })
        .chain(std::iter::once(annex_b_bytes.len()))
        .collect();
    start_code_ends
        .into_iter()
        .zip(next_starts)
        .map(move |((_, nal_unit_begin), nal_unit_end)| {
            &annex_b_bytes[nal_unit_begin..nal_unit_end]
        })
        .filter(|nal_unit| !nal_unit.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_high_profile_level_4_sps_spells_avc1_640028() {
        let access_unit = [
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0xAC, // SPS
            0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80, // PPS
            0, 0, 1, 0x65, 0x88, 0x84, // IDR slice
        ];
        assert_eq!(
            avc1_codec_string_of_the_sequence_parameter_set(&access_unit).as_deref(),
            Some("avc1.640028")
        );
    }

    #[test]
    fn an_access_unit_without_an_sps_spells_nothing() {
        let access_unit = [0, 0, 0, 1, 0x41, 0x9A, 0x02];
        assert_eq!(
            avc1_codec_string_of_the_sequence_parameter_set(&access_unit),
            None
        );
    }

    #[test]
    fn nal_units_split_on_three_and_four_byte_start_codes() {
        let annex_b = [0, 0, 0, 1, 0x67, 0x01, 0, 0, 1, 0x68, 0x02];
        let nal_units: Vec<&[u8]> = annex_b_nal_units(&annex_b).collect();
        assert_eq!(nal_units, [&[0x67, 0x01][..], &[0x68, 0x02][..]]);
    }
}
