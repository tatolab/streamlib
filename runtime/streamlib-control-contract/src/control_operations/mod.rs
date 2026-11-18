// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Every operation the registry holds, the table the adapters iterate, and the one call type a
//! dispatcher matches on.

use serde_json::Value;

use crate::control_operation::{
    ControlOperation, ControlOperationDeclaration, ControlOperationExposure,
};
use crate::control_operation_refusals::{ParamsThatDoNotMatchTheirSchema, the_params_read_as};
use crate::control_operation_requests::{
    AddDependencyRequest, AddGroupRequest, AddNodeRequest, AddNoteRequest, CloseViewerRequest,
    CollapseSelectionIntoSubgraphRequest, CreateNodeFileRequest, CreateProjectRequest,
    CreateStreamRequest, DeleteAssetRequest, DeleteStreamRequest, DescribeAssetRequest,
    DescribeRuntimeRequest, DiscoverRequest, DuplicateProjectRequest, GraphRequest,
    ImportAssetRequest, ImportGraphGeneratorRequest, ImportProjectRequest, ListAssetsRequest,
    ListDependenciesRequest, ListProjectFilesRequest, ListProjectNodeTypesRequest,
    ListProjectViewsRequest, ListSamplesRequest, ListStreamsRequest, LogsRequest,
    OneLinkGraphEditRequest, OneNodeGraphEditRequest, OneProjectByItsDirectoryRequest,
    OneStreamActionRequest, OpenViewerRequest, ReadAssetPreviewRequest, ReadProjectFileRequest,
    RefreshFragmentsRequest, RelocateAssetRequest, RemoveDependencyRequest, RemoveGroupRequest,
    RemoveNoteRequest, RenameNodeRequest, RenameProjectRequest, ReplaceAssetRequest,
    SetAssetSettingsRequest, SetExposedConfigFieldsRequest, SetGroupFieldsRequest,
    SetNodeConfigRequest, SetNodeInputValueRequest, SetNoteFieldsRequest, SetPinValueRequest,
    SetProjectCoverRequest, SetProjectDescriptionRequest, SetStreamClockScaleRequest,
    SetStreamDisplayNameRequest, SetStudioFieldsRequest, SubscribeInputValueChangesRequest,
    SubscribeProjectChangesRequest, SubscribeProjectLogRecordsRequest,
    SubscribeStreamActionChangesRequest, SubscribeStreamGraphChangesRequest, TransactRequest,
    UndoOrRedoRequest, UnlockSubgraphRequest, UpdateDependencyRequest, UseDependencyStreamRequest,
    WriteGraphInputRequest, WriteProjectFileRequest, WriteStudioViewOutputRequest,
};

mod assets_and_streams;
mod dependencies;
mod discovery_and_subscriptions;
mod graph_edits;
mod playback_and_stream_actions;
mod project_files_and_views;
mod project_lifecycle;

pub use assets_and_streams::{
    CreateStream, DeleteAsset, DeleteStream, DescribeAsset, ImportAsset, ListAssets,
    ReadAssetPreview, RelocateAsset, ReplaceAsset, SetAssetSettings, SetStreamDisplayName,
};
pub use dependencies::{
    AddDependency, ListDependencies, RefreshFragments, RemoveDependency, UpdateDependency,
    UseDependencyStream,
};
pub use discovery_and_subscriptions::{
    DescribeRuntime, Discover, Graph, ListStreams, Logs, ReadProjectChanges,
    SubscribeInputValueChanges, SubscribeProjectChanges, SubscribeProjectLogRecords,
    SubscribeStreamActionChanges, SubscribeStreamGraphChanges,
};
pub use graph_edits::{
    AddGroup, AddNode, AddNote, CollapseSelectionIntoSubgraph, Connect, Disconnect,
    ImportGraphGenerator, Redo, RemoveGroup, RemoveNode, RemoveNote, RenameNode,
    SetExposedConfigFields, SetGroupFields, SetNodeConfig, SetNodeInputValue, SetNoteFields,
    SetPinValue, SetStudioFields, Transact, Undo, UnlockSubgraph, WriteGraphInput,
};
pub use playback_and_stream_actions::{
    CancelStreamAction, DescribeStreamAction, PausePlayback, ResumePlayback, SetStreamClockScale,
    StartPlayback, StopPlayback,
};
pub use project_files_and_views::{
    CloseViewer, CreateNodeFile, ListProjectFiles, ListProjectNodeTypes, ListProjectViews,
    OpenViewer, ReadProjectFile, WriteProjectFile, WriteStudioViewOutput,
};
pub use project_lifecycle::{
    CloseProject, CreateProject, DeleteProject, DuplicateProject, ImportProject, ListSamples,
    OpenProject, ReloadProject, RemoveProject, RenameProject, SetProjectCover,
    SetProjectDescription,
};

