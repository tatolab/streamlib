// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one dispatcher every adapter calls: it runs a registered operation against the runtime
//! and answers its result or its refusal, holding no adapter's wire shape.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use serde::Serialize;
use serde_json::Value;
use streamlib::sdk::error::{PrivacyGatedCaptureDevice, WhyANodeOfAStreamIsNotRunning};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, StreamActionQueued, StreamActionStep,
    StreamListingState, StreamLocation,
};
use streamlib_control_contract::{
    CONTROL_CONTRACT_VERSION, ControlContractVersionMismatch, ControlOperationCall,
    DescribeRuntimeRequest, DiscoverRequest, DiscoverResult, GraphEditMode, GraphRequest,
    ListStreamsRequest, OneProjectByItsDirectoryRequest, ProjectTransactionAuthorArgument,
    ProjectTransactionAuthorKindArgument, SubscribeInputValueChangesRequest,
    SubscribeInputValueChangesResult, SubscribeProjectChangesRequest,
    SubscribeProjectChangesResult, SubscribeProjectLogRecordsRequest,
    SubscribeProjectLogRecordsResult, SubscribeStreamActionChangesRequest,
    SubscribeStreamActionChangesResult, SubscribeStreamGraphChangesRequest,
    SubscribeStreamGraphChangesResult,
};
use streamlib_project_repository::{
    ProjectRepositoriesOpenInThisRuntime, ProjectRepository, ProjectRepositoryRefusal,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, StreamGraphAction,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, CancelStreamActionToolResult, CaptureDeviceOnTheWire,
    CloseViewerToolResult, ControlOperationRefusalCode, CreateNodeFileToolResult,
    CreateProjectToolResult, DeleteProjectToolResult, DependencyTransactionToolResult,
    DescribeAssetToolResult, DescribeRuntimeToolResult, DescribeStreamActionToolResult,
    DuplicateProjectToolResult, GraphTransactionToolResult, ImportProjectToolResult,
    ListAssetsToolResult, ListDependenciesToolResult, ListProjectFilesToolResult,
    ListProjectNodeTypesToolResult, ListProjectViewsToolResult, ListSamplesToolResult,
    ListStreamsToolResult, ListedStreamState, LogsToolResult, NodeNotRunningBecauseOnTheWire,
    OpenViewerToolResult, PausePlaybackToolResult, ProjectActionRefusalOnTheWire,
    ProjectFileTransactionToolResult, ProjectSettingsTransactionToolResult,
    ProjectStreamActionQueuedToolResult, ProjectTransactionOnTheWire,
    ProjectTransactionsResourceContents, ReadAssetPreviewToolResult, ReadProjectFileToolResult,
    RemoveProjectToolResult, ResumePlaybackToolResult, SetStreamClockScaleToolResult,
    StreamActionStepOnTheWire, StreamLocationOnTheWire, UpdateDependencyToolResult,
    WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};
use uuid::Uuid;

use crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime;
use crate::control_operations_on_loaded_streams::ViewersOneConnectionHoldsOpen;
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;
use crate::control_subscriptions_of_one_connection::ControlSubscriptionsOfOneConnection;
use crate::mcp_project_change_feed::{
    ProjectActionTheRuntimeRefused, ProjectActionsRefusedSinceTheRuntimeStarted,
};
use crate::project_node_types_followed_for_changes::ProjectNodeTypesLastListed;
use crate::stream_graph_transaction_executor::{
    GraphTransactionRefusal, StreamGraphTransactionExecutor,
};

mod discovery_and_subscription_operation_answers;
mod graph_edit_operation_answers;
mod playback_and_stream_action_operation_answers;
mod project_lifecycle_operation_answers;

use discovery_and_subscription_operation_answers::subscribe_to_project_log_records;

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
    /// `subscribe_input_value_changes`'.
    SubscribeInputValueChanges(SubscribeInputValueChangesResult),
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
    /// The operation was understood and not carried out, in the runtime's own words.
    Failed(String),
    /// The runtime could not carry the operation out through no doing of the caller, in its own
    /// words.
    RuntimeFailed(String),
    /// The client was built against another contract version than the runtime serves.
    ContractVersionMismatch(ControlContractVersionMismatch),
}

impl ControlOperationRefusal {
    /// This refusal of `action` as every door carries it.
    pub(crate) fn into_the_refusal_on_the_wire(
        self,
        action: &str,
    ) -> ProjectActionRefusalOnTheWire {
        match self {
            Self::RefusedAction(refused) => *refused,
            Self::Failed(why) => ProjectActionRefusalOnTheWire::of_the_action(
                action,
                ControlOperationRefusalCode::Refused,
                why,
            ),
            Self::RuntimeFailed(why) => ProjectActionRefusalOnTheWire::of_the_action(
                action,
                ControlOperationRefusalCode::RuntimeFailed,
                why,
            ),
            Self::ContractVersionMismatch(mismatch) => {
                ProjectActionRefusalOnTheWire::of_the_action(
                    action,
                    ControlOperationRefusalCode::Refused,
                    format!("{action} failed: {mismatch}"),
                )
            }
        }
    }

    /// This refusal, of the same kind, its words led by `context`.
    pub(crate) fn with_its_words_after(self, context: impl std::fmt::Display) -> Self {
        match self {
            Self::Failed(why) => Self::Failed(format!("{context}: {why}")),
            Self::RuntimeFailed(why) => Self::RuntimeFailed(format!("{context}: {why}")),
            Self::RefusedAction(mut refused) => {
                refused.runtime_words = format!("{context}: {}", refused.runtime_words);
                Self::RefusedAction(refused)
            }
            Self::ContractVersionMismatch(mismatch) => Self::ContractVersionMismatch(mismatch),
        }
    }

