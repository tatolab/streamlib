// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Configuration for H.264 video encoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H264EncoderConfig {
    /// Target bitrate in bits per second (default: 2000000).
    #[serde(rename = "bitrate_bps")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_bps: Option<u32>,

    /// Vulkan API encoder-effort index
    /// (VkVideoEncodeQualityLevelInfoKHR::quality_level). Higher = more GPU
    /// work per frame (mode decision, RD-opt, motion search). NOT an H.264
    /// quality knob — profile, QP, and rate-control are configured elsewhere.
    /// Valid values are 0..VkVideoEncodeCapabilitiesKHR::maxQualityLevels; the
    /// session clamps as a safety floor. Unset = codec default.
    #[serde(rename = "effort_level")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_level: Option<u32>,

    /// Frames per second for encoder timing (default: 60).
    #[serde(rename = "fps")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,

    /// Video height in pixels (default: 720).
    #[serde(rename = "height")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    /// Frames between keyframes (overrides keyframe_interval_seconds if set).
    #[serde(rename = "keyframe_interval")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_interval: Option<u32>,

    /// Seconds between keyframes (default: 2.0). Converted to frames using the
    /// encoder's fps. Ignored if keyframe_interval (frames) is set.
    #[serde(rename = "keyframe_interval_seconds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_interval_seconds: Option<f32>,

    /// H.264 profile: baseline, main, or high (default: main).
    #[serde(rename = "profile")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// Video width in pixels (default: 1280).
    #[serde(rename = "width")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}
