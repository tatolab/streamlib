// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// Mixing strategy for combining signals
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Strategy {

    #[serde(rename = "Sum")]
    Sum,

    #[serde(rename = "SumClipped")]
    SumClipped,

    #[serde(rename = "SumNormalized")]
    SumNormalized,
}


/// Configuration for mixing two mono signals into stereo
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioMixerConfig {

    /// Mixing strategy for combining signals
    #[serde(rename = "strategy")]
    pub strategy: Strategy,
}

