// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};



/// Configuration for camera capture (macOS/iOS)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {

    /// Camera device unique ID. If None, uses default camera
    #[serde(rename = "device_id")]
    pub device_id: Option<String>,

    /// Maximum frames per second (ceiling). If None, uses main display refresh rate
    #[serde(rename = "max_fps")]
    pub max_fps: Option<f64>,

    /// Minimum frames per second (floor). Default: 60.0
    #[serde(rename = "min_fps")]
    pub min_fps: Option<f64>,
}

