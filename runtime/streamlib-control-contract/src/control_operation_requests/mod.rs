// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params each registered operation takes, and the argument types several share.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

mod asset_and_stream_requests;
mod dependency_requests;
mod discovery_and_subscription_requests;
mod graph_edit_requests;
mod playback_and_stream_action_requests;
mod project_file_and_view_requests;
mod project_lifecycle_requests;

pub use asset_and_stream_requests::{
    ASSET_ARGUMENT_DESCRIPTION, CreateStreamRequest, DeleteAssetRequest, DeleteStreamRequest,
    DescribeAssetRequest, ImportAssetRequest, ListAssetsRequest, ProjectAssetKindArgument,
    ReadAssetPreviewRequest, RelocateAssetRequest, ReplaceAssetRequest,
    STREAM_ASSET_ARGUMENT_DESCRIPTION, STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION,
    SetAssetSettingsRequest, SetStreamDisplayNameRequest,
};
pub use dependency_requests::{
    AddDependencyRequest, IMPORT_SOURCE_ARGUMENT_DESCRIPTION,
    IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, ListDependenciesRequest,
    RefreshFragmentsRequest, RemoveDependencyRequest, UpdateDependencyRequest,
    UseDependencyStreamRequest,
};
pub use discovery_and_subscription_requests::{
    DescribeRuntimeRequest, DiscoverRequest, GRAPH_STREAM_ARGUMENT_DESCRIPTION, GraphRequest,
    ListStreamsRequest, LogsRequest, SubscribeProjectChangesRequest,
    SubscribeProjectLogRecordsRequest, SubscribeStreamActionChangesRequest,
    SubscribeStreamGraphChangesRequest,
};
pub use graph_edit_requests::{
    AddGroupRequest, AddNodeRequest, AddNoteRequest, CollapseSelectionIntoSubgraphRequest,
    ImportGraphGeneratorRequest, OneLinkGraphEditRequest, OneNodeGraphEditRequest,
    RemoveGroupRequest, RemoveNoteRequest, RenameNodeRequest, SetExposedConfigFieldsRequest,
    SetGroupFieldsRequest, SetNodeConfigRequest, SetNoteFieldsRequest, SetPinValueRequest,
    SetStudioFieldsRequest, TransactRequest, UndoOrRedoRequest, UnlockSubgraphRequest,
    WriteGraphInputRequest,
};
pub use playback_and_stream_action_requests::{
    OneStreamActionRequest, STREAM_ACTION_ARGUMENT_DESCRIPTION,
    STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION, SetStreamClockScaleRequest,
};
pub use project_file_and_view_requests::{
    CloseViewerRequest, CreateNodeFileRequest, ListProjectFilesRequest,
    ListProjectNodeTypesRequest, ListProjectViewsRequest, OpenViewerRequest,
    PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION, ReadProjectFileRequest, STREAM_ARGUMENT_DESCRIPTION,
    WriteProjectFileRequest, WriteStudioViewOutputRequest,
};
pub use project_lifecycle_requests::{
    CreateProjectRequest, DuplicateProjectRequest, ImportProjectRequest, ListSamplesRequest,
    RenameProjectRequest, SetProjectCoverRequest, SetProjectDescriptionRequest,
};

/// What a graph edit's `mode` argument says.
pub const GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION: &str = "`commit`, the default: the edit is applied to the running stream, written to the stream's graph document and recorded as one commit on the project's history, so it is kept on the next load and undo reverses it. `live`: applied to the running stream alone, gone on the next load, never in history. Send a continuous gesture as one transaction when it ends.";

/// What a change's `author` argument says.
pub const AUTHOR_ARGUMENT_DESCRIPTION: &str = "Who the change is by, as history records it and undo follows it. Omit it to be the agent the MCP client named in its `clientInfo`.";

/// What a change's `idempotency_key` argument says.
pub const IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION: &str = "A key the caller mints once per user gesture, such as a slider drag committed on release. Sent again under a key the project's history holds, the action applies nothing and answers the transaction the key first applied. Omit it to apply the action each time it is sent. A `live` edit is never recorded, so its key is not kept.";

/// What `project_directory` says where it names the project an operation acts on.
pub const PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str = "The project, by absolute path: a folder holding `tatolab-project.json` and its stream assets' graph documents.";

/// What `asset_project_directory` says.
pub const ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project, by absolute path: a folder holding `tatolab-project.json`.";

/// Whether a graph edit is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphEditMode {
    /// Applied, written to the graph document and recorded in history.
    #[default]
    Commit,
    /// Applied to the running stream alone.
    Live,
}

impl GraphEditMode {
    /// The mode as the wire spells it.
    pub fn on_the_wire(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Live => "live",
        }
    }
}

/// Who a change is by, as a caller names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTransactionAuthorKindArgument {
    /// A person, through the studio or the CLI.
    Person,
    /// An agent.
    Agent,
}

/// A change's author as a caller names it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectTransactionAuthorArgument {
    /// Whether a person or an agent made the change.
    #[schemars(description = "`person` or `agent`.")]
    pub kind: ProjectTransactionAuthorKindArgument,
    /// The author's name.
    #[schemars(description = "The author's name as history shows it.")]
    pub name: String,
}

/// The params of an operation on one project, named by its directory.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneProjectByItsDirectoryRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}
