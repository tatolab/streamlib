// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for Linux MP4 video writing via ffmpeg encode + mux.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxMp4WriterConfig {
    /// Path to write the output MP4 file.
    #[serde(rename = "output_path")]
    pub output_path: String,

    /// Fallback frame rate if not provided by upstream Videoframe.
    #[serde(rename = "fps")]
    pub fps: u32,

    /// Expected duration in seconds (for silent audio track length).
    #[serde(rename = "duration_secs")]
    pub duration_secs: Option<u32>,
}
