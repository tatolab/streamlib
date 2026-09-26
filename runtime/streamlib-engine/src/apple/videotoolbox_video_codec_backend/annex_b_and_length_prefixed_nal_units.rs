// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The seam's Annex-B wire and VideoToolbox's length-prefixed NAL units,
//! converted in both directions.
//!
//! VideoToolbox keeps parameter sets in the format description and never in
//! a sample; the wire carries them in band, in front of every sync point. So
//! the walk into VideoToolbox sorts parameter sets out of the access unit, and
//! the walk back puts the format description's in front again.

use crate::core::context::VideoCodecElementaryStream;
use crate::core::{Error, Result};

/// The start code this arm writes in front of every NAL unit. Three and four
/// bytes are both legal (ITU-T H.264 Annex B); four is what the Linux arm's
/// access units open with.
const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// How wide the big-endian length in front of each NAL unit of a sample this
/// arm builds is. The format description is created with the same width, so
/// the two cannot disagree.
pub(super) const LENGTH_PREFIX_BYTES_OF_SAMPLES_HANDED_TO_VIDEOTOOLBOX: usize = 4;

/// H.264 `nal_unit_type`s of the parameter sets (ITU-T H.264 §7.4.1).
const H264_SEQUENCE_PARAMETER_SET: u8 = 7;
const H264_PICTURE_PARAMETER_SET: u8 = 8;
/// H.265 `nal_unit_type`s of the parameter sets (ITU-T H.265 §7.4.2.2).
const H265_VIDEO_PARAMETER_SET: u8 = 32;
const H265_SEQUENCE_PARAMETER_SET: u8 = 33;
const H265_PICTURE_PARAMETER_SET: u8 = 34;

/// Every NAL unit of an Annex-B access unit, in stream order, start codes and
/// trailing zero bytes removed.
pub(super) fn nal_units_of_annex_b_access_unit(annex_b_access_unit_bytes: &[u8]) -> Vec<&[u8]> {
    let mut nal_units = Vec::new();
    let mut current_nal_unit_start: Option<usize> = None;
    let mut index = 0;
    while index + 3 <= annex_b_access_unit_bytes.len() {
        if annex_b_access_unit_bytes[index..index + 3] == [0x00, 0x00, 0x01] {
            if let Some(start) = current_nal_unit_start {
                push_nal_unit_without_trailing_zeros(
                    &mut nal_units,
                    &annex_b_access_unit_bytes[start..index],
                );
            }
            index += 3;
            current_nal_unit_start = Some(index);
        } else {
            index += 1;
        }
    }
    if let Some(start) = current_nal_unit_start {
        push_nal_unit_without_trailing_zeros(&mut nal_units, &annex_b_access_unit_bytes[start..]);
    }
    nal_units
}

/// A NAL unit never ends in a zero byte, so trailing zeros are the next start
/// code's leading byte or `trailing_zero_8bits` padding.
fn push_nal_unit_without_trailing_zeros<'a>(nal_units: &mut Vec<&'a [u8]>, bytes: &'a [u8]) {
    let meaningful_length = bytes
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    if meaningful_length > 0 {
        nal_units.push(&bytes[..meaningful_length]);
    }
}

/// The NAL unit's `nal_unit_type`, read by `elementary_stream`'s header
/// grammar.
fn nal_unit_type(elementary_stream: VideoCodecElementaryStream, nal_unit: &[u8]) -> Option<u8> {
    let first_header_byte = *nal_unit.first()?;
    Some(match elementary_stream {
        VideoCodecElementaryStream::H264 => first_header_byte & 0x1F,
        VideoCodecElementaryStream::H265 => (first_header_byte >> 1) & 0x3F,
    })
}

/// Whether the NAL unit is a parameter set, which lives in the format
/// description rather than a sample.
fn is_parameter_set(elementary_stream: VideoCodecElementaryStream, nal_unit: &[u8]) -> bool {
    let parameter_set_types: &[u8] = match elementary_stream {
        VideoCodecElementaryStream::H264 => {
            &[H264_SEQUENCE_PARAMETER_SET, H264_PICTURE_PARAMETER_SET]
        }
        VideoCodecElementaryStream::H265 => &[
            H265_VIDEO_PARAMETER_SET,
            H265_SEQUENCE_PARAMETER_SET,
            H265_PICTURE_PARAMETER_SET,
        ],
    };
    nal_unit_type(elementary_stream, nal_unit)
        .is_some_and(|nal_type| parameter_set_types.contains(&nal_type))
}

/// Whether `parameter_sets` is enough to build a format description from:
/// SPS and PPS for H.264, VPS, SPS and PPS for H.265.
pub(super) fn parameter_sets_are_complete(
    elementary_stream: VideoCodecElementaryStream,
    parameter_sets: &[Vec<u8>],
) -> bool {
    let required_types: &[u8] = match elementary_stream {
        VideoCodecElementaryStream::H264 => {
            &[H264_SEQUENCE_PARAMETER_SET, H264_PICTURE_PARAMETER_SET]
        }
        VideoCodecElementaryStream::H265 => &[
            H265_VIDEO_PARAMETER_SET,
            H265_SEQUENCE_PARAMETER_SET,
            H265_PICTURE_PARAMETER_SET,
        ],
    };
    required_types.iter().all(|required_type| {
        parameter_sets.iter().any(|parameter_set| {
            nal_unit_type(elementary_stream, parameter_set) == Some(*required_type)
        })
    })
}

