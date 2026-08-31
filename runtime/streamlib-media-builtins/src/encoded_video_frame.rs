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
//! forwards a stream it knows is broken. A bag a reader cannot read is
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
    /// The bag is not a named map carrying the encoded-frame keys.
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
                "the bag carries no encoded-video-frame keys ({decode_failure}) — the reader \
                 reads `codec`, `bitstream`, `is_sync_point`, `group_index`, `sequence_index`, \
                 `width`, `height` and `color`"
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

/// The ordering pair `(group_index, sequence_index)` an encoded frame
/// carries, accounted per published frame by its producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedFrameOrderingPair {
    /// Index of the sync-point-delimited group the frame opens or extends.
    pub group_index: u64,
    /// Publication-order index of the frame within the session.
    pub sequence_index: u64,
}

/// Per-producer counter for the ordering pair: a sync point after the
/// first frame opens the next group, and `sequence_index` never resets —
/// the property a consumer's gap detection rests on.
#[derive(Debug, Default)]
pub struct EncodedFrameOrderingPairCounter {
    frames_accounted: u64,
    current_group_index: u64,
}

impl EncodedFrameOrderingPairCounter {
    /// Account one published frame, handing back the pair it carries.
    pub fn account_published_frame(&mut self, is_sync_point: bool) -> EncodedFrameOrderingPair {
        if is_sync_point && self.frames_accounted > 0 {
            self.current_group_index += 1;
        }
        let pair = EncodedFrameOrderingPair {
            group_index: self.current_group_index,
            sequence_index: self.frames_accounted,
        };
        self.frames_accounted += 1;
        pair
    }
}

/// What the loss doctrine says to do with one arriving encoded frame, given
/// everything the gate has seen on its link before it.
///
/// `#[must_use]` because dropping it is a silent bug: a reader that ignores
/// the disposition decodes a frame the doctrine said to discard, and nothing
/// downstream can tell.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivingEncodedFrameDisposition {
    /// Feed it: it continues a stream whose continuity is intact.
    Decode,
    /// Reset the reader's state, then feed it: this frame is the sync point
    /// that re-enters a stream whose continuity was broken.
    ReEnterAtThisSyncPoint,
    /// Discard it: the stream's continuity is broken and this frame is not a
    /// re-entry point, so its reference frames were never seen.
    DiscardUntilTheNextSyncPoint,
}

/// Per-link gate applying the decided loss doctrine to an encoded stream: a
/// consumer that sees a `sequence_index` gap discards until the producer's
/// next sync point, and never forwards a stream it knows is broken.
///
/// Consumer-side twin of [`EncodedFrameOrderingPairCounter`], and codec-free
/// for the same reason it is: both read only the convention's own ordering
/// fields, so every decoder of every codec shares one of each.
///
/// A gate opens broken — [`Default`] says so, because that is the invariant
/// and not a step a caller can forget. The first bag a subscriber receives is
/// not necessarily the first bag the producer published: an attach mid-group
/// hands over frames whose sync point is already gone, and feeding those is
/// exactly how a decoder ends a run having decoded nothing.
#[derive(Debug)]
pub struct EncodedStreamSyncPointGate {
    /// `None` until the first frame arrives; afterwards the newest
    /// `sequence_index` seen, decoded or discarded.
    newest_sequence_index_seen: Option<u64>,
    awaiting_a_sync_point: bool,
    frames_lost_to_gaps: u64,
    frames_discarded_awaiting_a_sync_point: u64,
    sync_points_entered_at: u64,
}

impl Default for EncodedStreamSyncPointGate {
    fn default() -> Self {
        Self::opening_at_the_next_sync_point()
    }
}

impl EncodedStreamSyncPointGate {
    /// Open a gate that has seen nothing and is therefore waiting for a sync
    /// point to enter the stream at.
    pub fn opening_at_the_next_sync_point() -> Self {
        Self {
            newest_sequence_index_seen: None,
            awaiting_a_sync_point: true,
            frames_lost_to_gaps: 0,
            frames_discarded_awaiting_a_sync_point: 0,
            sync_points_entered_at: 0,
        }
    }

    /// Admit one arriving frame, accounting the gap it exposes.
    pub fn admit(
        &mut self,
        sequence_index: u64,
        is_sync_point: bool,
    ) -> ArrivingEncodedFrameDisposition {
        if let Some(newest_seen) = self.newest_sequence_index_seen
            && sequence_index.checked_sub(newest_seen) != Some(1)
        {
            // Any step other than exactly one breaks continuity: a forward
            // jump is loss, and a repeat or a step backwards is a producer
            // this reader's decode state cannot describe either way. The
            // indices come off the wire unchecked, so the arithmetic that
            // measures the gap must survive any pair of them.
            self.frames_lost_to_gaps = self
                .frames_lost_to_gaps
                .saturating_add(sequence_index.saturating_sub(newest_seen).saturating_sub(1));
            self.awaiting_a_sync_point = true;
        }
        self.newest_sequence_index_seen = Some(sequence_index);

        if !self.awaiting_a_sync_point {
            return ArrivingEncodedFrameDisposition::Decode;
        }
        if is_sync_point {
            self.awaiting_a_sync_point = false;
            self.sync_points_entered_at += 1;
            return ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint;
        }
        self.frames_discarded_awaiting_a_sync_point += 1;
        ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
    }

