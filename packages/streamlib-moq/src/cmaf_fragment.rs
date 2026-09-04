// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One MoQ object on the `cmaf` container: a `moof` atom immediately followed
//! by its `mdat`, byte-concatenated with nothing between and nothing around.
//!
//! Both directions live here because the `data_offset` a publisher writes and
//! the one a subscriber resolves are the same arithmetic read twice, and a
//! disagreement between them is silent — a player shows the wrong bytes rather
//! than failing.

use bytes::Bytes;
use mp4_atom::{Atom, Decode, Encode, Header, Mdat, Mfhd, Moof, Tfdt, Tfhd, Traf, Trun, TrunEntry};

use crate::error::{MoqExtensionError, Result};

/// The track id this wheel's publisher passes to [`build_cmaf_fragment`].
///
/// The init segment describes exactly one track, and the reference relay names
/// a media track after its `tkhd.track_id` (`1.m4s`), so this number is part of
/// the track name a subscriber asks for and not an internal choice. It is not a
/// guarantee about the bytes on the wire: [`build_cmaf_fragment`] writes the
/// track id it is handed, and [`read_cmaf_fragment`] lifts samples out of a
/// fragment whatever track id it carries.
pub(crate) const CMAF_FRAGMENT_TRACK_ID: u32 = 1;

/// What a refusal on this path calls the container it was reading or writing.
const CMAF_CONTAINER_NAME: &str = "cmaf";

/// An ISOBMFF box header: a 32-bit big-endian size followed by a four-character
/// code (ISO/IEC 14496-12 §4.2). The `mdat` header is written by hand rather
/// than through [`Mdat`], whose `data: Vec<u8>` would copy the payload twice.
const MDAT_BOX_HEADER_BYTES: u32 = 8;

/// `sample_depends_on = 2` with `sample_is_non_sync_sample = 0` — the sample
/// depends on nothing and is a random access point (ISO/IEC 14496-12 §8.8.3.1).
const SAMPLE_FLAGS_OF_A_SYNC_POINT: u32 = 0x0200_0000;

/// `sample_depends_on = 1` with `sample_is_non_sync_sample = 1` — the sample
/// depends on others and is not a random access point.
const SAMPLE_FLAGS_OF_A_NON_SYNC_POINT: u32 = 0x0101_0000;

/// Build one CMAF chunk carrying one sample, ready to send as a single MoQ
/// object.
pub(crate) fn build_cmaf_fragment(
    track_id: u32,
    sequence_number: u32,
    decode_time: u64,
    sample_duration: u32,
    is_sync_point: bool,
    sample_bytes: &[u8],
) -> Result<Bytes> {
    let mdat_total_bytes: u32 = u32::try_from(sample_bytes.len())
        .ok()
        .and_then(|count| count.checked_add(MDAT_BOX_HEADER_BYTES))
        .ok_or_else(|| {
            MoqExtensionError::Refused {
                what: format!(
                    "a sample of {} bytes cannot be written as a CMAF fragment: an mdat box size is 32 bits",
                    sample_bytes.len()
                ),
            }
        })?;
    let sample_byte_count = mdat_total_bytes - MDAT_BOX_HEADER_BYTES;

    // `data_offset: Some(x)` occupies four bytes for every x, so a moof encoded
    // with a placeholder offset is byte-for-byte as long as the one encoded
    // with the real offset. That is what lets the offset be measured from an
    // encoding of the very box it sits inside.
    let mut moof_sizing_pass_bytes: Vec<u8> = Vec::new();
    cmaf_fragment_moof(
        track_id,
        sequence_number,
        decode_time,
        sample_duration,
        is_sync_point,
        sample_byte_count,
        0,
    )
    .encode(&mut moof_sizing_pass_bytes)
    .map_err(refuse_cmaf_fragment_that_would_not_encode)?;

    // `default_base_is_moof` puts the offset base at the first byte of this
    // moof, so the sample starts one mdat header past the moof's last byte.
    let data_offset_from_moof_start = i32::try_from(moof_sizing_pass_bytes.len())
        .ok()
        .and_then(|moof_bytes| moof_bytes.checked_add(MDAT_BOX_HEADER_BYTES as i32))
        .ok_or_else(|| MoqExtensionError::Refused {
            what: format!(
                "a moof of {} bytes cannot address its own mdat: a trun data_offset is a signed 32-bit value",
                moof_sizing_pass_bytes.len()
            ),
        })?;

    let mut fragment_bytes: Vec<u8> =
        Vec::with_capacity(moof_sizing_pass_bytes.len() + mdat_total_bytes as usize);
    cmaf_fragment_moof(
        track_id,
        sequence_number,
        decode_time,
        sample_duration,
        is_sync_point,
        sample_byte_count,
        data_offset_from_moof_start,
    )
    .encode(&mut fragment_bytes)
    .map_err(refuse_cmaf_fragment_that_would_not_encode)?;

    fragment_bytes.extend_from_slice(&mdat_total_bytes.to_be_bytes());
    fragment_bytes.extend_from_slice(b"mdat");
    fragment_bytes.extend_from_slice(sample_bytes);

    Ok(Bytes::from(fragment_bytes))
}

