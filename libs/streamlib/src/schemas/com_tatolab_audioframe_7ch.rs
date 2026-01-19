// Generated from com.tatolab.audioframe.7ch@1.0.0
// DO NOT EDIT - regenerate with `streamlib schema sync`

use serde::{Deserialize, Serialize};

/// 7.0 surround audio frame (7 channels, interleaved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audioframe7ch {
    /// Interleaved audio samples
    pub samples: Vec<f32>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Monotonic timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Sequential frame counter
    pub frame_index: u64,
}

impl Audioframe7ch {
    /// Deserialize from MessagePack bytes.
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    /// Serialize to MessagePack bytes.
    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}
