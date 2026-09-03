// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The encoded-video-frame bag convention the codec built-ins produce and
//! consume — the tree's first encoded-domain link.
//!
//! A link carries a self-describing msgpack named map; these types are the
//! optional Rust cast for it — never declared on a port, never registered
//! anywhere. The keys ARE the wire contract: `codec`, `bitstream`,
//! `is_sync_point`, `group_index`, `sequence_index`, `width`, `height`,
//! `color`. The map is open — a producer may carry extra keys and this cast
//! ignores them, matching every other cast in this crate. The frame's
//! timestamp rides the frame header like every bag; it is not a bag field.
//!
//! Unlike a video frame, an encoded frame carries its payload inline: one
//! Annex-B access unit rides the bag as msgpack `bin`. No surface ids, no
//! claims, no lifetime contract.
//!
//! A consumer of an encoded stream must bound loss: on a `sequence_index`
//! gap it discards until the producer's next `is_sync_point`, and never
//! forwards a stream it knows is broken. That doctrine reads only the
//! ordering fields, so it lives medium-free in
//! [`crate::encoded_stream_ordering`]. A bag a reader cannot read is
//! refused by name via [`read_encoded_video_frame_bag`], never reshaped.

use serde::{Deserialize, Serialize};

use crate::video_frame::ColorInfo;

/// Elementary-stream identity of an encoded frame's bitstream, spelled the
/// way the wire spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodedVideoCodec {
    #[serde(rename = "h264")]
    H264,
    #[serde(rename = "h265")]
    H265,
}

impl EncodedVideoCodec {
    /// Every codec the convention legalises, in wire order.
    pub const ALL: [EncodedVideoCodec; 2] = [EncodedVideoCodec::H264, EncodedVideoCodec::H265];

    /// The wire spelling, which is also what a refusal names. The serde
    /// renames on the enum are the same strings, locked together by the
    /// wire-key test.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            EncodedVideoCodec::H264 => "h264",
            EncodedVideoCodec::H265 => "h265",
        }
    }

    /// Read a wire spelling, or `None` for one this convention does not
    /// legalise.
    pub fn from_wire_str(codec: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|legal_codec| legal_codec.as_wire_str() == codec)
    }
}

/// Encoded video frame bag: one Annex-B access unit riding the link inline,
/// described by the codec, ordering pair, and coded extent beside it.
///
/// `group_index` / `sequence_index` are the MoQ-mappable ordering pair:
/// `sequence_index` is monotonic in publication order for the life of the
/// producer — it survives a session re-mint, so a gap is always loss and
/// never a restart — `group_index` counts sync points, and a consumer that
/// sees a `sequence_index` gap discards to the next `is_sync_point`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodedVideoFrame {
    /// Elementary-stream identity — `"h264"` / `"h265"` on the wire.
    pub codec: EncodedVideoCodec,
    /// One Annex-B access unit (start-code-prefixed NAL units; parameter
    /// sets prepended at sync points per the producing session's config).
    ///
    /// The wire key is `bitstream` and the payload is msgpack `bin` — a
    /// byte buffer, never an array of numbers.
    #[serde(rename = "bitstream", with = "serde_bytes")]
    pub annex_b_access_unit_bytes: Vec<u8>,
    /// `true` on a group boundary — an IDR (H.264) / CRA-class (H.265)
    /// access unit a decoder can enter the stream at.
    pub is_sync_point: bool,
    /// Index of the sync-point-delimited group this frame belongs to.
    pub group_index: u64,
    /// Publication-order index of this frame within its producer — never
    /// resets at a group boundary or a session re-mint, so a gap is always
    /// visible.
    pub sequence_index: u64,
    /// Coded width before the conformance crop (the codec-aligned extent).
    pub width: u32,
    /// Coded height before the conformance crop (the codec-aligned extent).
    pub height: u32,
    /// H.273 tuple describing the encoded stream's color, as baked into the
    /// bitstream's parameter sets. Absent means unspecified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorInfo>,
}

