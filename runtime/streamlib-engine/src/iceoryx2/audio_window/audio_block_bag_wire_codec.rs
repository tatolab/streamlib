// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's own reader and writer for the six `AudioBlock` wire keys.
//!
//! The `AudioBlock` cast lives in `streamlib-media-builtins`, which depends on
//! the engine, so the read-side stage cannot reach it and owns this instead.
//! The keys are the contract, not the types: `samples`, `sample_rate`,
//! `channels`, `sample_count`, `dtype`, `first_sample_timestamp_ns`, with
//! `samples` as msgpack `bin` carrying interleaved little-endian scalars. A
//! window this module encodes is an ordinary `AudioBlock` bag, which is what
//! keeps `read(into=AudioBlock)` and Rust's `read::<AudioBlock>` working
//! unchanged on a windowed port.
//!
//! Reading borrows: the payload is a `bin` run inside the frame body, so
//! [`AudioBlockReadFromTheWire`] holds a slice of it rather than a copy —
//! which is what lets the readiness gate decode a queued bag's header fields
//! without paying for its samples.

use serde::{Deserialize, Serialize};

/// How the scalars in an audio block's payload are encoded — the two an
/// `AudioBlock` legalises, spelled the way the wire spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioBlockSampleDtype {
    F32,
    I16,
}

impl AudioBlockSampleDtype {
    /// Bytes one scalar occupies in the payload.
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            AudioBlockSampleDtype::F32 => 4,
            AudioBlockSampleDtype::I16 => 2,
        }
    }

    /// The wire spelling, which is also the `audio_window` declaration's.
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
            AudioBlockSampleDtype::F32 => "f32",
            AudioBlockSampleDtype::I16 => "i16",
        }
    }

    /// Read a wire spelling, or `None` for one this engine does not know.
    pub(crate) fn from_wire_str(dtype: &str) -> Option<Self> {
        match dtype {
            "f32" => Some(AudioBlockSampleDtype::F32),
            "i16" => Some(AudioBlockSampleDtype::I16),
            _ => None,
        }
    }
}

/// The named map as serde reads it off the wire — every field borrowed, so a
/// decode allocates nothing for the payload.
///
/// The map is open: a producer may carry extra keys and this ignores them,
/// matching both existing casts. `dtype` is optional because absent means
/// `f32` by wire contract.
#[derive(Debug, Deserialize)]
struct AudioBlockNamedMapOnTheWire<'wire> {
    #[serde(rename = "samples", with = "serde_bytes", borrow)]
    interleaved_sample_bytes: &'wire [u8],
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    #[serde(default, borrow)]
    dtype: Option<&'wire str>,
    first_sample_timestamp_ns: i64,
}

/// One audio block decoded out of a bag, borrowing its payload from the frame
/// body it was decoded from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AudioBlockReadFromTheWire<'wire> {
    /// Interleaved little-endian scalars, read according to `dtype`.
    pub(crate) interleaved_sample_bytes: &'wire [u8],
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    /// Per-channel samples: the payload carries `sample_count × channels`
    /// scalars.
    pub(crate) sample_count: u32,
    pub(crate) dtype: AudioBlockSampleDtype,
    pub(crate) first_sample_timestamp_ns: i64,
}

/// Why a bag could not be read as an audio block, in the terms the refusal
/// names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioBlockWireRefusal {
    /// The bag is not a named map carrying the audio-block keys.
    NotAnAudioBlockBag { decode_failure: String },
    /// `dtype` names an encoding this engine does not know.
    UnknownSampleDtype { dtype: String },
    /// The payload length does not agree with the count, channels and dtype
    /// beside it, so reshaping it would invent a plausible wrong answer.
    PayloadLengthDisagreesWithTheCount {
        payload_bytes: usize,
        expected_bytes: usize,
        sample_count: u32,
        channels: u32,
        dtype: &'static str,
    },
}

impl std::fmt::Display for AudioBlockWireRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioBlockWireRefusal::NotAnAudioBlockBag { decode_failure } => write!(
                formatter,
                "the bag carries no audio-block keys ({decode_failure}) — the stage reads \
                 `samples`, `sample_rate`, `channels`, `sample_count`, `dtype` and \
                 `first_sample_timestamp_ns`"
            ),
            AudioBlockWireRefusal::UnknownSampleDtype { dtype } => write!(
                formatter,
                "`dtype` is `\"{dtype}\"`, which names no encoding the stage can read — \
                 expected `\"f32\"` or `\"i16\"`"
            ),
            AudioBlockWireRefusal::PayloadLengthDisagreesWithTheCount {
                payload_bytes,
                expected_bytes,
                sample_count,
                channels,
                dtype,
            } => write!(
                formatter,
                "`samples` carries {payload_bytes} bytes but {sample_count} samples × \
                 {channels} channels × `{dtype}` is {expected_bytes} bytes"
            ),
        }
    }
}

