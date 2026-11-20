// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The names a runtime's local API speaks on its socket and its loopback HTTP listener on its
//! port, and the shape of each result a client parses, written once for the runtime that serves
//! them and every client that calls them.

mod graph_answer_link_wire_types;
mod graph_answer_node_component_wire_types;
mod graph_answer_wire_types;
mod graph_wire_types;
mod link_check_wire_types;
mod log_wire_types;
mod node_type_wire_types;
mod node_ui_wire_types;
mod project_code_check_wire_types;
mod project_wire_types;
mod runtime_health_wire_types;
mod stream_clock_spec;
mod stream_condition_wire_types;
mod stream_wire_types;

pub use graph_answer_link_wire_types::{
    GraphInterfacePortDirectionOnTheWire, GraphInterfacePortOnTheWire,
    GraphLinkComponentsOnTheWire, GraphLinkOnTheWire, GraphLinkPortRefOnTheWire,
    GraphLinkStateComponentOnTheWire, GraphLinkStateOnTheWire, GraphVariableOnTheWire,
    LinkCarriedThroughTheFunctionNodePoolOnTheWire,
};
pub use graph_answer_node_component_wire_types::{
    FunctionNodeHostOnTheWire, FunctionNodePromotionOnTheWire, GraphNodeComponentsOnTheWire,
    HelperProcessRestartsOnTheWire, NodeInstanceOnTheWire, NodeMetricsOnTheWire,
    NodeReadyBarrierOnTheWire, NodeShutdownChannelOnTheWire, NodeStateOnTheWire,
    NodeThreadHandleOnTheWire, ShaderNodeFusedGroupOnTheWire,
};
pub use graph_answer_wire_types::{
    BundleMemberValueReadOnTheWire, ConfigNodeReferenceEdgeOnTheWire,
    FunctionNodeLanguageOnTheWire, GraphFragmentOnTheWire, GraphNodeKindOnTheWire,
    GraphNodeOnTheWire, GraphNodePortOnTheWire, GraphNodePortsOnTheWire, GraphToolResult,
    InputValueComesFromOnTheWire, InputValueReadOnTheWire, LabelledNodeGroup,
    MachineWideGraphOnTheWire, PortKindOnTheWire, StreamGraphOnTheWire,
};
pub use graph_wire_types::{
    GraphLinkEndOnTheWire, GraphTransactionAddedGroup, GraphTransactionAddedNode,
    GraphTransactionAddedNote, GraphTransactionConnectedLink,
    GraphTransactionInputOverriddenByALink, GraphTransactionNodeInsertedBecause,
    GraphTransactionRebuiltNode, GraphTransactionToolResult, InsertedNodeOnTheWire,
    RefusedGraphLinkOnTheWire,
};
pub use link_check_wire_types::{
    ConvertNodeToInsertOnTheWire, LinkCandidateChecked, LinkCandidateEnd, LinkCandidateNotChecked,
    LinkCandidatesCheckResult, LinkCheckResult, PinLinkMarksOnTheWire, PinLinkOutcomeOnTheWire,
    PinLinkWarningOnTheWire,
};
pub use log_wire_types::LogsToolResult;
pub use node_type_wire_types::{
    DescribeNodeTypesToolResult, GraphGeneratorListed, ListNodeTypesToolResult,
    ListPinTypesToolResult, NodeTypeCategoryFrom, NodeTypeDescribedFrom, NodeTypeDescribedOrWhyNot,
    NodeTypeDescription, NodeTypeInputDescribed, NodeTypeInputMode, NodeTypeKind, NodeTypeListed,
    NodeTypeNotDescribed, NodeTypePortDescribed, NodeTypePortPin, NodeTypeSourceListed,
    NodeTypeSourceListedOrigin, PinTypeConversionListed, PinTypeListed, SetNodeTypeGroupToolResult,
};
pub use node_ui_wire_types::{
    ListProjectNodeUiElementsToolResult, NodeUiManifest, NodeUiManifestControl,
    NodeUiManifestNodeBody, ProjectNodeUiElement, ProjectNodeUiElementListed,
    ProjectNodeUiManifestListed, ProjectNodeUiManifestNotRead, ProjectNodeUiModule,
};
pub use project_code_check_wire_types::{
    ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckResult, ProjectCodeCheckRun,
    ProjectCodeCheckSeverity, ProjectCodeCheckState, ProjectCodeCheckVerdict, ProjectFileChecked,
};
pub use project_wire_types::{
    AssetAudioFactsOnTheWire, AssetImageFactsOnTheWire, AssetLutFactsOnTheWire,
    AssetMediaFactsOnTheWire, AssetMediaFactsReadOnTheWire, AssetPreviewOnTheWire,
    AssetSceneFactsOnTheWire, AssetTransactionToolResult, AssetVideoFactsOnTheWire,
    ControlOperationRefusalCode, CreateNodeFileToolResult, CreateNodeUiElementToolResult,
    CreateProjectToolResult, DeleteProjectToolResult, DependencyTransactionToolResult,
    DescribeAssetToolResult, DuplicateProjectToolResult,
    GraphGeneratorPromotedSettingReadFromSource, GraphGeneratorReadFromSource,
    ImportProjectToolResult, ListAssetsToolResult, ListDependenciesToolResult,
    ListProjectFilesToolResult, ListProjectNodeTypesToolResult, ListProjectViewsToolResult,
    ListSamplesToolResult, ListSamplesToolResultSample, ListedProjectDependencyOnTheWire,
    NodeModuleNotReadable, NodeNotRunningBecauseOnTheWire, NodeTypeOfOneProjectSource,
    NodeTypePortReadFromSource, NodeTypeReadFromSource, NodeTypeSourceOrigin,
    NodeTypeSourceReadWithoutRunningIt, ProjectActionRefusalOnTheWire, ProjectAssetOnTheWire,
    ProjectAssetWithItsMediaFactsOnTheWire, ProjectChangeFeedEntryOnTheWire,
    ProjectDependencyOnTheWire, ProjectFileOnTheWire, ProjectFilePlaceOnTheWire,
    ProjectFileTransactionToolResult, ProjectSettingsTransactionToolResult,
    ProjectTransactionOnTheWire, ProjectTransactionsResourceContents, ProjectViewDeclaredPort,
    ProjectViewListed, ReadAssetPreviewToolResult, ReadProjectFileToolResult,
    RefusedProjectActionOnTheWire, RemoveProjectToolResult, SuggestedOperationCall,
    UpdateDependencyToolResult,
};
pub use runtime_health_wire_types::{
    DescribeRuntimeToolResult, LOOPBACK_HTTP_LISTENER_DEFAULT_PORT,
    MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH, MachineRuntimeLockHolderPidAnswer,
};
pub use stream_clock_spec::{DEFAULT_SCALED_STREAM_CLOCK_STEP_NS, StreamClockSpec};
pub use stream_condition_wire_types::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticPlaceOnTheWire,
    DiagnosticRecoveryOnTheWire, NodeHealthOnTheWire, StreamConditionOnTheWire,
    StreamHealthOnTheWire, StreamLoadOnTheWire, StreamPlaybackOnTheWire,
};
pub use stream_wire_types::{
    CancelStreamActionToolResult, CaptureDeviceOnTheWire, CloseViewerToolResult,
    DescribeStreamActionToolResult, DescribeStreamActionToolResultStream, ListStreamsToolResult,
    ListStreamsToolResultStream, ListedStreamState, OpenViewerToolResult, OutputPortViewerReads,
    PausePlaybackToolResult, ProjectStreamActionQueuedToolResult, ProjectVenvSyncOnTheWire,
    ProjectVenvSyncPackageKindOnTheWire, ProjectVenvSyncPackageOnTheWire,
    ProjectVenvSyncPackageStateOnTheWire, ProjectVenvSyncPhaseOnTheWire, ResumePlaybackToolResult,
    SetStreamClockScaleToolResult, StreamActionKindOnTheWire, StreamActionStateOnTheWire,
    StreamActionStepOnTheWire, StreamActionsUnderWayResourceContents, StreamLocationOnTheWire,
    WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};

