// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One bag bound for a MoQ track: encoded media, or a data object.
//!
//! Media crosses the CPython boundary as typed fields and is encoded here, in
//! whichever container the broadcast writes. A data object crosses as bytes
//! Python already encoded — the envelope around the user's own bag — and this
//! Rust writes those bytes as the object payload and parses nothing.

use bytes::Bytes;

use crate::encoded_media_sample::{EncodedMediaSample, TrackMedium};

/// One bag as the source of one MoQ object.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MoqTrackSample {
    EncodedMedia(EncodedMediaSample),
    DataObject(DataTrackObject),
}

/// The envelope Python built around a user's bag and encoded with the engine's
/// bag codec. Written whole: the stamp and the sequence index inside it are the
/// subscriber's to read, never this Rust's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DataTrackObject {
    pub(crate) envelope_bytes: Bytes,
}

/// What a track carries: one of the two media, or data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoqTrackKind {
    Media(TrackMedium),
    Data,
}

impl MoqTrackKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MoqTrackKind::Media(medium) => medium.as_str(),
            MoqTrackKind::Data => "data",
        }
    }
}

impl From<EncodedMediaSample> for MoqTrackSample {
    fn from(sample: EncodedMediaSample) -> Self {
        MoqTrackSample::EncodedMedia(sample)
    }
}

impl MoqTrackSample {
    /// Which kind of track this sample belongs on.
    pub(crate) fn kind(&self) -> MoqTrackKind {
        match self {
            MoqTrackSample::EncodedMedia(sample) => MoqTrackKind::Media(sample.medium()),
            MoqTrackSample::DataObject(_) => MoqTrackKind::Data,
        }
    }

    /// The wire codec a media sample names; a data object names none.
    pub(crate) fn wire_codec(&self) -> Option<&str> {
        match self {
            MoqTrackSample::EncodedMedia(EncodedMediaSample::VideoAccessUnit(unit)) => {
                Some(&unit.codec)
            }
            MoqTrackSample::EncodedMedia(EncodedMediaSample::AudioPacket(packet)) => {
                Some(&packet.codec)
            }
            MoqTrackSample::DataObject(_) => None,
        }
    }
}
