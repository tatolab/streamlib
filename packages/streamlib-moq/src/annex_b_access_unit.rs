// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Annex-B in, length-prefixed samples out — and back again.
//!
//! A bag carries one Annex-B access unit; an ISOBMFF sample carries the same
//! NAL units with four-byte big-endian lengths and the parameter sets lifted
//! out into the sample entry. Both directions live here so the round trip is
//! one test rather than two halves that can drift.
//!
//! Spelled in this wheel rather than imported: an extension links no engine
//! crate, and the engine's own copy reaches into its H.264 parser for the
//! start-code walk. The walk here is the same grammar with a local scanner.

/// Which elementary stream an access unit's NAL headers are read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnexBNalHeaderGrammar {
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
pub(crate) const H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET: u8 = 32;
/// H.265 `nal_unit_type` for a sequence parameter set.
pub(crate) const H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET: u8 = 33;
/// H.265 `nal_unit_type` for a picture parameter set.
pub(crate) const H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET: u8 = 34;

/// How `avc1` and `hvc1` prefix each NAL unit inside a sample. Four bytes is
/// what `avcC.length_size` / `hvcC.length_size_minus_one` declare, and the two
/// must agree or every sample mis-parses.
pub(crate) const NAL_UNIT_LENGTH_PREFIX_BYTES: u8 = 4;

/// The Annex-B start code each NAL unit carries outside a container. Three and
/// four bytes are both legal and a decoder reads either (ITU-T H.264 Annex B),
/// so the width here is free; it is stated once so the split and the join
/// cannot disagree about it.
pub(crate) const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

impl AnnexBNalHeaderGrammar {
    /// Which grammar a wire codec spelling reads by, or `None` for a codec
    /// this container path does not carry.
    pub(crate) fn of_wire_codec(codec: &str) -> Option<Self> {
        match codec {
            "h264" => Some(Self::H264),
            "h265" => Some(Self::H265),
            _ => None,
        }
    }

    /// How many bytes this grammar's NAL header occupies — one for H.264
    /// (ITU-T H.264 §7.3.1), two for H.265 (§7.3.1.2).
    pub(crate) fn nal_unit_header_bytes(self) -> usize {
        match self {
            Self::H264 => 1,
            Self::H265 => 2,
        }
    }

    fn nal_unit_type(self, nal_unit_bytes: &[u8]) -> Option<u8> {
        let header = *nal_unit_bytes.first()?;
        if nal_unit_bytes.len() < self.nal_unit_header_bytes() {
            return None;
        }
        Some(match self {
            Self::H264 => header & 0x1F,
            Self::H265 => (header >> 1) & 0x3F,
        })
    }

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

/// The parameter sets an access unit carried, piled by kind.
///
/// `PartialEq` is what detects a mid-stream parameter-set change: a CMAF init
/// segment is written once and describes the stream for its whole life, so a
/// second, different set is a reconfiguration this publisher cannot express.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParameterSetsFromAnnexBAccessUnit {
    pub(crate) video_parameter_set_nal_units: Vec<Vec<u8>>,
    pub(crate) sequence_parameter_set_nal_units: Vec<Vec<u8>>,
    pub(crate) picture_parameter_set_nal_units: Vec<Vec<u8>>,
}

impl ParameterSetsFromAnnexBAccessUnit {
    /// Whether these describe a whole track. H.264 needs an SPS and a PPS;
    /// H.265 needs a VPS beside them. This is the gate deciding when the init
    /// segment can first be minted.
    pub(crate) fn is_complete_for(&self, grammar: AnnexBNalHeaderGrammar) -> bool {
        let has_sequence_and_picture = !self.sequence_parameter_set_nal_units.is_empty()
            && !self.picture_parameter_set_nal_units.is_empty();
        match grammar {
            AnnexBNalHeaderGrammar::H264 => has_sequence_and_picture,
            AnnexBNalHeaderGrammar::H265 => {
                has_sequence_and_picture && !self.video_parameter_set_nal_units.is_empty()
            }
        }
    }
}

/// One access unit as a sample, plus the parameter sets lifted out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LengthPrefixedSampleFromAnnexBAccessUnit {
    pub(crate) length_prefixed_sample_bytes: Vec<u8>,
    pub(crate) parameter_sets: ParameterSetsFromAnnexBAccessUnit,
}

/// Where each NAL unit starts — the offset just past every `00 00 01`.
fn nal_unit_start_offsets(annex_b_access_unit_bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    while offset + 2 < annex_b_access_unit_bytes.len() {
        if annex_b_access_unit_bytes[offset] == 0x00
            && annex_b_access_unit_bytes[offset + 1] == 0x00
            && annex_b_access_unit_bytes[offset + 2] == 0x01
        {
            starts.push(offset + 3);
            offset += 3;
        } else {
            offset += 1;
        }
    }
    starts
}