    /// The refusal of a task handed off the async runtime that did not finish, `task` naming it,
    /// as in `"logs failed: the read"`.
    pub(crate) fn the_task_did_not_finish(
        task: impl std::fmt::Display,
    ) -> impl FnOnce(tokio::task::JoinError) -> Self {
        move |join_failure| Self::RuntimeFailed(format!("{task} did not finish: {join_failure}"))
    }
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
            Self::Failed(why) | Self::RuntimeFailed(why) => formatter.write_str(why),
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
            ControlOperationCall::SubscribeInputValueChanges(
                SubscribeInputValueChangesRequest { streams },
            ) => self
                .subscribe_to_input_value_changes(streams, request_origin)
                .await
                .map(ControlOperationAnswer::SubscribeInputValueChanges),
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
            ControlOperationCall::Transact(request) => {
                self.answer_transact(request, request_origin).await
            }
            ControlOperationCall::AddNode(request) => {
                self.answer_add_node(request, request_origin).await
            }
            ControlOperationCall::Connect(request) => {
                self.answer_connect(request, request_origin).await
            }
            ControlOperationCall::ImportGraphGenerator(request) => {
                self.answer_import_graph_generator(request, request_origin)
                    .await
            }
            ControlOperationCall::RemoveNode(request) => {
                self.answer_remove_node(request, request_origin).await
            }
            ControlOperationCall::Disconnect(request) => {
                self.answer_disconnect(request, request_origin).await
            }
            ControlOperationCall::SetNodeConfig(request) => {
                self.answer_set_node_config(request, request_origin).await
            }
            ControlOperationCall::SetPinValue(request) => {
                self.answer_set_pin_value(request, request_origin).await
            }
            ControlOperationCall::SetNodeInputValue(request) => {
                self.answer_set_node_input_value(request, request_origin)
                    .await
            }
            ControlOperationCall::RenameNode(request) => {
                self.answer_rename_node(request, request_origin).await
            }
            ControlOperationCall::SetStudioFields(request) => {
                self.answer_set_studio_fields(request, request_origin).await
            }
            ControlOperationCall::SetExposedConfigFields(request) => {
                self.answer_set_exposed_config_fields(request, request_origin)
                    .await
            }
            ControlOperationCall::AddGroup(request) => {
                self.answer_add_group(request, request_origin).await
            }
            ControlOperationCall::SetGroupFields(request) => {
                self.answer_set_group_fields(request, request_origin).await
            }
            ControlOperationCall::CollapseSelectionIntoSubgraph(request) => {
                self.answer_collapse_selection_into_subgraph(request, request_origin)
                    .await
            }
            ControlOperationCall::UnlockSubgraph(request) => {
                self.answer_unlock_subgraph(request, request_origin).await
            }
            ControlOperationCall::AddNote(request) => {
                self.answer_add_note(request, request_origin).await
            }
            ControlOperationCall::SetNoteFields(request) => {
                self.answer_set_note_fields(request, request_origin).await
            }
            ControlOperationCall::RemoveNote(request) => {
                self.answer_remove_note(request, request_origin).await
            }
            ControlOperationCall::RemoveGroup(request) => {
                self.answer_remove_group(request, request_origin).await
            }
            ControlOperationCall::OpenProject(request) => self.answer_open_project(request).await,
            ControlOperationCall::ReloadProject(request) => {
                self.answer_reload_project(request).await
            }
            ControlOperationCall::CloseProject(request) => self.answer_close_project(request).await,
            ControlOperationCall::StartPlayback(request) => {
                self.answer_start_playback(request).await
            }
            ControlOperationCall::StopPlayback(request) => self.answer_stop_playback(request).await,
            ControlOperationCall::RemoveProject(request) => {
                self.answer_remove_project(request).await
            }
            ControlOperationCall::PausePlayback(request) => {
                self.answer_pause_playback(request).await
            }
            ControlOperationCall::ResumePlayback(request) => {
                self.answer_resume_playback(request).await
            }
            ControlOperationCall::DescribeStreamAction(request) => {
                self.answer_describe_stream_action(request).await
            }
            ControlOperationCall::CancelStreamAction(request) => {
                self.answer_cancel_stream_action(request).await
            }
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
        .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
            "{method_name} failed: the call"
        )))?
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
            .map_err(ControlOperationRefusal::the_task_did_not_finish(
                "the stream action",
            ))?
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
        .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
            "{action_name} failed: the transaction"
        )))?
    }
}

/// Refuse a client that names another contract version than the runtime serves.
pub(crate) fn refuse_another_contract_version(
    client_contract_version: Option<u32>,
) -> Result<(), ControlContractVersionMismatch> {
    match client_contract_version {
        Some(client_contract_version) if client_contract_version != CONTROL_CONTRACT_VERSION => {
            Err(ControlContractVersionMismatch {
                client_contract_version,
                runtime_contract_version: CONTROL_CONTRACT_VERSION,
            })
        }
        _ => Ok(()),
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
            ..ProjectActionRefusalOnTheWire::of_the_action(
                method_name,
                ControlOperationRefusalCode::NodeNotRunning,
                runtime_words,
            )
        })),
        refusal => ControlOperationRefusal::Failed(format!("{method_name} failed: {refusal}")),
    }
}

#[cfg(test)]
mod tests;
