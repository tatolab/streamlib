// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one dispatcher every adapter calls: it runs a registered operation against the runtime
//! and answers its result or its refusal, holding no adapter's wire shape.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use serde::Serialize;
use serde_json::Value;
use streamlib::sdk::error::{PrivacyGatedCaptureDevice, WhyANodeOfAStreamIsNotRunning};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, ProjectOpenOrReloadRequest, StreamActionQueued,
    StreamActionStep, StreamListingState, StreamLocation,
};
use streamlib_control_contract::{
    AddGroupRequest, AddNodeRequest, AddNoteRequest, CONTROL_CONTRACT_VERSION, CONTROL_OPERATIONS,
    CollapseSelectionIntoSubgraphRequest, ControlContractVersionMismatch, ControlOperationCall,
    DescribeRuntimeRequest, DiscoverRequest, DiscoverResult, GraphEditMode, GraphRequest,
    ImportGraphGeneratorRequest, ListStreamsRequest, OneLinkGraphEditRequest,
    OneNodeGraphEditRequest, OneProjectByItsDirectoryRequest, OneStreamActionRequest,
    ProjectTransactionAuthorArgument, ProjectTransactionAuthorKindArgument, RemoveGroupRequest,
    RemoveNoteRequest, RenameNodeRequest, SetExposedConfigFieldsRequest, SetGroupFieldsRequest,
    SetNodeConfigRequest, SetNoteFieldsRequest, SetPinValueRequest, SetStudioFieldsRequest,
    SubscribeProjectChangesRequest, SubscribeProjectChangesResult,
    SubscribeProjectLogRecordsRequest, SubscribeProjectLogRecordsResult,
    SubscribeStreamActionChangesRequest, SubscribeStreamActionChangesResult,
    SubscribeStreamGraphChangesRequest, SubscribeStreamGraphChangesResult, TransactRequest,
    UnlockSubgraphRequest,
};
use streamlib_project_repository::{
    ProjectRepositoriesOpenInThisRuntime, ProjectRepository, ProjectRepositoryRefusal,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, StreamGraphAction,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
    CancelStreamActionToolResult, CaptureDeviceOnTheWire, CloseViewerToolResult,
    CreateNodeFileToolResult, CreateProjectToolResult, DeleteProjectToolResult,
    DependencyTransactionToolResult, DescribeAssetToolResult, DescribeRuntimeToolResult,
    DescribeStreamActionToolResult, DuplicateProjectToolResult, GraphTransactionToolResult,
    ImportProjectToolResult, ListAssetsToolResult, ListDependenciesToolResult,
    ListProjectFilesToolResult, ListProjectNodeTypesToolResult, ListProjectViewsToolResult,
    ListSamplesToolResult, ListStreamsToolResult, ListStreamsToolResultStream, ListedStreamState,
    LogsToolResult, NodeNotRunningBecauseOnTheWire, OpenViewerToolResult, PausePlaybackToolResult,
    ProjectActionRefusalOnTheWire, ProjectFileTransactionToolResult,
    ProjectSettingsTransactionToolResult, ProjectStreamActionQueuedToolResult,
    ProjectTransactionOnTheWire, ProjectTransactionsResourceContents, ReadAssetPreviewToolResult,
    ReadProjectFileToolResult, RemoveProjectToolResult, ResumePlaybackToolResult,
    SetStreamClockScaleToolResult, StreamActionStepOnTheWire, StreamLocationOnTheWire,
    UpdateDependencyToolResult, WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};
use streamlib_runtime_client_contract::project_log_records::project_log_cursor_at_its_end;
use uuid::Uuid;

use crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime;
use crate::control_operations_on_loaded_streams::ViewersOneConnectionHoldsOpen;
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;
use crate::control_subscriptions_of_one_connection::{
    ControlSubscriptionsOfOneConnection, ProjectLogAsSubscribedWithItsEnd,
};
use crate::mcp::{describe_stream_action_tool_result_of, stream_action_state_on_the_wire};
use crate::mcp_project_change_feed::{
    ProjectActionTheRuntimeRefused, ProjectActionsRefusedSinceTheRuntimeStarted,
};
use crate::project_node_types_followed_for_changes::ProjectNodeTypesLastListed;
use crate::stream_graph_transaction_executor::{
    GraphTransactionRefusal, StreamGraphTransactionExecutor,
};

/// The author a change by an unnamed client is recorded under.
const AUTHOR_NAME_OF_AN_UNNAMED_CLIENT: &str = "an MCP client";

/// Where a control request came from, as the runtime saw it arrive.
#[derive(Clone, Debug)]
pub(crate) struct ControlRequestOrigin {
    /// The name the client gave itself, if it gave one.
    pub(crate) client_name: Option<String>,
    /// The subscriptions of the connection it arrived on, when that connection stays open to be
    /// sent notifications.
    pub(crate) subscriptions: Option<Arc<ControlSubscriptionsOfOneConnection>>,
    /// The viewers the connection it arrived on holds open, when that connection stays open.
    pub(crate) viewers_held_open: Option<Arc<ViewersOneConnectionHoldsOpen>>,
}

