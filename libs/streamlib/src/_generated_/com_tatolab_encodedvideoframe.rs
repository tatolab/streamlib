// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Encoded video frame with H.264/H.265 NAL unit data
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Encodedvideoframe {
    /// Encoded NAL units (H.264/H.265 bitstream data)
    #[serde(rename = "data")]
    pub data: Vec<u8>,

    /// Sequential frame number (uint64 as string)
    #[serde(rename = "frame_number")]
    pub frame_number: String,

    /// Whether this is a keyframe (I-frame)
    #[serde(rename = "is_keyframe")]
    pub is_keyframe: bool,

    /// Monotonic timestamp in nanoseconds (int64 as string)
    #[serde(rename = "timestamp_ns")]
    pub timestamp_ns: String,
}
