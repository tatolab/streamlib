// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for WebRTC WHEP receiving
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebrtcWhepConfig {
    /// WHEP endpoint configuration
    #[serde(rename = "whep")]
    pub whep: Whep,
}

/// WHEP endpoint configuration
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Whep {
    /// Optional bearer token for authentication
    #[serde(rename = "auth_token")]
    pub auth_token: Option<String>,

    /// WHEP endpoint URL
    #[serde(rename = "endpoint_url")]
    pub endpoint_url: String,

    /// Connection timeout in milliseconds
    #[serde(rename = "timeout_ms")]
    pub timeout_ms: u32,
}
