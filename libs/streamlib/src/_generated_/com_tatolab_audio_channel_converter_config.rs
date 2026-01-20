// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// Channel conversion mode
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Mode {

    #[serde(rename = "Duplicate")]
    Duplicate,

    #[serde(rename = "LeftOnly")]
    LeftOnly,

    #[serde(rename = "RightOnly")]
    RightOnly,
}


/// Configuration for mono to stereo channel conversion
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioChannelConverterConfig {

    /// Channel conversion mode
    #[serde(rename = "mode")]
    pub mode: Mode,
}

