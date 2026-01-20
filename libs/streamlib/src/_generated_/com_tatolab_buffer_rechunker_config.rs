// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// Configuration for rechunking audio buffers to fixed size
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BufferRechunkerConfig {

    /// Target buffer size in samples per channel
    #[serde(rename = "target_buffer_size")]
    pub target_buffer_size: u32,
}

