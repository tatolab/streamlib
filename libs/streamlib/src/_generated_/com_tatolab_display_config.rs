// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// How video content is scaled within the window
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScalingMode {

    #[serde(rename = "Crop")]
    Crop,

    #[serde(rename = "Letterbox")]
    Letterbox,

    #[serde(rename = "Stretch")]
    Stretch,
}


/// Configuration for video display window (macOS/iOS)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {

    /// Number of drawable buffers (2=double, 3=triple). Default: 2
    #[serde(rename = "drawable_count")]
    pub drawable_count: u32,

    /// Window height in pixels
    #[serde(rename = "height")]
    pub height: u32,

    /// How video content is scaled within the window
    #[serde(rename = "scaling_mode")]
    pub scaling_mode: ScalingMode,

    /// Window title. If None, uses 'streamlib Display'
    #[serde(rename = "title")]
    pub title: Option<String>,

    /// Enable vsync (synchronize to display refresh rate). Default: true
    #[serde(rename = "vsync")]
    pub vsync: bool,

    /// Window width in pixels
    #[serde(rename = "width")]
    pub width: u32,
}