/// What a registered operation answers when it is not refused; it serializes as the result it
/// holds.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ControlOperationAnswer {
    /// `discover`'s.
    Discover(DiscoverResult),
    /// `subscribe_project_changes`'.
    SubscribeProjectChanges(SubscribeProjectChangesResult),
    /// `subscribe_project_log_records`'.
    SubscribeProjectLogRecords(SubscribeProjectLogRecordsResult),
    /// `subscribe_stream_graph_changes`'.
    SubscribeStreamGraphChanges(SubscribeStreamGraphChangesResult),
    /// `subscribe_stream_action_changes`'.
    SubscribeStreamActionChanges(SubscribeStreamActionChangesResult),
    /// `describe_runtime`'s.
    DescribeRuntime(DescribeRuntimeToolResult),
    /// `list_streams`'s.
    ListStreams(ListStreamsToolResult),
    /// `graph`'s: one stream's graph, or every loaded stream's.
    Graph(Value),
    /// A graph edit's: the transaction it applied.
    GraphTransaction(GraphTransactionToolResult),
    /// A queued project stream action's: its id and its project.
    ProjectStreamActionQueued(ProjectStreamActionQueuedToolResult),
    /// `remove_project`'s.
    RemoveProject(RemoveProjectToolResult),
    /// `pause_playback`'s.
    PausePlayback(PausePlaybackToolResult),
    /// `resume_playback`'s.
    ResumePlayback(ResumePlaybackToolResult),
    /// `describe_stream_action`'s.
    DescribeStreamAction(DescribeStreamActionToolResult),
    /// `cancel_stream_action`'s.
    CancelStreamAction(CancelStreamActionToolResult),
    /// A project settings change's: the transaction it applied and how people see the project.
    ProjectSettingsTransaction(ProjectSettingsTransactionToolResult),
    /// `duplicate_project`'s.
    DuplicateProject(DuplicateProjectToolResult),
    /// `delete_project`'s.
    DeleteProject(DeleteProjectToolResult),
    /// `list_assets`'s.
    ListAssets(ListAssetsToolResult),
    /// `read_project_changes`'.
    ReadProjectChanges(ProjectTransactionsResourceContents),
    /// `describe_asset`'s.
    DescribeAsset(DescribeAssetToolResult),
    /// `read_asset_preview`'s.
    ReadAssetPreview(ReadAssetPreviewToolResult),
    /// `import_asset`'s.
    AssetTransaction(AssetTransactionToolResult),
    /// `add_dependency`'s.
    DependencyTransaction(DependencyTransactionToolResult),
    /// `update_dependency`'s.
    UpdateDependency(UpdateDependencyToolResult),
    /// `list_dependencies`'s.
    ListDependencies(ListDependenciesToolResult),
    /// `list_project_files`'s.
    ListProjectFiles(ListProjectFilesToolResult),
    /// `read_project_file`'s.
    ReadProjectFile(ReadProjectFileToolResult),
    /// `create_node_file`'s.
    CreateNodeFile(CreateNodeFileToolResult),
    /// `write_project_file`'s.
    ProjectFileTransaction(ProjectFileTransactionToolResult),
    /// `list_project_node_types`'s.
    ListProjectNodeTypes(ListProjectNodeTypesToolResult),
    /// `list_project_views`'s.
    ListProjectViews(ListProjectViewsToolResult),
    /// `write_studio_view_output`'s.
    WriteStudioViewOutput(WriteStudioViewOutputToolResult),
    /// `write_graph_input`'s.
    WriteGraphInput(WriteGraphInputToolResult),
    /// `import_project`'s.
    ImportProject(ImportProjectToolResult),
    /// `logs`'s.
    Logs(LogsToolResult),
    /// `open_viewer`'s.
    OpenViewer(OpenViewerToolResult),
    /// `close_viewer`'s.
    CloseViewer(CloseViewerToolResult),
    /// `set_stream_clock_scale`'s.
    SetStreamClockScale(SetStreamClockScaleToolResult),
    /// `create_project`'s.
    CreateProject(CreateProjectToolResult),
    /// `list_samples`'s.
    ListSamples(ListSamplesToolResult),
    /// `undo`'s.
    ProjectTransaction(ProjectTransactionOnTheWire),
}

/// Why a registered operation answered no result.
#[derive(Debug)]
pub(crate) enum ControlOperationRefusal {
    /// The runtime refused the action, naming what it refused in its own words.
    RefusedAction(Box<ProjectActionRefusalOnTheWire>),
    /// The operation could not be carried out, in the runtime's own words.
    Failed(String),
    /// The client was built against another contract version than the runtime serves.
    ContractVersionMismatch(ControlContractVersionMismatch),
}

impl std::fmt::Display for ControlOperationRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RefusedAction(refused) => {
                write!(
                    formatter,
                    "{} failed: {}",
                    refused.action, refused.runtime_words
                )
            }
            Self::Failed(why) => formatter.write_str(why),
            Self::ContractVersionMismatch(mismatch) => mismatch.fmt(formatter),
        }
    }
}

impl From<String> for ControlOperationRefusal {
    fn from(why_in_the_runtime_words: String) -> Self {
        Self::Failed(why_in_the_runtime_words)
    }
}

/// The runtime's services every operation reaches.
#[derive(Clone)]
pub(crate) struct ControlOperationDispatcher {
    pub(crate) operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    pub(crate) project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    pub(crate) project_actions_refused: Arc<ProjectActionsRefusedSinceTheRuntimeStarted>,
    pub(crate) asset_media_facts_reads: Arc<AssetMediaFactsReadsInThisRuntime>,
    pub(crate) project_node_types_last_listed: Arc<ProjectNodeTypesLastListed>,
}

/// A dispatcher's services held without keeping any of them alive, for a task that outlives no
/// handler.
#[derive(Clone)]
pub(crate) struct ControlOperationDispatcherHeldWeakly {
    operations_on_the_loaded_streams: Weak<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_repositories: Weak<ProjectRepositoriesOpenInThisRuntime>,
    project_actions_refused: Weak<ProjectActionsRefusedSinceTheRuntimeStarted>,
    asset_media_facts_reads: Weak<AssetMediaFactsReadsInThisRuntime>,
    project_node_types_last_listed: Weak<ProjectNodeTypesLastListed>,
}

impl ControlOperationDispatcherHeldWeakly {
    /// The dispatcher again, while every one of its services is still alive.
    pub(crate) fn upgrade(&self) -> Option<ControlOperationDispatcher> {
        Some(ControlOperationDispatcher {
            operations_on_the_loaded_streams: self.operations_on_the_loaded_streams.upgrade()?,
            project_repositories: self.project_repositories.upgrade()?,
            project_actions_refused: self.project_actions_refused.upgrade()?,
            asset_media_facts_reads: self.asset_media_facts_reads.upgrade()?,
            project_node_types_last_listed: self.project_node_types_last_listed.upgrade()?,
        })
    }
}

