// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Generated from com.tatolab.audioframe.2ch@1.0.0
// DO NOT EDIT - regenerate with `streamlib schema sync`

use serde::{Deserialize, Serialize};

/// Stereo audio frame (2 channels, interleaved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audioframe2ch {
    /// Interleaved audio samples (L, R, L, R, ...)
    pub samples: Vec<f32>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Monotonic timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Sequential frame counter
    pub frame_index: u64,
}

impl Audioframe2ch {
    /// Deserialize from MessagePack bytes.
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    /// Serialize to MessagePack bytes.
    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}
