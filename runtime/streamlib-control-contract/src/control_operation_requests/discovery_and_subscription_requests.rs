// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations that describe the runtime, its streams, logs and changes.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use super::PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

/// What `graph`'s `stream` argument says.
pub const GRAPH_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream whose graph to export, by the stream id `list_streams` reports. Omit it for every loaded stream's graph under the machine's name.";

/// `describe_runtime`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeRuntimeRequest {}

/// `list_audio_capture_devices`' params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAudioCaptureDevicesRequest {}

/// `list_streams`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListStreamsRequest {}

/// `graph`'s params.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphRequest {
    /// The stream whose graph to export; every loaded stream's when absent.
    #[schemars(description = GRAPH_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Option<String>,
}

/// `discover`'s params.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverRequest {
    /// The contract version the client was built against; another version than the runtime's is
    /// refused naming the older side.
    pub contract_version: Option<u32>,
    /// The contract revision the client was built against, 0 when absent; a higher revision than
    /// the runtime's is refused naming the runtime as the older side.
    pub contract_revision: Option<u32>,
}

/// `subscribe_project_changes`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeProjectChangesRequest {
    /// The projects, by UUID, whose changes the connection is sent; refused when empty.
    pub projects: Vec<Uuid>,
}

/// `subscribe_stream_graph_changes`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeStreamGraphChangesRequest {
    /// The loaded streams, by stream id, whose graph changes the connection is sent; refused when
    /// empty or when one is not loaded.
    pub streams: Vec<String>,
}

/// `subscribe_input_value_changes`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeInputValueChangesRequest {
    /// The loaded streams, by stream id, whose input value changes the connection is sent;
    /// refused when empty or when one is not loaded.
    pub streams: Vec<String>,
}

/// `subscribe_project_log_records`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeProjectLogRecordsRequest {
    /// The projects, by absolute directory, whose new log records the connection is told of;
    /// refused when empty, or when one is relative or names no folder on this machine.
    pub project_directories: Vec<PathBuf>,
}

/// `subscribe_stream_action_changes`' params, which name nothing.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeStreamActionChangesRequest {}

/// `logs`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsRequest {
    /// The project, by absolute path.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Only the records of this stream, by its stream id.
    #[schemars(
        description = "Only the records of this stream, by its stream id. Omit it for every record of the project, its loads' among them."
    )]
    pub stream: Option<String>,
    /// Only the records of this stream action, by its action id.
    #[schemars(description = "Only the records of this stream action, by its action id.")]
    pub action: Option<String>,
    /// Return the records after this opaque cursor, the previous result's `next_after`.
    #[schemars(
        description = "Return the records after this opaque cursor, the previous result's `next_after`. Omit it for the oldest the project's log holds."
    )]
    pub after: Option<String>,
    /// Return no record at or past this opaque cursor.
    #[schemars(
        description = "Return no record at or past this opaque cursor, a `next_after` a result or a `project_log_records_written` notification answered: a page that reaches it answers it as `next_after`. Omit it to read to the log's end."
    )]
    pub until: Option<String>,
    /// The most records to return.
    #[schemars(
        range(min = 1),
        description = "The most records to return. Defaults to 256; a larger value than 4096 is clamped to it."
    )]
    pub count: Option<u32>,
}
