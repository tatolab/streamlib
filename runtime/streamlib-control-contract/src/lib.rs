// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every operation a StreamLib runtime's control plane serves, each declared once by an
//! [`ControlOperation`] impl, and the contract file every adapter, SDK and reference page is
//! generated from.

mod agent_surface;
mod control_contract_document;
mod control_notification;
mod control_operation;
mod control_operation_requests;
mod control_operation_results;
mod control_operations;

pub use agent_surface::{
    AGENT_DISCOVERY_TOOLS, AgentDiscoveryToolDeclaration, AgentOperationDescribed,
    AgentOperationGroup, AgentOperationInItsGroup, AgentToolListed, CALL_OPERATION_TOOL_NAME,
    CallOperationRequest, DESCRIBE_OPERATIONS_TOOL_NAME, DescribeOperationsRequest,
    DescribeOperationsResult, LIST_OPERATION_GROUPS_TOOL_NAME, ListOperationGroupsRequest,
    ListOperationGroupsResult, a_json_schema_carrying_its_own_definitions,
    the_agent_tool_call_that_reaches, the_agent_tools_listed,
    the_operation_groups_served_to_agents, the_operation_served_to_agents_as,
    the_operations_described_for, the_operations_served_to_agents,
};
pub use control_contract_document::{
    CONTROL_CONTRACT_FILE_WORKSPACE_PATH, CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
    CONTROL_OPERATION_FAILED_ERROR_CODE, CONTROL_OPERATION_REFUSED_ERROR_CODE,
    OPENRPC_DOCUMENT_VERSION, control_contract_document, control_contract_file_contents,
};
pub use control_notification::{
    CONTROL_NOTIFICATIONS, ControlNotification, ControlNotificationDeclaration, NodeStateOnTheWire,
    ProjectChangeKindOnTheWire, ProjectChanged, ProjectChangedNotificationParams,
    ProjectLogRecordsChangeKindOnTheWire, ProjectLogRecordsWritten,
    ProjectLogRecordsWrittenNotificationParams, StreamActionChangeKindOnTheWire,
    StreamActionChanged, StreamActionChangedNotificationParams, StreamGraphChangeKindOnTheWire,
    StreamGraphChanged, StreamGraphChangedNotificationParams,
};
pub use control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationDeclaration,
    ControlOperationEffect, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
pub use control_operation_requests::{
    ASSET_ARGUMENT_DESCRIPTION, ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION,
    AUTHOR_ARGUMENT_DESCRIPTION, AddDependencyRequest, AddGroupRequest, AddNodeRequest,
    AddNoteRequest, CloseViewerRequest, CollapseSelectionIntoSubgraphRequest,
    CreateNodeFileRequest, CreateProjectRequest, CreateStreamRequest, DeleteAssetRequest,
    DeleteStreamRequest, DescribeAssetRequest, DescribeRuntimeRequest, DiscoverRequest,
    DuplicateProjectRequest, GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION,
    GRAPH_STREAM_ARGUMENT_DESCRIPTION, GraphEditMode, GraphRequest,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, IMPORT_SOURCE_ARGUMENT_DESCRIPTION,
    IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, ImportAssetRequest,
    ImportGraphGeneratorRequest, ImportProjectRequest, ListAssetsRequest, ListDependenciesRequest,
    ListProjectFilesRequest, ListProjectNodeTypesRequest, ListProjectViewsRequest,
    ListSamplesRequest, ListStreamsRequest, LogsRequest, OneLinkGraphEditRequest,
    OneNodeGraphEditRequest, OneProjectByItsDirectoryRequest, OneStreamActionRequest,
    OpenViewerRequest, PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION,
    PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION, ProjectAssetKindArgument,
    ProjectTransactionAuthorArgument, ProjectTransactionAuthorKindArgument,
    ReadAssetPreviewRequest, ReadProjectFileRequest, RefreshFragmentsRequest, RelocateAssetRequest,
    RemoveDependencyRequest, RemoveGroupRequest, RemoveNoteRequest, RenameNodeRequest,
    RenameProjectRequest, ReplaceAssetRequest, STREAM_ACTION_ARGUMENT_DESCRIPTION,
    STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION, STREAM_ARGUMENT_DESCRIPTION,
    STREAM_ASSET_ARGUMENT_DESCRIPTION, STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION,
    SetAssetSettingsRequest, SetExposedConfigFieldsRequest, SetGroupFieldsRequest,
    SetNodeConfigRequest, SetNoteFieldsRequest, SetPinValueRequest, SetProjectCoverRequest,
    SetProjectDescriptionRequest, SetStreamClockScaleRequest, SetStreamDisplayNameRequest,
    SetStudioFieldsRequest, SubscribeProjectChangesRequest, SubscribeProjectLogRecordsRequest,
    SubscribeStreamActionChangesRequest, SubscribeStreamGraphChangesRequest, TransactRequest,
    UndoOrRedoRequest, UnlockSubgraphRequest, UpdateDependencyRequest, UseDependencyStreamRequest,
    WriteGraphInputRequest, WriteProjectFileRequest, WriteStudioViewOutputRequest,
};
pub use control_operation_results::{
    ControlContractVersionMismatch, DiscoverResult, SubscribeProjectChangesResult,
    SubscribeProjectLogRecordsResult, SubscribeStreamActionChangesResult,
    SubscribeStreamGraphChangesResult,
};
pub use control_operations::{
    AddDependency, AddGroup, AddNode, AddNote, CONTROL_CONTRACT_VERSION, CONTROL_OPERATIONS,
    CancelStreamAction, CloseProject, CloseViewer, CollapseSelectionIntoSubgraph, Connect,
    ControlOperationCall, ControlOperationCallParseRefusal, CreateNodeFile, CreateProject,
    CreateStream, DeleteAsset, DeleteProject, DeleteStream, DescribeAsset, DescribeRuntime,
    DescribeStreamAction, Disconnect, Discover, DuplicateProject, Graph, ImportAsset,
    ImportGraphGenerator, ImportProject, ListAssets, ListDependencies, ListProjectFiles,
    ListProjectNodeTypes, ListProjectViews, ListSamples, ListStreams, Logs, OpenProject,
    OpenViewer, PausePlayback, ReadAssetPreview, ReadProjectChanges, ReadProjectFile, Redo,
    RefreshFragments, ReloadProject, RelocateAsset, RemoveDependency, RemoveGroup, RemoveNode,
    RemoveNote, RemoveProject, RenameNode, RenameProject, ReplaceAsset, ResumePlayback,
    SetAssetSettings, SetExposedConfigFields, SetGroupFields, SetNodeConfig, SetNoteFields,
    SetPinValue, SetProjectCover, SetProjectDescription, SetStreamClockScale, SetStreamDisplayName,
    SetStudioFields, StartPlayback, StopPlayback, SubscribeProjectChanges,
    SubscribeProjectLogRecords, SubscribeStreamActionChanges, SubscribeStreamGraphChanges,
    Transact, Undo, UnlockSubgraph, UpdateDependency, UseDependencyStream, WriteGraphInput,
    WriteProjectFile, WriteStudioViewOutput,
};