/// The contract's version: bumped only by a breaking change to an operation already in it.
pub const CONTROL_CONTRACT_VERSION: u32 = 3;

/// The contract's revision: raised by an addition, and reset to 0 when the version is bumped.
pub const CONTROL_CONTRACT_REVISION: u32 = 4;

const EVERY_SURFACE: &[ControlOperationExposure] = &[
    ControlOperationExposure::Studio,
    ControlOperationExposure::Agent,
    ControlOperationExposure::Public,
];

const EVERY_SURFACE_BUT_THE_AGENT_SURFACE: &[ControlOperationExposure] = &[
    ControlOperationExposure::Studio,
    ControlOperationExposure::Public,
];

/// Every operation the registry holds, in the order the contract file lists them.
pub const CONTROL_OPERATIONS: &[ControlOperationDeclaration] = &[
    ControlOperationDeclaration::of::<Discover>(),
    ControlOperationDeclaration::of::<SubscribeProjectChanges>(),
    ControlOperationDeclaration::of::<SubscribeStreamGraphChanges>(),
    ControlOperationDeclaration::of::<SubscribeInputValueChanges>(),
    ControlOperationDeclaration::of::<SubscribeStreamActionChanges>(),
    ControlOperationDeclaration::of::<SubscribeProjectLogRecords>(),
    ControlOperationDeclaration::of::<DescribeRuntime>(),
    ControlOperationDeclaration::of::<ListStreams>(),
    ControlOperationDeclaration::of::<Graph>(),
    ControlOperationDeclaration::of::<Transact>(),
    ControlOperationDeclaration::of::<AddNode>(),
    ControlOperationDeclaration::of::<Connect>(),
    ControlOperationDeclaration::of::<ImportGraphGenerator>(),
    ControlOperationDeclaration::of::<RemoveNode>(),
    ControlOperationDeclaration::of::<Disconnect>(),
    ControlOperationDeclaration::of::<SetNodeConfig>(),
    ControlOperationDeclaration::of::<SetPinValue>(),
    ControlOperationDeclaration::of::<SetNodeInputValue>(),
    ControlOperationDeclaration::of::<RenameNode>(),
    ControlOperationDeclaration::of::<SetStudioFields>(),
    ControlOperationDeclaration::of::<SetExposedConfigFields>(),
    ControlOperationDeclaration::of::<AddGroup>(),
    ControlOperationDeclaration::of::<SetGroupFields>(),
    ControlOperationDeclaration::of::<CollapseSelectionIntoSubgraph>(),
    ControlOperationDeclaration::of::<UnlockSubgraph>(),
    ControlOperationDeclaration::of::<AddNote>(),
    ControlOperationDeclaration::of::<SetNoteFields>(),
    ControlOperationDeclaration::of::<RemoveNote>(),
    ControlOperationDeclaration::of::<RemoveGroup>(),
    ControlOperationDeclaration::of::<OpenProject>(),
    ControlOperationDeclaration::of::<ReloadProject>(),
    ControlOperationDeclaration::of::<CloseProject>(),
    ControlOperationDeclaration::of::<RemoveProject>(),
    ControlOperationDeclaration::of::<StartPlayback>(),
    ControlOperationDeclaration::of::<StopPlayback>(),
    ControlOperationDeclaration::of::<PausePlayback>(),
    ControlOperationDeclaration::of::<ResumePlayback>(),
    ControlOperationDeclaration::of::<DescribeStreamAction>(),
    ControlOperationDeclaration::of::<CancelStreamAction>(),
    ControlOperationDeclaration::of::<RenameProject>(),
    ControlOperationDeclaration::of::<SetProjectDescription>(),
    ControlOperationDeclaration::of::<SetProjectCover>(),
    ControlOperationDeclaration::of::<DuplicateProject>(),
    ControlOperationDeclaration::of::<DeleteProject>(),
    ControlOperationDeclaration::of::<ListAssets>(),
    ControlOperationDeclaration::of::<ReadProjectChanges>(),
    ControlOperationDeclaration::of::<DescribeAsset>(),
    ControlOperationDeclaration::of::<ReadAssetPreview>(),
    ControlOperationDeclaration::of::<ImportAsset>(),
    ControlOperationDeclaration::of::<RelocateAsset>(),
    ControlOperationDeclaration::of::<DeleteAsset>(),
    ControlOperationDeclaration::of::<SetAssetSettings>(),
    ControlOperationDeclaration::of::<ReplaceAsset>(),
    ControlOperationDeclaration::of::<CreateStream>(),
    ControlOperationDeclaration::of::<DeleteStream>(),
    ControlOperationDeclaration::of::<SetStreamDisplayName>(),
    ControlOperationDeclaration::of::<AddDependency>(),
    ControlOperationDeclaration::of::<UpdateDependency>(),
    ControlOperationDeclaration::of::<ListDependencies>(),
    ControlOperationDeclaration::of::<RemoveDependency>(),
    ControlOperationDeclaration::of::<UseDependencyStream>(),
    ControlOperationDeclaration::of::<ListProjectFiles>(),
    ControlOperationDeclaration::of::<ReadProjectFile>(),
    ControlOperationDeclaration::of::<CreateNodeFile>(),
    ControlOperationDeclaration::of::<WriteProjectFile>(),
    ControlOperationDeclaration::of::<ListProjectNodeTypes>(),
    ControlOperationDeclaration::of::<ListProjectViews>(),
    ControlOperationDeclaration::of::<WriteStudioViewOutput>(),
    ControlOperationDeclaration::of::<WriteGraphInput>(),
    ControlOperationDeclaration::of::<ImportProject>(),
    ControlOperationDeclaration::of::<Logs>(),
    ControlOperationDeclaration::of::<OpenViewer>(),
    ControlOperationDeclaration::of::<CloseViewer>(),
    ControlOperationDeclaration::of::<SetStreamClockScale>(),
    ControlOperationDeclaration::of::<CreateProject>(),
    ControlOperationDeclaration::of::<ListSamples>(),
    ControlOperationDeclaration::of::<RefreshFragments>(),
    ControlOperationDeclaration::of::<Undo>(),
    ControlOperationDeclaration::of::<Redo>(),
];

