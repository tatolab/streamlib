// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Configuration for the runtime API server
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiServerConfig {
    /// Host address to bind to
    #[serde(rename = "host")]
    pub host: String,

    /// Port number to listen on
    #[serde(rename = "port")]
    pub port: u16,

    /// Log file path for surface-share registration
    #[serde(rename = "log_path")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,

    /// Runtime name for surface-share registration (auto-generated if not
    /// provided)
    #[serde(rename = "name")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
