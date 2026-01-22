// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Generated from StreamLib schemas
// DO NOT EDIT - regenerate with `streamlib schema sync`

use serde::{Deserialize, Serialize};

// ============================================================================
// com.streamlib.test.message@1.0.0
// ============================================================================

/// Test message for basic validation.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestMessage {
    pub sequence: u64,
    pub timestamp_ns: i64,
    pub label: String,
    pub values: Vec<f32>,
}

#[allow(dead_code)]
impl TestMessage {
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}

// ============================================================================
// com.streamlib.bench.videoframemeta@1.0.0
// ============================================================================

/// VideoFrame metadata (pixels stay on GPU via IOSurface/Metal texture).
/// Size: ~48 bytes serialized
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrameMeta {
    /// GPU surface/texture handle
    pub surface_id: u64,
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Presentation timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Pixel format code
    pub format: u32,
    /// Color space identifier
    pub color_space: u32,
    /// Monotonic frame counter
    pub frame_number: u64,
}

impl VideoFrameMeta {
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}

// ============================================================================
// com.streamlib.bench.audioframe@1.0.0
// ============================================================================

/// AudioFrame with full sample data.
/// Size: ~4KB (512 samples * 2 channels * 4 bytes + metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFrame {
    /// Presentation timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of audio channels
    pub channels: u32,
    /// Monotonic frame counter
    pub frame_number: u64,
    /// Interleaved audio samples
    pub samples: Vec<f32>,
}

impl AudioFrame {
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}

// ============================================================================
// com.streamlib.bench.dataframe@1.0.0
// ============================================================================

/// DataFrame with large binary payload.
/// Size: up to 16KB
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFrame {
    /// Timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Monotonic frame counter
    pub frame_number: u64,
    /// Binary payload data
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

impl DataFrame {
    pub fn from_msgpack(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }

    pub fn to_msgpack(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(self)
    }
}