    /// Break continuity deliberately, so the next sync point is entered as a
    /// fresh stream. For a reader that has learned something the ordering
    /// pair cannot tell it — a producer that renegotiated its extent, say.
    pub fn break_continuity(&mut self) {
        self.awaiting_a_sync_point = true;
    }

    /// How many frames the `sequence_index` gaps say the link lost.
    pub fn frames_lost_to_gaps(&self) -> u64 {
        self.frames_lost_to_gaps
    }

    /// How many arriving frames were discarded because they were not a
    /// re-entry point into a broken stream.
    pub fn frames_discarded_awaiting_a_sync_point(&self) -> u64 {
        self.frames_discarded_awaiting_a_sync_point
    }

    /// How many times the gate has entered the stream — once in a healthy
    /// run, once more per break.
    pub fn sync_points_entered_at(&self) -> u64 {
        self.sync_points_entered_at
    }
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

    /// The #1077 shape, as logic: a subscriber that attaches mid-GOP is
    /// handed slices whose IDR is already gone. Feeding those is what ends
    /// a run at `frames_decoded = 0`; the gate discards them and enters at
    /// the producer's next sync point instead.
    #[test]
    fn a_stream_joined_mid_group_is_discarded_until_its_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();

        assert_eq!(
            gate.admit(7, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(8, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(9, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(10, false),
            ArrivingEncodedFrameDisposition::Decode
        );
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
        // Contiguous arrivals are not loss, however late the join was.
        assert_eq!(gate.frames_lost_to_gaps(), 0);
    }

    /// The decided loss doctrine: a `sequence_index` gap breaks the stream,
    /// and every frame until the producer's next sync point is discarded
    /// rather than decoded against reference frames that were never seen.
    #[test]
    fn a_sequence_index_gap_discards_until_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::Decode
        );

        // 2 and 3 were overwritten in the ring; 4 is a non-sync-point.
        assert_eq!(
            gate.admit(4, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(5, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(6, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(7, false),
            ArrivingEncodedFrameDisposition::Decode
        );

        assert_eq!(gate.frames_lost_to_gaps(), 2);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
    }

    /// A gap landing exactly on a sync point costs nothing but the gap: the
    /// sync point is itself the re-entry point, so nothing is discarded.
    #[test]
    fn a_gap_landing_on_a_sync_point_re_enters_without_discarding_anything() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(30, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.frames_lost_to_gaps(), 29);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 0);
    }

    /// `sequence_index` is monotonic for the life of a producer, so a repeat
    /// or a step backwards describes a stream this reader's decode state
    /// cannot continue — it re-enters rather than decoding on.
    #[test]
    fn a_sequence_index_that_does_not_advance_by_one_breaks_continuity_too() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::Decode
        );
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(0, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        // Neither backwards step is counted as frames lost — no frame went
        // missing, the producer's numbering stopped making sense.
        assert_eq!(gate.frames_lost_to_gaps(), 0);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
    }

    /// The invariant the type exists for, stated where a caller cannot skip
    /// it: a gate nobody configured is still waiting for a sync point.
    ///
    /// Mental revert: `#[derive(Default)]` on the gate. The default becomes
    /// permissive, a reader that never calls the named constructor admits
    /// whatever bag arrives first, and this is what notices.
    #[test]
    fn a_gate_nobody_configured_still_opens_at_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::default();
        assert_eq!(
            gate.admit(41, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(42, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
    }

    /// The indices come off the wire unchecked, so the gap arithmetic must
    /// survive any pair of them rather than overflowing on a hostile one.
    #[test]
    fn a_sequence_index_at_the_top_of_its_range_does_not_overflow_the_gap_arithmetic() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(u64::MAX, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(u64::MAX, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(gate.frames_lost_to_gaps(), 0);

        // A hostile producer alternating the extremes accumulates two
        // near-u64::MAX gaps; the tally saturates rather than wrapping or
        // panicking under overflow checks.
        let mut alternating = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        for hostile_index in [0, u64::MAX, 0, u64::MAX] {
            assert_eq!(
                alternating.admit(hostile_index, true),
                ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
            );
        }
        assert_eq!(alternating.frames_lost_to_gaps(), u64::MAX);
    }

    /// A reader that learned of a discontinuity the ordering pair cannot
    /// show it — a producer that renegotiated its extent — re-enters the
    /// same way a gap does.
    #[test]
    fn a_deliberately_broken_continuity_re_enters_at_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::Decode
        );
        gate.break_continuity();
        assert_eq!(
            gate.admit(2, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(3, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.sync_points_entered_at(), 2);
        // A break is not loss: nothing went missing on the wire.
        assert_eq!(gate.frames_lost_to_gaps(), 0);
    }

    /// The ordering pair a consumer's gap detection rests on: the sequence
    /// index never resets, and a sync point after the first frame opens the
    /// next group.
    #[test]
    fn a_sync_point_opens_the_next_group_and_the_sequence_never_resets() {
        let mut counter = EncodedFrameOrderingPairCounter::default();
        let published: Vec<EncodedFrameOrderingPair> = [true, false, false, true, false, true]
            .into_iter()
            .map(|is_sync_point| counter.account_published_frame(is_sync_point))
            .collect();

        let group_indices: Vec<u64> = published.iter().map(|pair| pair.group_index).collect();
        let sequence_indices: Vec<u64> = published.iter().map(|pair| pair.sequence_index).collect();
        assert_eq!(group_indices, vec![0, 0, 0, 1, 1, 2]);
        assert_eq!(sequence_indices, vec![0, 1, 2, 3, 4, 5]);
    }
}
