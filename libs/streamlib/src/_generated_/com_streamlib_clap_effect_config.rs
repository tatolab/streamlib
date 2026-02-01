// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for CLAP audio plugin processing
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectConfig {
    /// Processing buffer size in samples
    #[serde(rename = "buffer_size")]
    pub buffer_size: u32,

    /// Path to the CLAP plugin file
    #[serde(rename = "plugin_path")]
    pub plugin_path: String,

    /// Index of the plugin to load (if multiple in file)
    #[serde(rename = "plugin_index")]
    pub plugin_index: Option<u32>,

    /// Name of the plugin to load (if multiple in file)
    #[serde(rename = "plugin_name")]
    pub plugin_name: Option<String>,
}
