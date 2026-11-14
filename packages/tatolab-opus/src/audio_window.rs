// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One audio window as the codec state reads it: interleaved samples and the
//! fields of the Channels block that carried them.

use crate::opus_codec_refusal::{OpusCodecRefusal, Result};

/// How an audio block's samples are encoded, as the wire spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleDtype {
    F32,
    I16,
}

impl AudioSampleDtype {
    /// The dtype `spelling` names on the wire, refused by name otherwise.
    pub fn from_wire_spelling(spelling: &str) -> Result<Self> {
        match spelling {
            "f32" => Ok(Self::F32),
            "i16" => Ok(Self::I16),
            other => Err(OpusCodecRefusal(format!(
                "an audio block's dtype is {other:?}, which is neither `f32` nor `i16`"
            ))),
        }
    }

    /// The width in bytes of one sample of this dtype.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::I16 => 2,
        }
    }
}

/// One audio window handed to the encoder.
#[derive(Debug, Clone, Copy)]
pub struct AudioWindowToEncode<'samples> {
    /// Little-endian samples, interleaved by channel.
    pub interleaved_sample_bytes: &'samples [u8],
    pub sample_rate: u32,
    pub channels: u32,
    /// Per-channel samples.
    pub sample_count: u32,
    pub dtype: AudioSampleDtype,
}

/// One decoded block of interleaved little-endian `f32` at Opus's rate.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudioBlock {
    pub interleaved_sample_bytes: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_count: u32,
    pub first_sample_timestamp_ns: i64,
}
