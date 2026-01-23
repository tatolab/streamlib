// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Video frame for IPC - references GPU surface by ID
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Videoframe {
    /// Sequential frame counter (uint64 as string - parse to native uint64)
    #[serde(rename = "frame_index")]
    pub frame_index: String,

    /// Frame height in pixels
    #[serde(rename = "height")]
    pub height: u32,

    /// GPU surface ID (IOSurface on macOS)
    #[serde(rename = "surface_id")]
    pub surface_id: String,

    /// Monotonic timestamp in nanoseconds (int64 as string - parse to native int64)
    #[serde(rename = "timestamp_ns")]
    pub timestamp_ns: String,

    /// Frame width in pixels
    #[serde(rename = "width")]
    pub width: u32,
}
