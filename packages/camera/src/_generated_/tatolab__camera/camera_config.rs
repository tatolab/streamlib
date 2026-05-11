// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Configuration for camera capture (V4L2 on Linux, AVFoundation on macOS/iOS)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    /// Camera device path (V4L2 /dev/videoN) or AVCaptureDevice name. If None,
    /// picks the first available capture device.
    #[serde(rename = "device_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,

    /// Maximum frame rate. AVFoundation only; ignored on Linux. Defaults to the
    /// main display refresh rate.
    #[serde(rename = "max_fps")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fps: Option<f64>,

    /// Minimum frame rate. AVFoundation only; ignored on Linux. Default: 60.0
    #[serde(rename = "min_fps")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_fps: Option<f64>,
}
