// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for H.264 video encoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H264EncoderConfig {
    /// Video width in pixels (default: 1280).
    #[serde(rename = "width")]
    pub width: Option<u32>,

    /// Video height in pixels (default: 720).
    #[serde(rename = "height")]
    pub height: Option<u32>,

    /// Target bitrate in bits per second (default: 2000000).
    #[serde(rename = "bitrate_bps")]
    pub bitrate_bps: Option<u32>,

    /// Frames between keyframes (default: 30).
    #[serde(rename = "keyframe_interval")]
    pub keyframe_interval: Option<u32>,
}