/// One call of a registered operation with its parsed params; a dispatcher matches on it
/// exhaustively, so an operation added here and not served is a compile error.
#[derive(Debug)]
pub enum ControlOperationCall {
    /// A `discover` call.
    Discover(DiscoverRequest),
    /// A `subscribe_project_changes` call.
    SubscribeProjectChanges(SubscribeProjectChangesRequest),
    /// A `subscribe_stream_graph_changes` call.
    SubscribeStreamGraphChanges(SubscribeStreamGraphChangesRequest),
    /// A `subscribe_input_value_changes` call.
    SubscribeInputValueChanges(SubscribeInputValueChangesRequest),
    /// A `subscribe_stream_action_changes` call.
    SubscribeStreamActionChanges(SubscribeStreamActionChangesRequest),
    /// A `subscribe_project_log_records` call.
    SubscribeProjectLogRecords(SubscribeProjectLogRecordsRequest),
    /// A `describe_runtime` call.
    DescribeRuntime(DescribeRuntimeRequest),
    /// A `list_streams` call.
    ListStreams(ListStreamsRequest),
    /// A `graph` call.
    Graph(GraphRequest),
    /// A `transact` call.
    Transact(TransactRequest),
    /// An `add_node` call.
    AddNode(AddNodeRequest),
    /// A `connect` call.
    Connect(OneLinkGraphEditRequest),
    /// A `import_graph_generator` call.
    ImportGraphGenerator(ImportGraphGeneratorRequest),
    /// A `remove_node` call.
    RemoveNode(OneNodeGraphEditRequest),
    /// A `disconnect` call.
    Disconnect(OneLinkGraphEditRequest),
    /// A `set_node_config` call.
    SetNodeConfig(SetNodeConfigRequest),
    /// A `set_pin_value` call.
    SetPinValue(SetPinValueRequest),
    /// A `set_node_input_value` call.
    SetNodeInputValue(SetNodeInputValueRequest),
    /// A `rename_node` call.
    RenameNode(RenameNodeRequest),
    /// A `set_studio_fields` call.
    SetStudioFields(SetStudioFieldsRequest),
    /// A `set_exposed_config_fields` call.
    SetExposedConfigFields(SetExposedConfigFieldsRequest),
    /// A `add_group` call.
    AddGroup(AddGroupRequest),
    /// A `set_group_fields` call.
    SetGroupFields(SetGroupFieldsRequest),
    /// A `collapse_selection_into_subgraph` call.
    CollapseSelectionIntoSubgraph(CollapseSelectionIntoSubgraphRequest),
    /// A `unlock_subgraph` call.
    UnlockSubgraph(UnlockSubgraphRequest),
    /// A `add_note` call.
    AddNote(AddNoteRequest),
    /// A `set_note_fields` call.
    SetNoteFields(SetNoteFieldsRequest),
    /// A `remove_note` call.
    RemoveNote(RemoveNoteRequest),
    /// A `remove_group` call.
    RemoveGroup(RemoveGroupRequest),
    /// An `open_project` call.
    OpenProject(OneProjectByItsDirectoryRequest),
    /// A `reload_project` call.
    ReloadProject(OneProjectByItsDirectoryRequest),
    /// A `close_project` call.
    CloseProject(OneProjectByItsDirectoryRequest),
    /// A `remove_project` call.
    RemoveProject(OneProjectByItsDirectoryRequest),
    /// A `start_playback` call.
    StartPlayback(OneProjectByItsDirectoryRequest),
    /// A `stop_playback` call.
    StopPlayback(OneProjectByItsDirectoryRequest),
    /// A `pause_playback` call.
    PausePlayback(OneProjectByItsDirectoryRequest),
    /// A `resume_playback` call.
    ResumePlayback(OneProjectByItsDirectoryRequest),
    /// A `describe_stream_action` call.
    DescribeStreamAction(OneStreamActionRequest),
    /// A `cancel_stream_action` call.
    CancelStreamAction(OneStreamActionRequest),
    /// A `rename_project` call.
    RenameProject(RenameProjectRequest),
    /// A `set_project_description` call.
    SetProjectDescription(SetProjectDescriptionRequest),
    /// A `set_project_cover` call.
    SetProjectCover(SetProjectCoverRequest),
    /// A `duplicate_project` call.
    DuplicateProject(DuplicateProjectRequest),
    /// A `delete_project` call.
    DeleteProject(OneProjectByItsDirectoryRequest),
    /// A `list_assets` call.
    ListAssets(ListAssetsRequest),
    /// A `read_project_changes` call.
    ReadProjectChanges(OneProjectByItsDirectoryRequest),
    /// A `describe_asset` call.
    DescribeAsset(DescribeAssetRequest),
    /// A `read_asset_preview` call.
    ReadAssetPreview(ReadAssetPreviewRequest),
    /// A `import_asset` call.
    ImportAsset(ImportAssetRequest),
    /// A `relocate_asset` call.
    RelocateAsset(RelocateAssetRequest),
    /// A `delete_asset` call.
    DeleteAsset(DeleteAssetRequest),
    /// A `set_asset_settings` call.
    SetAssetSettings(SetAssetSettingsRequest),
    /// A `replace_asset` call.
    ReplaceAsset(ReplaceAssetRequest),
    /// A `create_stream` call.
    CreateStream(CreateStreamRequest),
    /// A `delete_stream` call.
    DeleteStream(DeleteStreamRequest),
    /// A `set_stream_display_name` call.
    SetStreamDisplayName(SetStreamDisplayNameRequest),
    /// A `add_dependency` call.
    AddDependency(AddDependencyRequest),
    /// A `update_dependency` call.
    UpdateDependency(UpdateDependencyRequest),
    /// A `list_dependencies` call.
    ListDependencies(ListDependenciesRequest),
    /// A `remove_dependency` call.
    RemoveDependency(RemoveDependencyRequest),
    /// A `use_dependency_stream` call.
    UseDependencyStream(UseDependencyStreamRequest),
    /// A `list_project_files` call.
    ListProjectFiles(ListProjectFilesRequest),
    /// A `read_project_file` call.
    ReadProjectFile(ReadProjectFileRequest),
    /// A `create_node_file` call.
    CreateNodeFile(CreateNodeFileRequest),
    /// A `write_project_file` call.
    WriteProjectFile(WriteProjectFileRequest),
    /// A `list_project_node_types` call.
    ListProjectNodeTypes(ListProjectNodeTypesRequest),
    /// A `list_project_views` call.
    ListProjectViews(ListProjectViewsRequest),
    /// A `write_studio_view_output` call.
    WriteStudioViewOutput(WriteStudioViewOutputRequest),
    /// A `write_graph_input` call.
    WriteGraphInput(WriteGraphInputRequest),
    /// A `import_project` call.
    ImportProject(ImportProjectRequest),
    /// A `logs` call.
    Logs(LogsRequest),
    /// A `open_viewer` call.
    OpenViewer(OpenViewerRequest),
    /// A `close_viewer` call.
    CloseViewer(CloseViewerRequest),
    /// A `set_stream_clock_scale` call.
    SetStreamClockScale(SetStreamClockScaleRequest),
    /// A `create_project` call.
    CreateProject(CreateProjectRequest),
    /// A `list_samples` call.
    ListSamples(ListSamplesRequest),
    /// A `refresh_fragments` call.
    RefreshFragments(RefreshFragmentsRequest),
    /// A `undo` call.
    Undo(UndoOrRedoRequest),
    /// A `redo` call.
    Redo(UndoOrRedoRequest),
}

