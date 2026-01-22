// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Stereo audio frame (2 channels, interleaved)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audioframe2Ch {
    /// Sequential frame counter (uint64 as string - parse to native uint64)
    #[serde(rename = "frame_index")]
    pub frame_index: String,

    /// Sample rate in Hz
    #[serde(rename = "sample_rate")]
    pub sample_rate: u32,

    /// Interleaved audio samples (L, R, L, R, ...)
    #[serde(rename = "samples")]
    pub samples: Vec<f32>,

    /// Monotonic timestamp in nanoseconds (int64 as string - parse to native int64)
    #[serde(rename = "timestamp_ns")]
    pub timestamp_ns: String,
}
