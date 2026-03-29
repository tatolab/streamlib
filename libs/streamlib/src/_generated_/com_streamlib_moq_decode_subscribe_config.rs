// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for MoQ A/V subscription with H.264+Opus decoding
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqDecodeSubscribeConfig {
    /// MoQ relay endpoint URL (e.g., https://relay.example.com)
    #[serde(rename = "relay_endpoint_url")]
    pub relay_endpoint_url: String,

    /// Broadcast namespace path to subscribe to
    #[serde(rename = "broadcast_path")]
    pub broadcast_path: String,

    /// Disable TLS certificate verification (development only)
    #[serde(rename = "tls_disable_verify")]
    pub tls_disable_verify: Option<bool>,

    /// Opus audio sample rate in Hz (default: 48000)
    #[serde(rename = "audio_sample_rate")]
    pub audio_sample_rate: Option<u32>,

    /// Number of audio channels (default: 2 for stereo)
    #[serde(rename = "audio_channels")]
    pub audio_channels: Option<u8>,
}
