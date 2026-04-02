// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for publishing a single track to a MoQ relay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqPublishTrackConfig {
    /// MoQ relay URL including broadcast path.
    #[serde(rename = "url")]
    pub url: String,

    /// Track name (auto-generated from processor ID if not set).
    #[serde(rename = "track_name")]
    pub track_name: Option<String>,
}