/// One Annex-B access unit sorted into what VideoToolbox takes it as.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AnnexBAccessUnitSortedForVideoToolbox {
    /// The in-band parameter sets, in stream order.
    pub(super) parameter_sets: Vec<Vec<u8>>,
    /// Every other NAL unit, each behind a
    /// [`LENGTH_PREFIX_BYTES_OF_SAMPLES_HANDED_TO_VIDEOTOOLBOX`]-byte length.
    /// Empty when the access unit carried nothing but parameter sets.
    pub(super) length_prefixed_sample: Vec<u8>,
}

/// Sort an Annex-B access unit into its parameter sets and the length-prefixed
/// sample VideoToolbox decodes.
pub(super) fn sort_annex_b_access_unit_for_videotoolbox(
    elementary_stream: VideoCodecElementaryStream,
    annex_b_access_unit_bytes: &[u8],
) -> Result<AnnexBAccessUnitSortedForVideoToolbox> {
    let mut sorted = AnnexBAccessUnitSortedForVideoToolbox {
        parameter_sets: Vec::new(),
        length_prefixed_sample: Vec::with_capacity(annex_b_access_unit_bytes.len()),
    };
    for nal_unit in nal_units_of_annex_b_access_unit(annex_b_access_unit_bytes) {
        if is_parameter_set(elementary_stream, nal_unit) {
            sorted.parameter_sets.push(nal_unit.to_vec());
            continue;
        }
        let nal_unit_length = u32::try_from(nal_unit.len()).map_err(|_| {
            Error::Configuration(format!(
                "a {} byte NAL unit does not fit a 4-byte length prefix",
                nal_unit.len()
            ))
        })?;
        sorted
            .length_prefixed_sample
            .extend_from_slice(&nal_unit_length.to_be_bytes());
        sorted.length_prefixed_sample.extend_from_slice(nal_unit);
    }
    Ok(sorted)
}

