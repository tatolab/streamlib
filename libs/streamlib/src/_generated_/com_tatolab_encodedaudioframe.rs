// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Encoded audio frame with Opus/AAC bitstream data
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Encodedaudioframe {
    /// Encoded audio bitstream data (Opus/AAC)
    #[serde(rename = "data")]
    pub data: Vec<u8>,

    /// Number of audio samples per channel in this frame
    #[serde(rename = "sample_count")]
    pub sample_count: u32,

    /// Monotonic timestamp in nanoseconds (int64 as string)
    #[serde(rename = "timestamp_ns")]
    pub timestamp_ns: String,
}