/// Read a bag body as an audio block, refusing by name rather than reshaping
/// it into a plausible wrong answer.
pub(crate) fn read_an_audio_block_off_the_wire(
    bag_body: &[u8],
) -> Result<AudioBlockReadFromTheWire<'_>, AudioBlockWireRefusal> {
    let named_map: AudioBlockNamedMapOnTheWire<'_> =
        rmp_serde::from_slice(bag_body).map_err(|decode_failure| {
            AudioBlockWireRefusal::NotAnAudioBlockBag {
                decode_failure: decode_failure.to_string(),
            }
        })?;

    // Absent means `f32` by wire contract; present and unknown is a refusal.
    let dtype = match named_map.dtype {
        None => AudioBlockSampleDtype::F32,
        Some(spelling) => AudioBlockSampleDtype::from_wire_str(spelling).ok_or_else(|| {
            AudioBlockWireRefusal::UnknownSampleDtype {
                dtype: spelling.to_string(),
            }
        })?,
    };

    let expected_bytes =
        named_map.sample_count as usize * named_map.channels as usize * dtype.bytes_per_sample();
    if named_map.interleaved_sample_bytes.len() != expected_bytes {
        return Err(AudioBlockWireRefusal::PayloadLengthDisagreesWithTheCount {
            payload_bytes: named_map.interleaved_sample_bytes.len(),
            expected_bytes,
            sample_count: named_map.sample_count,
            channels: named_map.channels,
            dtype: dtype.as_wire_str(),
        });
    }

    Ok(AudioBlockReadFromTheWire {
        interleaved_sample_bytes: named_map.interleaved_sample_bytes,
        sample_rate: named_map.sample_rate,
        channels: named_map.channels,
        sample_count: named_map.sample_count,
        dtype,
        first_sample_timestamp_ns: named_map.first_sample_timestamp_ns,
    })
}

impl AudioBlockReadFromTheWire<'_> {
    /// The payload as f32 scalars, still interleaved, in the order the wire
    /// carries them.
    ///
    /// An iterator rather than a buffer so a caller that is going to reshape
    /// the samples anyway — which the stage's channel conversion always is —
    /// can write its result straight into the buffer it keeps, with no
    /// intermediate.
    ///
    /// `i16` divides by 32768 so the whole `i16` range maps into `[-1, 1)`,
    /// the inverse of what [`encode_an_audio_block_onto_the_wire`] applies.
    pub(crate) fn interleaved_samples_as_f32(&self) -> impl Iterator<Item = f32> + '_ {
        let bytes_per_sample = self.dtype.bytes_per_sample();
        let dtype = self.dtype;
        self.interleaved_sample_bytes
            .chunks_exact(bytes_per_sample)
            .map(move |scalar| match dtype {
                AudioBlockSampleDtype::F32 => {
                    f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]])
                }
                AudioBlockSampleDtype::I16 => {
                    i16::from_le_bytes([scalar[0], scalar[1]]) as f32 / 32_768.0
                }
            })
    }
}

/// The named map as serde writes it back — the same six keys, in the spelling
/// `AudioBlock` states.
#[derive(Debug, Serialize)]
struct EmittedAudioBlockNamedMap<'window> {
    #[serde(rename = "samples", with = "serde_bytes")]
    interleaved_sample_bytes: &'window [u8],
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    dtype: &'static str,
    first_sample_timestamp_ns: i64,
}

/// Encode one emitted window as an ordinary audio-block bag.
///
/// `interleaved_samples` are the f32 scalars the stage produced; `dtype` is
/// what the contract asked them to be written as. An `i16` contract saturates
/// rather than wrapping — a scalar past full scale clamps to the endpoint, so
/// a loud passage clips the way audio clips instead of inverting phase.
pub(crate) fn encode_an_audio_block_onto_the_wire(
    interleaved_samples: &[f32],
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    dtype: AudioBlockSampleDtype,
    first_sample_timestamp_ns: i64,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let interleaved_sample_bytes: Vec<u8> = match dtype {
        AudioBlockSampleDtype::F32 => interleaved_samples
            .iter()
            .flat_map(|scalar| scalar.to_le_bytes())
            .collect(),
        AudioBlockSampleDtype::I16 => interleaved_samples
            .iter()
            .flat_map(|scalar| saturating_i16_from_f32(*scalar).to_le_bytes())
            .collect(),
    };

    rmp_serde::to_vec_named(&EmittedAudioBlockNamedMap {
        interleaved_sample_bytes: &interleaved_sample_bytes,
        sample_rate,
        channels,
        sample_count,
        dtype: dtype.as_wire_str(),
        first_sample_timestamp_ns,
    })
}