/// Split one Annex-B access unit into its NAL units, start codes removed.
///
/// A leading byte run before the first start code is not a NAL unit and is
/// discarded — an encoder always emits a start code first, and a trailing
/// partial prefix would otherwise become a zero-length sample.
fn split_annex_b_access_unit_into_nal_units(annex_b_access_unit_bytes: &[u8]) -> Vec<&[u8]> {
    let starts = nal_unit_start_offsets(annex_b_access_unit_bytes);
    let mut nal_units = Vec::with_capacity(starts.len());
    for (index, &nal_unit_start) in starts.iter().enumerate() {
        // A start code is `00 00 01`, and `00 00 00 01` is the same prefix with
        // a leading zero, so the byte before the next NAL's start code may
        // belong to that prefix rather than to this NAL.
        let nal_unit_end = match starts.get(index + 1) {
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
pub(crate) fn length_prefix_annex_b_access_unit(
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
            if nal_unit.len() <= grammar.nal_unit_header_bytes() {
                // A parameter set that is only a header configures nothing.
                continue;
            }
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
            // A set repeated inside one access unit is the same set; an encoder
            // prepends the whole run at every sync point.
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

/// Why a sample's bytes are not the length-prefixed NAL units its sample entry
/// says they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SampleIsNotLengthPrefixedNalUnits {
    pub(crate) stopped_at_byte: usize,
    pub(crate) sample_bytes: usize,
}

impl std::fmt::Display for SampleIsNotLengthPrefixedNalUnits {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the sample's {} bytes stop mid-NAL at byte {}, so they are not four-byte \
             length-prefixed NAL units",
            self.sample_bytes, self.stopped_at_byte
        )
    }
}

/// Rebuild an Annex-B access unit from a length-prefixed sample, prepending the
/// parameter sets a decoder needs to enter the stream at it.
///
/// The inverse of [`length_prefix_annex_b_access_unit`], and the half a
/// subscriber uses: a CMAF track keeps its parameter sets in the sample entry,
/// while a bag's `bitstream` must carry them inline at every sync point.
pub(crate) fn annex_b_access_unit_from_length_prefixed_sample(
    length_prefixed_sample_bytes: &[u8],
    parameter_set_nal_units: &[Vec<u8>],
) -> std::result::Result<Vec<u8>, SampleIsNotLengthPrefixedNalUnits> {
    let mut annex_b = Vec::with_capacity(length_prefixed_sample_bytes.len() + 64);
    for parameter_set in parameter_set_nal_units {
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(parameter_set);
    }

    let prefix_bytes = NAL_UNIT_LENGTH_PREFIX_BYTES as usize;
    let mut offset = 0usize;
    while offset < length_prefixed_sample_bytes.len() {
        if offset + prefix_bytes > length_prefixed_sample_bytes.len() {
            return Err(SampleIsNotLengthPrefixedNalUnits {
                stopped_at_byte: offset,
                sample_bytes: length_prefixed_sample_bytes.len(),
            });
        }
        let length = u32::from_be_bytes([
            length_prefixed_sample_bytes[offset],
            length_prefixed_sample_bytes[offset + 1],
            length_prefixed_sample_bytes[offset + 2],
            length_prefixed_sample_bytes[offset + 3],
        ]) as usize;
        let nal_unit_start = offset + prefix_bytes;
        let nal_unit_end = nal_unit_start.saturating_add(length);
        if length == 0 || nal_unit_end > length_prefixed_sample_bytes.len() {
            return Err(SampleIsNotLengthPrefixedNalUnits {
                stopped_at_byte: offset,
                sample_bytes: length_prefixed_sample_bytes.len(),
            });
        }
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(&length_prefixed_sample_bytes[nal_unit_start..nal_unit_end]);
        offset = nal_unit_end;
    }
    Ok(annex_b)
}

/// Strip H.264/H.265 emulation-prevention bytes from an RBSP.
///
/// `00 00 03` inside a NAL payload encodes a literal `00 00`; the `03` is
/// removed before any fixed-offset read of the payload.
pub(crate) fn remove_emulation_prevention_bytes(nal_unit_payload: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(nal_unit_payload.len());
    let mut zero_run = 0usize;
    for &byte in nal_unit_payload {
        if zero_run == 2 && byte == 0x03 {
            zero_run = 0;
            continue;
        }
        if byte == 0x00 {
            zero_run += 1;
        } else {
            zero_run = 0;
        }
        rbsp.push(byte);
    }
    rbsp
}
