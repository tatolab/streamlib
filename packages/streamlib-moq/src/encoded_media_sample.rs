// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One encoded bag, in the terms both container formats are written from.
//!
//! The publisher's Python hands one of these across the CPython boundary and
//! the subscriber's Python reads one back, so every key the engine's encoded
//! wire contract requires appears here exactly once — spelled the way the
//! contract spells it, not the way either container happens to store it.
//!
//! `timestamp_ns` is the odd one out: it is not a bag key. A bag's stamp rides
//! the frame header, so it crosses a MoQ object as an ordinary field here and
//! goes back out through `ctx.outputs.write(..., timestamp_ns=...)`.

use std::collections::BTreeMap;

/// The H.273 four-tuple as the bag carries it — an open map of axis to wire
/// spelling, passed through rather than parsed. An axis this wheel does not
/// recognise is still the engine cast's to refuse by name on read; inventing a
/// second opinion here would refuse a stream the engine would have accepted.
pub(crate) type ColorAxesOnTheWire = BTreeMap<String, String>;

/// One encoded bag: an access unit or an Opus packet, never both.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EncodedMediaSample {
    VideoAccessUnit(EncodedVideoAccessUnit),
    AudioPacket(EncodedAudioPacket),
}

impl EncodedMediaSample {
    /// Which medium a track carrying this sample is, for the port it lands on
    /// and the track it is published to.
    pub(crate) fn medium(&self) -> TrackMedium {
        match self {
            EncodedMediaSample::VideoAccessUnit(_) => TrackMedium::Video,
            EncodedMediaSample::AudioPacket(_) => TrackMedium::Audio,
        }
    }

    /// The ordering pair the producer wrote. A group boundary is what opens a
    /// MoQ subgroup, so the publisher reads this before it decides where the
    /// object goes.
    pub(crate) fn ordering_pair(&self) -> (u64, u64) {
        match self {
            EncodedMediaSample::VideoAccessUnit(unit) => (unit.group_index, unit.sequence_index),
            EncodedMediaSample::AudioPacket(packet) => (packet.group_index, packet.sequence_index),
        }
    }

    /// `true` on a bag a decoder can enter the stream at.
    pub(crate) fn is_sync_point(&self) -> bool {
        match self {
            EncodedMediaSample::VideoAccessUnit(unit) => unit.is_sync_point,
            EncodedMediaSample::AudioPacket(packet) => packet.is_sync_point,
        }
    }

    /// The producer's stamp, on `CLOCK_MONOTONIC`.
    pub(crate) fn timestamp_ns(&self) -> i64 {
        match self {
            EncodedMediaSample::VideoAccessUnit(unit) => unit.timestamp_ns,
            EncodedMediaSample::AudioPacket(packet) => packet.timestamp_ns,
        }
    }
}

/// Which of the subscriber's two output ports a sample belongs on, and which
/// half of the catalog describes its track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackMedium {
    Video,
    Audio,
}

impl TrackMedium {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TrackMedium::Video => "video",
            TrackMedium::Audio => "audio",
        }
    }
}

/// One Annex-B access unit and everything the encoded-video wire contract
/// requires beside it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncodedVideoAccessUnit {
    /// `"h264"` or `"h265"`, carried through rather than re-derived: the
    /// engine's cast is what legalises the set, and a wheel that kept its own
    /// list would refuse a codec the engine had just added.
    pub(crate) codec: String,
    /// Start-code-prefixed NAL units, parameter sets prepended at sync points.
    pub(crate) annex_b_access_unit: bytes::Bytes,
    pub(crate) is_sync_point: bool,
    pub(crate) group_index: u64,
    pub(crate) sequence_index: u64,
    /// The coded extent, before the conformance crop.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Absent means unspecified — never a map of nulls.
    pub(crate) color: Option<ColorAxesOnTheWire>,
    pub(crate) timestamp_ns: i64,
}

/// One Opus packet and everything the encoded-audio wire contract requires
/// beside it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncodedAudioPacket {
    /// `"opus"`, carried through for the same reason the video codec is.
    pub(crate) codec: String,
    pub(crate) opus_packet: bytes::Bytes,
    /// Every Opus packet is a decode entry point, so this is `true` on the
    /// wire — carried rather than assumed, because the field is the contract.
    pub(crate) is_sync_point: bool,
    pub(crate) group_index: u64,
    pub(crate) sequence_index: u64,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) sample_count: u32,
    pub(crate) pre_skip: u32,
    pub(crate) timestamp_ns: i64,
}