/// One sample lifted back out of a CMAF chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmafFragmentSample {
    pub(crate) sample_bytes: Vec<u8>,
    pub(crate) decode_time: u64,
    pub(crate) duration: u32,
    pub(crate) is_sync_point: bool,
}

/// Split one CMAF chunk back into the samples it carries, in decode order.
pub(crate) fn read_cmaf_fragment(object_bytes: &[u8]) -> Result<Vec<CmafFragmentSample>> {
    let mut unread_object_bytes: &[u8] = object_bytes;

    let moof_header = Header::decode(&mut unread_object_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_fragment(format!(
            "the object is too short to open with a moof atom header: {failure}"
        ))
    })?;
    if moof_header.kind != Moof::KIND {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the object opens with a `{}` atom, but a CMAF chunk opens with a moof",
            moof_header.kind
        )));
    }
    let moof_body_byte_count = moof_header.size.ok_or_else(|| {
        refuse_as_malformed_cmaf_fragment(
            "the moof atom declares no size, so the mdat that must follow it cannot be found"
                .to_string(),
        )
    })?;
    if moof_body_byte_count > unread_object_bytes.len() {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the object declares a {moof_body_byte_count} byte moof body but carries only {} bytes after the moof header",
            unread_object_bytes.len()
        )));
    }
    let mut unparsed_moof_body_bytes = &unread_object_bytes[..moof_body_byte_count];
    let moof = Moof::decode_body(&mut unparsed_moof_body_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_fragment(format!("the moof atom does not parse: {failure}"))
    })?;
    if !unparsed_moof_body_bytes.is_empty() {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the moof atom declares a {moof_body_byte_count} byte body but {} bytes of it parse as no atom",
            unparsed_moof_body_bytes.len()
        )));
    }
    unread_object_bytes = &unread_object_bytes[moof_body_byte_count..];

    let mdat_header = Header::decode(&mut unread_object_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_fragment(format!(
            "the moof atom is not followed by an mdat atom header: {failure}"
        ))
    })?;
    if mdat_header.kind != Mdat::KIND {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the moof atom is followed by a `{}` atom, but a CMAF chunk's moof is followed by its mdat",
            mdat_header.kind
        )));
    }
    // A zero size field means "to the end of the enclosing container", which
    // for a self-delimiting MoQ object is the end of the object.
    let mdat_payload_byte_count = mdat_header.size.unwrap_or(unread_object_bytes.len());
    if mdat_payload_byte_count > unread_object_bytes.len() {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the object declares a {mdat_payload_byte_count} byte mdat payload but carries only {} bytes after the mdat header",
            unread_object_bytes.len()
        )));
    }
    let object_byte_count_after_the_mdat_payload =
        unread_object_bytes.len() - mdat_payload_byte_count;
    if object_byte_count_after_the_mdat_payload != 0 {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the object carries {object_byte_count_after_the_mdat_payload} bytes after its mdat payload, but a CMAF chunk is one moof and one mdat and nothing else"
        )));
    }
    let mdat_payload_bytes = &unread_object_bytes[..mdat_payload_byte_count];
    let object_offset_of_mdat_payload = object_bytes.len() - unread_object_bytes.len();

    let traf = match moof.traf.as_slice() {
        [only_traf] => only_traf,
        other_trafs => {
            return Err(refuse_as_malformed_cmaf_fragment(format!(
                "the moof carries {} traf atoms, but a CMAF chunk carries exactly one track",
                other_trafs.len()
            )));
        }
    };

    // With exactly one traf, an absent `base_data_offset` puts the offset base
    // at the first byte of the enclosing moof whether or not
    // `default_base_is_moof` is set (ISO/IEC 14496-12 §8.8.7.1), so that field
    // is the only thing that can move the base off this object's first byte.
    if let Some(base_data_offset) = traf.tfhd.base_data_offset {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the traf sets tfhd.base_data_offset to {base_data_offset}, but this reader resolves sample offsets from the first byte of the moof and supports no other base"
        )));
    }

    let mut samples: Vec<CmafFragmentSample> = Vec::new();
    let mut decode_time_of_next_sample = traf
        .tfdt
        .as_ref()
        .map_or(0, |tfdt| tfdt.base_media_decode_time);
    let mut mdat_payload_read_cursor: usize = 0;
    let mut mdat_payload_byte_count_covered_by_samples: usize = 0;

    for trun in &traf.trun {
        if let Some(data_offset) = trun.data_offset {
            // `default_base_is_moof` bases the offset on the first byte of the
            // moof, which is the first byte of the object.
            let offset_from_moof_start = usize::try_from(data_offset).map_err(|_| {
                refuse_as_malformed_cmaf_fragment(format!(
                    "a trun places its samples {data_offset} bytes from the start of the moof, which is before the object begins"
                ))
            })?;
            mdat_payload_read_cursor = offset_from_moof_start
                .checked_sub(object_offset_of_mdat_payload)
                .ok_or_else(|| {
                    refuse_as_malformed_cmaf_fragment(format!(
                        "a trun places its samples at object offset {offset_from_moof_start}, before the mdat payload that starts at {object_offset_of_mdat_payload}"
                    ))
                })?;
        }

        for (entry_index, trun_entry) in trun.entries.iter().enumerate() {
            let sample_byte_count = trun_entry
                .size
                .or(traf.tfhd.default_sample_size)
                .ok_or_else(|| {
                    refuse_as_malformed_cmaf_fragment(format!(
                        "trun entry {entry_index} carries no sample size and the tfhd declares no default"
                    ))
                })? as usize;
            let sample_end_in_mdat_payload = mdat_payload_read_cursor
                .checked_add(sample_byte_count)
                .filter(|end| *end <= mdat_payload_bytes.len())
                .ok_or_else(|| {
                    refuse_as_malformed_cmaf_fragment(format!(
                        "trun entry {entry_index} runs from {mdat_payload_read_cursor} for {sample_byte_count} bytes, past the {} byte mdat payload",
                        mdat_payload_bytes.len()
                    ))
                })?;

            let duration = trun_entry
                .duration
                .or(traf.tfhd.default_sample_duration)
                .ok_or_else(|| {
                    refuse_as_malformed_cmaf_fragment(format!(
                        "trun entry {entry_index} carries no sample duration and the tfhd declares no default"
                    ))
                })?;
            // `first_sample_flags` is folded into entry zero's own flags when a
            // trun is decoded, so the per-entry field already carries the
            // override the reference publisher writes for a keyframe.
            let sample_flags = trun_entry
                .flags
                .or(traf.tfhd.default_sample_flags)
                .unwrap_or(SAMPLE_FLAGS_OF_A_NON_SYNC_POINT);

            samples.push(CmafFragmentSample {
                sample_bytes: mdat_payload_bytes
                    [mdat_payload_read_cursor..sample_end_in_mdat_payload]
                    .to_vec(),
                decode_time: decode_time_of_next_sample,
                duration,
                is_sync_point: sample_flags_mark_a_sync_point(sample_flags),
            });

            decode_time_of_next_sample =
                decode_time_of_next_sample.saturating_add(u64::from(duration));
            mdat_payload_read_cursor = sample_end_in_mdat_payload;
            mdat_payload_byte_count_covered_by_samples += sample_byte_count;
        }
    }

    if mdat_payload_byte_count_covered_by_samples != mdat_payload_bytes.len() {
        return Err(refuse_as_malformed_cmaf_fragment(format!(
            "the trun sample sizes account for {mdat_payload_byte_count_covered_by_samples} bytes but the mdat payload is {} bytes",
            mdat_payload_bytes.len()
        )));
    }

    Ok(samples)
}

