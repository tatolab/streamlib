// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The encoded-audio-packet bag convention the Opus built-ins produce and
//! consume — the encoded-frame convention applied to audio.
//!
//! A link carries a self-describing msgpack named map; these types are the
//! optional Rust cast for it — never declared on a port, never registered
//! anywhere. The keys ARE the wire contract: `codec`, `bitstream`,
//! `is_sync_point`, `group_index`, `sequence_index`, `sample_rate`,
//! `channels`, `sample_count`, `pre_skip`. The map is open — a producer may
//! carry extra keys and this cast ignores them, matching every other cast in
//! this crate. The packet's timestamp rides the frame header like every bag;
//! it is not a bag field.
//!
//! *Packet*, not *frame*: RFC 6716 §3 spends the word "frame" on a
//! subdivision of one Opus packet, so a type named for the frame would mean
//! two things at the seam it crosses. One bag carries exactly one Opus
//! packet.
//!
//! `is_sync_point` is `true` on every packet — a decoder enters an Opus
//! stream at any of them — so each packet is its own group and the loss
//! doctrine in [`crate::encoded_stream_ordering`] collapses to "a
//! `sequence_index` step other than one re-enters here". A bag a reader
//! cannot read is refused by name via [`read_encoded_audio_packet_bag`],
//! never reshaped.

use serde::{Deserialize, Serialize};

/// `bitstream` as msgpack `bin` in both directions, refusing every other
/// msgpack type rather than coercing it.
///
/// Wire contract, and the reason this is not `serde_bytes`: that visitor
/// also implements `visit_seq`, so a producer sending the packet as a
/// msgpack **array** of integers — five bytes per byte, and the shape a
/// hand-rolled encoder in another language most easily gets wrong — is read
/// back as a plausible-looking payload instead of being refused. This
/// visitor implements the two byte arms and nothing else, so serde's own
/// `invalid_type` is what an array gets.
mod bitstream_as_msgpack_bin {
    use serde::de::{Error, Visitor};
    use serde::{Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    struct OnlyAMsgpackBinaryPayload;

    impl<'de> Visitor<'de> for OnlyAMsgpackBinaryPayload {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("`bitstream` as a msgpack binary payload")
        }

        fn visit_bytes<E: Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(bytes.to_vec())
        }

        fn visit_byte_buf<E: Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
            Ok(bytes)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        deserializer.deserialize_bytes(OnlyAMsgpackBinaryPayload)
    }
}

/// Elementary-stream identity of an encoded packet's bitstream, spelled the
/// way the wire spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodedAudioCodec {
    #[serde(rename = "opus")]
    Opus,
}

impl EncodedAudioCodec {
    /// Every codec the convention legalises, in wire order.
    pub const ALL: [EncodedAudioCodec; 1] = [EncodedAudioCodec::Opus];

