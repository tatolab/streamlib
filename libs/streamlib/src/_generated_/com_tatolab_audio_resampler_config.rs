// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// Resampling quality level
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Quality {

    #[serde(rename = "High")]
    High,

    #[serde(rename = "Low")]
    Low,

    #[serde(rename = "Medium")]
    Medium,
}


/// Configuration for audio sample rate conversion
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioResamplerConfig {

    /// Resampling quality level
    #[serde(rename = "quality")]
    pub quality: Quality,

    /// Source audio sample rate in Hz
    #[serde(rename = "source_sample_rate")]
    pub source_sample_rate: u32,

    /// Target audio sample rate in Hz
    #[serde(rename = "target_sample_rate")]
    pub target_sample_rate: u32,
}

