// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Configuration for H.265 video encoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H265EncoderConfig {
    /// Target bitrate in bits per second (default: 2000000).
    #[serde(rename = "bitrate_bps")]
    pub bitrate_bps: Option<u32>,

    /// Frames per second for encoder timing (default: 60).
    #[serde(rename = "fps")]
    pub fps: Option<u32>,

    /// Video height in pixels (default: 720).
    #[serde(rename = "height")]
    pub height: Option<u32>,

    /// Frames between keyframes (overrides keyframe_interval_seconds if set).
    #[serde(rename = "keyframe_interval")]
    pub keyframe_interval: Option<u32>,

    /// Seconds between keyframes (default: 2.0). Converted to frames using the
    /// encoder's fps. Ignored if keyframe_interval (frames) is set.
    #[serde(rename = "keyframe_interval_seconds")]
    pub keyframe_interval_seconds: Option<f32>,

    /// Driver encode quality level (VkVideoEncodeQualityLevelInfoKHR). Higher
    /// = better quality, more GPU compute per frame; does not add frame
    /// delay. Unset = codec-specific real-time default; clamped against
    /// VkVideoEncodeCapabilitiesKHR::maxQualityLevels. Note: NVIDIA RTX 3090
    /// exposes only level 0 for H.265.
    #[serde(rename = "quality_level")]
    pub quality_level: Option<u32>,

    /// Video width in pixels (default: 1280).
    #[serde(rename = "width")]
    pub width: Option<u32>,
}
