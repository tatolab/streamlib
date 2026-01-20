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

    /// Log file path for broker registration
    #[serde(rename = "log_path")]
    pub log_path: Option<String>,

    /// Runtime name for broker registration (auto-generated if not provided)
    #[serde(rename = "name")]
    pub name: Option<String>,

    /// Port number to listen on
    #[serde(rename = "port")]
    pub port: u16,
}

