// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every operation a StreamLib runtime's control plane serves, each declared once by an
//! [`ControlOperation`] impl, and the contract file every adapter, SDK and reference page is
//! generated from.

mod control_contract_document;
mod control_notification;
mod control_operation;
mod control_operation_requests;
mod control_operation_results;
mod control_operations;

pub use control_contract_document::{
    CONTROL_CONTRACT_FILE_WORKSPACE_PATH, CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
    CONTROL_OPERATION_FAILED_ERROR_CODE, CONTROL_OPERATION_REFUSED_ERROR_CODE,
    OPENRPC_DOCUMENT_VERSION, control_contract_document, control_contract_file_contents,
};
pub use control_notification::{
    CONTROL_NOTIFICATIONS, ControlNotification, ControlNotificationDeclaration,
    ProjectChangeKindOnTheWire, ProjectChanged, ProjectChangedNotificationParams,
};
pub use control_operation::{
    ControlOperation, ControlOperationDeclaration, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
pub use control_operation_requests::{
    AUTHOR_ARGUMENT_DESCRIPTION, AddGroupRequest, AddNodeRequest, AddNoteRequest,
    CollapseSelectionIntoSubgraphRequest, DescribeRuntimeRequest, DiscoverRequest,
    GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION, GRAPH_STREAM_ARGUMENT_DESCRIPTION, GraphEditMode,
    GraphRequest, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ImportGraphGeneratorRequest,
    ListStreamsRequest, OneLinkGraphEditRequest, OneNodeGraphEditRequest,
    OneProjectByItsDirectoryRequest, OneStreamActionRequest,
    PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
    ProjectTransactionAuthorKindArgument, RemoveGroupRequest, RemoveNoteRequest, RenameNodeRequest,
    STREAM_ACTION_ARGUMENT_DESCRIPTION, SetExposedConfigFieldsRequest, SetGroupFieldsRequest,
    SetNodeConfigRequest, SetNoteFieldsRequest, SetPinValueRequest, SetStudioFieldsRequest,
    SubscribeProjectChangesRequest, TransactRequest, UnlockGeneratorFragmentRequest,
};
pub use control_operation_results::{
    ControlContractVersionMismatch, DiscoverResult, SubscribeProjectChangesResult,
};
pub use control_operations::{
    AddGroup, AddNode, AddNote, CONTROL_CONTRACT_VERSION, CONTROL_OPERATIONS, CancelStreamAction,
    CloseProject, CollapseSelectionIntoSubgraph, Connect, ControlOperationCall,
    ControlOperationCallParseRefusal, DescribeRuntime, DescribeStreamAction, Disconnect, Discover,
    Graph, ImportGraphGenerator, ListStreams, OpenProject, PausePlayback, ReloadProject,
    RemoveGroup, RemoveNode, RemoveNote, RemoveProject, RenameNode, ResumePlayback,
    SetExposedConfigFields, SetGroupFields, SetNodeConfig, SetNoteFields, SetPinValue,
    SetStudioFields, StartPlayback, StopPlayback, SubscribeProjectChanges, Transact,
    UnlockGeneratorFragment,
};
