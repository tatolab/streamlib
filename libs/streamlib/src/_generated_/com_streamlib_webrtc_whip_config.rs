// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Audio encoder configuration
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    /// Target bitrate in bits per second
    #[serde(rename = "bitrate_bps")]
    pub bitrate_bps: u32,

    /// Number of audio channels
    #[serde(rename = "channels")]
    pub channels: u32,

    /// Sample rate in Hz
    #[serde(rename = "sample_rate")]
    pub sample_rate: u32,
}

/// Video encoder configuration
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Video {
    /// Target bitrate in bits per second
    #[serde(rename = "bitrate_bps")]
    pub bitrate_bps: u32,

    /// Frames per second
    #[serde(rename = "fps")]
    pub fps: u32,

    /// Video height in pixels
    #[serde(rename = "height")]
    pub height: u32,

    /// Video width in pixels
    #[serde(rename = "width")]
    pub width: u32,
}

/// WHIP endpoint configuration
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Whip {
    /// WHIP endpoint URL
    #[serde(rename = "endpoint_url")]
    pub endpoint_url: String,

    /// Connection timeout in milliseconds
    #[serde(rename = "timeout_ms")]
    pub timeout_ms: u32,

    /// Optional bearer token for authentication
    #[serde(rename = "auth_token")]
    pub auth_token: Option<String>,
}

/// Configuration for WebRTC WHIP streaming
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebrtcWhipConfig {
    /// Audio encoder configuration
    #[serde(rename = "audio")]
    pub audio: Audio,

    /// Video encoder configuration
    #[serde(rename = "video")]
    pub video: Video,

    /// WHIP endpoint configuration
    #[serde(rename = "whip")]
    pub whip: Whip,
}