    /// The wire spelling, which is also what a refusal names. The serde
    /// renames on the enum are the same strings, locked together by the
    /// wire-key test.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            EncodedAudioCodec::Opus => "opus",
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

/// Encoded audio packet bag: one Opus packet riding the link inline,
/// described by the codec, ordering pair, and stream format beside it.
///
/// `group_index` / `sequence_index` are the same MoQ-mappable ordering pair
/// an encoded video frame carries, accounted by the same counter:
/// `sequence_index` is monotonic in publication order for the life of the
/// producer — it survives an encoder re-mint, so a gap is always loss and
/// never a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAudioPacket {
    /// Elementary-stream identity — `"opus"` on the wire.
    pub codec: EncodedAudioCodec,
    /// One Opus packet, as RFC 6716 §3 frames it: the TOC byte and the
    /// frames it describes, exactly as `opus_encode_float` produced them.
    ///
    /// The wire key is `bitstream` and the payload is msgpack `bin` — a
    /// byte buffer, never an array of numbers.
    #[serde(rename = "bitstream", with = "bitstream_as_msgpack_bin")]
    pub opus_packet_bytes: Vec<u8>,
    /// `true` on every Opus packet: a decoder enters the stream at any of
    /// them, so the flag is a constant of the convention rather than a
    /// property of the packet.
    pub is_sync_point: bool,
    /// Index of the sync-point-delimited group this packet belongs to.
    /// Every packet is a sync point, so every packet is its own group.
    pub group_index: u64,
    /// Publication-order index of this packet within its producer — never
    /// resets at a group boundary or an encoder re-mint, so a gap is always
    /// visible.
    pub sequence_index: u64,
    /// Always 48 000: Opus's own clock, the rate a decoder reconstructs at
    /// whatever the source was resampled from.
    pub sample_rate: u32,
    /// Channel count the packet decodes to — the declared output count, not
    /// the mono/stereo the TOC byte codes each frame at.
    pub channels: u32,
    /// Per-channel samples the packet spans — 960 at the 20 ms framing this
    /// convention uses, the unit `AudioBlock.sample_count` already counts in.
    pub sample_count: u32,
    /// The encoder's lookahead in 48 kHz samples: what a decoder discards at
    /// entry so its first emitted sample is the stamped instant.
    pub pre_skip: u32,
}

/// Why a bag could not be read as an encoded audio packet, in the terms the
/// refusal names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedAudioPacketBagRefusal {
    /// The bag could not be decoded as the encoded-packet named map — a key
    /// missing, or a value the reader cannot read, which the failure names.
    NotAnEncodedAudioPacketBag { decode_failure: String },
    /// `codec` names an elementary stream this reader does not know.
    UnknownCodec { codec: String },
    /// The bag carries no `codec` at all, so no decoder can be chosen for
    /// its bitstream.
    MissingCodec,
}

impl std::error::Error for EncodedAudioPacketBagRefusal {}

impl std::fmt::Display for EncodedAudioPacketBagRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodedAudioPacketBagRefusal::NotAnEncodedAudioPacketBag { decode_failure } => write!(
                formatter,
                "the bag could not be read as an encoded audio packet ({decode_failure}) — the \
                 reader reads `codec`, `bitstream`, `is_sync_point`, `group_index`, \
                 `sequence_index`, `sample_rate`, `channels`, `sample_count` and `pre_skip`"
            ),
            EncodedAudioPacketBagRefusal::UnknownCodec { codec } => write!(
                formatter,
                "`codec` is `\"{codec}\"`, which names no elementary stream this reader can \
                 decode — it reads `\"{}\"`",
                EncodedAudioCodec::Opus.as_wire_str(),
            ),
            EncodedAudioPacketBagRefusal::MissingCodec => write!(
                formatter,
                "the bag names no `codec`, so no decoder can be chosen for its bitstream — \
                 producers write `\"{}\"`",
                EncodedAudioCodec::Opus.as_wire_str(),
            ),
        }
    }
}

/// The named map with `codec` still a wire string, so an unknown codec is
/// refused naming the string rather than failing the whole decode opaquely.
#[derive(Deserialize)]
struct EncodedAudioPacketNamedMapOnTheWire {
    #[serde(default)]
    codec: Option<String>,
    #[serde(rename = "bitstream", with = "bitstream_as_msgpack_bin")]
    opus_packet_bytes: Vec<u8>,
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    pre_skip: u32,
}