/// Whether ISOBMFF sample flags name a random access point — `sample_depends_on
/// == 2` and `sample_is_non_sync_sample == 0` (ISO/IEC 14496-12 §8.8.3.1). Both
/// halves are read because a muxer that sets only one of them is common enough
/// that the reference publisher tests the pair too.
fn sample_flags_mark_a_sync_point(sample_flags: u32) -> bool {
    (sample_flags >> 24) & 0x3 == 0x2 && (sample_flags >> 16) & 0x1 == 0
}

fn cmaf_fragment_moof(
    track_id: u32,
    sequence_number: u32,
    decode_time: u64,
    sample_duration: u32,
    is_sync_point: bool,
    sample_byte_count: u32,
    data_offset_from_moof_start: i32,
) -> Moof {
    let sample_flags = if is_sync_point {
        SAMPLE_FLAGS_OF_A_SYNC_POINT
    } else {
        SAMPLE_FLAGS_OF_A_NON_SYNC_POINT
    };

    Moof {
        mfhd: Mfhd { sequence_number },
        // Exactly one traf: the reference publisher requires one track per moof
        // and names the media track after that track's id.
        traf: vec![Traf {
            tfhd: Tfhd {
                track_id,
                // Absent `base_data_offset` plus `default_base_is_moof` is what
                // makes a fragment relocatable — it addresses nothing outside
                // itself, so the object can be sent standalone.
                base_data_offset: None,
                default_base_is_moof: true,
                ..Default::default()
            },
            tfdt: Some(Tfdt {
                base_media_decode_time: decode_time,
            }),
            trun: vec![Trun {
                data_offset: Some(data_offset_from_moof_start),
                entries: vec![TrunEntry {
                    duration: Some(sample_duration),
                    size: Some(sample_byte_count),
                    flags: Some(sample_flags),
                    cts: None,
                }],
            }],
            ..Default::default()
        }],
    }
}