impl ControlOperationDispatcher {
    /// This dispatcher, held without keeping its services alive.
    pub(crate) fn held_weakly(&self) -> ControlOperationDispatcherHeldWeakly {
        ControlOperationDispatcherHeldWeakly {
            operations_on_the_loaded_streams: Arc::downgrade(
                &self.operations_on_the_loaded_streams,
            ),
            project_repositories: Arc::downgrade(&self.project_repositories),
            project_actions_refused: Arc::downgrade(&self.project_actions_refused),
            asset_media_facts_reads: Arc::downgrade(&self.asset_media_facts_reads),
            project_node_types_last_listed: Arc::downgrade(&self.project_node_types_last_listed),
        }
    }

    /// Run `call` for a request from `request_origin`.
    pub(crate) async fn dispatch(
        &self,
        call: ControlOperationCall,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        match call {
            ControlOperationCall::Discover(DiscoverRequest { contract_version }) => self
                .discover(contract_version)
                .map(ControlOperationAnswer::Discover),
            ControlOperationCall::SubscribeProjectChanges(SubscribeProjectChangesRequest {
                projects,
            }) => self
                .subscribe_to_project_changes(projects, request_origin)
                .map(ControlOperationAnswer::SubscribeProjectChanges),
            ControlOperationCall::SubscribeProjectLogRecords(
                SubscribeProjectLogRecordsRequest {
                    project_directories,
                },
            ) => subscribe_to_project_log_records(project_directories, request_origin)
                .map(ControlOperationAnswer::SubscribeProjectLogRecords),
            ControlOperationCall::SubscribeStreamGraphChanges(
                SubscribeStreamGraphChangesRequest { streams },
            ) => self
                .subscribe_to_stream_graph_changes(streams, request_origin)
                .await
                .map(ControlOperationAnswer::SubscribeStreamGraphChanges),
            ControlOperationCall::SubscribeStreamActionChanges(
                SubscribeStreamActionChangesRequest {},
            ) => self
                .subscribe_to_stream_action_changes(request_origin)
                .await
                .map(ControlOperationAnswer::SubscribeStreamActionChanges),
            ControlOperationCall::DescribeRuntime(DescribeRuntimeRequest {}) => {
                Ok(ControlOperationAnswer::DescribeRuntime(
                    self.operations_on_the_loaded_streams
                        .describe_this_runtime(),
                ))
            }
            ControlOperationCall::ListStreams(ListStreamsRequest {}) => self
                .list_streams()
                .await
                .map(ControlOperationAnswer::ListStreams),
            ControlOperationCall::Graph(GraphRequest { stream }) => {
                self.graph(stream).await.map(ControlOperationAnswer::Graph)
            }
            ControlOperationCall::Transact(TransactRequest {
                operations,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "transact",
                    operations,
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::AddNode(AddNodeRequest {
                stream,
                level,
                node_type,
                name,
                config,
                pins,
                exposed_config_fields,
                studio,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "add_node",
                    vec![StreamGraphAction::AddNode {
                        stream,
                        level: level.unwrap_or_default(),
                        node_type,
                        name,
                        config,
                        pins,
                        exposed_config_fields,
                        studio,
                        label: None,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::Connect(OneLinkGraphEditRequest {
                stream,
                level,
                source,
                target,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "connect",
                    vec![StreamGraphAction::Connect {
                        stream,
                        level: level.unwrap_or_default(),
                        source,
                        target,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::ImportGraphGenerator(ImportGraphGeneratorRequest {
                stream,
                generator,
                name,
                config,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "import_graph_generator",
                    vec![StreamGraphAction::ImportGraphGenerator {
                        stream,
                        generator,
                        name,
                        config,
                        label: None,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::RemoveNode(OneNodeGraphEditRequest {
                stream,
                level,
                node,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "remove_node",
                    vec![StreamGraphAction::RemoveNode {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::Disconnect(OneLinkGraphEditRequest {
                stream,
                level,
                source,
                target,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "disconnect",
                    vec![StreamGraphAction::Disconnect {
                        stream,
                        level: level.unwrap_or_default(),
                        source,
                        target,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetNodeConfig(SetNodeConfigRequest {
                stream,
                level,
                node,
                config,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_node_config",
                    vec![StreamGraphAction::SetNodeConfig {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                        config,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetPinValue(SetPinValueRequest {
                stream,
                level,
                node,
                input,
                value,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_pin_value",
                    vec![StreamGraphAction::SetPinValue {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                        input,
                        value,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::RenameNode(RenameNodeRequest {
                stream,
                level,
                node,
                name,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "rename_node",
                    vec![StreamGraphAction::RenameNode {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                        name,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetStudioFields(SetStudioFieldsRequest {
                stream,
                level,
                node,
                studio,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_studio_fields",
                    vec![StreamGraphAction::SetStudioFields {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                        studio,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetExposedConfigFields(SetExposedConfigFieldsRequest {
                stream,
                level,
                node,
                exposed_config_fields,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_exposed_config_fields",
                    vec![StreamGraphAction::SetExposedConfigFields {
                        stream,
                        level: level.unwrap_or_default(),
                        node,
                        exposed_config_fields,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::AddGroup(AddGroupRequest {
                stream,
                level,
                title,
                description,
                colour,
                nodes,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "add_group",
                    vec![StreamGraphAction::AddGroup {
                        stream,
                        level: level.unwrap_or_default(),
                        title,
                        description,
                        colour,
                        nodes,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetGroupFields(SetGroupFieldsRequest {
                stream,
                level,
                group,
                title,
                description,
                colour,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_group_fields",
                    vec![StreamGraphAction::SetGroupFields {
                        stream,
                        level: level.unwrap_or_default(),
                        group,
                        title,
                        description,
                        colour,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::CollapseSelectionIntoSubgraph(
                CollapseSelectionIntoSubgraphRequest {
                    stream,
                    level,
                    nodes,
                    name,
                    mode,
                    author,
                    idempotency_key,
                },
            ) => self
                .apply_a_graph_transaction(
                    "collapse_selection_into_subgraph",
                    vec![StreamGraphAction::CollapseSelectionIntoSubgraph {
                        stream,
                        level: level.unwrap_or_default(),
                        nodes,
                        name,
                        label: None,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::UnlockSubgraph(UnlockSubgraphRequest {
                stream,
                level,
                fragment,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "unlock_subgraph",
                    vec![StreamGraphAction::UnlockSubgraph {
                        stream,
                        level: level.unwrap_or_default(),
                        fragment,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::AddNote(AddNoteRequest {
                stream,
                text,
                position,
                size,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "add_note",
                    vec![StreamGraphAction::AddNote {
                        stream,
                        text,
                        position,
                        size,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::SetNoteFields(SetNoteFieldsRequest {
                stream,
                note,
                text,
                position,
                size,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "set_note_fields",
                    vec![StreamGraphAction::SetNoteFields {
                        stream,
                        note,
                        text,
                        position,
                        size,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::RemoveNote(RemoveNoteRequest {
                stream,
                note,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "remove_note",
                    vec![StreamGraphAction::RemoveNote { stream, note }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::RemoveGroup(RemoveGroupRequest {
                stream,
                level,
                group,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "remove_group",
                    vec![StreamGraphAction::RemoveGroup {
                        stream,
                        level: level.unwrap_or_default(),
                        group,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::OpenProject(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams
                            .open_project(ProjectOpenOrReloadRequest { project_directory })
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::ProjectStreamActionQueued(
                        ProjectStreamActionQueuedToolResult {
                            action: queued.action_id,
                            project_directory: queued.project_directory,
                        },
                    )
                }),
            ControlOperationCall::ReloadProject(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams
                            .reload_project(ProjectOpenOrReloadRequest { project_directory })
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::ProjectStreamActionQueued(
                        ProjectStreamActionQueuedToolResult {
                            action: queued.action_id,
                            project_directory: queued.project_directory,
                        },
                    )
                }),
            ControlOperationCall::CloseProject(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams.close_project(&project_directory)
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::ProjectStreamActionQueued(
                        ProjectStreamActionQueuedToolResult {
                            action: queued.action_id,
                            project_directory: queued.project_directory,
                        },
                    )
                }),
            ControlOperationCall::StartPlayback(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams.start_playback(&project_directory)
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::ProjectStreamActionQueued(
                        ProjectStreamActionQueuedToolResult {
                            action: queued.action_id,
                            project_directory: queued.project_directory,
                        },
                    )
                }),
            ControlOperationCall::StopPlayback(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams.stop_playback(&project_directory)
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::ProjectStreamActionQueued(
                        ProjectStreamActionQueuedToolResult {
                            action: queued.action_id,
                            project_directory: queued.project_directory,
                        },
                    )
                }),
            ControlOperationCall::RemoveProject(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .a_project_stream_action_queued_by(
                    project_directory,
                    |operations_on_the_loaded_streams, project_directory| {
                        operations_on_the_loaded_streams.remove_project(&project_directory)
                    },
                )
                .await
                .map(|queued| {
                    ControlOperationAnswer::RemoveProject(RemoveProjectToolResult {
                        action: queued.action_id,
                        project_directory: queued.project_directory,
                    })
                }),
            ControlOperationCall::PausePlayback(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => {
                refuse_a_relative_project_directory(&project_directory)
                    .map_err(ControlOperationRefusal::Failed)?;
                self.a_stream_action_off_the_async_runtime(
                    move |operations_on_the_loaded_streams| {
                        operations_on_the_loaded_streams.pause_playback(&project_directory)
                    },
                )
                .await
                .map(|paused| {
                    ControlOperationAnswer::PausePlayback(PausePlaybackToolResult {
                        project_directory: paused.project_directory,
                        streams: paused.stream_ids,
                    })
                })
            }
            ControlOperationCall::ResumePlayback(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => {
                refuse_a_relative_project_directory(&project_directory)
                    .map_err(ControlOperationRefusal::Failed)?;
                self.a_stream_action_off_the_async_runtime(
                    move |operations_on_the_loaded_streams| {
                        operations_on_the_loaded_streams.resume_playback(&project_directory)
                    },
                )
                .await
                .map(|resumed| {
                    ControlOperationAnswer::ResumePlayback(ResumePlaybackToolResult {
                        project_directory: resumed.project_directory,
                        streams: resumed.stream_ids,
                    })
                })
            }
            ControlOperationCall::DescribeStreamAction(OneStreamActionRequest { action }) => self
                .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                    operations_on_the_loaded_streams.describe_stream_action(&action)
                })
                .await
                .map(|stream_action| {
                    ControlOperationAnswer::DescribeStreamAction(
                        describe_stream_action_tool_result_of(stream_action),
                    )
                }),
            ControlOperationCall::CancelStreamAction(OneStreamActionRequest { action }) => self
                .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                    operations_on_the_loaded_streams.cancel_stream_action(&action)
                })
                .await
                .map(|stream_action| {
                    ControlOperationAnswer::CancelStreamAction(CancelStreamActionToolResult {
                        action: stream_action.action_id,
                        state: stream_action_state_on_the_wire(stream_action.state),
                    })
                }),
            ControlOperationCall::RenameProject(request) => self
                .rename_project(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectSettingsTransaction),
            ControlOperationCall::SetProjectDescription(request) => self
                .set_project_description(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectSettingsTransaction),
            ControlOperationCall::SetProjectCover(request) => self
                .set_project_cover(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectSettingsTransaction),
            ControlOperationCall::DuplicateProject(request) => self
                .duplicate_project(request, request_origin)
                .await
                .map(ControlOperationAnswer::DuplicateProject),
            ControlOperationCall::DeleteProject(request) => self
                .delete_project(request)
                .await
                .map(ControlOperationAnswer::DeleteProject),
            ControlOperationCall::ListAssets(request) => self
                .list_assets(request)
                .await
                .map(ControlOperationAnswer::ListAssets),
            ControlOperationCall::ReadProjectChanges(OneProjectByItsDirectoryRequest {
                project_directory,
            }) => self
                .read_project_changes(project_directory)
                .await
                .map(ControlOperationAnswer::ReadProjectChanges),
            ControlOperationCall::DescribeAsset(request) => self
                .describe_asset(request)
                .await
                .map(ControlOperationAnswer::DescribeAsset),
            ControlOperationCall::ReadAssetPreview(request) => self
                .read_asset_preview(request)
                .await
                .map(ControlOperationAnswer::ReadAssetPreview),
            ControlOperationCall::ImportAsset(request) => self
                .import_asset(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::RelocateAsset(request) => self
                .relocate_asset(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::DeleteAsset(request) => self
                .delete_asset(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::SetAssetSettings(request) => self
                .set_asset_settings(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::ReplaceAsset(request) => self
                .replace_asset(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::CreateStream(request) => self
                .create_stream(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::DeleteStream(request) => self
                .delete_stream(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::SetStreamDisplayName(request) => self
                .set_stream_display_name(request, request_origin)
                .await
                .map(ControlOperationAnswer::AssetTransaction),
            ControlOperationCall::AddDependency(request) => self
                .add_dependency(request, request_origin)
                .await
                .map(ControlOperationAnswer::DependencyTransaction),
            ControlOperationCall::UpdateDependency(request) => self
                .update_dependency(request, request_origin)
                .await
                .map(ControlOperationAnswer::UpdateDependency),
            ControlOperationCall::ListDependencies(request) => self
                .list_dependencies(request)
                .await
                .map(ControlOperationAnswer::ListDependencies),
            ControlOperationCall::RemoveDependency(request) => self
                .remove_dependency(request, request_origin)
                .await
                .map(ControlOperationAnswer::DependencyTransaction),
            ControlOperationCall::UseDependencyStream(request) => self
                .use_dependency_stream(request, request_origin)
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::ListProjectFiles(request) => self
                .list_project_files(request)
                .await
                .map(ControlOperationAnswer::ListProjectFiles),
            ControlOperationCall::ReadProjectFile(request) => self
                .read_project_file(request)
                .await
                .map(ControlOperationAnswer::ReadProjectFile),
            ControlOperationCall::CreateNodeFile(request) => self
                .create_node_file(request, request_origin)
                .await
                .map(ControlOperationAnswer::CreateNodeFile),
            ControlOperationCall::WriteProjectFile(request) => self
                .write_project_file(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectFileTransaction),
            ControlOperationCall::ListProjectNodeTypes(request) => self
                .list_project_node_types(request)
                .await
                .map(ControlOperationAnswer::ListProjectNodeTypes),
            ControlOperationCall::ListProjectViews(request) => self
                .list_project_views(request)
                .await
                .map(ControlOperationAnswer::ListProjectViews),
            ControlOperationCall::WriteStudioViewOutput(request) => self
                .write_studio_view_output(request)
                .await
                .map(ControlOperationAnswer::WriteStudioViewOutput),
            ControlOperationCall::WriteGraphInput(request) => self
                .write_graph_input(request)
                .await
                .map(ControlOperationAnswer::WriteGraphInput),
            ControlOperationCall::ImportProject(request) => self
                .import_project(request, request_origin)
                .await
                .map(ControlOperationAnswer::ImportProject),
            ControlOperationCall::Logs(request) => {
                self.logs(request).await.map(ControlOperationAnswer::Logs)
            }
            ControlOperationCall::OpenViewer(request) => self
                .open_viewer(request, request_origin)
                .await
                .map(ControlOperationAnswer::OpenViewer),
            ControlOperationCall::CloseViewer(request) => self
                .close_viewer(request, request_origin)
                .await
                .map(ControlOperationAnswer::CloseViewer),
            ControlOperationCall::SetStreamClockScale(request) => self
                .set_stream_clock_scale(request)
                .await
                .map(ControlOperationAnswer::SetStreamClockScale),
            ControlOperationCall::CreateProject(request) => self
                .create_project(request)
                .await
                .map(ControlOperationAnswer::CreateProject),
            ControlOperationCall::ListSamples(request) => self
                .list_samples(request)
                .await
                .map(ControlOperationAnswer::ListSamples),
            ControlOperationCall::RefreshFragments(request) => self
                .refresh_fragments(request, request_origin)
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::Undo(request) => self
                .undo(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectTransaction),
            ControlOperationCall::Redo(request) => self
                .redo(request, request_origin)
                .await
                .map(ControlOperationAnswer::ProjectTransaction),
        }
    }

    /// Run `call` on the project at `project_directory`, its one writer held, on a blocking task;
    /// a refusal is the runtime's own words.
    pub(crate) async fn a_project_repository_call_off_the_async_runtime<
        ProjectRepositoryCallOutcome,
    >(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
        call: impl FnOnce(
            &mut ProjectRepository,
        ) -> Result<ProjectRepositoryCallOutcome, ProjectRepositoryRefusal>
        + Send
        + 'static,
    ) -> Result<ProjectRepositoryCallOutcome, ControlOperationRefusal>
    where
        ProjectRepositoryCallOutcome: Send + 'static,
    {
        let project_repositories = Arc::clone(&self.project_repositories);
        tokio::task::spawn_blocking(move || {
            let project_repository =
                project_repositories.the_project_repository_at(&project_directory)?;
            let mut project_repository = project_repository.lock();
            call(&mut project_repository)
        })
        .await
        .map_err(|join_failure| {
            ControlOperationRefusal::Failed(format!(
                "{method_name} failed: the call did not finish: {join_failure}"
            ))
        })?
        .map_err(|refusal| {
            ControlOperationRefusal::Failed(format!("{method_name} failed: {refusal}"))
        })
    }

    /// The operations on the stream `method_name`'s call names, or the refusal naming the loaded
    /// streams.
    pub(crate) fn the_stream_a_call_names(
        &self,
        method_name: &str,
        stream: &str,
    ) -> Result<Arc<dyn streamlib::sdk::runtime::RuntimeOperations>, ControlOperationRefusal> {
        self.operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(stream)
            .map_err(|refusal| {
                ControlOperationRefusal::Failed(format!("{method_name} failed: {refusal}"))
            })
    }

    /// Run the blocking stream action `action` over the runtime's streams on a blocking task; a
    /// refusal is the engine's own words.
    pub(crate) async fn a_stream_action_off_the_async_runtime<
        StreamActionOutcome: Send + 'static,
    >(
        &self,
        action: impl FnOnce(
            &dyn OperationsOnTheStreamsLoadedInThisRuntime,
        ) -> streamlib::sdk::error::Result<StreamActionOutcome>
        + Send
        + 'static,
    ) -> Result<StreamActionOutcome, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || action(operations_on_the_loaded_streams.as_ref()))
            .await
            .map_err(|join_failure| {
                ControlOperationRefusal::Failed(format!(
                    "the stream action did not finish: {join_failure}"
                ))
            })?
            .map_err(|refusal| ControlOperationRefusal::Failed(refusal.to_string()))
    }

    async fn a_project_stream_action_queued_by(
        &self,
        project_directory: PathBuf,
        queue_the_action: impl FnOnce(
            &dyn OperationsOnTheStreamsLoadedInThisRuntime,
            PathBuf,
        ) -> streamlib::sdk::error::Result<StreamActionQueued>
        + Send
        + 'static,
    ) -> Result<StreamActionQueued, ControlOperationRefusal> {
        refuse_a_relative_project_directory(&project_directory)
            .map_err(ControlOperationRefusal::Failed)?;
        self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
            queue_the_action(operations_on_the_loaded_streams, project_directory)
        })
        .await
    }

    fn discover(
        &self,
        client_contract_version: Option<u32>,
    ) -> Result<DiscoverResult, ControlOperationRefusal> {
        refuse_another_contract_version(client_contract_version)?;
        let this_runtime = self
            .operations_on_the_loaded_streams
            .describe_this_runtime();
        Ok(DiscoverResult {
            contract_version: CONTROL_CONTRACT_VERSION,
            runtime_version: this_runtime.version,
            build_id: this_runtime.build_id,
            operations: CONTROL_OPERATIONS
                .iter()
                .map(|declaration| declaration.method_name.to_owned())
                .collect(),
        })
    }

    async fn list_streams(&self) -> Result<ListStreamsToolResult, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let listings =
            tokio::task::spawn_blocking(move || operations_on_the_loaded_streams.list_streams())
                .await
                .map_err(|join_failure| {
                    ControlOperationRefusal::Failed(format!(
                        "the stream action did not finish: {join_failure}"
                    ))
                })?;
        Ok(ListStreamsToolResult {
            streams: listings
                .into_iter()
                .map(|listing| ListStreamsToolResultStream {
                    stream: listing.stream_id,
                    display_name: listing.display_name,
                    location: listing.stream_location.map(stream_location_on_the_wire),
                    state: listed_stream_state_on_the_wire(listing.state),
                    node_count: listing.node_count,
                    failed_because: listing.failed_because,
                    capture_devices_whose_access_was_never_asked_for: listing
                        .capture_devices_whose_access_was_never_asked_for
                        .into_iter()
                        .map(capture_device_on_the_wire)
                        .collect(),
                    step: listing
                        .stream_action_under_way
                        .as_ref()
                        .and_then(|working_on_it| working_on_it.step)
                        .map(stream_action_step_on_the_wire),
                    action: listing
                        .stream_action_under_way
                        .map(|working_on_it| working_on_it.action_id),
                })
                .collect(),
        })
    }

    async fn graph(&self, stream: Option<String>) -> Result<Value, ControlOperationRefusal> {
        match stream {
            Some(stream) => self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream)
                .map_err(|refusal| {
                    ControlOperationRefusal::Failed(format!("graph failed: {refusal}"))
                })?
                .to_json_async()
                .await
                .map_err(|e| ControlOperationRefusal::Failed(format!("graph export failed: {e}"))),
            None => {
                crate::handlers::machine_wide_graph_json(&self.operations_on_the_loaded_streams)
                    .await
                    .map_err(|e| {
                        ControlOperationRefusal::Failed(format!("graph export failed: {e}"))
                    })
            }
        }
    }

    /// Apply `operations` as one transaction, its refusal kept on the project's change feed
    /// under the action `action_name`.
    pub(crate) async fn apply_a_graph_transaction(
        &self,
        action_name: &'static str,
        operations: Vec<StreamGraphAction>,
        mode: Option<GraphEditMode>,
        author: Option<ProjectTransactionAuthorArgument>,
        idempotency_key: Option<String>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<GraphTransactionToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let mode = mode.unwrap_or_default();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        let project_actions_refused = Arc::clone(&self.project_actions_refused);
        let async_runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let executor = StreamGraphTransactionExecutor {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            };
            let operations_as_requested: Vec<Value> = operations
                .iter()
                .map(|operation| serde_json::to_value(operation).unwrap_or_default())
                .collect();
            let stream_assets_named: Vec<Uuid> =
                operations.iter().map(StreamGraphAction::stream).collect();
            let (idempotency_key, applied) =
                match the_idempotency_key_a_caller_sent(idempotency_key) {
                    Ok(idempotency_key) => {
                        let applied =
                            executor.apply(operations, mode, &author, idempotency_key.as_ref());
                        (idempotency_key, applied)
                    }
                    Err(refusal) => (None, Err(GraphTransactionRefusal::from(refusal))),
                };
            applied.map_err(|refusal| {
                let refusal = refusal.into_the_refusal_on_the_wire(action_name);
                if let Some(project) =
                    executor.the_project_of_the_first_stream_asset_of(&stream_assets_named)
                {
                    project_actions_refused.keep_and_announce(
                        &project_repositories,
                        project,
                        ProjectActionTheRuntimeRefused {
                            author: &author,
                            mode: mode.on_the_wire(),
                            operations_as_requested,
                            idempotency_key: idempotency_key.as_ref(),
                            refusal: refusal.clone(),
                        },
                    );
                }
                ControlOperationRefusal::RefusedAction(Box::new(refusal))
            })
        })
        .await
        .map_err(|join_failure| {
            ControlOperationRefusal::Failed(format!(
                "{action_name} failed: the transaction did not finish: {join_failure}"
            ))
        })?
    }
}

/// Refuse a client that names another contract version than the runtime serves.
pub(crate) fn refuse_another_contract_version(
    client_contract_version: Option<u32>,
) -> Result<(), ControlOperationRefusal> {
    match client_contract_version {
        Some(client_contract_version) if client_contract_version != CONTROL_CONTRACT_VERSION => {
            Err(ControlOperationRefusal::ContractVersionMismatch(
                ControlContractVersionMismatch {
                    client_contract_version,
                    runtime_contract_version: CONTROL_CONTRACT_VERSION,
                },
            ))
        }
        _ => Ok(()),
    }
}

/// The subscriptions of the connection a subscription by `method_name` arrived on, refused for a
/// connection that does not stay open.
fn the_subscriptions_of_the_connection_a_subscription_arrived_on<'origin>(
    method_name: &str,
    request_origin: &'origin ControlRequestOrigin,
) -> Result<&'origin Arc<ControlSubscriptionsOfOneConnection>, ControlOperationRefusal> {
    request_origin.subscriptions.as_ref().ok_or_else(|| {
        ControlOperationRefusal::Failed(format!(
            "{method_name} failed: it is served only on a connection that stays open to be sent \
             notifications, `{CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET}`"
        ))
    })
}

fn subscribe_to_project_log_records(
    project_directories: Vec<std::path::PathBuf>,
    request_origin: &ControlRequestOrigin,
) -> Result<SubscribeProjectLogRecordsResult, ControlOperationRefusal> {
    let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
        "subscribe_project_log_records",
        request_origin,
    )?;
    if project_directories.is_empty() {
        return Err(ControlOperationRefusal::Failed(
            "subscribe_project_log_records failed: `project_directories` names no project"
                .to_owned(),
        ));
    }
    for project_directory in &project_directories {
        refuse_a_relative_project_directory(project_directory).map_err(|why| {
            ControlOperationRefusal::Failed(format!("subscribe_project_log_records failed: {why}"))
        })?;
    }
    let project_logs_as_subscribed_with_symlinks_resolved_and_their_end = project_directories
        .into_iter()
        .map(|project_directory| {
            let project_directory_with_symlinks_resolved = std::fs::canonicalize(
                &project_directory,
            )
            .map_err(|not_resolved| {
                ControlOperationRefusal::Failed(format!(
                    "subscribe_project_log_records failed: `{}` is not a folder on this machine: \
                     {not_resolved}",
                    project_directory.display()
                ))
            })?;
            let cursor_at_the_logs_end = project_log_cursor_at_its_end(
                &project_directory_with_symlinks_resolved,
            )
            .map_err(|not_read| {
                ControlOperationRefusal::Failed(format!(
                    "subscribe_project_log_records failed: {not_read}"
                ))
            })?;
            Ok(ProjectLogAsSubscribedWithItsEnd {
                project_directory_as_subscribed: project_directory,
                project_directory_with_symlinks_resolved,
                cursor_at_the_logs_end,
            })
        })
        .collect::<Result<Vec<_>, ControlOperationRefusal>>()?;
    Ok(SubscribeProjectLogRecordsResult {
        project_directories: subscriptions.subscribe_to_the_project_logs(
            project_logs_as_subscribed_with_symlinks_resolved_and_their_end,
        ),
    })
}

impl ControlOperationDispatcher {
    fn subscribe_to_project_changes(
        &self,
        projects: Vec<Uuid>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeProjectChangesResult, ControlOperationRefusal> {
        let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
            "subscribe_project_changes",
            request_origin,
        )?;
        if projects.is_empty() {
            return Err(ControlOperationRefusal::Failed(
                "subscribe_project_changes failed: `projects` names no project".to_owned(),
            ));
        }
        Ok(SubscribeProjectChangesResult {
            projects: subscriptions.subscribe_to_the_projects(projects, |project| {
                self.project_repositories.watch_the_files_of(project)
            }),
        })
    }

    async fn subscribe_to_stream_graph_changes(
        &self,
        streams: Vec<String>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeStreamGraphChangesResult, ControlOperationRefusal> {
        let subscriptions = Arc::clone(
            the_subscriptions_of_the_connection_a_subscription_arrived_on(
                "subscribe_stream_graph_changes",
                request_origin,
            )?,
        );
        if streams.is_empty() {
            return Err(ControlOperationRefusal::Failed(
                "subscribe_stream_graph_changes failed: `streams` names no stream".to_owned(),
            ));
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || {
            let streams_as_loaded_now = streams
                .iter()
                .map(|stream| {
                    operations_on_the_loaded_streams
                        .loaded_stream_identity_of_the_stream_a_call_names(stream)
                })
                .collect::<streamlib::sdk::error::Result<Vec<_>>>()
                .map_err(|refusal| {
                    ControlOperationRefusal::Failed(format!(
                        "subscribe_stream_graph_changes failed: {refusal}"
                    ))
                })?;
            subscriptions
                .follow_the_stream_graphs(streams_as_loaded_now)
                .map(|streams| SubscribeStreamGraphChangesResult { streams })
                .map_err(|not_subscribed| {
                    ControlOperationRefusal::Failed(format!(
                        "subscribe_stream_graph_changes failed: {not_subscribed}"
                    ))
                })
        })
        .await
        .map_err(|not_run| {
            ControlOperationRefusal::Failed(format!(
                "subscribe_stream_graph_changes failed: {not_run}"
            ))
        })?
    }

    async fn subscribe_to_stream_action_changes(
        &self,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeStreamActionChangesResult, ControlOperationRefusal> {
        let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
            "subscribe_stream_action_changes",
            request_origin,
        )?;
        subscriptions.subscribe_to_stream_actions();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || SubscribeStreamActionChangesResult {
            actions_under_way: operations_on_the_loaded_streams
                .stream_actions_under_way()
                .into_iter()
                .map(|action| action.action_id)
                .collect(),
        })
        .await
        .map_err(|not_run| {
            ControlOperationRefusal::Failed(format!(
                "subscribe_stream_action_changes failed: {not_run}"
            ))
        })
    }
}

/// Refuses a `project_directory` that is not absolute, since the runtime cannot see the caller's
/// working directory.
pub(crate) fn refuse_a_relative_project_directory(
    project_directory: &std::path::Path,
) -> Result<(), String> {
    if project_directory.is_absolute() {
        return Ok(());
    }
    Err(format!(
        "`project_directory` must be an absolute path; got `{}`",
        project_directory.display()
    ))
}

/// The author a change names: the caller's own, or the agent the request's client named itself.
pub(crate) fn the_author_of_a_change(
    author: Option<ProjectTransactionAuthorArgument>,
    request_origin: &ControlRequestOrigin,
) -> ProjectTransactionAuthor {
    match author {
        Some(ProjectTransactionAuthorArgument { kind, name }) => ProjectTransactionAuthor {
            kind: match kind {
                ProjectTransactionAuthorKindArgument::Person => {
                    ProjectTransactionAuthorKind::Person
                }
                ProjectTransactionAuthorKindArgument::Agent => ProjectTransactionAuthorKind::Agent,
            },
            name,
        },
        None => ProjectTransactionAuthor {
            kind: ProjectTransactionAuthorKind::Agent,
            name: request_origin
                .client_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| AUTHOR_NAME_OF_AN_UNNAMED_CLIENT.to_owned()),
        },
    }
}

pub(crate) fn stream_location_on_the_wire(
    stream_location: StreamLocation,
) -> StreamLocationOnTheWire {
    StreamLocationOnTheWire {
        project_directory: stream_location.project_directory,
        project: stream_location.project,
        stream_asset: stream_location.stream_asset,
    }
}

pub(crate) fn listed_stream_state_on_the_wire(state: StreamListingState) -> ListedStreamState {
    match state {
        StreamListingState::Running => ListedStreamState::Running,
        StreamListingState::Stopped => ListedStreamState::Stopped,
        StreamListingState::Paused => ListedStreamState::Paused,
        StreamListingState::Failed => ListedStreamState::Failed,
        StreamListingState::Preparing => ListedStreamState::Preparing,
        StreamListingState::Stopping => ListedStreamState::Stopping,
        StreamListingState::Closed => ListedStreamState::Closed,
    }
}

fn capture_device_on_the_wire(device: PrivacyGatedCaptureDevice) -> CaptureDeviceOnTheWire {
    match device {
        PrivacyGatedCaptureDevice::Camera => CaptureDeviceOnTheWire::Camera,
        PrivacyGatedCaptureDevice::Microphone => CaptureDeviceOnTheWire::Microphone,
    }
}

pub(crate) fn stream_action_step_on_the_wire(step: StreamActionStep) -> StreamActionStepOnTheWire {
    match step {
        StreamActionStep::MakingTheVenv => StreamActionStepOnTheWire::MakingTheVenv,
        StreamActionStep::ReadingTheGraphDocuments => {
            StreamActionStepOnTheWire::ReadingTheGraphDocuments
        }
        StreamActionStep::Starting => StreamActionStepOnTheWire::Starting,
        StreamActionStep::Stopping => StreamActionStepOnTheWire::Stopping,
    }
}

/// Why a reach for a node of a stream was refused: structured, naming `not_running_because`,
/// when the node does not run, and the runtime's words otherwise.
pub(crate) fn the_refusal_of_a_reach_for_a_node(
    method_name: &str,
    refusal: streamlib::sdk::error::Error,
) -> ControlOperationRefusal {
    match refusal {
        streamlib::sdk::error::Error::NodeOfAStreamIsNotRunning {
            not_running_because,
            runtime_words,
        } => ControlOperationRefusal::RefusedAction(Box::new(ProjectActionRefusalOnTheWire {
            action: method_name.to_owned(),
            refused_operation_index: None,
            refused_node: None,
            refused_link: None,
            refused_field: None,
            not_running_because: Some(match not_running_because {
                WhyANodeOfAStreamIsNotRunning::PlaybackStopped => {
                    NodeNotRunningBecauseOnTheWire::PlaybackStopped
                }
                WhyANodeOfAStreamIsNotRunning::PlaybackPaused => {
                    NodeNotRunningBecauseOnTheWire::PlaybackPaused
                }
                WhyANodeOfAStreamIsNotRunning::Preparing => {
                    NodeNotRunningBecauseOnTheWire::Preparing
                }
                WhyANodeOfAStreamIsNotRunning::NodeFailed => {
                    NodeNotRunningBecauseOnTheWire::NodeFailed
                }
            }),
            runtime_words,
        })),
        refusal => ControlOperationRefusal::Failed(format!("{method_name} failed: {refusal}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_origin_named(client_name: Option<&str>) -> ControlRequestOrigin {
        ControlRequestOrigin {
            client_name: client_name.map(str::to_owned),
            subscriptions: None,
            viewers_held_open: None,
        }
    }

    #[test]
    fn a_change_with_no_author_is_by_the_agent_its_client_named() {
        let author = the_author_of_a_change(None, &an_origin_named(Some("tato-studio")));
        assert_eq!(author.kind, ProjectTransactionAuthorKind::Agent);
        assert_eq!(author.name, "tato-studio");
    }

    #[test]
    fn a_change_from_an_unnamed_or_blank_client_is_by_an_unnamed_client() {
        for client_name in [None, Some("  ")] {
            let author = the_author_of_a_change(None, &an_origin_named(client_name));
            assert_eq!(author.name, AUTHOR_NAME_OF_AN_UNNAMED_CLIENT);
        }
    }

    #[test]
    fn a_named_author_is_recorded_as_named_whatever_the_client() {
        let author = the_author_of_a_change(
            Some(ProjectTransactionAuthorArgument {
                kind: ProjectTransactionAuthorKindArgument::Person,
                name: "Dana".to_owned(),
            }),
            &an_origin_named(Some("tato-studio")),
        );
        assert_eq!(author.kind, ProjectTransactionAuthorKind::Person);
        assert_eq!(author.name, "Dana");
    }
}
