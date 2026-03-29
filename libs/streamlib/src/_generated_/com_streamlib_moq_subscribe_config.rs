// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for MoQ relay subscription
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqSubscribeConfig {
    /// MoQ relay endpoint URL (e.g., https://relay.example.com)
    #[serde(rename = "relay_endpoint_url")]
    pub relay_endpoint_url: String,

    /// Broadcast namespace path to subscribe to
    #[serde(rename = "broadcast_path")]
    pub broadcast_path: String,

    /// Track names to subscribe to within the broadcast
    #[serde(rename = "track_names")]
    pub track_names: Vec<String>,

    /// Disable TLS certificate verification (development only)
    #[serde(rename = "tls_disable_verify")]
    pub tls_disable_verify: Option<bool>,
}
