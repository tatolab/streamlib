// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for MP4 file writer processor
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mp4WriterConfig {
    /// Path to output MP4 file
    #[serde(rename = "output_path")]
    pub output_path: String,

    /// Audio bitrate in bits per second. Default: 128000 (128 kbps)
    #[serde(rename = "audio_bitrate")]
    pub audio_bitrate: Option<u32>,

    /// Audio codec identifier. Default: 'aac'
    #[serde(rename = "audio_codec")]
    pub audio_codec: Option<String>,

    /// A/V sync tolerance in milliseconds. Default: 33.3ms
    #[serde(rename = "sync_tolerance_ms")]
    pub sync_tolerance_ms: Option<f64>,

    /// Video bitrate in bits per second. Default: 5000000 (5 Mbps)
    #[serde(rename = "video_bitrate")]
    pub video_bitrate: Option<u32>,

    /// Video codec identifier. Default: 'avc1' (H.264)
    #[serde(rename = "video_codec")]
    pub video_codec: Option<String>,
}
