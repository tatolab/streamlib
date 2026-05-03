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

    /// Source frame rate in frames per second (set by capture device)
    #[serde(rename = "fps")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,

    /// Frame height in pixels
    #[serde(rename = "height")]
    pub height: u32,

    /// GPU surface ID (IOSurface on macOS)
    #[serde(rename = "surface_id")]
    pub surface_id: String,

    /// Monotonic timestamp in nanoseconds (int64 as string - parse to native
    /// int64)
    #[serde(rename = "timestamp_ns")]
    pub timestamp_ns: String,

    /// Producer's published VkImageLayout for this frame's texture (#633).
    /// Per-frame override of the per-surface current_image_layout published
    /// via surface-share register/update_layout. Encoded as the raw int32
    /// VkImageLayout enumerant. Absent when the producer relies on the per-
    /// surface default.
    #[serde(rename = "texture_layout")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_layout: Option<i32>,

    /// Frame width in pixels
    #[serde(rename = "width")]
    pub width: u32,
}