/// One f32 scalar as `i16`, saturating at both endpoints.
///
/// The 32768 scale is the inverse of the decode's, so an `i16` source read in
/// and written back out again survives the round trip unchanged. A NaN — which
/// no comparison orders — lands on silence rather than on whatever `as i16`
/// would produce for it.
fn saturating_i16_from_f32(scalar: f32) -> i16 {
    let scaled = (scalar * 32_768.0).round();
    if scaled.is_nan() {
        return 0;
    }
    scaled.clamp(-32_768.0, 32_767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bag written key by key, so a test can state a payload, a dtype or a
    /// count the encoder would never produce together.
    ///
    /// Built through `rmp_serde` rather than a `serde_json::json!` tree because
    /// only this path puts `samples` on the wire as msgpack `bin`; a JSON value
    /// carries a byte buffer as an array of integers, which is exactly the
    /// mistake the wire contract exists to rule out.
    #[derive(Serialize)]
    struct HandWrittenBag<'a> {
        #[serde(rename = "samples", with = "serde_bytes")]
        interleaved_sample_bytes: &'a [u8],
        sample_rate: u32,
        channels: u32,
        sample_count: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        dtype: Option<&'a str>,
        first_sample_timestamp_ns: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        recorded_by: Option<&'a str>,
    }

    impl<'a> HandWrittenBag<'a> {
        fn new(interleaved_sample_bytes: &'a [u8], channels: u32, sample_count: u32) -> Self {
            Self {
                interleaved_sample_bytes,
                sample_rate: 16_000,
                channels,
                sample_count,
                dtype: Some("f32"),
                first_sample_timestamp_ns: 11,
                recorded_by: None,
            }
        }

        fn onto_the_wire(&self) -> Vec<u8> {
            rmp_serde::to_vec_named(self).expect("a hand-written bag encodes")
        }
    }

    fn f32_bag(scalars: &[f32], sample_rate: u32, channels: u32, timestamp_ns: i64) -> Vec<u8> {
        encode_an_audio_block_onto_the_wire(
            scalars,
            sample_rate,
            channels,
            scalars.len() as u32 / channels,
            AudioBlockSampleDtype::F32,
            timestamp_ns,
        )
        .expect("an audio block encodes")
    }

    #[test]
    fn a_block_written_by_the_stage_reads_back_as_the_block_it_wrote() {
        let scalars = [-1.0f32, -0.5, 0.0, 0.25, 0.5, 1.0];
        let wire = f32_bag(&scalars, 48_000, 2, 123_456_789);

        let read = read_an_audio_block_off_the_wire(&wire).expect("the stage reads its own block");
        assert_eq!(read.sample_rate, 48_000);
        assert_eq!(read.channels, 2);
        assert_eq!(read.sample_count, 3);
        assert_eq!(read.dtype, AudioBlockSampleDtype::F32);
        assert_eq!(read.first_sample_timestamp_ns, 123_456_789);
        assert_eq!(
            read.interleaved_samples_as_f32().collect::<Vec<_>>(),
            scalars
        );
    }

    /// The payload is a `bin` run inside the body, so the decode hands back a
    /// slice of it. This is what the readiness gate leans on — it decodes
    /// every queued bag's header fields and never pays for their samples.
    #[test]
    fn a_decoded_block_borrows_its_payload_out_of_the_frame_body_rather_than_copying_it() {
        let wire = f32_bag(&[0.25, -0.25, 0.5, -0.5], 16_000, 1, 7);

        let read = read_an_audio_block_off_the_wire(&wire).expect("decodes");
        let payload_start = read.interleaved_sample_bytes.as_ptr() as usize;
        let body_start = wire.as_ptr() as usize;
        assert!(
            payload_start >= body_start && payload_start < body_start + wire.len(),
            "the samples slice must point inside the frame body"
        );
    }

    #[test]
    fn an_absent_dtype_reads_as_f32_the_way_the_wire_contract_says() {
        let payload = 0.5f32.to_le_bytes();
        let without_dtype = HandWrittenBag {
            dtype: None,
            ..HandWrittenBag::new(&payload, 1, 1)
        }
        .onto_the_wire();

        let read = read_an_audio_block_off_the_wire(&without_dtype).expect("decodes");
        assert_eq!(read.dtype, AudioBlockSampleDtype::F32);
    }

    #[test]
    fn a_bag_carrying_extra_keys_is_read_rather_than_refused() {
        let payload = 0.5f32.to_le_bytes();
        let with_extra = HandWrittenBag {
            recorded_by: Some("a producer this stage never heard of"),
            ..HandWrittenBag::new(&payload, 1, 1)
        }
        .onto_the_wire();

        read_an_audio_block_off_the_wire(&with_extra).expect("the map is open");
    }

    #[test]
    fn an_unknown_dtype_is_refused_naming_the_value_and_the_legal_ones() {
        let wire = HandWrittenBag {
            dtype: Some("f64"),
            ..HandWrittenBag::new(&[0u8; 8], 1, 1)
        }
        .onto_the_wire();

        let refusal =
            read_an_audio_block_off_the_wire(&wire).expect_err("an unknown dtype is refused");
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("f64") && rendered.contains("f32") && rendered.contains("i16"),
            "the refusal must name the value and the legal ones; got {rendered}"
        );
    }

    #[test]
    fn a_payload_that_disagrees_with_the_count_is_refused_naming_both_lengths() {
        let wire = HandWrittenBag::new(&[0u8; 12], 2, 4).onto_the_wire();

        let refusal = read_an_audio_block_off_the_wire(&wire)
            .expect_err("a payload that does not agree with the count is refused");
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("12") && rendered.contains("32"),
            "the refusal must name the carried and the expected length; got {rendered}"
        );
    }

    #[test]
    fn a_bag_with_no_audio_block_keys_is_refused_rather_than_reshaped() {
        let wire = rmp_serde::to_vec_named(&std::collections::BTreeMap::from([
            ("width", 1920),
            ("height", 1080),
        ]))
        .expect("encodes");

        let refusal = read_an_audio_block_off_the_wire(&wire)
            .expect_err("a bag with no audio-block keys is refused");
        assert!(
            matches!(refusal, AudioBlockWireRefusal::NotAnAudioBlockBag { .. }),
            "got {refusal:?}"
        );
    }

    #[test]
    fn an_i16_contract_saturates_at_both_endpoints_rather_than_wrapping() {
        let wire = encode_an_audio_block_onto_the_wire(
            &[2.0, -2.0, 1.0, -1.0, 0.0],
            16_000,
            1,
            5,
            AudioBlockSampleDtype::I16,
            0,
        )
        .expect("encodes");

        let read = read_an_audio_block_off_the_wire(&wire).expect("decodes");
        let scalars: Vec<i16> = read
            .interleaved_sample_bytes
            .chunks_exact(2)
            .map(|scalar| i16::from_le_bytes([scalar[0], scalar[1]]))
            .collect();
        assert_eq!(scalars, vec![32_767, -32_768, 32_767, -32_768, 0]);
    }

    /// The two scales are inverses, so a source that was already `i16` reaches
    /// an `i16` contract unchanged — no half-LSB creep per hop.
    #[test]
    fn an_i16_scalar_survives_the_decode_and_encode_round_trip_unchanged() {
        let source: Vec<i16> = vec![-32_768, -1, 0, 1, 32_767];
        let payload: Vec<u8> = source
            .iter()
            .flat_map(|scalar| scalar.to_le_bytes())
            .collect();
        let wire = HandWrittenBag {
            dtype: Some("i16"),
            first_sample_timestamp_ns: 0,
            ..HandWrittenBag::new(&payload, 1, 5)
        }
        .onto_the_wire();

        let read = read_an_audio_block_off_the_wire(&wire).expect("decodes");
        let re_encoded = encode_an_audio_block_onto_the_wire(
            &read.interleaved_samples_as_f32().collect::<Vec<_>>(),
            16_000,
            1,
            5,
            AudioBlockSampleDtype::I16,
            0,
        )
        .expect("encodes");

        let read_back = read_an_audio_block_off_the_wire(&re_encoded).expect("decodes");
        let round_tripped: Vec<i16> = read_back
            .interleaved_sample_bytes
            .chunks_exact(2)
            .map(|scalar| i16::from_le_bytes([scalar[0], scalar[1]]))
            .collect();
        assert_eq!(round_tripped, source);
    }
}
