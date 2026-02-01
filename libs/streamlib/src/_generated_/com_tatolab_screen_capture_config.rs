// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// What to capture: Display, Window, or Application
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum TargetType {
    #[serde(rename = "Application")]
    #[default]
    Application,

    #[serde(rename = "Display")]
    Display,

    #[serde(rename = "Window")]
    Window,
}

/// Configuration for screen capture using ScreenCaptureKit (macOS 12.3+)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenCaptureConfig {
    /// What to capture: Display, Window, or Application
    #[serde(rename = "target_type")]
    pub target_type: TargetType,

    /// Bundle identifier for Application mode (e.g., 'com.apple.Safari')
    #[serde(rename = "app_bundle_id")]
    pub app_bundle_id: Option<String>,

    /// Display index for Application mode (default: 0)
    #[serde(rename = "app_display_index")]
    pub app_display_index: Option<u32>,

    /// Display index for Display mode (default: 0 for main display)
    #[serde(rename = "display_index")]
    pub display_index: Option<u32>,

    /// Exclude current application from capture (default: true)
    #[serde(rename = "exclude_current_app")]
    pub exclude_current_app: Option<bool>,

    /// Target frame rate in fps (default: 30.0)
    #[serde(rename = "frame_rate")]
    pub frame_rate: Option<f64>,

    /// Whether to capture cursor (default: false)
    #[serde(rename = "show_cursor")]
    pub show_cursor: Option<bool>,

    /// Window ID for Window mode
    #[serde(rename = "window_id")]
    pub window_id: Option<u32>,

    /// Window title substring for Window mode
    #[serde(rename = "window_title")]
    pub window_title: Option<String>,
}
