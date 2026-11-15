// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations on one stream action and on a stream's clock.

use schemars::JsonSchema;
use serde::Deserialize;

/// What `action` says where it names a stream action.
pub const STREAM_ACTION_ARGUMENT_DESCRIPTION: &str =
    "The stream action, by the action id a tool that queues one answered.";

/// The params of an operation on one stream action, named by its id.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneStreamActionRequest {
    /// The stream action's id.
    #[schemars(description = STREAM_ACTION_ARGUMENT_DESCRIPTION)]
    pub action: String,
}

/// What a stream action's `stream` argument says.
pub const STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION: &str =
    "The stream, by the stream id `list_streams` reports: loaded, or recorded and not loaded.";

/// `set_stream_clock_scale`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStreamClockScaleRequest {
    /// The stream, by the stream id `list_streams` reports.
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: String,
    /// Stream seconds per machine second from now on.
    #[schemars(
        description = "Stream seconds per machine second from now on: 0.5 runs the stream at half speed, 1 at real time. A finite number greater than zero, at most 1."
    )]
    pub scale: f64,
}
