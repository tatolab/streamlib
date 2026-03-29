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

/// MoQ relay connection configuration
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relay {
    /// Broadcast namespace path for publishing
    #[serde(rename = "broadcast_path")]
    pub broadcast_path: String,

    /// MoQ relay endpoint URL (e.g., https://relay.example.com)
    #[serde(rename = "endpoint_url")]
    pub endpoint_url: String,

    /// Disable TLS certificate verification (development only)
    #[serde(rename = "tls_disable_verify")]
    pub tls_disable_verify: Option<bool>,
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

/// Configuration for MoQ A/V publishing (encode + relay)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqPublishConfig {
    /// Audio encoder configuration
    #[serde(rename = "audio")]
    pub audio: Audio,

    /// MoQ relay connection configuration
    #[serde(rename = "relay")]
    pub relay: Relay,

    /// Video encoder configuration
    #[serde(rename = "video")]
    pub video: Video,
}
