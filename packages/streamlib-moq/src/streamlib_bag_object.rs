// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `streamlib_bag` container: one MoQ object is one bag's own wire keys.
//!
//! Why this exists beside CMAF, which every MoQ player already reads: CMAF is
//! lossy against the bag contract. The producer's `group_index` /
//! `sequence_index` become container timing, `pre_skip` becomes the `dOps`
//! box's, and the colour tuple becomes VUI bits inside the bitstream. A
//! StreamLib subscriber that has to hand a decoder back the exact pair the
//! producer wrote — which is what §Networking requires — cannot get it from a
//! fragment. It can get it from here.
//!
//! Encoded as a msgpack **named map**, not the compact array serde defaults to:
//! the keys are the point, and a reader in another language should see them.
//! `bitstream` is msgpack `bin`, never an array of numbers, exactly as it rides
//! a link.

use serde::{Deserialize, Serialize};

use crate::encoded_media_sample::{
    ColorAxesOnTheWire, EncodedAudioPacket, EncodedMediaSample, EncodedVideoAccessUnit, TrackMedium,
};
use crate::error::{MoqExtensionError, Result};
use crate::moq_broadcast_catalog::STREAMLIB_BAG_PACKAGING;

/// A video object's keys, which are the encoded-video bag's keys plus the stamp
/// that normally rides the frame header.
#[derive(Debug, Deserialize)]
struct VideoObjectOnTheWire {
    codec: String,
    #[serde(rename = "bitstream", with = "serde_bytes")]
    annex_b_access_unit: Vec<u8>,
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    width: u32,
    height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<ColorAxesOnTheWire>,
    timestamp_ns: i64,
}

/// An audio object's keys, which are the encoded-audio bag's keys plus the
/// stamp.
#[derive(Debug, Deserialize)]
struct AudioObjectOnTheWire {
    codec: String,
    #[serde(rename = "bitstream", with = "serde_bytes")]
    opus_packet: Vec<u8>,
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    pre_skip: u32,
    timestamp_ns: i64,
}

/// The write side of a video object, borrowing what the read side owns.
///
/// Encoding runs once per published bag, so an owned mirror would copy the
/// whole access unit out of its `Bytes` purely to hand it to serde, which then
/// copies it again into the msgpack output.
#[derive(Debug, Serialize)]
struct VideoObjectBeingWritten<'sample> {
    codec: &'sample str,
    #[serde(rename = "bitstream", with = "serde_bytes")]
    annex_b_access_unit: &'sample [u8],
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    width: u32,
    height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'sample ColorAxesOnTheWire>,
    timestamp_ns: i64,
}

/// The write side of an audio object, borrowing for the same reason.
#[derive(Debug, Serialize)]
struct AudioObjectBeingWritten<'sample> {
    codec: &'sample str,
    #[serde(rename = "bitstream", with = "serde_bytes")]
    opus_packet: &'sample [u8],
    is_sync_point: bool,
    group_index: u64,
    sequence_index: u64,
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    pre_skip: u32,
    timestamp_ns: i64,
}

/// Just enough of any object to tell the two apart, relying on serde reading
/// past the keys it was not given — which is why the map form matters.
#[derive(Debug, Deserialize)]
struct MediumProbeOnTheWire {
    #[serde(default)]
    codec: Option<String>,
}

/// Encode one sample as this container's object payload.
pub(crate) fn encode_object(sample: &EncodedMediaSample) -> Result<bytes::Bytes> {
    let encoded = match sample {
        EncodedMediaSample::VideoAccessUnit(unit) => {
            rmp_serde::to_vec_named(&VideoObjectBeingWritten {
                codec: &unit.codec,
                annex_b_access_unit: &unit.annex_b_access_unit,
                is_sync_point: unit.is_sync_point,
                group_index: unit.group_index,
                sequence_index: unit.sequence_index,
                width: unit.width,
                height: unit.height,
                color: unit.color.as_ref(),
                timestamp_ns: unit.timestamp_ns,
            })
        }
        EncodedMediaSample::AudioPacket(packet) => {
            rmp_serde::to_vec_named(&AudioObjectBeingWritten {
                codec: &packet.codec,
                opus_packet: &packet.opus_packet,
                is_sync_point: packet.is_sync_point,
                group_index: packet.group_index,
                sequence_index: packet.sequence_index,
                sample_rate: packet.sample_rate,
                channels: packet.channels,
                sample_count: packet.sample_count,
                pre_skip: packet.pre_skip,
                timestamp_ns: packet.timestamp_ns,
            })
        }
    };
    encoded
        .map(bytes::Bytes::from)
        .map_err(|failure| MoqExtensionError::MalformedObject {
            container: STREAMLIB_BAG_PACKAGING,
            what: format!("the object could not be encoded: {failure}"),
        })
}