/// The route MCP's Streamable HTTP transport is served on.
pub const MCP_STREAMABLE_HTTP_ROUTE_PATH: &str = "/mcp";

/// The request target a client upgrades to MCP's stdio framing on.
pub const MCP_STDIO_UPGRADE_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token [`MCP_STDIO_UPGRADE_REQUEST_TARGET`] switches to, matched
/// case-insensitively.
pub const MCP_STDIO_UPGRADE_PROTOCOL_TOKEN: &str = "mcp-stdio";

/// The route each control operation is served on as one JSON-RPC 2.0 request per HTTP `POST`.
pub const CONTROL_JSON_RPC_ROUTE_PATH: &str = "/control";

/// The request target a client opens a WebSocket on that carries one JSON-RPC 2.0 message per
/// text frame both ways, and stays open for notifications.
pub const CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET: &str = "/control/ws";

/// The header a control client names the contract version it was built against in; another
/// version than the runtime's is refused before any call is served.
pub const CONTROL_CONTRACT_VERSION_HEADER_NAME: &str = "tatolab-control-contract-version";

/// The header a control client names the contract revision it was built against in, read as 0
/// when absent; a higher revision than the runtime's is refused before any call is served.
pub const CONTROL_CONTRACT_REVISION_HEADER_NAME: &str = "tatolab-control-contract-revision";

/// The header a control client names itself in, which a change it makes with no author is
/// recorded under.
pub const CONTROL_CLIENT_NAME_HEADER_NAME: &str = "tatolab-control-client-name";

#[cfg(test)]
mod tests;