/// Why a bag could not be read as an encoded video frame, in the terms the
/// refusal names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedVideoFrameBagRefusal {
    /// The bag could not be decoded as the encoded-frame named map — a key
    /// missing, or a value the reader cannot read, which the failure names.
    NotAnEncodedVideoFrameBag { decode_failure: String },
    /// `codec` names an elementary stream this reader does not know.
    UnknownCodec { codec: String },
    /// The bag carries no `codec` at all, so no decoder can be chosen for
    /// its bitstream.
    MissingCodec,
}

impl std::error::Error for EncodedVideoFrameBagRefusal {}

impl std::fmt::Display for EncodedVideoFrameBagRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodedVideoFrameBagRefusal::NotAnEncodedVideoFrameBag { decode_failure } => write!(
                formatter,
                "the bag could not be read as an encoded video frame ({decode_failure}) — the \
                 reader reads `codec`, `bitstream`, `is_sync_point`, `group_index`, \
                 `sequence_index`, `width`, `height` and `color`"
            ),
            EncodedVideoFrameBagRefusal::UnknownCodec { codec } => write!(
                formatter,
                "`codec` is `\"{codec}\"`, which names no elementary stream this reader can \
                 decode — it reads `\"{}\"` and `\"{}\"`",
                EncodedVideoCodec::H264.as_wire_str(),
                EncodedVideoCodec::H265.as_wire_str(),
            ),
            EncodedVideoFrameBagRefusal::MissingCodec => write!(
                formatter,
                "the bag names no `codec`, so no decoder can be chosen for its bitstream — \
                 producers write `\"{}\"` or `\"{}\"`",
                EncodedVideoCodec::H264.as_wire_str(),
                EncodedVideoCodec::H265.as_wire_str(),
            ),
        }
    }
}

/// The named map with `codec` still a wire string, so an unknown codec is
/// refused naming the string rather than failing the whole decode opaquely.
#[derive(Deserialize)]
struct EncodedVideoFrameNamedMapOnTheWire {
    #[serde(default)]
    codec: Option<String>,
    #[serde(rename = "bitstream", with = "serde_bytes")]
    annex_b_access_unit_bytes: Vec<u8>,
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    width: u32,
    height: u32,
    #[serde(default)]
    color: Option<ColorInfo>,
}

