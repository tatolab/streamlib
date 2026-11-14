// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A video file's first sync sample as one Annex-B access unit, its parameter sets ahead of it,
//! which the engine's H.264 and H.265 decoders take whole to make a poster frame.

use std::path::Path;

use crate::{AssetMediaFactsUnreadable, iso_base_media_file_facts, the_lowercase_extension_of};

/// The largest sync sample read for a poster frame; a keyframe past it is no picture Studio plays.
pub(crate) const LARGEST_SYNC_SAMPLE_READ_BYTES: usize = 64 * 1024 * 1024;

const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The codecs the engine decodes a poster frame from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PosterFrameVideoCodec {
    /// ITU-T H.264.
    H264,
    /// ITU-T H.265.
    H265,
}

impl PosterFrameVideoCodec {
    /// The codec's name as `media_facts.video.codec` spells it.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    /// The codec `wire_name` names.
    pub fn of_wire_name(wire_name: &str) -> Option<Self> {
        match wire_name {
            "h264" => Some(Self::H264),
            "h265" => Some(Self::H265),
            _ => None,
        }
    }
}

/// A video file's first sync sample, decodable on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstVideoSyncSampleAsAnnexB {
    /// The codec it is coded in.
    pub codec: PosterFrameVideoCodec,
    /// Its parameter sets, then its NAL units, each behind a four-byte start code.
    pub annex_b_access_unit: Vec<u8>,
}

/// A codec configuration record's parameter sets (`avcC`, `hvcC`, or Matroska's
/// `CodecPrivate`, which holds the same record) and the byte length of each NAL unit's length
/// prefix in the samples.
pub(crate) struct ParameterSetsOfACodecConfigurationRecord {
    pub(crate) parameter_set_nal_units: Vec<Vec<u8>>,
    pub(crate) nal_unit_length_prefix_bytes: usize,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn length_prefixed_nal_unit_at(bytes: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let nal_unit_len = usize::from(read_u16(bytes, offset)?);
    let nal_unit = bytes.get(offset + 2..offset + 2 + nal_unit_len)?;
    Some((nal_unit, offset + 2 + nal_unit_len))
}

/// The parameter sets of an `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §5.3.3.1).
pub(crate) fn parameter_sets_of_an_avc_configuration_record(
    record: &[u8],
) -> Option<ParameterSetsOfACodecConfigurationRecord> {
    let nal_unit_length_prefix_bytes = usize::from(record.get(4)? & 0b11) + 1;
    let sequence_parameter_set_count = record.get(5)? & 0b1_1111;
    let mut offset = 6;
    let mut parameter_set_nal_units = Vec::new();
    for _ in 0..sequence_parameter_set_count {
        let (nal_unit, next) = length_prefixed_nal_unit_at(record, offset)?;
        parameter_set_nal_units.push(nal_unit.to_vec());
        offset = next;
    }
    let picture_parameter_set_count = *record.get(offset)?;
    offset += 1;
    for _ in 0..picture_parameter_set_count {
        let (nal_unit, next) = length_prefixed_nal_unit_at(record, offset)?;
        parameter_set_nal_units.push(nal_unit.to_vec());
        offset = next;
    }
    Some(ParameterSetsOfACodecConfigurationRecord {
        parameter_set_nal_units,
        nal_unit_length_prefix_bytes,
    })
}

/// The parameter sets of an `HEVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §8.3.3.1).
pub(crate) fn parameter_sets_of_an_hevc_configuration_record(
    record: &[u8],
) -> Option<ParameterSetsOfACodecConfigurationRecord> {
    let nal_unit_length_prefix_bytes = usize::from(record.get(21)? & 0b11) + 1;
    let array_count = *record.get(22)?;
    let mut offset = 23;
    let mut parameter_set_nal_units = Vec::new();
    for _ in 0..array_count {
        let nal_unit_count = read_u16(record, offset + 1)?;
        offset += 3;
        for _ in 0..nal_unit_count {
            let (nal_unit, next) = length_prefixed_nal_unit_at(record, offset)?;
            parameter_set_nal_units.push(nal_unit.to_vec());
            offset = next;
        }
    }
    Some(ParameterSetsOfACodecConfigurationRecord {
        parameter_set_nal_units,
        nal_unit_length_prefix_bytes,
    })
}

/// `sample`, whose NAL units each sit behind a big-endian length of
/// `parameter_sets.nal_unit_length_prefix_bytes`, as one Annex-B access unit led by the
/// parameter sets.
pub(crate) fn annex_b_access_unit_of_a_length_prefixed_sample(
    parameter_sets: &ParameterSetsOfACodecConfigurationRecord,
    sample: &[u8],
) -> Option<Vec<u8>> {
    let mut access_unit = Vec::with_capacity(sample.len() + 256);
    for parameter_set in &parameter_sets.parameter_set_nal_units {
        access_unit.extend_from_slice(&ANNEX_B_START_CODE);
        access_unit.extend_from_slice(parameter_set);
    }
    let prefix_bytes = parameter_sets.nal_unit_length_prefix_bytes;
    let mut offset = 0;
    while offset < sample.len() {
        let length_prefix = sample.get(offset..offset + prefix_bytes)?;
        let nal_unit_len = length_prefix
            .iter()
            .fold(0usize, |len, byte| (len << 8) | usize::from(*byte));
        let nal_unit_start = offset + prefix_bytes;
        let nal_unit_end = nal_unit_start.checked_add(nal_unit_len)?;
        access_unit.extend_from_slice(&ANNEX_B_START_CODE);
        access_unit.extend_from_slice(sample.get(nal_unit_start..nal_unit_end)?);
        offset = nal_unit_end;
    }
    Some(access_unit)
}

/// The first sync sample of the video file at `video_file_path`, chosen by its extension.
pub fn first_video_sync_sample_of_a_video_file(
    video_file_path: &Path,
) -> Result<FirstVideoSyncSampleAsAnnexB, AssetMediaFactsUnreadable> {
    match the_lowercase_extension_of(video_file_path).as_str() {
        "mp4" | "m4v" | "mov" => {
            iso_base_media_file_facts::first_video_sync_sample_of_a_movie_file(video_file_path)
        }
        other => Err(AssetMediaFactsUnreadable::because(format!(
            "the runtime reads no poster frame from `.{other}` files"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_avc_record_and_a_four_byte_prefixed_sample_become_one_annex_b_access_unit() {
        let record = [
            1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 3, 0x67, 0xAA, 0xBB, 1, 0, 2, 0x68, 0xCC,
        ];
        let parameter_sets = parameter_sets_of_an_avc_configuration_record(&record).unwrap();
        assert_eq!(parameter_sets.nal_unit_length_prefix_bytes, 4);
        let sample = [0, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 1, 0x06];
        assert_eq!(
            annex_b_access_unit_of_a_length_prefixed_sample(&parameter_sets, &sample).unwrap(),
            [
                0, 0, 0, 1, 0x67, 0xAA, 0xBB, 0, 0, 0, 1, 0x68, 0xCC, 0, 0, 0, 1, 0x65, 0x01, 0, 0,
                0, 1, 0x06
            ]
        );
    }

    #[test]
    fn a_sample_whose_nal_unit_runs_past_its_end_is_refused() {
        let parameter_sets = ParameterSetsOfACodecConfigurationRecord {
            parameter_set_nal_units: Vec::new(),
            nal_unit_length_prefix_bytes: 4,
        };
        assert_eq!(
            annex_b_access_unit_of_a_length_prefixed_sample(
                &parameter_sets,
                &[0xFF, 0xFF, 0xFF, 0xFF, 0x65]
            ),
            None
        );
    }
}
