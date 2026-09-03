// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Annex-B access units into the length-prefixed samples an `avc1`/`hvc1`
//! track carries, and the parameter sets its sample entry is built from.
//!
//! ISO/IEC 14496-15 forbids in-band parameter sets under `avc1` and `hvc1`
//! — they belong in the sample entry's `avcC`/`hvcC` and nowhere else — so
//! the walk below sorts each access unit's NAL units into the two piles the
//! container wants them in: parameter sets out to the configuration record,
//! everything else 4-byte length-prefixed into the sample.
//!
//! The start-code scan is the engine's own
//! [`StartCodeFinder`], not a fourth splitter.

use streamlib::sdk::engine::video::nv_video_parser::byte_stream_parser::StartCodeFinder;

/// Which elementary stream an access unit's NAL headers are read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnexBNalHeaderGrammar {
    /// One-byte header; `nal_unit_type` is the low five bits.
    H264,
    /// Two-byte header; `nal_unit_type` is bits 1..7 of the first.
    H265,
}

/// H.264 `nal_unit_type` for a sequence parameter set (ITU-T H.264 §7.4.1).
const H264_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET: u8 = 7;
/// H.264 `nal_unit_type` for a picture parameter set.
const H264_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET: u8 = 8;
/// H.265 `nal_unit_type` for a video parameter set (ITU-T H.265 §7.4.2.2).
const H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET: u8 = 32;
/// H.265 `nal_unit_type` for a sequence parameter set.
const H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET: u8 = 33;
/// H.265 `nal_unit_type` for a picture parameter set.
const H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET: u8 = 34;

/// How `avc1` and `hvc1` prefix each NAL unit inside a sample. Four bytes is
/// what `avcC.length_size` / `hvcC.length_size_minus_one` below declare, and
/// the two must agree or every sample mis-parses.
pub const NAL_UNIT_LENGTH_PREFIX_BYTES: u8 = 4;

impl AnnexBNalHeaderGrammar {
    /// The `nal_unit_type` this grammar reads out of a NAL unit's header.
    fn nal_unit_type(self, nal_unit_bytes: &[u8]) -> Option<u8> {
        match self {
            Self::H264 => nal_unit_bytes.first().map(|header| header & 0x1F),
            Self::H265 => nal_unit_bytes.first().map(|header| (header >> 1) & 0x3F),
        }
    }

    /// Whether this NAL unit is a parameter set, which a sample must not
    /// carry and a sample entry must.
    fn is_parameter_set(self, nal_unit_type: u8) -> bool {
        match self {
            Self::H264 => matches!(
                nal_unit_type,
                H264_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET
                    | H264_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET
            ),
            Self::H265 => matches!(
                nal_unit_type,
                H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET
                    | H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET
                    | H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET
            ),
        }
    }
}

impl From<crate::encoded_video_frame::EncodedVideoCodec> for AnnexBNalHeaderGrammar {
    fn from(codec: crate::encoded_video_frame::EncodedVideoCodec) -> Self {
        match codec {
            crate::encoded_video_frame::EncodedVideoCodec::H264 => Self::H264,
            crate::encoded_video_frame::EncodedVideoCodec::H265 => Self::H265,
        }
    }
}

/// The parameter sets one access unit carried, in the piles `avcC` and
/// `hvcC` keep them in. Empty for a non-sync-point access unit, which the
/// engine's encoder prepends nothing to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParameterSetsFromAnnexBAccessUnit {
    /// H.265 only; `avcC` has no video parameter set.
    pub video_parameter_set_nal_units: Vec<Vec<u8>>,
    pub sequence_parameter_set_nal_units: Vec<Vec<u8>>,
    pub picture_parameter_set_nal_units: Vec<Vec<u8>>,
}

impl ParameterSetsFromAnnexBAccessUnit {
    /// Whether this access unit carried the sets a sample entry needs. An
    /// `hvcC` additionally wants a VPS, which [`Self::is_complete_for`]
    /// checks per grammar.
    pub fn is_complete_for(&self, grammar: AnnexBNalHeaderGrammar) -> bool {
        let has_sequence_and_picture_sets = !self.sequence_parameter_set_nal_units.is_empty()
            && !self.picture_parameter_set_nal_units.is_empty();
        match grammar {
            AnnexBNalHeaderGrammar::H264 => has_sequence_and_picture_sets,
            AnnexBNalHeaderGrammar::H265 => {
                has_sequence_and_picture_sets && !self.video_parameter_set_nal_units.is_empty()
            }
        }
    }
}

