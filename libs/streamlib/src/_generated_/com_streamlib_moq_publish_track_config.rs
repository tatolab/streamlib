// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for publishing a single track to a MoQ relay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqPublishTrackConfig {
    /// Track name (auto-generated from processor ID if not set).
    #[serde(rename = "track_name")]
    pub track_name: Option<String>,

    /// MoQ relay URL (defaults to Cloudflare draft-14 relay with auto-generated broadcast path).
    #[serde(rename = "url")]
    pub url: Option<String>,
}