/// The Annex-B access unit a length-prefixed sample is, with
/// `parameter_sets_in_front` written ahead of its first NAL unit.
pub(super) fn annex_b_access_unit_from_length_prefixed_sample(
    parameter_sets_in_front: &[&[u8]],
    length_prefixed_sample: &[u8],
    length_prefix_bytes: usize,
) -> Result<Vec<u8>> {
    if !(1..=4).contains(&length_prefix_bytes) {
        return Err(Error::Configuration(format!(
            "a {length_prefix_bytes}-byte NAL unit length prefix is none ISO/IEC 14496-15 allows"
        )));
    }
    let mut annex_b_access_unit_bytes = Vec::with_capacity(
        length_prefixed_sample.len()
            + parameter_sets_in_front
                .iter()
                .map(|parameter_set| parameter_set.len() + ANNEX_B_START_CODE.len())
                .sum::<usize>(),
    );
    for parameter_set in parameter_sets_in_front {
        annex_b_access_unit_bytes.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b_access_unit_bytes.extend_from_slice(parameter_set);
    }
    let mut remaining = length_prefixed_sample;
    while !remaining.is_empty() {
        let (length_bytes, after_length) = remaining
            .split_at_checked(length_prefix_bytes)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "a length-prefixed sample ends {} byte(s) into a {length_prefix_bytes}-byte \
                     length",
                    remaining.len()
                ))
            })?;
        let nal_unit_length = length_bytes
            .iter()
            .fold(0usize, |length, &byte| (length << 8) | usize::from(byte));
        let (nal_unit, after_nal_unit) = after_length
            .split_at_checked(nal_unit_length)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "a length-prefixed sample names a {nal_unit_length} byte NAL unit with {} \
                     byte(s) left",
                    after_length.len()
                ))
            })?;
        annex_b_access_unit_bytes.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b_access_unit_bytes.extend_from_slice(nal_unit);
        remaining = after_nal_unit;
    }
    Ok(annex_b_access_unit_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_SPS: &[u8] = &[0x67, 0x64, 0x00, 0x1F, 0xAC];
    const H264_PPS: &[u8] = &[0x68, 0xEE, 0x3C, 0x80];
    const H264_IDR_SLICE: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x33];
    const H265_VPS: &[u8] = &[0x40, 0x01, 0x0C];
    const H265_SPS: &[u8] = &[0x42, 0x01, 0x01];
    const H265_PPS: &[u8] = &[0x44, 0x01, 0xC1];
    const H265_IDR_SLICE: &[u8] = &[0x26, 0x01, 0xAF];

    fn annex_b(start_code: &[u8], nal_units: &[&[u8]]) -> Vec<u8> {
        nal_units
            .iter()
            .flat_map(|nal_unit| start_code.iter().chain(nal_unit.iter()).copied())
            .collect()
    }

    #[test]
    fn both_start_code_widths_split_into_the_same_nal_units() {
        let nal_units = [H264_SPS, H264_PPS, H264_IDR_SLICE];
        for start_code in [&[0u8, 0, 1][..], &[0, 0, 0, 1]] {
            assert_eq!(
                nal_units_of_annex_b_access_unit(&annex_b(start_code, &nal_units)),
                nal_units
            );
        }
    }

    #[test]
    fn trailing_zero_padding_is_not_part_of_a_nal_unit() {
        let mut padded = annex_b(&ANNEX_B_START_CODE, &[H264_SPS]);
        padded.extend_from_slice(&[0, 0]);
        padded.extend_from_slice(&annex_b(&ANNEX_B_START_CODE, &[H264_PPS]));
        padded.push(0);
        assert_eq!(
            nal_units_of_annex_b_access_unit(&padded),
            [H264_SPS, H264_PPS]
        );
    }

    #[test]
    fn bytes_before_the_first_start_code_are_not_a_nal_unit() {
        let mut leading_garbage = vec![0x12, 0x34];
        leading_garbage.extend_from_slice(&annex_b(&ANNEX_B_START_CODE, &[H264_IDR_SLICE]));
        assert_eq!(
            nal_units_of_annex_b_access_unit(&leading_garbage),
            [H264_IDR_SLICE]
        );
    }

    #[test]
    fn an_h264_sync_point_sorts_its_parameter_sets_out_of_the_sample() {
        let sorted = sort_annex_b_access_unit_for_videotoolbox(
            VideoCodecElementaryStream::H264,
            &annex_b(&ANNEX_B_START_CODE, &[H264_SPS, H264_PPS, H264_IDR_SLICE]),
        )
        .unwrap();
        assert_eq!(sorted.parameter_sets, [H264_SPS, H264_PPS]);
        let mut expected_sample = (H264_IDR_SLICE.len() as u32).to_be_bytes().to_vec();
        expected_sample.extend_from_slice(H264_IDR_SLICE);
        assert_eq!(sorted.length_prefixed_sample, expected_sample);
    }

    #[test]
    fn an_h265_sync_point_sorts_all_three_parameter_sets_out() {
        let sorted = sort_annex_b_access_unit_for_videotoolbox(
            VideoCodecElementaryStream::H265,
            &annex_b(
                &ANNEX_B_START_CODE,
                &[H265_VPS, H265_SPS, H265_PPS, H265_IDR_SLICE],
            ),
        )
        .unwrap();
        assert_eq!(sorted.parameter_sets, [H265_VPS, H265_SPS, H265_PPS]);
        assert!(parameter_sets_are_complete(
            VideoCodecElementaryStream::H265,
            &sorted.parameter_sets
        ));
    }

    #[test]
    fn a_parameter_set_list_missing_one_is_incomplete() {
        assert!(!parameter_sets_are_complete(
            VideoCodecElementaryStream::H264,
            &[H264_SPS.to_vec()]
        ));
        assert!(!parameter_sets_are_complete(
            VideoCodecElementaryStream::H265,
            &[H265_SPS.to_vec(), H265_PPS.to_vec()]
        ));
    }

    #[test]
    fn a_sorted_sync_point_rebuilds_into_the_access_unit_it_came_from() {
        let access_unit = annex_b(&ANNEX_B_START_CODE, &[H264_SPS, H264_PPS, H264_IDR_SLICE]);
        let sorted = sort_annex_b_access_unit_for_videotoolbox(
            VideoCodecElementaryStream::H264,
            &access_unit,
        )
        .unwrap();
        let parameter_sets: Vec<&[u8]> = sorted.parameter_sets.iter().map(Vec::as_slice).collect();
        assert_eq!(
            annex_b_access_unit_from_length_prefixed_sample(
                &parameter_sets,
                &sorted.length_prefixed_sample,
                LENGTH_PREFIX_BYTES_OF_SAMPLES_HANDED_TO_VIDEOTOOLBOX,
            )
            .unwrap(),
            access_unit
        );
    }

    #[test]
    fn a_two_byte_length_prefix_reads_as_wide_as_it_is() {
        let mut sample = vec![0x00, H264_IDR_SLICE.len() as u8];
        sample.extend_from_slice(H264_IDR_SLICE);
        assert_eq!(
            annex_b_access_unit_from_length_prefixed_sample(&[], &sample, 2).unwrap(),
            annex_b(&ANNEX_B_START_CODE, &[H264_IDR_SLICE])
        );
    }

    #[test]
    fn a_sample_cut_short_inside_a_nal_unit_is_refused() {
        let mut sample = 10u32.to_be_bytes().to_vec();
        sample.extend_from_slice(&[0x65, 0x88]);
        assert!(annex_b_access_unit_from_length_prefixed_sample(&[], &sample, 4).is_err());
    }

    #[test]
    fn a_length_prefix_wider_than_four_bytes_is_refused() {
        assert!(annex_b_access_unit_from_length_prefixed_sample(&[], &[], 5).is_err());
    }
}