/// Why a method and its params do not make a call.
#[derive(Debug, PartialEq, Eq)]
pub enum ControlOperationCallParseRefusal {
    /// No registered operation has the method name.
    NoSuchMethod {
        /// The method name as sent.
        method_name: String,
    },
    /// The params do not match the operation's request.
    InvalidParams {
        /// The method name.
        method_name: &'static str,
        /// What the request's parse said.
        why: String,
        /// The JSON pointer of the value the parse stopped at; absent when it stopped at the
        /// params as a whole.
        params_pointer: Option<String>,
    },
}

impl std::fmt::Display for ControlOperationCallParseRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchMethod { method_name } => {
                write!(
                    formatter,
                    "the runtime serves no operation named `{method_name}`"
                )
            }
            Self::InvalidParams {
                method_name, why, ..
            } => {
                write!(
                    formatter,
                    "`{method_name}`'s params do not match its request: {why}"
                )
            }
        }
    }
}

impl ControlOperationCall {
    /// The call `method_name` names with `params`; absent params read as `{}`.
    pub fn parse(
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Self, ControlOperationCallParseRefusal> {
        let params = params.unwrap_or_else(|| Value::Object(Default::default()));
        match method_name {
            Discover::METHOD_NAME => the_request_of::<Discover>(params).map(Self::Discover),
            SubscribeProjectChanges::METHOD_NAME => {
                the_request_of::<SubscribeProjectChanges>(params).map(Self::SubscribeProjectChanges)
            }
            SubscribeStreamGraphChanges::METHOD_NAME => {
                the_request_of::<SubscribeStreamGraphChanges>(params)
                    .map(Self::SubscribeStreamGraphChanges)
            }
            SubscribeInputValueChanges::METHOD_NAME => {
                the_request_of::<SubscribeInputValueChanges>(params)
                    .map(Self::SubscribeInputValueChanges)
            }
            SubscribeStreamActionChanges::METHOD_NAME => {
                the_request_of::<SubscribeStreamActionChanges>(params)
                    .map(Self::SubscribeStreamActionChanges)
            }
            SubscribeProjectLogRecords::METHOD_NAME => {
                the_request_of::<SubscribeProjectLogRecords>(params)
                    .map(Self::SubscribeProjectLogRecords)
            }
            DescribeRuntime::METHOD_NAME => {
                the_request_of::<DescribeRuntime>(params).map(Self::DescribeRuntime)
            }
            ListStreams::METHOD_NAME => {
                the_request_of::<ListStreams>(params).map(Self::ListStreams)
            }
            Graph::METHOD_NAME => the_request_of::<Graph>(params).map(Self::Graph),
            Transact::METHOD_NAME => the_request_of::<Transact>(params).map(Self::Transact),
            AddNode::METHOD_NAME => the_request_of::<AddNode>(params).map(Self::AddNode),
            Connect::METHOD_NAME => the_request_of::<Connect>(params).map(Self::Connect),
            ImportGraphGenerator::METHOD_NAME => {
                the_request_of::<ImportGraphGenerator>(params).map(Self::ImportGraphGenerator)
            }
            RemoveNode::METHOD_NAME => the_request_of::<RemoveNode>(params).map(Self::RemoveNode),
            Disconnect::METHOD_NAME => the_request_of::<Disconnect>(params).map(Self::Disconnect),
            SetNodeConfig::METHOD_NAME => {
                the_request_of::<SetNodeConfig>(params).map(Self::SetNodeConfig)
            }
            SetPinValue::METHOD_NAME => {
                the_request_of::<SetPinValue>(params).map(Self::SetPinValue)
            }
            SetNodeInputValue::METHOD_NAME => {
                the_request_of::<SetNodeInputValue>(params).map(Self::SetNodeInputValue)
            }
            RenameNode::METHOD_NAME => the_request_of::<RenameNode>(params).map(Self::RenameNode),
            SetStudioFields::METHOD_NAME => {
                the_request_of::<SetStudioFields>(params).map(Self::SetStudioFields)
            }
            SetExposedConfigFields::METHOD_NAME => {
                the_request_of::<SetExposedConfigFields>(params).map(Self::SetExposedConfigFields)
            }
            AddGroup::METHOD_NAME => the_request_of::<AddGroup>(params).map(Self::AddGroup),
            SetGroupFields::METHOD_NAME => {
                the_request_of::<SetGroupFields>(params).map(Self::SetGroupFields)
            }
            CollapseSelectionIntoSubgraph::METHOD_NAME => {
                the_request_of::<CollapseSelectionIntoSubgraph>(params)
                    .map(Self::CollapseSelectionIntoSubgraph)
            }
            UnlockSubgraph::METHOD_NAME => {
                the_request_of::<UnlockSubgraph>(params).map(Self::UnlockSubgraph)
            }
            AddNote::METHOD_NAME => the_request_of::<AddNote>(params).map(Self::AddNote),
            SetNoteFields::METHOD_NAME => {
                the_request_of::<SetNoteFields>(params).map(Self::SetNoteFields)
            }
            RemoveNote::METHOD_NAME => the_request_of::<RemoveNote>(params).map(Self::RemoveNote),
            RemoveGroup::METHOD_NAME => {
                the_request_of::<RemoveGroup>(params).map(Self::RemoveGroup)
            }
            OpenProject::METHOD_NAME => {
                the_request_of::<OpenProject>(params).map(Self::OpenProject)
            }
            ReloadProject::METHOD_NAME => {
                the_request_of::<ReloadProject>(params).map(Self::ReloadProject)
            }
            CloseProject::METHOD_NAME => {
                the_request_of::<CloseProject>(params).map(Self::CloseProject)
            }
            RemoveProject::METHOD_NAME => {
                the_request_of::<RemoveProject>(params).map(Self::RemoveProject)
            }
            StartPlayback::METHOD_NAME => {
                the_request_of::<StartPlayback>(params).map(Self::StartPlayback)
            }
            StopPlayback::METHOD_NAME => {
                the_request_of::<StopPlayback>(params).map(Self::StopPlayback)
            }
            PausePlayback::METHOD_NAME => {
                the_request_of::<PausePlayback>(params).map(Self::PausePlayback)
            }
            ResumePlayback::METHOD_NAME => {
                the_request_of::<ResumePlayback>(params).map(Self::ResumePlayback)
            }
            DescribeStreamAction::METHOD_NAME => {
                the_request_of::<DescribeStreamAction>(params).map(Self::DescribeStreamAction)
            }
            CancelStreamAction::METHOD_NAME => {
                the_request_of::<CancelStreamAction>(params).map(Self::CancelStreamAction)
            }
            RenameProject::METHOD_NAME => {
                the_request_of::<RenameProject>(params).map(Self::RenameProject)
            }
            SetProjectDescription::METHOD_NAME => {
                the_request_of::<SetProjectDescription>(params).map(Self::SetProjectDescription)
            }
            SetProjectCover::METHOD_NAME => {
                the_request_of::<SetProjectCover>(params).map(Self::SetProjectCover)
            }
            DuplicateProject::METHOD_NAME => {
                the_request_of::<DuplicateProject>(params).map(Self::DuplicateProject)
            }
            DeleteProject::METHOD_NAME => {
                the_request_of::<DeleteProject>(params).map(Self::DeleteProject)
            }
            ListAssets::METHOD_NAME => the_request_of::<ListAssets>(params).map(Self::ListAssets),
            ReadProjectChanges::METHOD_NAME => {
                the_request_of::<ReadProjectChanges>(params).map(Self::ReadProjectChanges)
            }
            DescribeAsset::METHOD_NAME => {
                the_request_of::<DescribeAsset>(params).map(Self::DescribeAsset)
            }
            ReadAssetPreview::METHOD_NAME => {
                the_request_of::<ReadAssetPreview>(params).map(Self::ReadAssetPreview)
            }
            ImportAsset::METHOD_NAME => {
                the_request_of::<ImportAsset>(params).map(Self::ImportAsset)
            }
            RelocateAsset::METHOD_NAME => {
                the_request_of::<RelocateAsset>(params).map(Self::RelocateAsset)
            }
            DeleteAsset::METHOD_NAME => {
                the_request_of::<DeleteAsset>(params).map(Self::DeleteAsset)
            }
            SetAssetSettings::METHOD_NAME => {
                the_request_of::<SetAssetSettings>(params).map(Self::SetAssetSettings)
            }
            ReplaceAsset::METHOD_NAME => {
                the_request_of::<ReplaceAsset>(params).map(Self::ReplaceAsset)
            }
            CreateStream::METHOD_NAME => {
                the_request_of::<CreateStream>(params).map(Self::CreateStream)
            }
            DeleteStream::METHOD_NAME => {
                the_request_of::<DeleteStream>(params).map(Self::DeleteStream)
            }
            SetStreamDisplayName::METHOD_NAME => {
                the_request_of::<SetStreamDisplayName>(params).map(Self::SetStreamDisplayName)
            }
            AddDependency::METHOD_NAME => {
                the_request_of::<AddDependency>(params).map(Self::AddDependency)
            }
            UpdateDependency::METHOD_NAME => {
                the_request_of::<UpdateDependency>(params).map(Self::UpdateDependency)
            }
            ListDependencies::METHOD_NAME => {
                the_request_of::<ListDependencies>(params).map(Self::ListDependencies)
            }
            RemoveDependency::METHOD_NAME => {
                the_request_of::<RemoveDependency>(params).map(Self::RemoveDependency)
            }
            UseDependencyStream::METHOD_NAME => {
                the_request_of::<UseDependencyStream>(params).map(Self::UseDependencyStream)
            }
            ListProjectFiles::METHOD_NAME => {
                the_request_of::<ListProjectFiles>(params).map(Self::ListProjectFiles)
            }
            ReadProjectFile::METHOD_NAME => {
                the_request_of::<ReadProjectFile>(params).map(Self::ReadProjectFile)
            }
            CreateNodeFile::METHOD_NAME => {
                the_request_of::<CreateNodeFile>(params).map(Self::CreateNodeFile)
            }
            WriteProjectFile::METHOD_NAME => {
                the_request_of::<WriteProjectFile>(params).map(Self::WriteProjectFile)
            }
            ListProjectNodeTypes::METHOD_NAME => {
                the_request_of::<ListProjectNodeTypes>(params).map(Self::ListProjectNodeTypes)
            }
            ListProjectViews::METHOD_NAME => {
                the_request_of::<ListProjectViews>(params).map(Self::ListProjectViews)
            }
            WriteStudioViewOutput::METHOD_NAME => {
                the_request_of::<WriteStudioViewOutput>(params).map(Self::WriteStudioViewOutput)
            }
            WriteGraphInput::METHOD_NAME => {
                the_request_of::<WriteGraphInput>(params).map(Self::WriteGraphInput)
            }
            ImportProject::METHOD_NAME => {
                the_request_of::<ImportProject>(params).map(Self::ImportProject)
            }
            Logs::METHOD_NAME => the_request_of::<Logs>(params).map(Self::Logs),
            OpenViewer::METHOD_NAME => the_request_of::<OpenViewer>(params).map(Self::OpenViewer),
            CloseViewer::METHOD_NAME => {
                the_request_of::<CloseViewer>(params).map(Self::CloseViewer)
            }
            SetStreamClockScale::METHOD_NAME => {
                the_request_of::<SetStreamClockScale>(params).map(Self::SetStreamClockScale)
            }
            CreateProject::METHOD_NAME => {
                the_request_of::<CreateProject>(params).map(Self::CreateProject)
            }
            ListSamples::METHOD_NAME => {
                the_request_of::<ListSamples>(params).map(Self::ListSamples)
            }
            RefreshFragments::METHOD_NAME => {
                the_request_of::<RefreshFragments>(params).map(Self::RefreshFragments)
            }
            Undo::METHOD_NAME => the_request_of::<Undo>(params).map(Self::Undo),
            Redo::METHOD_NAME => the_request_of::<Redo>(params).map(Self::Redo),
            unknown => Err(ControlOperationCallParseRefusal::NoSuchMethod {
                method_name: unknown.to_owned(),
            }),
        }
    }

    /// The method name of the operation called.
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::Discover(_) => Discover::METHOD_NAME,
            Self::SubscribeProjectChanges(_) => SubscribeProjectChanges::METHOD_NAME,
            Self::SubscribeProjectLogRecords(_) => SubscribeProjectLogRecords::METHOD_NAME,
            Self::SubscribeStreamGraphChanges(_) => SubscribeStreamGraphChanges::METHOD_NAME,
            Self::SubscribeInputValueChanges(_) => SubscribeInputValueChanges::METHOD_NAME,
            Self::SubscribeStreamActionChanges(_) => SubscribeStreamActionChanges::METHOD_NAME,
            Self::DescribeRuntime(_) => DescribeRuntime::METHOD_NAME,
            Self::ListStreams(_) => ListStreams::METHOD_NAME,
            Self::Graph(_) => Graph::METHOD_NAME,
            Self::Transact(_) => Transact::METHOD_NAME,
            Self::AddNode(_) => AddNode::METHOD_NAME,
            Self::Connect(_) => Connect::METHOD_NAME,
            Self::ImportGraphGenerator(_) => ImportGraphGenerator::METHOD_NAME,
            Self::RemoveNode(_) => RemoveNode::METHOD_NAME,
            Self::Disconnect(_) => Disconnect::METHOD_NAME,
            Self::SetNodeConfig(_) => SetNodeConfig::METHOD_NAME,
            Self::SetPinValue(_) => SetPinValue::METHOD_NAME,
            Self::SetNodeInputValue(_) => SetNodeInputValue::METHOD_NAME,
            Self::RenameNode(_) => RenameNode::METHOD_NAME,
            Self::SetStudioFields(_) => SetStudioFields::METHOD_NAME,
            Self::SetExposedConfigFields(_) => SetExposedConfigFields::METHOD_NAME,
            Self::AddGroup(_) => AddGroup::METHOD_NAME,
            Self::SetGroupFields(_) => SetGroupFields::METHOD_NAME,
            Self::CollapseSelectionIntoSubgraph(_) => CollapseSelectionIntoSubgraph::METHOD_NAME,
            Self::UnlockSubgraph(_) => UnlockSubgraph::METHOD_NAME,
            Self::AddNote(_) => AddNote::METHOD_NAME,
            Self::SetNoteFields(_) => SetNoteFields::METHOD_NAME,
            Self::RemoveNote(_) => RemoveNote::METHOD_NAME,
            Self::RemoveGroup(_) => RemoveGroup::METHOD_NAME,
            Self::OpenProject(_) => OpenProject::METHOD_NAME,
            Self::ReloadProject(_) => ReloadProject::METHOD_NAME,
            Self::CloseProject(_) => CloseProject::METHOD_NAME,
            Self::RemoveProject(_) => RemoveProject::METHOD_NAME,
            Self::StartPlayback(_) => StartPlayback::METHOD_NAME,
            Self::StopPlayback(_) => StopPlayback::METHOD_NAME,
            Self::PausePlayback(_) => PausePlayback::METHOD_NAME,
            Self::ResumePlayback(_) => ResumePlayback::METHOD_NAME,
            Self::DescribeStreamAction(_) => DescribeStreamAction::METHOD_NAME,
            Self::CancelStreamAction(_) => CancelStreamAction::METHOD_NAME,
            Self::RenameProject(_) => RenameProject::METHOD_NAME,
            Self::SetProjectDescription(_) => SetProjectDescription::METHOD_NAME,
            Self::SetProjectCover(_) => SetProjectCover::METHOD_NAME,
            Self::DuplicateProject(_) => DuplicateProject::METHOD_NAME,
            Self::DeleteProject(_) => DeleteProject::METHOD_NAME,
            Self::ListAssets(_) => ListAssets::METHOD_NAME,
            Self::ReadProjectChanges(_) => ReadProjectChanges::METHOD_NAME,
            Self::DescribeAsset(_) => DescribeAsset::METHOD_NAME,
            Self::ReadAssetPreview(_) => ReadAssetPreview::METHOD_NAME,
            Self::ImportAsset(_) => ImportAsset::METHOD_NAME,
            Self::RelocateAsset(_) => RelocateAsset::METHOD_NAME,
            Self::DeleteAsset(_) => DeleteAsset::METHOD_NAME,
            Self::SetAssetSettings(_) => SetAssetSettings::METHOD_NAME,
            Self::ReplaceAsset(_) => ReplaceAsset::METHOD_NAME,
            Self::CreateStream(_) => CreateStream::METHOD_NAME,
            Self::DeleteStream(_) => DeleteStream::METHOD_NAME,
            Self::SetStreamDisplayName(_) => SetStreamDisplayName::METHOD_NAME,
            Self::AddDependency(_) => AddDependency::METHOD_NAME,
            Self::UpdateDependency(_) => UpdateDependency::METHOD_NAME,
            Self::ListDependencies(_) => ListDependencies::METHOD_NAME,
            Self::RemoveDependency(_) => RemoveDependency::METHOD_NAME,
            Self::UseDependencyStream(_) => UseDependencyStream::METHOD_NAME,
            Self::ListProjectFiles(_) => ListProjectFiles::METHOD_NAME,
            Self::ReadProjectFile(_) => ReadProjectFile::METHOD_NAME,
            Self::CreateNodeFile(_) => CreateNodeFile::METHOD_NAME,
            Self::WriteProjectFile(_) => WriteProjectFile::METHOD_NAME,
            Self::ListProjectNodeTypes(_) => ListProjectNodeTypes::METHOD_NAME,
            Self::ListProjectViews(_) => ListProjectViews::METHOD_NAME,
            Self::WriteStudioViewOutput(_) => WriteStudioViewOutput::METHOD_NAME,
            Self::WriteGraphInput(_) => WriteGraphInput::METHOD_NAME,
            Self::ImportProject(_) => ImportProject::METHOD_NAME,
            Self::Logs(_) => Logs::METHOD_NAME,
            Self::OpenViewer(_) => OpenViewer::METHOD_NAME,
            Self::CloseViewer(_) => CloseViewer::METHOD_NAME,
            Self::SetStreamClockScale(_) => SetStreamClockScale::METHOD_NAME,
            Self::CreateProject(_) => CreateProject::METHOD_NAME,
            Self::ListSamples(_) => ListSamples::METHOD_NAME,
            Self::RefreshFragments(_) => RefreshFragments::METHOD_NAME,
            Self::Undo(_) => Undo::METHOD_NAME,
            Self::Redo(_) => Redo::METHOD_NAME,
        }
    }
}

fn the_request_of<Operation: ControlOperation>(
    params: Value,
) -> Result<Operation::Request, ControlOperationCallParseRefusal> {
    the_params_read_as(params).map_err(|ParamsThatDoNotMatchTheirSchema { pointer, why }| {
        ControlOperationCallParseRefusal::InvalidParams {
            method_name: Operation::METHOD_NAME,
            why,
            params_pointer: pointer,
        }
    })
}

#[cfg(test)]
mod tests;