/// One access unit split the way the container wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LengthPrefixedSampleFromAnnexBAccessUnit {
    /// Every non-parameter-set NAL unit, each preceded by its
    /// [`NAL_UNIT_LENGTH_PREFIX_BYTES`]-byte big-endian length. This is the
    /// sample's bytes verbatim.
    pub length_prefixed_sample_bytes: Vec<u8>,
    /// The parameter sets that were stripped out on the way.
    pub parameter_sets: ParameterSetsFromAnnexBAccessUnit,
}

/// Split one Annex-B access unit into its NAL units, dropping the start
/// codes.
///
/// A leading byte run before the first start code is not a NAL unit and is
/// discarded — the encoder always emits a start code first, and a trailing
/// partial prefix would otherwise become a zero-length sample.
fn split_annex_b_access_unit_into_nal_units(annex_b_access_unit_bytes: &[u8]) -> Vec<&[u8]> {
    let mut start_code_finder = StartCodeFinder::new();
    let mut nal_unit_start_offsets = Vec::new();
    let mut offset = 0usize;

    while offset < annex_b_access_unit_bytes.len() {
        let search = start_code_finder.next_start_code(&annex_b_access_unit_bytes[offset..]);
        offset += search.bytes_consumed;
        if search.found {
            nal_unit_start_offsets.push(offset);
        }
    }

    let mut nal_units = Vec::with_capacity(nal_unit_start_offsets.len());
    for (index, &nal_unit_start) in nal_unit_start_offsets.iter().enumerate() {
        // A start code is `00 00 01`, and `00 00 00 01` is the same prefix
        // with a leading zero, so the byte before the next NAL's start code
        // may belong to that prefix rather than to this NAL.
        let nal_unit_end = match nal_unit_start_offsets.get(index + 1) {
            Some(&next_start) => {
                let three_byte_prefix_start = next_start.saturating_sub(3);
                if three_byte_prefix_start > nal_unit_start
                    && annex_b_access_unit_bytes[three_byte_prefix_start - 1] == 0x00
                {
                    three_byte_prefix_start - 1
                } else {
                    three_byte_prefix_start
                }
            }
            None => annex_b_access_unit_bytes.len(),
        };
        if nal_unit_end > nal_unit_start {
            nal_units.push(&annex_b_access_unit_bytes[nal_unit_start..nal_unit_end]);
        }
    }
    nal_units
}

