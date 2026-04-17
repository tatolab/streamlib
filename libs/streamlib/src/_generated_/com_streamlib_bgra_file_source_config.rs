// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for streaming raw BGRA frames from a file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BgraFileSourceConfig {
    /// Path to raw BGRA file (width * height * 4 bytes per frame).
    #[serde(rename = "file_path")]
    pub file_path: String,

    /// Frame width in pixels.
    #[serde(rename = "width")]
    pub width: u32,

    /// Frame height in pixels.
    #[serde(rename = "height")]
    pub height: u32,

    /// Playback frame rate.
    #[serde(rename = "fps")]
    pub fps: u32,

    /// Number of frames in the file.
    #[serde(rename = "frame_count")]
    pub frame_count: u32,
}