/// Read a bag's msgpack bytes as an [`EncodedVideoFrame`], refusing a bag
/// this reader cannot read by name — never reshaping it into a
/// plausible-looking wrong answer. Extra keys are read past, not refused.
pub fn read_encoded_video_frame_bag(
    bag_bytes: &[u8],
) -> std::result::Result<EncodedVideoFrame, EncodedVideoFrameBagRefusal> {
    let on_the_wire: EncodedVideoFrameNamedMapOnTheWire = rmp_serde::from_slice(bag_bytes)
        .map_err(
            |decode_failure| EncodedVideoFrameBagRefusal::NotAnEncodedVideoFrameBag {
                decode_failure: decode_failure.to_string(),
            },
        )?;
    let codec = match on_the_wire.codec.as_deref() {
        Some(wire_codec) => EncodedVideoCodec::from_wire_str(wire_codec).ok_or_else(|| {
            EncodedVideoFrameBagRefusal::UnknownCodec {
                codec: wire_codec.to_string(),
            }
        })?,
        None => return Err(EncodedVideoFrameBagRefusal::MissingCodec),
    };
    Ok(EncodedVideoFrame {
        codec,
        annex_b_access_unit_bytes: on_the_wire.annex_b_access_unit_bytes,
        is_sync_point: on_the_wire.is_sync_point,
        group_index: on_the_wire.group_index,
        sequence_index: on_the_wire.sequence_index,
        width: on_the_wire.width,
        height: on_the_wire.height,
        color: on_the_wire.color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_wire_test_support::{
        decode_msgpack_named_map_entries, wire_map_entry_named,
    };
    use crate::video_frame::{Matrix, Primaries, Range, Transfer};

    fn an_encoded_frame() -> EncodedVideoFrame {
        EncodedVideoFrame {
            codec: EncodedVideoCodec::H264,
            annex_b_access_unit_bytes: vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88],
            is_sync_point: true,
            group_index: 3,
            sequence_index: 91,
            width: 1920,
            height: 1088,
            color: Some(ColorInfo {
                primaries: Some(Primaries::Bt709),
                transfer: Some(Transfer::Bt709),
                matrix: Some(Matrix::Bt709),
                range: Some(Range::Limited),
            }),
        }
    }

    /// The actual wire is msgpack via `rmp_serde::to_vec_named` (what
    /// `OutputWriter::write` does) — lock the named-map encoding and the
    /// documented keys at that boundary. `bitstream` is asserted as the
    /// msgpack *binary* type: a `Vec<u8>` without `serde_bytes` would encode
    /// as an array of numbers, a shape another language reads as a list.
    #[test]
    fn encoded_video_frame_msgpack_wire_carries_the_documented_keys() {
        let frame = an_encoded_frame();
        let wire_bytes = rmp_serde::to_vec_named(&frame).expect("msgpack serialize");
        let entries = decode_msgpack_named_map_entries(&wire_bytes);
        let key = |name: &str| wire_map_entry_named(&entries, name);

        assert_eq!(key("codec").as_str(), Some("h264"));
        assert_eq!(
            key("bitstream"),
            rmpv::Value::Binary(frame.annex_b_access_unit_bytes.clone()),
            "the bitstream must ride the wire as msgpack bin, never an array"
        );
        assert_eq!(key("is_sync_point").as_bool(), Some(true));
        assert_eq!(key("group_index").as_u64(), Some(3));
        assert_eq!(key("sequence_index").as_u64(), Some(91));
        assert_eq!(key("width").as_u64(), Some(1920));
        assert_eq!(key("height").as_u64(), Some(1088));
        let color = key("color");
        let color_map = color.as_map().expect("color is a named map");
        assert_eq!(
            wire_map_entry_named(color_map, "primaries").as_str(),
            Some("bt709")
        );
    }

    #[test]
    fn encoded_video_frame_wire_round_trips_through_the_reader() {
        let frame = an_encoded_frame();
        let wire_bytes = rmp_serde::to_vec_named(&frame).expect("msgpack serialize");
        let read_back = read_encoded_video_frame_bag(&wire_bytes).expect("readable bag");
        assert_eq!(read_back, frame);
    }

    /// The bag map is open: a producer carrying extra keys must be read,
    /// not refused — by the reader and by the serde cast alike.
    #[test]
    fn a_bag_carrying_extra_keys_is_read_rather_than_refused() {
        let mut frame = an_encoded_frame();
        frame.color = None;
        let mut value = rmpv::Value::Map(decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&frame).expect("msgpack serialize"),
        ));
        if let rmpv::Value::Map(entries) = &mut value {
            entries.push((
                rmpv::Value::from("a_future_key"),
                rmpv::Value::from("ignored"),
            ));
        }
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &value).expect("re-encode");

        let read_back = read_encoded_video_frame_bag(&wire_bytes).expect("open map");
        assert_eq!(read_back, frame);
        let cast: EncodedVideoFrame = rmp_serde::from_slice(&wire_bytes).expect("open map");
        assert_eq!(cast, frame);
    }

    #[test]
    fn a_bag_with_no_encoded_frame_keys_is_refused_naming_the_keys() {
        let audio_shaped_bag = rmp_serde::to_vec_named(&serde_json::json!({
            "samples": "not even close",
            "sample_rate": 48_000,
        }))
        .expect("msgpack serialize");
        let refusal = read_encoded_video_frame_bag(&audio_shaped_bag).expect_err("must be refused");
        assert!(matches!(
            refusal,
            EncodedVideoFrameBagRefusal::NotAnEncodedVideoFrameBag { .. }
        ));
        assert!(
            refusal.to_string().contains("`bitstream`"),
            "the refusal names the keys the reader reads: {refusal}"
        );
    }

    /// The serde renames and `as_wire_str` are two spellings of one
    /// vocabulary; this is what keeps them one.
    #[test]
    fn the_serde_spelling_and_the_wire_str_pair_agree_for_every_codec() {
        for codec in EncodedVideoCodec::ALL {
            let serde_spelling = serde_json::to_value(codec).expect("serialize");
            assert_eq!(serde_spelling, codec.as_wire_str());
            assert_eq!(
                EncodedVideoCodec::from_wire_str(codec.as_wire_str()),
                Some(codec)
            );
        }
        assert_eq!(EncodedVideoCodec::from_wire_str("av1"), None);
    }

    #[test]
    fn a_bag_carrying_no_codec_at_all_is_refused_as_missing() {
        let mut frame = an_encoded_frame();
        frame.color = None;
        let entries = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&frame).expect("msgpack serialize"),
        )
        .into_iter()
        .filter(|(key, _)| key.as_str() != Some("codec"))
        .collect();
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries)).expect("re-encode");

        let refusal = read_encoded_video_frame_bag(&wire_bytes).expect_err("must be refused");
        assert_eq!(refusal, EncodedVideoFrameBagRefusal::MissingCodec);
        assert!(
            refusal.to_string().contains("no `codec`"),
            "the refusal says what is missing: {refusal}"
        );
    }

    #[test]
    fn a_bag_naming_an_unknown_codec_is_refused_naming_the_codec() {
        let mut frame = an_encoded_frame();
        frame.color = None;
        let mut entries = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&frame).expect("msgpack serialize"),
        );
        for (key, entry_value) in &mut entries {
            if key.as_str() == Some("codec") {
                *entry_value = rmpv::Value::from("av1");
            }
        }
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries)).expect("re-encode");

        let refusal = read_encoded_video_frame_bag(&wire_bytes).expect_err("must be refused");
        assert_eq!(
            refusal,
            EncodedVideoFrameBagRefusal::UnknownCodec {
                codec: "av1".to_string()
            }
        );
        assert!(
            refusal.to_string().contains("av1"),
            "the refusal names the codec it cannot decode: {refusal}"
        );
    }

    /// One unplaceable colour name used to be reported as the bag carrying
    /// no encoded-frame keys at all; the refusal names the axis and the
    /// value, and never claims the keys are missing.
    #[test]
    fn a_colour_name_the_vocabulary_cannot_place_is_refused_naming_the_axis_not_the_keys() {
        let mut entries = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&an_encoded_frame()).expect("msgpack serialize"),
        );
        for (key, entry_value) in &mut entries {
            if key.as_str() == Some("color") {
                *entry_value = rmpv::Value::Map(vec![(
                    rmpv::Value::from("primaries"),
                    rmpv::Value::from("bt_709"),
                )]);
            }
        }
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries)).expect("re-encode");

        let refusal = read_encoded_video_frame_bag(&wire_bytes).expect_err("must be refused");
        assert!(matches!(
            refusal,
            EncodedVideoFrameBagRefusal::NotAnEncodedVideoFrameBag { .. }
        ));
        let text = refusal.to_string();
        assert!(
            text.contains("`primaries`") && text.contains("bt_709"),
            "the refusal names the axis and the value: {text}"
        );
        assert!(
            !text.contains("carries no"),
            "the refusal must not claim the keys are missing: {text}"
        );
    }

    #[test]
    fn absent_color_stays_off_the_wire() {
        let mut frame = an_encoded_frame();
        frame.color = None;
        let wire_bytes = rmp_serde::to_vec_named(&frame).expect("msgpack serialize");
        let entries = decode_msgpack_named_map_entries(&wire_bytes);
        assert!(
            !entries.iter().any(|(key, _)| key.as_str() == Some("color")),
            "absent optionals stay off the wire"
        );
    }
}