/// Decode one object payload back into the sample the producer published.
///
/// `expected_medium` is what the track was subscribed as, and a payload whose
/// `codec` says otherwise is refused by name rather than decoded into the wrong
/// shape — a broadcast that put audio on the video track is a publisher bug and
/// the message is the only place to catch it.
pub(crate) fn decode_object(
    payload: &[u8],
    expected_medium: TrackMedium,
) -> Result<EncodedMediaSample> {
    let probe: MediumProbeOnTheWire =
        rmp_serde::from_slice(payload).map_err(|failure| MoqExtensionError::MalformedObject {
            container: STREAMLIB_BAG_PACKAGING,
            what: format!("the object is not a named map of bag keys: {failure}"),
        })?;
    let codec = probe
        .codec
        .ok_or_else(|| MoqExtensionError::MalformedObject {
            container: STREAMLIB_BAG_PACKAGING,
            what: "the object names no `codec`, so no decoder can be chosen for its bitstream"
                .to_owned(),
        })?;

    let carried_medium =
        medium_of_codec(&codec).ok_or_else(|| MoqExtensionError::MalformedObject {
            container: STREAMLIB_BAG_PACKAGING,
            what: format!(
                "the object names codec `{codec}`, which this subscriber does not carry — it \
                 reads {VIDEO_CODECS_ON_THE_WIRE:?} and {AUDIO_CODECS_ON_THE_WIRE:?}"
            ),
        })?;
    if carried_medium != expected_medium {
        return Err(MoqExtensionError::MalformedObject {
            container: STREAMLIB_BAG_PACKAGING,
            what: format!(
                "an object on the {} track names codec `{codec}`, which is {}; one track is one \
                 medium",
                expected_medium.as_str(),
                carried_medium.as_str()
            ),
        });
    }

    match expected_medium {
        TrackMedium::Video => {
            let on_the_wire: VideoObjectOnTheWire =
                rmp_serde::from_slice(payload).map_err(malformed_for(TrackMedium::Video))?;
            Ok(EncodedMediaSample::VideoAccessUnit(
                EncodedVideoAccessUnit {
                    codec: on_the_wire.codec,
                    annex_b_access_unit: bytes::Bytes::from(on_the_wire.annex_b_access_unit),
                    is_sync_point: on_the_wire.is_sync_point,
                    group_index: on_the_wire.group_index,
                    sequence_index: on_the_wire.sequence_index,
                    width: on_the_wire.width,
                    height: on_the_wire.height,
                    color: on_the_wire.color,
                    timestamp_ns: on_the_wire.timestamp_ns,
                },
            ))
        }
        TrackMedium::Audio => {
            let on_the_wire: AudioObjectOnTheWire =
                rmp_serde::from_slice(payload).map_err(malformed_for(TrackMedium::Audio))?;
            Ok(EncodedMediaSample::AudioPacket(EncodedAudioPacket {
                codec: on_the_wire.codec,
                opus_packet: bytes::Bytes::from(on_the_wire.opus_packet),
                is_sync_point: on_the_wire.is_sync_point,
                group_index: on_the_wire.group_index,
                sequence_index: on_the_wire.sequence_index,
                sample_rate: on_the_wire.sample_rate,
                channels: on_the_wire.channels,
                sample_count: on_the_wire.sample_count,
                pre_skip: on_the_wire.pre_skip,
                timestamp_ns: on_the_wire.timestamp_ns,
            }))
        }
    }
}

/// The video codecs the engine's encoded-video convention legalises.
const VIDEO_CODECS_ON_THE_WIRE: [&str; 2] = ["h264", "h265"];
/// The audio codecs the engine's encoded-audio convention legalises.
const AUDIO_CODECS_ON_THE_WIRE: [&str; 1] = ["opus"];

/// Which medium a wire codec spelling belongs to, or `None` for one neither
/// convention legalises.
pub(crate) fn medium_of_codec(codec: &str) -> Option<TrackMedium> {
    if VIDEO_CODECS_ON_THE_WIRE.contains(&codec) {
        Some(TrackMedium::Video)
    } else if AUDIO_CODECS_ON_THE_WIRE.contains(&codec) {
        Some(TrackMedium::Audio)
    } else {
        None
    }
}

fn malformed_for(medium: TrackMedium) -> impl Fn(rmp_serde::decode::Error) -> MoqExtensionError {
    move |failure| MoqExtensionError::MalformedObject {
        container: STREAMLIB_BAG_PACKAGING,
        what: format!(
            "the object is not a complete encoded-{} bag ({failure})",
            medium.as_str()
        ),
    }
}
