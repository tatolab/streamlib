// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations on a project's files, node types, views and viewers.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, AUTHOR_ARGUMENT_DESCRIPTION,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
};

/// What `project_file_path` says.
pub const PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION: &str = "The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`; refused outside the project, inside `.tatolab/`, or through a symbolic link.";

/// `list_project_files`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectFilesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// A folder of the project, `/`-separated and relative to the project directory, to list alone.
    #[schemars(
        description = "A folder of the project, `/`-separated and relative to the project directory, to list alone; the whole project when omitted."
    )]
    pub under: Option<String>,
}

/// `read_project_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadProjectFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`.
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    pub path: String,
}

/// `write_project_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteProjectFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`.
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    pub path: String,
    /// The file's whole new content as UTF-8 text.
    #[schemars(
        description = "The file's whole new content as UTF-8 text. Send this or `content_base64`, never both."
    )]
    pub content: Option<String>,
    /// The file's whole new content as standard base64 with padding, for bytes that are not text.
    #[schemars(
        description = "The file's whole new content as standard base64 with padding, for bytes that are not text. Send this or `content`, never both."
    )]
    pub content_base64: Option<String>,
    /// The `content_sha256` `read_project_file` or the last `write_project_file` answered for the
    /// file.
    #[schemars(
        description = "The `content_sha256` `read_project_file` or the last `write_project_file` answered for the file: the write is refused, and the file left as it is, when the file no longer holds those bytes. Omitted, the write goes over whatever the file holds."
    )]
    pub expected_sha256: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `create_node_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNodeFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The new Python file, `/`-separated and relative to the project directory, such as
    /// `nodes/edge_glow.py`.
    #[schemars(
        description = "The new Python file, `/`-separated and relative to the project directory, such as `nodes/edge_glow.py`: each folder and the file's name a Python identifier, the class named after the file (`EdgeGlow`). Refused where a file already is, outside the project, inside `.tatolab/` or `imports/`, or through a symbolic link."
    )]
    pub path: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `list_project_node_types`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectNodeTypesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `check_project_code`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckProjectCodeRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The files to check, each `/`-separated and relative to the project directory.
    #[schemars(
        description = "The files to check, each `/`-separated and relative to the project directory, such as `nodes/blur.py`; every module the project's node type listing names when omitted. Refused for a path that is not a file of the project."
    )]
    pub paths: Option<Vec<String>>,
}

/// `list_project_views`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectViewsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `list_project_node_ui_elements`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectNodeUiElementsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `write_studio_view_output`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteStudioViewOutputRequest {
    /// The stream id of the stream holding the view node.
    #[schemars(description = "The stream id of the stream holding the view node.")]
    pub stream: String,
    /// The view node's UUID, as `graph` lists it under `id`.
    #[schemars(description = "The view node's UUID, as `graph` lists it under `id`.")]
    pub node: String,
    /// The view node's output port the value is published on.
    #[schemars(description = "The view node's output port the value is published on.")]
    pub port: String,
    /// The value, as the port's family carries it.
    #[schemars(
        description = "The value, as the port's family carries it: a Records port takes a JSON object; an Array port a number or a list of numbers, nested to any depth, as its `values` attribute of float64, or an object of such attributes by name; a Channels port a number, as one control value, or `{\"samples\": [[...], ...], \"sample_rate\"}`, float32 channel by channel."
    )]
    pub value: serde_json::Value,
}

/// What every required `stream` argument says.
pub const STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream this call is about, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";

/// `open_viewer`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenViewerRequest {
    /// The loaded stream this call is about, by the stream id `list_streams` and `graph` report.
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: String,
    /// The node's id, as `graph` lists it under `id`.
    #[schemars(
        description = "The node's id, as `graph` lists it under `id`; its name is a label, not its identity."
    )]
    pub node_id: String,
    /// The node's output port, as `graph` lists it under `ports.outputs`.
    #[schemars(description = "The node's output port, as `graph` lists it under `ports.outputs`.")]
    pub port: String,
    /// The process id of the viewer that imports the port's Image frames.
    #[schemars(
        description = "The process id of the viewer that imports the port's Image frames. On macOS the surface-sharing service admits only that process, a running process of the runtime's user, to new connections while the viewer stays open, and without it the answer names no `surface_share_address`; Linux admits whoever may open its socket."
    )]
    #[serde(default)]
    pub viewer_process_id: Option<u32>,
}

/// `close_viewer`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseViewerRequest {
    /// The viewer, by the id `open_viewer` answered.
    #[schemars(description = "The viewer, by the id `open_viewer` answered.")]
    pub viewer: String,
}
