// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.

use serde::{Deserialize, Serialize};

/// Configuration for subscribing to a single track from a MoQ relay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqSubscribeTrackConfig {
    /// MoQ relay URL including broadcast path.
    #[serde(rename = "url")]
    pub url: String,

    /// Track name to subscribe to.
    #[serde(rename = "track_name")]
    pub track_name: String,
}
