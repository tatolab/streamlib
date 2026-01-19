// Generated from com.tatolab.audioframe.8ch@1.0.0
// DO NOT EDIT - regenerate with `streamlib schema sync`

use serde::{Deserialize, Serialize};

/// 7.1 surround audio frame (8 channels, interleaved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audioframe8ch {
    /// Interleaved audio samples
    pub samples: Vec<f32>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Monotonic timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Sequential frame counter
    pub frame_index: u64,
}

impl Audioframe8ch {
    /// Deserialize from MessagePack bytes.
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    /// Serialize to MessagePack bytes.
    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}
