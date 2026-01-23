// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for C major chord generation
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChordGeneratorConfig {
    /// Output amplitude (0.0 to 1.0)
    #[serde(rename = "amplitude")]
    pub amplitude: f64,

    /// Output buffer size in samples
    #[serde(rename = "buffer_size")]
    pub buffer_size: u32,

    /// Audio sample rate in Hz
    #[serde(rename = "sample_rate")]
    pub sample_rate: u32,
}