/// Read a bag's msgpack bytes as an [`EncodedAudioPacket`], refusing a bag
/// this reader cannot read by name — never reshaping it into a
/// plausible-looking wrong answer. Extra keys are read past, not refused.
pub fn read_encoded_audio_packet_bag(
    bag_bytes: &[u8],
) -> std::result::Result<EncodedAudioPacket, EncodedAudioPacketBagRefusal> {
    let on_the_wire: EncodedAudioPacketNamedMapOnTheWire = rmp_serde::from_slice(bag_bytes)
        .map_err(
            |decode_failure| EncodedAudioPacketBagRefusal::NotAnEncodedAudioPacketBag {
                decode_failure: decode_failure.to_string(),
            },
        )?;
    let codec = match on_the_wire.codec.as_deref() {
        Some(wire_codec) => EncodedAudioCodec::from_wire_str(wire_codec).ok_or_else(|| {
            EncodedAudioPacketBagRefusal::UnknownCodec {
                codec: wire_codec.to_string(),
            }
        })?,
        None => return Err(EncodedAudioPacketBagRefusal::MissingCodec),
    };
    Ok(EncodedAudioPacket {
        codec,
        opus_packet_bytes: on_the_wire.opus_packet_bytes,
        is_sync_point: on_the_wire.is_sync_point,
        group_index: on_the_wire.group_index,
        sequence_index: on_the_wire.sequence_index,
        sample_rate: on_the_wire.sample_rate,
        channels: on_the_wire.channels,
        sample_count: on_the_wire.sample_count,
        pre_skip: on_the_wire.pre_skip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_wire_test_support::{
        decode_msgpack_named_map_entries, wire_map_entry_named,
    };

    fn an_encoded_packet() -> EncodedAudioPacket {
        EncodedAudioPacket {
            codec: EncodedAudioCodec::Opus,
            opus_packet_bytes: vec![0x78, 0x01, 0x02, 0x03],
            is_sync_point: true,
            group_index: 3,
            sequence_index: 3,
            sample_rate: 48_000,
            channels: 2,
            sample_count: 960,
            pre_skip: 312,
        }
    }

    #[test]
    fn encoded_audio_packet_msgpack_wire_carries_the_documented_keys() {
        let wire_bytes = rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize");
        let entries = decode_msgpack_named_map_entries(&wire_bytes);

        let keys: Vec<&str> = entries.iter().filter_map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "codec",
                "bitstream",
                "is_sync_point",
                "group_index",
                "sequence_index",
                "sample_rate",
                "channels",
                "sample_count",
                "pre_skip",
            ],
            "the keys are the wire contract a consumer in another language reads"
        );
        assert_eq!(
            wire_map_entry_named(&entries, "codec").as_str(),
            Some("opus")
        );
    }

    #[test]
    fn the_bitstream_crosses_the_wire_as_a_binary_payload_not_an_array() {
        let wire_bytes = rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize");
        let entries = decode_msgpack_named_map_entries(&wire_bytes);

        let bitstream = wire_map_entry_named(&entries, "bitstream");
        assert!(
            matches!(bitstream, rmpv::Value::Binary(_)),
            "an Opus packet is msgpack `bin`, never an array of numbers — got {bitstream:?}"
        );
        assert_eq!(bitstream.as_slice(), Some(&[0x78u8, 0x01, 0x02, 0x03][..]));
    }

    #[test]
    fn encoded_audio_packet_wire_round_trips_through_the_reader() {
        let packet = an_encoded_packet();
        let wire_bytes = rmp_serde::to_vec_named(&packet).expect("msgpack serialize");
        assert_eq!(
            read_encoded_audio_packet_bag(&wire_bytes).expect("reads back"),
            packet
        );
    }

    #[test]
    fn the_serde_spelling_and_the_wire_str_pair_agree_for_every_codec() {
        for codec in EncodedAudioCodec::ALL {
            let mut packet = an_encoded_packet();
            packet.codec = codec;
            let wire_bytes = rmp_serde::to_vec_named(&packet).expect("msgpack serialize");
            let entries = decode_msgpack_named_map_entries(&wire_bytes);
            assert_eq!(
                wire_map_entry_named(&entries, "codec").as_str(),
                Some(codec.as_wire_str()),
                "the serde rename and `as_wire_str` are one spelling, not two"
            );
            assert_eq!(
                EncodedAudioCodec::from_wire_str(codec.as_wire_str()),
                Some(codec)
            );
        }
    }

    #[test]
    fn a_bag_carrying_extra_keys_is_read_rather_than_refused() {
        let mut entries = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize"),
        );
        entries.push((
            rmpv::Value::from("a_key_this_reader_has_never_heard_of"),
            rmpv::Value::from(17),
        ));
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries))
            .expect("msgpack encode");

        assert_eq!(
            read_encoded_audio_packet_bag(&wire_bytes).expect("reads past what it does not know"),
            an_encoded_packet()
        );
    }

    #[test]
    fn a_bag_with_no_encoded_packet_keys_is_refused_naming_the_keys() {
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(
            &mut wire_bytes,
            &rmpv::Value::Map(vec![(
                rmpv::Value::from("samples"),
                rmpv::Value::Binary(vec![0, 1, 2, 3]),
            )]),
        )
        .expect("msgpack encode");

        let refusal = read_encoded_audio_packet_bag(&wire_bytes).expect_err("refused");
        assert!(matches!(
            refusal,
            EncodedAudioPacketBagRefusal::NotAnEncodedAudioPacketBag { .. }
        ));
        let named = refusal.to_string();
        for key in [
            "codec",
            "bitstream",
            "is_sync_point",
            "group_index",
            "sequence_index",
            "sample_rate",
            "channels",
            "sample_count",
            "pre_skip",
        ] {
            assert!(
                named.contains(key),
                "the refusal names every key it reads; {key:?} missing from {named:?}"
            );
        }
    }

    #[test]
    fn a_bag_carrying_no_codec_at_all_is_refused_as_missing() {
        let entries: Vec<(rmpv::Value, rmpv::Value)> = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize"),
        )
        .into_iter()
        .filter(|(key, _)| key.as_str() != Some("codec"))
        .collect();
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries))
            .expect("msgpack encode");

        let refusal = read_encoded_audio_packet_bag(&wire_bytes).expect_err("refused");
        assert_eq!(refusal, EncodedAudioPacketBagRefusal::MissingCodec);
        assert!(
            refusal.to_string().contains("opus"),
            "the refusal names what a producer should have written"
        );
    }

    #[test]
    fn a_bag_naming_an_unknown_codec_is_refused_naming_the_codec() {
        let entries: Vec<(rmpv::Value, rmpv::Value)> = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize"),
        )
        .into_iter()
        .map(|(key, value)| match key.as_str() {
            Some("codec") => (key, rmpv::Value::from("vorbis")),
            _ => (key, value),
        })
        .collect();
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries))
            .expect("msgpack encode");

        let refusal = read_encoded_audio_packet_bag(&wire_bytes).expect_err("refused");
        assert_eq!(
            refusal,
            EncodedAudioPacketBagRefusal::UnknownCodec {
                codec: "vorbis".to_string()
            }
        );
        let named = refusal.to_string();
        assert!(named.contains("vorbis"), "names the codec it was handed");
        assert!(named.contains("opus"), "names the one it reads");
    }

    #[test]
    fn a_bitstream_sent_as_an_array_of_numbers_is_refused_rather_than_read() {
        let entries: Vec<(rmpv::Value, rmpv::Value)> = decode_msgpack_named_map_entries(
            &rmp_serde::to_vec_named(&an_encoded_packet()).expect("msgpack serialize"),
        )
        .into_iter()
        .map(|(key, value)| match key.as_str() {
            Some("bitstream") => (
                key,
                rmpv::Value::Array(vec![rmpv::Value::from(0x78), rmpv::Value::from(0x01)]),
            ),
            _ => (key, value),
        })
        .collect();
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &rmpv::Value::Map(entries))
            .expect("msgpack encode");

        assert!(matches!(
            read_encoded_audio_packet_bag(&wire_bytes).expect_err("refused"),
            EncodedAudioPacketBagRefusal::NotAnEncodedAudioPacketBag { .. }
        ));
    }
}