/// Convert one Annex-B access unit into a length-prefixed sample plus the
/// parameter sets it carried.
pub fn length_prefix_annex_b_access_unit(
    annex_b_access_unit_bytes: &[u8],
    grammar: AnnexBNalHeaderGrammar,
) -> LengthPrefixedSampleFromAnnexBAccessUnit {
    let mut length_prefixed_sample_bytes = Vec::with_capacity(annex_b_access_unit_bytes.len());
    let mut parameter_sets = ParameterSetsFromAnnexBAccessUnit::default();

    for nal_unit in split_annex_b_access_unit_into_nal_units(annex_b_access_unit_bytes) {
        let Some(nal_unit_type) = grammar.nal_unit_type(nal_unit) else {
            continue;
        };
        if grammar.is_parameter_set(nal_unit_type) {
            let pile = match (grammar, nal_unit_type) {
                (AnnexBNalHeaderGrammar::H264, H264_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET)
                | (AnnexBNalHeaderGrammar::H265, H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET) => {
                    &mut parameter_sets.sequence_parameter_set_nal_units
                }
                (AnnexBNalHeaderGrammar::H264, H264_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET)
                | (AnnexBNalHeaderGrammar::H265, H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET) => {
                    &mut parameter_sets.picture_parameter_set_nal_units
                }
                _ => &mut parameter_sets.video_parameter_set_nal_units,
            };
            // A set repeated inside one access unit is the same set; the
            // encoder prepends the whole run at every sync point.
            if !pile.iter().any(|already| already == nal_unit) {
                pile.push(nal_unit.to_vec());
            }
            continue;
        }
        length_prefixed_sample_bytes.extend_from_slice(&(nal_unit.len() as u32).to_be_bytes());
        length_prefixed_sample_bytes.extend_from_slice(nal_unit);
    }

    LengthPrefixedSampleFromAnnexBAccessUnit {
        length_prefixed_sample_bytes,
        parameter_sets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for nal_unit in nal_units {
            bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            bytes.extend_from_slice(nal_unit);
        }
        bytes
    }

    #[test]
    fn h264_parameter_sets_leave_the_sample_and_land_in_their_piles() {
        let sequence_parameter_set: &[u8] = &[0x67, 0x42, 0xC0, 0x1E];
        let picture_parameter_set: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];
        let coded_slice: &[u8] = &[0x65, 0x88, 0x84, 0x00];

        let split = length_prefix_annex_b_access_unit(
            &annex_b(&[sequence_parameter_set, picture_parameter_set, coded_slice]),
            AnnexBNalHeaderGrammar::H264,
        );

        assert_eq!(
            split.parameter_sets.sequence_parameter_set_nal_units,
            vec![sequence_parameter_set.to_vec()]
        );
        assert_eq!(
            split.parameter_sets.picture_parameter_set_nal_units,
            vec![picture_parameter_set.to_vec()]
        );
        assert!(
            split
                .parameter_sets
                .video_parameter_set_nal_units
                .is_empty(),
            "H.264 has no video parameter set"
        );

        let mut expected_sample = Vec::new();
        expected_sample.extend_from_slice(&4u32.to_be_bytes());
        expected_sample.extend_from_slice(coded_slice);
        assert_eq!(
            split.length_prefixed_sample_bytes, expected_sample,
            "the sample is the slice alone, 4-byte length-prefixed"
        );
    }

    #[test]
    fn h265_sorts_all_three_parameter_set_kinds() {
        let video_parameter_set: &[u8] = &[0x40, 0x01, 0x0C, 0x01];
        let sequence_parameter_set: &[u8] = &[0x42, 0x01, 0x01, 0x01];
        let picture_parameter_set: &[u8] = &[0x44, 0x01, 0xC0, 0x73];
        let coded_slice: &[u8] = &[0x26, 0x01, 0xAF, 0x00];

        let split = length_prefix_annex_b_access_unit(
            &annex_b(&[
                video_parameter_set,
                sequence_parameter_set,
                picture_parameter_set,
                coded_slice,
            ]),
            AnnexBNalHeaderGrammar::H265,
        );

        assert_eq!(
            split.parameter_sets.video_parameter_set_nal_units,
            vec![video_parameter_set.to_vec()]
        );
        assert!(
            split
                .parameter_sets
                .is_complete_for(AnnexBNalHeaderGrammar::H265)
        );
        assert_eq!(
            &split.length_prefixed_sample_bytes[..4],
            &4u32.to_be_bytes(),
            "the slice is the only sample NAL and carries its own length"
        );
    }

    #[test]
    fn a_three_byte_start_code_splits_the_same_as_a_four_byte_one() {
        let mut three_byte_prefixed = Vec::new();
        three_byte_prefixed.extend_from_slice(&[0x00, 0x00, 0x01]);
        three_byte_prefixed.extend_from_slice(&[0x65, 0xAA, 0xBB]);
        three_byte_prefixed.extend_from_slice(&[0x00, 0x00, 0x01]);
        three_byte_prefixed.extend_from_slice(&[0x41, 0xCC, 0xDD]);

        let split =
            length_prefix_annex_b_access_unit(&three_byte_prefixed, AnnexBNalHeaderGrammar::H264);

        let mut expected = Vec::new();
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&[0x65, 0xAA, 0xBB]);
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&[0x41, 0xCC, 0xDD]);
        assert_eq!(split.length_prefixed_sample_bytes, expected);
    }

    #[test]
    fn a_non_sync_point_access_unit_carries_no_parameter_sets() {
        let split = length_prefix_annex_b_access_unit(
            &annex_b(&[&[0x41, 0x9A, 0x00]]),
            AnnexBNalHeaderGrammar::H264,
        );
        assert_eq!(
            split.parameter_sets,
            ParameterSetsFromAnnexBAccessUnit::default()
        );
        assert!(
            !split
                .parameter_sets
                .is_complete_for(AnnexBNalHeaderGrammar::H264),
            "a sample entry cannot be built from an access unit with no sets"
        );
    }

    #[test]
    fn every_nal_units_length_prefix_names_its_own_byte_count() {
        let short_nal: &[u8] = &[0x41, 0x01];
        let long_nal: &[u8] = &[0x41; 300];
        let split = length_prefix_annex_b_access_unit(
            &annex_b(&[short_nal, long_nal]),
            AnnexBNalHeaderGrammar::H264,
        );

        let bytes = &split.length_prefixed_sample_bytes;
        assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().unwrap()), 2);
        let second_prefix_at = 4 + 2;
        assert_eq!(
            u32::from_be_bytes(
                bytes[second_prefix_at..second_prefix_at + 4]
                    .try_into()
                    .unwrap()
            ),
            300
        );
        assert_eq!(bytes.len(), 4 + 2 + 4 + 300);
    }
}
