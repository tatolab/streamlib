// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for Opus audio decoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpusDecoderConfig {
    /// Output sample rate in Hz (default: 48000).
    #[serde(rename = "sample_rate")]
    pub sample_rate: Option<u32>,

    /// Output channel count (default: 2).
    #[serde(rename = "channels")]
    pub channels: Option<u32>,
}
