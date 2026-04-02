// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for publishing a single track to a MoQ relay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqPublishTrackConfig {
    /// MoQ relay endpoint URL.
    #[serde(rename = "relay_endpoint_url")]
    pub relay_endpoint_url: String,

    /// MoQ broadcast namespace path.
    #[serde(rename = "broadcast_path")]
    pub broadcast_path: String,

    /// MoQ track name to publish to.
    #[serde(rename = "track_name")]
    pub track_name: String,

    /// Disable TLS certificate verification (development only).
    #[serde(rename = "tls_disable_verify")]
    pub tls_disable_verify: Option<bool>,
}
