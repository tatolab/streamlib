// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for Opus audio encoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpusEncoderConfig {
    /// Target bitrate in bits per second (default: 128000).
    #[serde(rename = "bitrate_bps")]
    pub bitrate_bps: Option<u32>,
}