fn refuse_cmaf_fragment_that_would_not_encode(failure: mp4_atom::Error) -> MoqExtensionError {
    MoqExtensionError::MalformedObject {
        container: CMAF_CONTAINER_NAME,
        what: format!("the fragment's moof could not be encoded: {failure}"),
    }
}

fn refuse_as_malformed_cmaf_fragment(what: String) -> MoqExtensionError {
    MoqExtensionError::MalformedObject {
        container: CMAF_CONTAINER_NAME,
        what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp4_atom::Any;

    const A_SAMPLE: [u8; 64] = [0x5A; 64];

    /// The offset a hand-built moof carries through its sizing pass, before the
    /// real one is measured from that encoding.
    const PLACEHOLDER_DATA_OFFSET: i32 = 0;

    /// Encode a hand-built moof, point its first trun at
    /// `sample_start_in_mdat_payload` bytes into the mdat payload, and append
    /// that payload as the mdat.
    fn object_bytes_of_a_moof_and_an_mdat_payload(
        mut moof_to_encode: Moof,
        mdat_payload_bytes: &[u8],
        sample_start_in_mdat_payload: i32,
    ) -> Vec<u8> {
        let mut moof_sizing_pass_bytes: Vec<u8> = Vec::new();
        moof_to_encode
            .encode(&mut moof_sizing_pass_bytes)
            .expect("a moof encodes");

        moof_to_encode.traf[0].trun[0].data_offset = Some(
            i32::try_from(moof_sizing_pass_bytes.len()).expect("a moof written here is small")
                + MDAT_BOX_HEADER_BYTES as i32
                + sample_start_in_mdat_payload,
        );

        let mut object_bytes: Vec<u8> = Vec::new();
        moof_to_encode
            .encode(&mut object_bytes)
            .expect("a moof encodes");
        object_bytes.extend_from_slice(
            &(MDAT_BOX_HEADER_BYTES
                + u32::try_from(mdat_payload_bytes.len())
                    .expect("a payload written here is small"))
            .to_be_bytes(),
        );
        object_bytes.extend_from_slice(b"mdat");
        object_bytes.extend_from_slice(mdat_payload_bytes);
        object_bytes
    }

    fn moof_byte_count_of(fragment_bytes: &[u8]) -> usize {
        let mut unread: &[u8] = fragment_bytes;
        let header = Header::decode(&mut unread).expect("the fragment opens with an atom header");
        let header_byte_count = fragment_bytes.len() - unread.len();
        header_byte_count + header.size.expect("a moof written here declares its size")
    }

    fn trun_of(fragment_bytes: &[u8]) -> Trun {
        let mut unread: &[u8] = fragment_bytes;
        let header = Header::decode(&mut unread).expect("the fragment opens with an atom header");
        let body_byte_count = header.size.expect("a moof written here declares its size");
        let mut body = &unread[..body_byte_count];
        let moof = Moof::decode_body(&mut body).expect("the moof parses");
        moof.traf[0].trun[0].clone()
    }

    #[test]
    fn a_sample_survives_the_round_trip_through_a_fragment_intact() {
        let fragment_bytes = build_cmaf_fragment(
            CMAF_FRAGMENT_TRACK_ID,
            7,
            123_456_789,
            33_000_000,
            true,
            &A_SAMPLE,
        )
        .expect("a 64 byte sample builds a fragment");

        let samples = read_cmaf_fragment(&fragment_bytes).expect("the fragment reads back");

        assert_eq!(
            samples,
            vec![CmafFragmentSample {
                sample_bytes: A_SAMPLE.to_vec(),
                decode_time: 123_456_789,
                duration: 33_000_000,
                is_sync_point: true,
            }]
        );
    }

    #[test]
    fn a_non_sync_sample_reads_back_as_a_non_sync_sample() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 2, 0, 3000, false, &A_SAMPLE)
                .expect("a non-sync sample builds a fragment");

        let samples = read_cmaf_fragment(&fragment_bytes).expect("the fragment reads back");

        assert!(
            !samples[0].is_sync_point,
            "a subscriber that mistakes a delta sample for a random access point starts decoding garbage"
        );
    }

    #[test]
    fn a_moof_is_the_same_length_whatever_data_offset_it_carries() {
        let mut with_a_placeholder_offset: Vec<u8> = Vec::new();
        cmaf_fragment_moof(1, 1, 0, 3000, true, 64, 0)
            .encode(&mut with_a_placeholder_offset)
            .expect("a moof encodes");

        for offset in [8i32, 4096, i32::MAX] {
            let mut with_a_real_offset: Vec<u8> = Vec::new();
            cmaf_fragment_moof(1, 1, 0, 3000, true, 64, offset)
                .encode(&mut with_a_real_offset)
                .expect("a moof encodes");

            assert_eq!(
                with_a_real_offset.len(),
                with_a_placeholder_offset.len(),
                "the offset is measured from a placeholder encoding, so a width that varies with the value would point the trun at the wrong bytes"
            );
        }
    }

    #[test]
    fn a_fragment_is_self_contained_so_its_data_offset_needs_no_preceding_bytes() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");

        let moof_byte_count = moof_byte_count_of(&fragment_bytes);

        assert_eq!(
            trun_of(&fragment_bytes).data_offset,
            Some(i32::try_from(moof_byte_count + 8).expect("a moof written here is small")),
            "with default_base_is_moof the base is byte zero of the moof, so the sample sits one mdat header past its end"
        );
        assert_eq!(
            fragment_bytes.len(),
            moof_byte_count + 8 + A_SAMPLE.len(),
            "a fragment is its moof and its mdat and nothing else — no styp, no padding"
        );
    }

    #[test]
    fn a_fragment_is_exactly_a_moof_then_an_mdat_to_any_reader_of_the_box_format() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");

        let mut unread: &[u8] = &fragment_bytes;
        let first_atom = Any::decode(&mut unread).expect("the first atom parses");
        let second_atom = Any::decode(&mut unread).expect("the second atom parses");

        assert_eq!(first_atom.kind(), Moof::KIND);
        assert_eq!(second_atom.kind(), Mdat::KIND);
        assert!(
            unread.is_empty(),
            "a trailing byte would make the object something other than one CMAF chunk"
        );
    }

    #[test]
    fn a_truncated_object_is_refused_by_name_rather_than_read_as_a_short_sample() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");
        let moof_byte_count = moof_byte_count_of(&fragment_bytes);

        let refusal_of_a_cut_inside_the_moof_body =
            read_cmaf_fragment(&fragment_bytes[..moof_byte_count - 4])
                .expect_err("half a moof is not a CMAF chunk");

        assert!(
            matches!(
                refusal_of_a_cut_inside_the_moof_body,
                MoqExtensionError::MalformedObject {
                    container: CMAF_CONTAINER_NAME,
                    ..
                }
            ),
            "got {refusal_of_a_cut_inside_the_moof_body}"
        );
        assert!(
            refusal_of_a_cut_inside_the_moof_body
                .to_string()
                .contains("moof body"),
            "got {refusal_of_a_cut_inside_the_moof_body}"
        );

        let refusal_of_a_cut_inside_the_mdat_payload =
            read_cmaf_fragment(&fragment_bytes[..fragment_bytes.len() - 8])
                .expect_err("a sample cut short is not a CMAF chunk");

        assert!(
            matches!(
                refusal_of_a_cut_inside_the_mdat_payload,
                MoqExtensionError::MalformedObject {
                    container: CMAF_CONTAINER_NAME,
                    ..
                }
            ),
            "got {refusal_of_a_cut_inside_the_mdat_payload}"
        );
        assert!(
            refusal_of_a_cut_inside_the_mdat_payload
                .to_string()
                .contains("mdat payload"),
            "got {refusal_of_a_cut_inside_the_mdat_payload}"
        );
    }

    #[test]
    fn an_object_carrying_bytes_after_its_mdat_is_refused_by_name() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");

        let mut fragment_with_trailing_bytes: Vec<u8> = fragment_bytes.to_vec();
        fragment_with_trailing_bytes.extend_from_slice(&[0x11; 100]);

        let refusal = read_cmaf_fragment(&fragment_with_trailing_bytes)
            .expect_err("bytes the reader cannot describe are not silently dropped");

        assert!(
            refusal
                .to_string()
                .contains("carries 100 bytes after its mdat payload"),
            "got {refusal}"
        );
    }

    #[test]
    fn two_chunks_concatenated_are_refused_rather_than_read_as_the_first_alone() {
        let first_chunk_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");
        let second_chunk_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 2, 3000, 3000, false, &A_SAMPLE)
                .expect("a fragment builds");

        let mut both_chunks_bytes: Vec<u8> = first_chunk_bytes.to_vec();
        both_chunks_bytes.extend_from_slice(&second_chunk_bytes);

        let refusal = read_cmaf_fragment(&both_chunks_bytes)
            .expect_err("a second chunk in the same object is not the first chunk's business");

        assert!(
            refusal.to_string().contains("after its mdat payload"),
            "a subscriber handed two chunks must be told, not quietly given one: got {refusal}"
        );
    }

    #[test]
    fn a_trun_entry_with_no_duration_and_no_tfhd_default_is_refused_by_name() {
        let mut moof_of_three_sized_but_undated_samples = cmaf_fragment_moof(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            1000,
            3000,
            true,
            4,
            PLACEHOLDER_DATA_OFFSET,
        );
        moof_of_three_sized_but_undated_samples.traf[0]
            .tfhd
            .default_sample_duration = None;
        moof_of_three_sized_but_undated_samples.traf[0].trun[0].entries = vec![
            TrunEntry {
                duration: None,
                size: Some(4),
                flags: Some(SAMPLE_FLAGS_OF_A_SYNC_POINT),
                cts: None,
            };
            3
        ];

        let object_bytes = object_bytes_of_a_moof_and_an_mdat_payload(
            moof_of_three_sized_but_undated_samples,
            &[0x5A; 12],
            0,
        );

        let refusal = read_cmaf_fragment(&object_bytes).expect_err(
            "a duration that would come from a trex the fragment does not carry is not zero",
        );

        assert!(
            refusal.to_string().contains(
                "trun entry 0 carries no sample duration and the tfhd declares no default"
            ),
            "got {refusal}"
        );
    }

    #[test]
    fn a_data_offset_that_skips_leading_mdat_bytes_is_refused_by_name() {
        let moof_of_one_sample_placed_past_the_payload_start = cmaf_fragment_moof(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            0,
            3000,
            true,
            64,
            PLACEHOLDER_DATA_OFFSET,
        );

        let object_bytes = object_bytes_of_a_moof_and_an_mdat_payload(
            moof_of_one_sample_placed_past_the_payload_start,
            &[0x5A; 128],
            64,
        );

        let refusal = read_cmaf_fragment(&object_bytes)
            .expect_err("media data no sample names is not a chunk this reader can describe");

        assert!(
            refusal
                .to_string()
                .contains("account for 64 bytes but the mdat payload is 128 bytes"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_traf_that_bases_its_offsets_outside_the_moof_is_refused_by_the_field_that_did_it() {
        let mut moof_based_on_an_absolute_file_offset = cmaf_fragment_moof(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            0,
            3000,
            true,
            64,
            PLACEHOLDER_DATA_OFFSET,
        );
        moof_based_on_an_absolute_file_offset.traf[0]
            .tfhd
            .default_base_is_moof = false;
        moof_based_on_an_absolute_file_offset.traf[0]
            .tfhd
            .base_data_offset = Some(4096);
        moof_based_on_an_absolute_file_offset.traf[0].trun[0].data_offset = Some(0);

        let mut object_bytes: Vec<u8> = Vec::new();
        moof_based_on_an_absolute_file_offset
            .encode(&mut object_bytes)
            .expect("a moof encodes");
        object_bytes.extend_from_slice(&(MDAT_BOX_HEADER_BYTES + 64).to_be_bytes());
        object_bytes.extend_from_slice(b"mdat");
        object_bytes.extend_from_slice(&A_SAMPLE);

        let refusal = read_cmaf_fragment(&object_bytes)
            .expect_err("an offset base this reader does not resolve is not guessed at");

        assert!(
            refusal.to_string().contains("tfhd.base_data_offset"),
            "the unsupported field is what the far end has to change: got {refusal}"
        );
    }

    #[test]
    fn a_moof_body_carrying_bytes_that_parse_as_no_atom_is_refused_by_name() {
        const JUNK_BYTE_COUNT: usize = 16;

        let mut moof_sizing_pass_bytes: Vec<u8> = Vec::new();
        cmaf_fragment_moof(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            0,
            3000,
            true,
            64,
            PLACEHOLDER_DATA_OFFSET,
        )
        .encode(&mut moof_sizing_pass_bytes)
        .expect("a moof encodes");

        // The junk sits inside the moof body and the sample still starts at the
        // first byte of the mdat payload, so nothing but the leftover check can
        // notice it.
        let data_offset_past_the_junk =
            i32::try_from(moof_sizing_pass_bytes.len() + JUNK_BYTE_COUNT)
                .expect("a moof written here is small")
                + MDAT_BOX_HEADER_BYTES as i32;
        let mut object_bytes: Vec<u8> = Vec::new();
        cmaf_fragment_moof(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            0,
            3000,
            true,
            64,
            data_offset_past_the_junk,
        )
        .encode(&mut object_bytes)
        .expect("a moof encodes");
        let moof_box_byte_count_including_the_junk =
            u32::try_from(object_bytes.len() + JUNK_BYTE_COUNT)
                .expect("a moof written here is small");
        object_bytes[..4].copy_from_slice(&moof_box_byte_count_including_the_junk.to_be_bytes());
        object_bytes.extend_from_slice(&[0xAA; JUNK_BYTE_COUNT]);
        object_bytes.extend_from_slice(&(MDAT_BOX_HEADER_BYTES + 64).to_be_bytes());
        object_bytes.extend_from_slice(b"mdat");
        object_bytes.extend_from_slice(&A_SAMPLE);

        let refusal = read_cmaf_fragment(&object_bytes)
            .expect_err("a moof body the box layer cannot account for is not a chunk");

        assert!(
            refusal
                .to_string()
                .contains("16 bytes of it parse as no atom"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_fragment_written_with_another_track_id_still_reads_back() {
        let fragment_bytes = build_cmaf_fragment(9, 1, 0, 3000, true, &A_SAMPLE)
            .expect("a fragment on another track builds");

        let samples = read_cmaf_fragment(&fragment_bytes)
            .expect("the reader takes the track id the publisher numbered its track with");

        assert_eq!(samples[0].sample_bytes, A_SAMPLE.to_vec());
    }

    #[test]
    fn an_object_that_is_not_moof_then_mdat_is_refused_by_name() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");
        let moof_byte_count = moof_byte_count_of(&fragment_bytes);

        let mut moof_then_free: Vec<u8> = fragment_bytes[..moof_byte_count].to_vec();
        moof_then_free.extend_from_slice(&8u32.to_be_bytes());
        moof_then_free.extend_from_slice(b"free");

        let refusal = read_cmaf_fragment(&moof_then_free)
            .expect_err("a moof followed by free is not a chunk");

        assert!(refusal.to_string().contains("mdat"), "got {refusal}");
    }

    #[test]
    fn a_sample_size_that_runs_past_the_mdat_payload_is_refused_by_name() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");
        let moof_byte_count = moof_byte_count_of(&fragment_bytes);

        let mut fragment_with_a_starved_mdat: Vec<u8> = fragment_bytes[..moof_byte_count].to_vec();
        fragment_with_a_starved_mdat.extend_from_slice(&16u32.to_be_bytes());
        fragment_with_a_starved_mdat.extend_from_slice(b"mdat");
        fragment_with_a_starved_mdat.extend_from_slice(&A_SAMPLE[..8]);

        let refusal = read_cmaf_fragment(&fragment_with_a_starved_mdat)
            .expect_err("a sample cannot be read from bytes that are not there");

        assert!(
            refusal.to_string().contains("past the 8 byte mdat payload"),
            "got {refusal}"
        );
    }

    #[test]
    fn trun_sizes_that_do_not_sum_to_the_mdat_payload_are_refused_by_name() {
        let fragment_bytes =
            build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, true, &A_SAMPLE)
                .expect("a fragment builds");
        let moof_byte_count = moof_byte_count_of(&fragment_bytes);

        let mut fragment_with_an_overfed_mdat: Vec<u8> = fragment_bytes[..moof_byte_count].to_vec();
        fragment_with_an_overfed_mdat.extend_from_slice(&(8u32 + 128).to_be_bytes());
        fragment_with_an_overfed_mdat.extend_from_slice(b"mdat");
        fragment_with_an_overfed_mdat.extend_from_slice(&[0x5A; 128]);

        let refusal = read_cmaf_fragment(&fragment_with_an_overfed_mdat)
            .expect_err("unaccounted media data is not a chunk this reader can describe");

        assert!(
            refusal
                .to_string()
                .contains("account for 64 bytes but the mdat payload is 128 bytes"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_moof_carrying_more_than_one_traf_is_refused_by_name() {
        let mut two_track_moof = cmaf_fragment_moof(1, 1, 0, 3000, true, 64, 0);
        two_track_moof.traf.push(two_track_moof.traf[0].clone());
        let mut object_bytes: Vec<u8> = Vec::new();
        two_track_moof
            .encode(&mut object_bytes)
            .expect("a two-traf moof encodes");
        object_bytes.extend_from_slice(&(8u32 + 64).to_be_bytes());
        object_bytes.extend_from_slice(b"mdat");
        object_bytes.extend_from_slice(&A_SAMPLE);

        let refusal = read_cmaf_fragment(&object_bytes)
            .expect_err("a multi-track fragment is not what this container path carries");

        assert!(
            refusal.to_string().contains("2 traf atoms"),
            "got {refusal}"
        );
    }

    #[test]
    fn an_empty_object_is_refused_rather_than_read_as_an_empty_fragment() {
        let refusal = read_cmaf_fragment(&[]).expect_err("no bytes are not a chunk");

        assert!(
            matches!(
                refusal,
                MoqExtensionError::MalformedObject {
                    container: CMAF_CONTAINER_NAME,
                    ..
                }
            ),
            "got {refusal}"
        );
    }

    #[test]
    fn a_zero_length_sample_still_makes_a_readable_fragment() {
        let fragment_bytes = build_cmaf_fragment(CMAF_FRAGMENT_TRACK_ID, 1, 0, 3000, false, &[])
            .expect("an empty sample builds a fragment");

        let samples = read_cmaf_fragment(&fragment_bytes).expect("the fragment reads back");

        assert_eq!(samples[0].sample_bytes, Vec::<u8>::new());
    }

    #[test]
    fn sync_point_flags_are_read_by_both_halves_of_the_pair() {
        assert!(sample_flags_mark_a_sync_point(SAMPLE_FLAGS_OF_A_SYNC_POINT));
        assert!(!sample_flags_mark_a_sync_point(
            SAMPLE_FLAGS_OF_A_NON_SYNC_POINT
        ));
        assert!(
            !sample_flags_mark_a_sync_point(SAMPLE_FLAGS_OF_A_SYNC_POINT | 0x0001_0000),
            "sample_is_non_sync_sample set alongside sample_depends_on == 2 is a contradiction, and the conservative reading is not a sync point"
        );
    }

    #[test]
    fn a_decode_time_beyond_thirty_two_bits_survives_the_round_trip() {
        let decode_time = u64::from(u32::MAX) + 1_000;
        let fragment_bytes = build_cmaf_fragment(
            CMAF_FRAGMENT_TRACK_ID,
            1,
            decode_time,
            3000,
            true,
            &A_SAMPLE,
        )
        .expect("a fragment builds");

        let samples = read_cmaf_fragment(&fragment_bytes).expect("the fragment reads back");

        assert_eq!(
            samples[0].decode_time, decode_time,
            "a nanosecond timescale passes 32 bits about four seconds into a stream"
        );
    }
}
