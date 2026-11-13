// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every operation the registry holds, the table the adapters iterate, and the one call type a
//! dispatcher matches on.

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, CancelStreamActionToolResult, CloseViewerToolResult,
    CreateNodeFileToolResult, CreateProjectToolResult, DeleteProjectToolResult,
    DependencyTransactionToolResult, DescribeAssetToolResult, DescribeRuntimeToolResult,
    DescribeStreamActionToolResult, DuplicateProjectToolResult, GraphTransactionToolResult,
    ImportProjectToolResult, ListAssetsToolResult, ListDependenciesToolResult,
    ListProjectFilesToolResult, ListProjectNodeTypesToolResult, ListProjectViewsToolResult,
    ListSamplesToolResult, ListStreamsToolResult, LogsToolResult, OpenViewerToolResult,
    PausePlaybackToolResult, ProjectFileTransactionToolResult,
    ProjectSettingsTransactionToolResult, ProjectStreamActionQueuedToolResult,
    ProjectTransactionOnTheWire, ProjectTransactionsResourceContents, ReadAssetPreviewToolResult,
    ReadProjectFileToolResult, RemoveProjectToolResult, ResumePlaybackToolResult,
    SetStreamClockScaleToolResult, UpdateDependencyToolResult, WriteGraphInputToolResult,
    WriteStudioViewOutputToolResult,
};

use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationDeclaration,
    ControlOperationEffect, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
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
    SetNodeConfigRequest, SetNoteFieldsRequest, SetPinValueRequest, SetProjectCoverRequest,
    SetProjectDescriptionRequest, SetStreamClockScaleRequest, SetStreamDisplayNameRequest,
    SetStudioFieldsRequest, SubscribeProjectChangesRequest, SubscribeProjectLogRecordsRequest,
    SubscribeStreamActionChangesRequest, SubscribeStreamGraphChangesRequest, TransactRequest,
    UndoOrRedoRequest, UnlockSubgraphRequest, UpdateDependencyRequest, UseDependencyStreamRequest,
    WriteGraphInputRequest, WriteProjectFileRequest, WriteStudioViewOutputRequest,
};
use crate::control_operation_results::{
    DiscoverResult, SubscribeProjectChangesResult, SubscribeProjectLogRecordsResult,
    SubscribeStreamActionChangesResult, SubscribeStreamGraphChangesResult,
};

/// The contract's version: bumped only by a breaking change to an operation already in it.
pub const CONTROL_CONTRACT_VERSION: u32 = 2;

const EVERY_SURFACE: &[ControlOperationExposure] = &[
    ControlOperationExposure::Studio,
    ControlOperationExposure::Agent,
    ControlOperationExposure::Public,
];

const EVERY_SURFACE_BUT_THE_AGENT_SURFACE: &[ControlOperationExposure] = &[
    ControlOperationExposure::Studio,
    ControlOperationExposure::Public,
];

/// `discover`: the contract a runtime serves, which a client checks before any other call.
pub struct Discover;

impl ControlOperation for Discover {
    type Request = DiscoverRequest;
    type Result = DiscoverResult;
    const METHOD_NAME: &'static str = "discover";
    const ONE_LINE_DOC: &'static str = "Answer the contract version this runtime serves, its version and build id, and every method it serves.";
    const AGENT_USAGE_NOTE: &'static str = "Answer `contract_version`, the version of the contract this runtime serves, its `runtime_version` and `build_id`, and `operations`, every method it serves. A client sends the `contract_version` it was built against; another version is refused naming both, and the client is not served.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_project_changes`: the connection is sent each change on some projects' feeds.
pub struct SubscribeProjectChanges;

impl ControlOperation for SubscribeProjectChanges {
    type Request = SubscribeProjectChangesRequest;
    type Result = SubscribeProjectChangesResult;
    const METHOD_NAME: &'static str = "subscribe_project_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `project_changed` notification for each change on the named projects' feeds.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the change feeds of `projects`, by UUID: from then on, each transaction applied to one, each action the runtime refuses on one, each media facts read it finishes, each making current of its `.venv` (`venv_made_current`) and each burst of files changed in its folder, an editor's saves among them, named under `files`, is sent as a `project_changed` notification, whoever made it, until the connection closes. A connection that falls behind, or a project whose folder changed in ways the OS did not name, is sent `missed`, and the client reads the change feed and lists the project's files and node types again. Served only on a connection that stays open; refused for an empty `projects`. Answers every project the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_project_log_records`: the connection is told each time some projects' logs have new
/// records.
pub struct SubscribeProjectLogRecords;

impl ControlOperation for SubscribeProjectLogRecords {
    type Request = SubscribeProjectLogRecordsRequest;
    type Result = SubscribeProjectLogRecordsResult;
    const METHOD_NAME: &'static str = "subscribe_project_log_records";
    const ONE_LINE_DOC: &'static str = "Send this connection each new record of the named projects' logs as `project_log_records_written` notifications.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the logs of the projects in `project_directories`, each an absolute path: from then on, each time the runtime writes new records to one's log, which it does at most every 100 ms, `project_log_records_written` carries them, at most 500 to a notification, naming its `project_directory` as the connection spelled it. Each notification's `after` is the previous one's `next_after`, starting at the log's end as the project is subscribed; a client that read the log with `logs` to another cursor reads `logs` from its cursor `until` the notification's `next_after` instead, and appends the notifications after that. Served only on a connection that stays open; refused for an empty `project_directories`, or one that is relative or names no folder on this machine. Answers every project the connection is subscribed to, by its directory as the connection spelled it.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `read_project_changes`: read a project's change feed as it stands.
pub struct ReadProjectChanges;

impl ControlOperation for ReadProjectChanges {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectTransactionsResourceContents;
    const METHOD_NAME: &'static str = "read_project_changes";
    const ONE_LINE_DOC: &'static str = "Read a project's change feed as it stands: its newest transactions, refused actions and media facts reads.";
    const AGENT_USAGE_NOTE: &'static str = "Read the change feed of the project in `project_directory`, opening it as `list_assets` does, as the resource `list_assets` names as `transactions_resource_uri` renders it: the project's UUID as `project`, its `project_directory`, and under `transactions` its newest 64 transactions, the actions refused and the assets whose media facts were read since the runtime started, newest first, each tagged by `status`. `subscribe_project_changes` sends a `project_changed` notification for each change after it, and `missed` when changes went unsent, after which the feed is read again and the project's files listed again. Refused for a folder with no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_stream_graph_changes`: the connection is sent each change in some loaded streams'
/// live graphs.
pub struct SubscribeStreamGraphChanges;

impl ControlOperation for SubscribeStreamGraphChanges {
    type Request = SubscribeStreamGraphChangesRequest;
    type Result = SubscribeStreamGraphChangesResult;
    const METHOD_NAME: &'static str = "subscribe_stream_graph_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `stream_graph_changed` notification for each change in the named loaded streams' live graphs.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the live graphs of `streams`, by stream id, each loaded: from then on, each edit of one's graph is sent as a `stream_graph_changed` notification with `change` `edited`, each change of a node's state with `node_state_changed`, its `node` by graph `id` and its `node_state`, and each start, stop, pause or resume of its playback, or failure of one, with `playback_state_changed`, until the connection closes. A connection that falls behind, and a stream loaded again or unloaded, is sent `missed`, and the client reads the stream's `graph` again; the subscription follows the stream id across a reload. Served only on a connection that stays open; refused for an empty `streams` or a stream not loaded. Answers every stream the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_stream_action_changes`: the connection is sent each change of a stream action.
pub struct SubscribeStreamActionChanges;

impl ControlOperation for SubscribeStreamActionChanges {
    type Request = SubscribeStreamActionChangesRequest;
    type Result = SubscribeStreamActionChangesResult;
    const METHOD_NAME: &'static str = "subscribe_stream_action_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `stream_action_changed` notification each time a stream action changes.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to every stream action: from then on, each time one is queued, moves on or ends, its id is sent as a `stream_action_changed` notification with `change` `changed`, and `describe_stream_action` describes it, until the connection closes. A connection that falls behind is sent `missed`, and the client describes each action it follows again. Served only on a connection that stays open. Answers `actions_under_way`, the id of each action queued or running as the subscription began.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `describe_runtime`: this runtime's version, build and uptime.
pub struct DescribeRuntime;

impl ControlOperation for DescribeRuntime {
    type Request = DescribeRuntimeRequest;
    type Result = DescribeRuntimeToolResult;
    const METHOD_NAME: &'static str = "describe_runtime";
    const ONE_LINE_DOC: &'static str = "Describe this runtime: its version, its build id, how long it has been up and where it logs.";
    const AGENT_USAGE_NOTE: &'static str = "Describe this runtime: its crate `version`, its `build_id` (`<version>+<git sha>.<per-build nonce>`, telling two builds of one version apart), `up_for_ms`, the monotonic milliseconds since its engine started — a restart resets it — and `runtime_log_directory`, the folder its own log is written to, `null` when it keeps none. Names no stream.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `list_streams`: every stream the runtime holds, with its state.
pub struct ListStreams;

impl ControlOperation for ListStreams {
    type Request = ListStreamsRequest;
    type Result = ListStreamsToolResult;
    const METHOD_NAME: &'static str = "list_streams";
    const ONE_LINE_DOC: &'static str =
        "List every stream the runtime holds, by stream id, with its location and state.";
    const AGENT_USAGE_NOTE: &'static str = "List every stream the runtime holds: its stream id as `stream`, its `display_name`, its `location` — its project directory, its project's UUID as `project` and its stream asset's UUID as `stream_asset`, `null` for a record an earlier runtime wrote — and its state: `running` (playing, or recorded playing and re-loaded playing at the runtime's next start), `stopped` (open with its playback stopped, its graph loaded to read and edit while its node count is set, until `start_playback`), `paused` (open with its playback paused by `pause_playback`, until `resume_playback`), `closed` (its project closed by `close_project`, not loaded, until `open_project`), `preparing` (an action that loads it — an `open_project`, `reload_project` or `start_playback` — is queued or under way on it, named by `action`, on its `step`: `reading_the_graph_documents`, `making_the_venv` or `starting`, `null` while it waits), `stopping` (a `stop_playback`, `close_project` or `remove_project` of its project is queued or under way on it, named by `action`, on the step `stopping`, `null` while it waits), or `failed` (its code, its venv, its load or its start failed, and held stopped from its document when that reads, its graph there to read and edit, until a play or a `reload_project` retries it; implicated in the runtime's last two crashes in a row, and held at a start from its document with none of its node code read until a `reload_project` of its project tries it; or recorded by an earlier runtime, never loaded, with `failed_because` saying to run it again and `remove_project` forgetting it), with the node count of its loaded graph (`null` when it is not loaded), `failed_because`, why a failed stream failed (`null` otherwise), and `capture_devices_whose_access_was_never_asked_for`, each of `camera` and `microphone` that a node of the loaded stream refused because nobody has asked the user for its access, as when the runtime re-loaded the stream on its own at its start (empty otherwise): a `close_project` then `open_project` of its project asks.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `graph`: a loaded stream's live graph, or every loaded stream's.
pub struct Graph;

impl ControlOperation for Graph {
    type Request = GraphRequest;
    type Result = Value;
    const METHOD_NAME: &'static str = "graph";
    const ONE_LINE_DOC: &'static str = "Export a loaded stream's live graph, or every loaded stream's, with each node's and link's state.";
    const AGENT_USAGE_NOTE: &'static str = "Export a loaded stream's current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, with each node's and link's live state and counters beside them, and this machine's name (`machine`), the first part of `<machine>/<stream id>/<node>/<port>`. A fragment — a graph generator's stored output — renders as one node of `kind` `fragment` whose ports are its Graph Input and Output nodes' and whose `fragment` holds its own nodes and links, a fragment inside it rendered the same way; its nodes run under their path of names, `look/soft/blur`. Without `stream`, `{machine, streams}` holds every loaded stream's graph.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `transact`: several graph operations of one project as one transaction.
pub struct Transact;

impl ControlOperation for Transact {
    type Request = TransactRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "transact";
    const ONE_LINE_DOC: &'static str =
        "Apply operations on one project's stream graphs as one transaction, whole or not at all.";
    const AGENT_USAGE_NOTE: &'static str = "Apply operations on the graphs of one project's loaded streams as one transaction by its `author`: applied in order, whole or not at all, to each running stream, and with `mode` `commit`, the default, written to each stream's graph document and recorded as one commit on the project's history, which `undo` reverses as one. Answers `mode`, the `transaction` and `commit` recording a committed edit, each operation as applied, `added_nodes` with each new node's UUID and the name it took, and `connected_links` with each new link's `state` — `pending`, `wired`, or `error` with its `error_reason`. Refused whole, in the runtime's own words, with nothing changed. A config value `{\"asset\": \"<uuid>\"}` must name an asset the project holds. An `add_node` may give its node an `as`, which later operations of the same transaction name the node by. The transaction is announced on the project's change feed, the resource `list_assets` names as `transactions_resource_uri`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `add_node`: one node added to a loaded stream's graph.
pub struct AddNode;

impl ControlOperation for AddNode {
    type Request = AddNodeRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "add_node";
    const ONE_LINE_DOC: &'static str =
        "Add a node to a loaded stream's graph by its type's import path, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Add a node to a loaded stream's graph by its type's import path, as one transaction, the runtime making the node's UUID. A Python type is named `module:QualifiedClassName` and must be importable by the stream's own interpreter — a module in the stream's project, or a package installed in its venv: the runtime never imports it, and describes it in that interpreter the first time it is added; each add spawns it in its own processor interpreter, which imports the module fresh. The type's port declaration is read when it is first described and kept; to change a type's ports, add it under a new class name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `connect`: one link between an output port and an input port.
pub struct Connect;

impl ControlOperation for Connect {
    type Request = OneLinkGraphEditRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "connect";
    const ONE_LINE_DOC: &'static str =
        "Link an output port to an input port in one loaded stream, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`. A link that would close a loop of function nodes with no `tatolab.stream:PreviousValue` node in it adds one and runs through it: the result names it under `added_nodes` and both of its links under `connected_links`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `import_graph_generator`: run one of the project's `@graph_generator` functions in the stream's own interpreter and place what it builds in a loaded stream's graph as a fragment node, as one transaction.
pub struct ImportGraphGenerator;

impl ControlOperation for ImportGraphGenerator {
    type Request = ImportGraphGeneratorRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "import_graph_generator";
    const ONE_LINE_DOC: &'static str = "Run one of the project's `@graph_generator` functions in the stream's own interpreter and place what it builds in a loaded stream's graph as a fragment node, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Run one of the project's `@graph_generator` functions in the stream's own interpreter and place what it builds in a loaded stream's graph as a fragment node, as one transaction: its generator as `type`, its promoted settings as `config`, and the nodes and links it built stored under `fragment`, each node with a UUID of its own. Run again under the name of a fragment the graph holds, it refreshes that fragment with the fresh run: each node it builds keeps the UUID of the node of the same name it built before, so links into the fragment and the graph's history hold. A generator that raises, refuses, or is not found is refused with its traceback, nothing changed. Answers as `transact` does, a new fragment node under `added_nodes`; undo restores the fragment as it was.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `remove_node`: remove a node from a loaded stream's graph by its UUID, as one transaction, stopping it.
pub struct RemoveNode;

impl ControlOperation for RemoveNode {
    type Request = OneNodeGraphEditRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "remove_node";
    const ONE_LINE_DOC: &'static str =
        "Remove a node from a loaded stream's graph by its UUID, as one transaction, stopping it.";
    const AGENT_USAGE_NOTE: &'static str = "Remove a node from a loaded stream's graph by its UUID, as one transaction, stopping it; its links go with it, and undo brings both back.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `disconnect`: take out the link between an output port and an input port in one loaded stream, as one transaction, each end a node's UUID and a port name.
pub struct Disconnect;

impl ControlOperation for Disconnect {
    type Request = OneLinkGraphEditRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "disconnect";
    const ONE_LINE_DOC: &'static str = "Take out the link between an output port and an input port in one loaded stream, as one transaction, each end a node's UUID and a port name.";
    const AGENT_USAGE_NOTE: &'static str = "Take out the link between an output port and an input port in one loaded stream, as one transaction, each end a node's UUID and a port name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_node_config`: replace a node's config whole, as one transaction, applied to the running node at the stream's next commit.
pub struct SetNodeConfig;

impl ControlOperation for SetNodeConfig {
    type Request = SetNodeConfigRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_node_config";
    const ONE_LINE_DOC: &'static str = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit.";
    const AGENT_USAGE_NOTE: &'static str = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit. A config value `{\"asset\": \"<uuid>\"}` references an asset of the project, which then cannot be deleted while the reference stands.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_pin_value`: set the value one of a node's inputs holds while no link feeds it, as one transaction.
pub struct SetPinValue;

impl ControlOperation for SetPinValue {
    type Request = SetPinValueRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_pin_value";
    const ONE_LINE_DOC: &'static str =
        "Set the value one of a node's inputs holds while no link feeds it, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Set the value one of a node's inputs holds while no link feeds it, as one transaction; the value is saved in the graph document as a link is.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `rename_node`: give a node another name, a label, as one transaction.
pub struct RenameNode;

impl ControlOperation for RenameNode {
    type Request = RenameNodeRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "rename_node";
    const ONE_LINE_DOC: &'static str = "Give a node another name, a label, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Give a node another name, a label, as one transaction; the node keeps its UUID, which every address names it by. Refused while another node's config names it by its name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_studio_fields`: replace what Tato Studio draws a node with — canvas position, group, comment, colour, collapsed, and a client's own `metadata` — as one transaction, the old fields replaced whole.
pub struct SetStudioFields;

impl ControlOperation for SetStudioFields {
    type Request = SetStudioFieldsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_studio_fields";
    const ONE_LINE_DOC: &'static str = "Replace what Tato Studio draws a node with — canvas position, group, comment, colour, collapsed, and a client's own `metadata` — as one transaction, the old fields replaced whole.";
    const AGENT_USAGE_NOTE: &'static str = "Replace what Tato Studio draws a node with — canvas position, group, comment, colour, collapsed, and a client's own `metadata` — as one transaction, the old fields replaced whole. The fields are held to the studio contract: a key of the wrong shape, or any key outside it, is refused by name. It changes nothing that runs.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_exposed_config_fields`: replace the config fields a node takes as inputs, as one transaction.
pub struct SetExposedConfigFields;

impl ControlOperation for SetExposedConfigFields {
    type Request = SetExposedConfigFieldsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_exposed_config_fields";
    const ONE_LINE_DOC: &'static str =
        "Replace the config fields a node takes as inputs, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Replace the config fields a node takes as inputs, as one transaction. Each is a number or bool field of the node's config schema and becomes an input of the field's name: linked, it drives the field, a function node reading it at each evaluation and a process node taking each new value as a live config update; unlinked, it reads the node's config. A field still linked cannot be taken off.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `add_group`: make a labelled group of nodes of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.
pub struct AddGroup;

impl ControlOperation for AddGroup {
    type Request = AddGroupRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "add_group";
    const ONE_LINE_DOC: &'static str = "Make a labelled group of nodes of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Make a labelled group of nodes of one level of a loaded stream, its own or the subgraph `level` names, as one transaction: a title, a description and a colour Tato Studio frames the nodes with, so the next reader knows what they do. It changes nothing that runs. Each node joins it by holding the group's UUID as its `studio.group`; a node of another group is refused, and a node leaves or joins later through `set_studio_fields`. Answers as `transact` does, the new group's UUID under `added_groups`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_group_fields`: set a labelled group's title, description or colour, as one transaction.
pub struct SetGroupFields;

impl ControlOperation for SetGroupFields {
    type Request = SetGroupFieldsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_group_fields";
    const ONE_LINE_DOC: &'static str =
        "Set a labelled group's title, description or colour, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Set a labelled group's title, description or colour, as one transaction; a field omitted is kept. Its nodes stay as they are.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `collapse_selection_into_subgraph`: collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction.
pub struct CollapseSelectionIntoSubgraph;

impl ControlOperation for CollapseSelectionIntoSubgraph {
    type Request = CollapseSelectionIntoSubgraphRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "collapse_selection_into_subgraph";
    const ONE_LINE_DOC: &'static str = "Collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction with `mode` `commit`: the nodes and the links between them are saved as a new stream of the project, `streams/<name>.graph.json`, and placed by reference where they were, so editing inside the subgraph edits that stream everywhere it is placed, and it runs only where it is placed. Each link that crossed the selection is carried through a `GraphInput` or `GraphOutput` of the new stream, named `<node>-<port>` and of the family the running stream declares at the link's source, so the stream runs the same nodes, each under the subgraph's name (`sub/blur`) and the UUID its placement gives it. Answers as `transact` does, the subgraph node under `added_nodes`; undo gives back the exact earlier graph document and takes the new stream out again, refused while another stream places it.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `unlock_subgraph`: unlock a subgraph node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.
pub struct UnlockSubgraph;

impl ControlOperation for UnlockSubgraph {
    type Request = UnlockSubgraphRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "unlock_subgraph";
    const ONE_LINE_DOC: &'static str = "Unlock a subgraph node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Unlock a subgraph node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction: its nodes and the links between them become plain nodes of its level, its `GraphInput` and `GraphOutput` nodes go, and each link into or out of one of its ports is carried straight to the node it fed or read. A graph generator's output unlocked this way is no longer refreshed. A placed stream's nodes come back as a copy under new UUIDs that no edit of that stream then reaches, and the stream stays in the project, running as its own stream once nothing places it. Refused for a node of the subgraph named as a node of the level is; undo gives back the exact earlier graph document.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `add_note`: place a canvas note on a loaded stream's graph, as one transaction.
pub struct AddNote;

impl ControlOperation for AddNote {
    type Request = AddNoteRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "add_note";
    const ONE_LINE_DOC: &'static str =
        "Place a canvas note on a loaded stream's graph, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Place a canvas note on a loaded stream's graph, as one transaction: text at a position and size Tato Studio draws it at, beside the nodes rather than on one. It changes nothing that runs. Answers as `transact` does, the new note's UUID under `added_notes`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_note_fields`: set a canvas note's text, position or size, as one transaction.
pub struct SetNoteFields;

impl ControlOperation for SetNoteFields {
    type Request = SetNoteFieldsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_note_fields";
    const ONE_LINE_DOC: &'static str =
        "Set a canvas note's text, position or size, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str =
        "Set a canvas note's text, position or size, as one transaction; a field omitted is kept.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `remove_note`: take a canvas note off a loaded stream's graph, as one transaction.
pub struct RemoveNote;

impl ControlOperation for RemoveNote {
    type Request = RemoveNoteRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "remove_note";
    const ONE_LINE_DOC: &'static str =
        "Take a canvas note off a loaded stream's graph, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Take a canvas note off a loaded stream's graph, as one transaction. Undo puts it back as it was.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `remove_group`: take out a labelled group, as one transaction.
pub struct RemoveGroup;

impl ControlOperation for RemoveGroup {
    type Request = RemoveGroupRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "remove_group";
    const ONE_LINE_DOC: &'static str = "Take out a labelled group, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Take out a labelled group, as one transaction: its nodes stay, ungrouped. Undo brings the group back with every node that belonged to it.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `open_project`: queue the open of a project: its streams are loaded and held with their playback stopped.
pub struct OpenProject;

impl ControlOperation for OpenProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "open_project";
    const ONE_LINE_DOC: &'static str =
        "Queue the open of a project: its streams are loaded and held with their playback stopped.";
    const AGENT_USAGE_NOTE: &'static str = "Open the project in `project_directory`: queue its open on its own stream action queue and answer at once with the action's id as `action`. The open reads its `tatolab-project.json` and each stream asset's graph document, makes the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run — however long uv takes — then loads each stream under its stream id, `<project>-<stream>-<hash>`, the hash over the project's UUID and the stream asset's UUID, and holds it with its playback stopped, recording each stream stopped or failed: a graph document that does not load, or a load that is refused, with its reason, and a `.venv` uv cannot make — uv's own words — failing every stream of the project. It starts no node. Once it is done, `graph`, every graph edit and the node catalog answer for each stream, and `start_playback` plays the project. The project stays open, held stopped or playing, across the runtime's restarts until `close_project`. It opens a project that was never opened, one `close_project` closed, and one whose every stream failed, retrying them. One project's actions run one at a time, in the order they were queued; different projects' run side by side. Follow the action with `describe_stream_action`, its streams with `list_streams` — `preparing`, with the action and its `step`, until it ends — and its records with `logs` given `action`. A directory that does not exist or holds no `tatolab-project.json` fails the action, never the call. Refused, with nothing queued: a `project_directory` that is not absolute, and a project open once its queued actions run, which `reload_project` refreshes.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `reload_project`: queue a soft reload of an open project from disk, keeping its playback.
pub struct ReloadProject;

impl ControlOperation for ReloadProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "reload_project";
    const ONE_LINE_DOC: &'static str =
        "Queue a soft reload of an open project from disk, keeping its playback.";
    const AGENT_USAGE_NOTE: &'static str = "Softly reload the open project in `project_directory` from disk: queue the load on its stream action queue and answer at once with the action's id as `action`. It re-reads every stream asset's graph document. While the project's node code — its `.py` files, `pyproject.toml`, `uv.lock` and views' `tatolab_view.json` — is as it was at a stream's load, the stream stays loaded and its graph is brought to its document in place, each node matched by its UUID, so only the nodes and links that changed change; otherwise the stream is loaded again under its same id, keeping its display name. A stream whose graph document is gone is unloaded and forgotten, a failed stream is loaded again, and the project's `.venv` is synced only when its dependency files changed or its `.venv/bin/python` does not run. The project keeps its playback: a playing project's streams start playing, a paused one's start paused, and a stopped one's are held stopped. `tatolab dev` asks for it on each save. A hard reload is `close_project` then `open_project`. Follow it as `open_project`'s open. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open, which `open_project` opens.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `close_project`: queue the close of an open project: its streams are unloaded and stay closed until it is opened.
pub struct CloseProject;

impl ControlOperation for CloseProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "close_project";
    const ONE_LINE_DOC: &'static str = "Queue the close of an open project: its streams are unloaded and stay closed until it is opened.";
    const AGENT_USAGE_NOTE: &'static str = "Close the open project in `project_directory`: queue the close on its stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, it ends the playback of every stream of the project, unloads each, forgets its function-node state, and records each closed, so the runtime leaves the project unloaded across its restarts until `open_project` opens it again. It deletes nothing: the project's files, its `.venv` and what uv synced into it stay. Its streams are `stopping` in `list_streams` until it ends, then `closed`; follow it with `describe_stream_action`. Each recorded stream carries the close on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open once its queued actions run.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `remove_project`: queue the removal of a project from the runtime: its streams are stopped and their records forgotten.
pub struct RemoveProject;

impl ControlOperation for RemoveProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = RemoveProjectToolResult;
    const METHOD_NAME: &'static str = "remove_project";
    const ONE_LINE_DOC: &'static str = "Queue the removal of a project from the runtime: its streams are stopped and their records forgotten.";
    const AGENT_USAGE_NOTE: &'static str = "Queue the removal of the whole project in `project_directory` on its stream action queue and answer at once with the action's id as `action`: once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, so the runtime no longer re-loads any of them, and deletes no file. `delete_asset` on a stream asset unloads and forgets that one stream. The project's streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute, and a project the runtime holds no stream of.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `start_playback`: queue the start of an open project's playback.
pub struct StartPlayback;

impl ControlOperation for StartPlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "start_playback";
    const ONE_LINE_DOC: &'static str = "Queue the start of an open project's playback.";
    const AGENT_USAGE_NOTE: &'static str = "Start the playback of the open project in `project_directory`: queue it on the project's stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, each stream the project holds stopped starts fresh from the graph it holds, edits made while stopped included, reading no graph document and making no `.venv`: every node instance is made anew, so only the state a node persists itself carries over from an earlier playback. Each stream is recorded running, so the runtime plays the project again across its restarts until `stop_playback`. A project plays or stops as a whole: no tool plays or stops one of its streams. Follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute; a project that is not open, which `open_project` opens; a paused project, which `resume_playback` resumes; and a project that plays once its queued actions run, which `stop_playback` stops.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `stop_playback`: queue the stop of a project's playback, its streams staying loaded.
pub struct StopPlayback;

impl ControlOperation for StopPlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "stop_playback";
    const ONE_LINE_DOC: &'static str =
        "Queue the stop of a project's playback, its streams staying loaded.";
    const AGENT_USAGE_NOTE: &'static str = "Stop the playback of the whole project in `project_directory`: queue the stop on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it ends the playback of every loaded stream of the project — every processor instance, device claim, function-node state and the clock torn down — keeps each stream's graph loaded, so `graph`, every graph edit and the node catalog still answer for it, and records each stopped, so the runtime holds the project open and stopped across its restarts until `start_playback` plays it again from the graph it holds. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Each recorded stream carries the stop on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, a project whose removal is queued, and a project none of whose streams plays once the actions queued before it run.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `pause_playback`: pause a playing project's playback at once.
pub struct PausePlayback;

impl ControlOperation for PausePlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = PausePlaybackToolResult;
    const METHOD_NAME: &'static str = "pause_playback";
    const ONE_LINE_DOC: &'static str = "Pause a playing project's playback at once.";
    const AGENT_USAGE_NOTE: &'static str = "Pause the playback of the whole project in `project_directory` at once, never waiting behind an action on its queue, and answer the `project_directory` and, as `streams`, the id of each loaded stream this call paused. Every processor of each loaded stream holds its state and processes nothing, and a scaled or stepped stream clock holds where it stands, opening no step, while a real-time clock is the machine's own and runs on; the streams stay loaded and are `paused` in `list_streams`, and a graph edit made meanwhile is applied and compiles on `resume_playback`. The project stays paused until `resume_playback`, `stop_playback`, `close_project` or `remove_project`: a stream a `reload_project` loads meanwhile, the one `tatolab dev` asks for on a save among them, starts paused. Pause is recorded, so a runtime restart re-loads the project paused. Pausing a paused project pauses nothing more. Refused: a `project_directory` that is not absolute, and a project none of whose streams plays — none loaded, or each held stopped, which `start_playback` plays — and whose last action queued is not a `start_playback`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `resume_playback`: resume a paused project's playback at once.
pub struct ResumePlayback;

impl ControlOperation for ResumePlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ResumePlaybackToolResult;
    const METHOD_NAME: &'static str = "resume_playback";
    const ONE_LINE_DOC: &'static str = "Resume a paused project's playback at once.";
    const AGENT_USAGE_NOTE: &'static str = "Resume the paused playback of the project in `project_directory` at once and answer the `project_directory` and, as `streams`, the id of each stream this call resumed: every processor carries on from the state it held, each held stream clock carries on from the stream time it was held at, and each stream compiles the graph edits made while it was paused, a compile that fails logged in the project's `logs` as a live edit's is. Refused: a `project_directory` that is not absolute, and a project that is not paused.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `describe_stream_action`: describe one stream action by its id: its state, its step and each stream it reached.
pub struct DescribeStreamAction;

impl ControlOperation for DescribeStreamAction {
    type Request = OneStreamActionRequest;
    type Result = DescribeStreamActionToolResult;
    const METHOD_NAME: &'static str = "describe_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Describe one stream action by its id: its state, its step and each stream it reached.";
    const AGENT_USAGE_NOTE: &'static str = "Describe the stream action `action`: its project, `kind` (`open`, `load`, `start_playback`, `stop`, `close`, `remove`, `delete`, `reload_at_start` or `make_venv_current`), `state` — `queued` behind the project's earlier actions, `in_progress` on its `step` (`reading_the_graph_documents`, `making_the_venv`, `starting`), `done`, `failed` with `failed_because`, or `cancelled` by `cancel_stream_action` — its `venv_sync`, what its latest uv sync of the project's `.venv` has done, read from uv's own lines from the moment uv starts and kept once it ends, `null` for an action that has not synced: its `phase` (`resolving`, `downloading`, `building`, `installing`, `installed`), `packages_resolved`, `packages_prepared` and `packages_installed` as uv counts them, `downloads_started` and `downloads_done` of the downloads uv names — only those over about a MiB — and under `packages` each package uv named, first named first, by `name`, `version` once uv names it, `kind` (`distribution` or `python_interpreter`), `state` (`downloading`, `downloaded`, `building`, `built`, `installed`, `uninstalled`) and `download_size_bytes` as uv states it; uv names no byte of a download as it arrives — and `streams`, each stream its project's graph documents named by `stream` id and `display_name`, with its `state` `running`, `stopped` or `failed` once the action took it in. Followed live as the resource `streamlib://stream-action/<action>`, which a `subscriptions/listen` naming it is sent `notifications/resources/updated` for each time the action changes. The runtime holds every queued and running action and the last 64 finished ones of each project; an older one is refused, its records still in the project's `logs`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `cancel_stream_action`: cancel one stream action that is still queued or under way.
pub struct CancelStreamAction;

impl ControlOperation for CancelStreamAction {
    type Request = OneStreamActionRequest;
    type Result = CancelStreamActionToolResult;
    const METHOD_NAME: &'static str = "cancel_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Cancel one stream action that is still queued or under way.";
    const AGENT_USAGE_NOTE: &'static str = "Cancel the stream action `action`, the only way an action ends early; nothing replaces or edits a queued one. A queued action is dropped from its project's queue, its pending mark taken off each stream's record so the record stays as it was, and answers `cancelled`. A running `open`, `load`, `reload_at_start` or `make_venv_current` answers `in_progress` and stops at the step it is on — a running uv sync is killed and a half-made `.venv` removed — then ends `cancelled`: a stream it was loading is recorded failed, and the streams it had not reached stay as recorded. Refused for a running `stop`, `close` or `remove`, whose unload has begun and cannot be cut short, which ends `done`; for an action already finished; and for one the runtime does not hold.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `rename_project`: rename a project, as one transaction on its history.
pub struct RenameProject;

impl ControlOperation for RenameProject {
    type Request = RenameProjectRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "rename_project";
    const ONE_LINE_DOC: &'static str = "Rename a project, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Rename a project, as one transaction by `author` on its history that undoes like any edit: its `name` in `tatolab-project.json` changes, and its UUID, its folder and its streams' ids stay as they are. Answers how people see the project now, with the `transaction` and `commit`. Refused for a blank name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_project_description`: describe a project, or clear its description, as one transaction on its history.
pub struct SetProjectDescription;

impl ControlOperation for SetProjectDescription {
    type Request = SetProjectDescriptionRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "set_project_description";
    const ONE_LINE_DOC: &'static str =
        "Describe a project, or clear its description, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Describe a project, or clear its description, as one transaction by `author` on its history that undoes like any edit: its `description` in `tatolab-project.json`, served by `list_assets` as `project_description`. Answers how people see the project now, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_project_cover`: give a project a cover, or take it away, as one transaction on its history.
pub struct SetProjectCover;

impl ControlOperation for SetProjectCover {
    type Request = SetProjectCoverRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "set_project_cover";
    const ONE_LINE_DOC: &'static str =
        "Give a project a cover, or take it away, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Give a project a cover, or take it away, as one transaction by `author` on its history that undoes like any edit: its `cover` in `tatolab-project.json` names a media asset the project holds by its UUID, served by `list_assets` as `project_cover`. An image a person picks, or a frame a viewer showed, is first imported with `import_asset` and then named here; the asset cannot be deleted while it is the cover. Answers how people see the project now, with the `transaction` and `commit`. Refused for an asset the project does not hold, or one that is not a media asset.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `duplicate_project`: duplicate a project into a folder that does not exist yet, as a project of its own.
pub struct DuplicateProject;

impl ControlOperation for DuplicateProject {
    type Request = DuplicateProjectRequest;
    type Result = DuplicateProjectToolResult;
    const METHOD_NAME: &'static str = "duplicate_project";
    const ONE_LINE_DOC: &'static str =
        "Duplicate a project into a folder that does not exist yet, as a project of its own.";
    const AGENT_USAGE_NOTE: &'static str = "Duplicate a project into `into`, a folder that does not exist yet, as a project of its own: every file a copy of it carries, its `imports/` included, and its whole history, so its edits can still be undone in the copy, under a new project UUID and the name `<name> copy`, the duplicate recorded as a commit on its history by `author`. Its `.venv` and the rest of its `.tatolab/` stay behind, and the duplicate is not opened: `open_project` opens it. Answers the duplicate's `project_directory`, `project`, `project_name` and `commit`. Refused, nothing left at `into`, for a relative `into`, one that exists, and one inside the project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `delete_project`: queue the deletion of a whole project.
pub struct DeleteProject;

impl ControlOperation for DeleteProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = DeleteProjectToolResult;
    const METHOD_NAME: &'static str = "delete_project";
    const ONE_LINE_DOC: &'static str = "Queue the deletion of a whole project.";
    const AGENT_USAGE_NOTE: &'static str = "Delete the whole project in `project_directory`: queue its deletion on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, as `remove_project` does, then moves the project's folder to this machine's Trash, from which its owner can put it back. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`, which answers `failed` with why when the folder could not be moved. Refused, with nothing queued: a `project_directory` that is not absolute, and a folder holding no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_assets`: list a project's assets, ordered by path.
pub struct ListAssets;

impl ControlOperation for ListAssets {
    type Request = ListAssetsRequest;
    type Result = ListAssetsToolResult;
    const METHOD_NAME: &'static str = "list_assets";
    const ONE_LINE_DOC: &'static str = "List a project's assets, ordered by path.";
    const AGENT_USAGE_NOTE: &'static str = "List a project's assets, ordered by path: each by its `asset` UUID, its `kind` (`stream`, `media`, `scene` or `data`), its `path` in the project, the SHA-256 and size of its bytes, its `import_settings`, and what the runtime has read from its bytes — `media_facts_state` (`read`, `reading`, `unreadable` with `media_facts_unreadable_because`, or `null` for a stream), `media_facts` and `preview`, as `describe_asset` answers them, each from what is already read, an asset not yet read queued for a read — with the project's UUID, `project_name`, `project_description`, `project_cover` — the UUID of the image asset it is shown by — and `project_last_edited_at`, when its history last changed in seconds since the Unix epoch, each `null` when it has none, and `transactions_resource_uri`, its change feed. `kind` narrows to one kind. Refused for a folder with no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `describe_asset`: describe one asset of a project.
pub struct DescribeAsset;

impl ControlOperation for DescribeAsset {
    type Request = DescribeAssetRequest;
    type Result = DescribeAssetToolResult;
    const METHOD_NAME: &'static str = "describe_asset";
    const ONE_LINE_DOC: &'static str = "Describe one asset of a project.";
    const AGENT_USAGE_NOTE: &'static str = "Describe one asset of a project: its UUID, kind, path, the SHA-256 and size of its bytes, its import settings, `file_at_its_path`, whether a file is at its path, and what the runtime has read from its bytes, which it reads once per SHA-256 after an import or a replace and keeps in the project's `.tatolab/`. `media_facts_state` is `read`, `reading` — a read queued or under way, after which the project's change feed carries a `media_facts_read` entry — `unreadable`, with why in `media_facts_unreadable_because`, or `null` for a stream asset. `media_facts` carries `duration_ms`; `video` with `pixel_width`, `pixel_height`, `frames_per_second` and `codec`; `audio` with `sample_rate_hz`, `channel_count` and `codec`; `image` with `pixel_width` and `pixel_height`; `lut` with `lut_1d_size` or `lut_3d_size`, `domain_min` and `domain_max`; and `scene` — each `null` when the file does not hold or state it. `preview` is the size and `media_type` of the picture `read_asset_preview` answers, `null` when the asset has none.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `read_asset_preview`: read an asset's preview picture.
pub struct ReadAssetPreview;

impl ControlOperation for ReadAssetPreview {
    type Request = ReadAssetPreviewRequest;
    type Result = ReadAssetPreviewToolResult;
    const METHOD_NAME: &'static str = "read_asset_preview";
    const ONE_LINE_DOC: &'static str = "Read an asset's preview picture.";
    const AGENT_USAGE_NOTE: &'static str = "Read an asset's preview picture, which the runtime makes from its bytes and keeps in the project's `.tatolab/`: a PNG at most 512 pixels on its longest edge, answered as `content_base64`, standard base64 with padding, with its `media_type`, `pixel_width`, `pixel_height` and `byte_len`. An image has one; video, audio, a LUT, a scene and other data have none yet, as `describe_asset`'s `preview` being `null` says. Refused while the runtime is still reading the asset's bytes — `describe_asset`'s `media_facts_state` reads `reading`, and the project's change feed carries a `media_facts_read` entry once it is done — for an asset with no preview, and for a stream asset.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `import_asset`: import a file as a new asset of a project, as one transaction.
pub struct ImportAsset;

impl ControlOperation for ImportAsset {
    type Request = ImportAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "import_asset";
    const ONE_LINE_DOC: &'static str =
        "Import a file as a new asset of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Import a file as a new asset of the project at `asset_path`, or, with no `asset_path`, in its kind's folder of the project's standard layout — `media/` for video, audio and images, `scenes/` for USD, glTF and other 3D scenes, `data/` for everything else — under `file_name` or the source file's own name, numbered `<name>-2.<extension>`, `-3`, … past a name an asset or another file already takes there. Answers the `asset`, with the UUID the runtime made for it, which every later relocation keeps, and the `path` it landed at, and the `transaction` and `commit` that record it. The bytes are copied once, to `asset_path`, and only the asset's sidecar, `<asset_path>.asset.json`, enters history. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there; a file already at `asset_path` is imported in place. Undoing the import removes the file at `asset_path`, one imported in place included, and nothing keeps its bytes. Refused whole, with nothing written, for a path another asset or another file holds, a path outside the project, and a stream, which is never imported.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `relocate_asset`: move or rename an asset inside its project, as one transaction.
pub struct RelocateAsset;

impl ControlOperation for RelocateAsset {
    type Request = RelocateAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "relocate_asset";
    const ONE_LINE_DOC: &'static str =
        "Move or rename an asset inside its project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Put an asset at another path of the project, renaming it, moving it to another folder or both, its sidecar going with it, as one transaction; the asset keeps its UUID. Answers the asset as it now is, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `delete_asset`: delete an asset from a project, as one transaction.
pub struct DeleteAsset;

impl ControlOperation for DeleteAsset {
    type Request = DeleteAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "delete_asset";
    const ONE_LINE_DOC: &'static str = "Delete an asset from a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Stop tracking an asset as one transaction: its file and sidecar leave the project, and nothing keeps its bytes, so undoing the transaction brings back its sidecar and the asset reads missing until its file is at its path again. A stream asset's graph document leaves the project, recorded in the transaction so an undo writes it back, and its stream is unloaded and forgotten. Refused while a stream's graph document references the asset, the refusal naming each document. Answers the asset as it was, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_asset_settings`: replace an asset's import settings, as one transaction.
pub struct SetAssetSettings;

impl ControlOperation for SetAssetSettings {
    type Request = SetAssetSettingsRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "set_asset_settings";
    const ONE_LINE_DOC: &'static str = "Replace an asset's import settings, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Replace an asset's import settings whole, as one transaction. Answers the asset as it now is, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `replace_asset`: replace an asset's bytes, as one transaction.
pub struct ReplaceAsset;

impl ControlOperation for ReplaceAsset {
    type Request = ReplaceAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "replace_asset";
    const ONE_LINE_DOC: &'static str = "Replace an asset's bytes, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Put new bytes at an asset's path as one transaction: the asset keeps its UUID, path, kind and settings, so every node referencing it reads the new bytes at its next load, and its sidecar records their SHA-256 and size. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there. The bytes replaced are kept as an upload under their SHA-256, so an undo puts them back with the earlier sidecar; history records the sidecar alone. A file replaced outside the runtime is not supported. Answers the asset as it now is, with the `transaction` and `commit`; bytes the sidecar already records apply nothing and are refused as an empty transaction. Refused for a stream asset, whose graph document graph edits change.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `create_stream`: create a stream in a project, as one transaction.
pub struct CreateStream;

impl ControlOperation for CreateStream {
    type Request = CreateStreamRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "create_stream";
    const ONE_LINE_DOC: &'static str = "Create a stream in a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a stream in the project as one transaction: a new stream asset, its graph document written at `path` holding no nodes, its UUID, which the runtime makes, and its `display_name`. While the project runs, the new stream is loaded on the project's queue and the answer carries that load's `action`, which `describe_stream_action` follows. Answers the stream asset as `asset`, with the `transaction` and `commit`; an undo takes the document out and unloads the stream. Refused for a path another file holds or outside the project, and a blank `display_name`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `delete_stream`: delete a stream from a project, as one transaction.
pub struct DeleteStream;

impl ControlOperation for DeleteStream {
    type Request = DeleteStreamRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "delete_stream";
    const ONE_LINE_DOC: &'static str = "Delete a stream from a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Delete a stream from the project as one transaction: its graph document leaves the project, recorded in the transaction so an undo writes it back and loads the stream again while the project runs, and its stream is unloaded and forgotten. Answers the stream asset as it was, with the `transaction` and `commit`. Refused for a UUID the project holds no stream asset under.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_stream_display_name`: set the name a stream is shown under, as one transaction.
pub struct SetStreamDisplayName;

impl ControlOperation for SetStreamDisplayName {
    type Request = SetStreamDisplayNameRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "set_stream_display_name";
    const ONE_LINE_DOC: &'static str = "Set the name a stream is shown under, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Show a stream under another display name as one transaction, written in its graph document so every machine the project is on shows it and an undo writes the earlier one. A loaded stream is shown under it at once and keeps its stream id. Answers the stream asset as it now is, with the `transaction` and `commit`. Refused for a blank `display_name`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `add_dependency`: import another project into a project, as one transaction.
pub struct AddDependency;

impl ControlOperation for AddDependency {
    type Request = AddDependencyRequest;
    type Result = DependencyTransactionToolResult;
    const METHOD_NAME: &'static str = "add_dependency";
    const ONE_LINE_DOC: &'static str = "Import another project into a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Import another project, as one transaction by `author`: the project `source` holds, a folder or a `.zip` of one on this machine, is copied whole into `imports/<folder>/` and recorded in `tatolab-project.json` under `dependencies` with the `version` its copy carries, the SHA-256 over its files. Nothing reaches the network. The copy is this project's own from then on: its assets are referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read from its folder, its streams are placed with `use_dependency_stream`, and to edit it, open `imports/<folder>/` as a project of its own. Answers the `dependency`. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so the node types its imports install list. Refused, nothing copied, for a source holding no `tatolab-project.json` or no project UUID in it, naming what is missing, for this project itself, a project already imported, or a folder already held.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `update_dependency`: bring an imported project up to date with its source, as one transaction.
pub struct UpdateDependency;

impl ControlOperation for UpdateDependency {
    type Request = UpdateDependencyRequest;
    type Result = UpdateDependencyToolResult;
    const METHOD_NAME: &'static str = "update_dependency";
    const ONE_LINE_DOC: &'static str =
        "Bring an imported project up to date with its source, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Copy an import again, whole, as one transaction by `author`, from `source` or from the source it was last copied from, replacing its folder under `imports/` and its `version`. Every loaded stream holding one of its streams as a fragment follows the new copy. When files of its folder were edited since its copy, nothing changes: it answers them under `edited_files` with `overwrite_confirmation_needed`, and is sent again with `overwrite_edited_files` to overwrite them. Undo puts the folder back as it was. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so the node types its imports install list. Refused, nothing changed, for a project not imported, or a source holding another project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_dependencies`: list the projects a project imports.
pub struct ListDependencies;

impl ControlOperation for ListDependencies {
    type Request = ListDependenciesRequest;
    type Result = ListDependenciesToolResult;
    const METHOD_NAME: &'static str = "list_dependencies";
    const ONE_LINE_DOC: &'static str = "List the projects a project imports.";
    const AGENT_USAGE_NOTE: &'static str = "List the projects this one imports, each with its `folder`, `source` and `version` as `tatolab-project.json` holds them, and the files of its folder edited since its copy under `edited_files`; changes nothing. With `check_for_newer`, each source is read again and each import also answers `version_on_the_source` and `newer_version_exists`, or, for a source that could not be read, `check_refused_because`; nothing is copied until `update_dependency` copies it.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `remove_dependency`: take an imported project out of a project, as one transaction.
pub struct RemoveDependency;

impl ControlOperation for RemoveDependency {
    type Request = RemoveDependencyRequest;
    type Result = DependencyTransactionToolResult;
    const METHOD_NAME: &'static str = "remove_dependency";
    const ONE_LINE_DOC: &'static str =
        "Take an imported project out of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Stop importing a project, as one transaction by `author`: its folder leaves `imports/` and its entry `tatolab-project.json`; undo puts both back as they were. Answers the `dependency` as it was. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so its node types list without the import's. Refused, nothing changed, while a node of any of this project's streams — a fragment holding one of its streams, or a config holding `{\"asset\", \"from\"}` naming it, at any depth — still references it, naming each such node. Refused for a project not imported.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `use_dependency_stream`: place an imported project's stream in a loaded stream's graph as a
/// fragment node, as one transaction.
pub struct UseDependencyStream;

impl ControlOperation for UseDependencyStream {
    type Request = UseDependencyStreamRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "use_dependency_stream";
    const ONE_LINE_DOC: &'static str = "Place an imported project's stream in a loaded stream's graph as a fragment node, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Place a stream of an imported project in a loaded stream's graph, as one committed transaction: a fragment node of type `tatolab.project:StreamFragment` whose config `{\"stream\": {\"asset\", \"from\"}}` references the stream. The graph document stores the reference alone; each load, and each change to the stream or its import, places the stream's nodes and links as its graph document under `imports/<folder>/` is on disk, each node under the UUID UUIDv5(fragment node's UUID, its own). Its Graph Input and Output nodes are the fragment's ports. Answers as `transact` does. Refused for a name a node of the graph already holds, and for a stream that holds the stream it is placed in, naming each stream of the cycle.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_project_files`: list the files of a project that are not assets.
pub struct ListProjectFiles;

impl ControlOperation for ListProjectFiles {
    type Request = ListProjectFilesRequest;
    type Result = ListProjectFilesToolResult;
    const METHOD_NAME: &'static str = "list_project_files";
    const ONE_LINE_DOC: &'static str = "List the files of a project that are not assets.";
    const AGENT_USAGE_NOTE: &'static str = "List the project's files, or those under `under`, each by its path and length in bytes, sorted by path. Hidden folders such as `.tatolab/`, Python virtual environments, `node_modules/`, `__pycache__/` and symbolic links are not listed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `read_project_file`: read one file of a project.
pub struct ReadProjectFile;

impl ControlOperation for ReadProjectFile {
    type Request = ReadProjectFileRequest;
    type Result = ReadProjectFileToolResult;
    const METHOD_NAME: &'static str = "read_project_file";
    const ONE_LINE_DOC: &'static str = "Read one file of a project.";
    const AGENT_USAGE_NOTE: &'static str = "Read one of the project's files whole, answering its bytes as `content_base64`, standard base64 with padding, its `byte_len`, and its `content_sha256`, which a `write_project_file` sends back as `expected_sha256`. Refused for a path that is not a file of the project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `create_node_file`: create a node file in a project from the template, as one transaction.
pub struct CreateNodeFile;

impl ControlOperation for CreateNodeFile {
    type Request = CreateNodeFileRequest;
    type Result = CreateNodeFileToolResult;
    const METHOD_NAME: &'static str = "create_node_file";
    const ONE_LINE_DOC: &'static str =
        "Create a node file in a project from the template, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a new node file of the project from the runtime's node template, as one transaction an undo reverses: a `@node.process` class in the current `tatolab.stream` idiom, named after the file, with one Image input `video_in` whose frames it hands on unchanged to one Image output `video_out`, each with its description. It loads as a node type of the project with no edits and `list_project_node_types` lists it at once. Answers the `path`, the `node_type` it declares — `<module>:<class>`, what `add_node` takes — and the `transaction` and `commit`. Refused for a path that already holds a file, that is not a `.py` file importable as a Python module, or whose class name the template cannot declare, and for every path `write_project_file` refuses.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `write_project_file`: write one file of a project, as one transaction.
pub struct WriteProjectFile;

impl ControlOperation for WriteProjectFile {
    type Request = WriteProjectFileRequest;
    type Result = ProjectFileTransactionToolResult;
    const METHOD_NAME: &'static str = "write_project_file";
    const ONE_LINE_DOC: &'static str = "Write one file of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Write one of the project's files whole, as one transaction: a new file is created with its folders, an existing one overwritten. An undo puts back what the file held, or takes a new file out. Answers the `path`, the `content_sha256` of the bytes written, and the `transaction` and `commit`. With `expected_sha256`, refused when the file changed or was taken out since it was read, so a save never overwrites another change. Content the file already holds is refused, as it changes nothing. Refused for what another tool changes — a stream's graph document, an asset's sidecar or bytes (`replace_asset`), `tatolab-project.json`, a file under `imports/` — and for a file inside a hidden folder or a Python virtual environment, which history does not record.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_project_node_types`: list the node types a project offers, from its own node files and the
/// packages it installs.
pub struct ListProjectNodeTypes;

impl ControlOperation for ListProjectNodeTypes {
    type Request = ListProjectNodeTypesRequest;
    type Result = ListProjectNodeTypesToolResult;
    const METHOD_NAME: &'static str = "list_project_node_types";
    const ONE_LINE_DOC: &'static str = "List the node types a project offers, from its own node files and the packages it installs.";
    const AGENT_USAGE_NOTE: &'static str = "List the node types and graph generators a project offers, read from source without running any of its code: its own node code outside `imports/`, each import's as its folder holds it now, and each installed distribution that declares `tatolab.nodes` entry points, each under `sources` by `source` (`project`, `dependency` or `installed_distribution`). A node type has its `type` (what `add_node` takes; an import's is led by its folder, `imports/<folder>/<module>:<class>`, and imported from there), `kind` (`process`, `function` or `shader`), `description`, `category`, and `inputs` and `outputs` where its declaration states them, each port's `family` null where source alone does not say. A graph generator has its `generator` (what `import_graph_generator` takes), its docstring as `description`, and its `promoted_config`. A module that does not parse is listed under its source's `unreadable_modules` with why and where; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_project_views`: list the Tato Studio views a project brings.
pub struct ListProjectViews;

impl ControlOperation for ListProjectViews {
    type Request = ListProjectViewsRequest;
    type Result = ListProjectViewsToolResult;
    const METHOD_NAME: &'static str = "list_project_views";
    const ONE_LINE_DOC: &'static str = "List the Tato Studio views a project brings.";
    const AGENT_USAGE_NOTE: &'static str = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which is a node type of each stream the project runs when the view opens and its `placement` is `node`; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its `view_folder_in_the_project`, the folder relative to the project, as `views/<folder>` or `imports/<import>/views/<folder>`, which names its files to a project file read; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, a `family`, a `description` and an Array port's `range`; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `write_studio_view_output`: write a value a Tato Studio view produced onto its node's output
/// port.
pub struct WriteStudioViewOutput;

impl ControlOperation for WriteStudioViewOutput {
    type Request = WriteStudioViewOutputRequest;
    type Result = WriteStudioViewOutputToolResult;
    const METHOD_NAME: &'static str = "write_studio_view_output";
    const ONE_LINE_DOC: &'static str =
        "Write a value a Tato Studio view produced onto its node's output port.";
    const AGENT_USAGE_NOTE: &'static str = "Hand a value a Tato Studio panel wrote to the view node `node` of the stream `stream`, which publishes it on its output `port` to every link and viewer reading that port, stamped with stream time now. A view node is a node of a view type, `<project name>:<view folder>`, which each view of the stream's project or of one of its imports is when it opens and its `placement` is `node`; its ports are the view's declared inputs and outputs, and a panel reads its inputs with `open_viewer` on the port linked into each. A write is live data: the last value written is what a reader keeps, many panels may write one node and the last write wins, and nothing is recorded in the graph document or the project's history. Array, Channels and Records outputs are written; an Image or Packets output is refused. Refused for a stream whose project is stopped, naming `start_playback`; a node of another type; a node that is not running; and a value its port's family does not take. A refusal because the node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `write_graph_input`: write a value onto one of a loaded stream's graph inputs.
pub struct WriteGraphInput;

impl ControlOperation for WriteGraphInput {
    type Request = WriteGraphInputRequest;
    type Result = WriteGraphInputToolResult;
    const METHOD_NAME: &'static str = "write_graph_input";
    const ONE_LINE_DOC: &'static str = "Write a value onto one of a loaded stream's graph inputs.";
    const AGENT_USAGE_NOTE: &'static str = "Hand a value to the Input `input` of the stream `stream`: the stream's top-level `GraphInput` declaring it publishes the value on its `value` output to every node it is linked to, stamped with stream time now, and each reads it on its next process. A write is live data: many clients may write one Input and the last write wins, and nothing is recorded in the graph document or the project's history. An Input whose config's `lifetime` is `kept`, the default, keeps the last value in the project's store, so it carries it again after a Stop, a reload and a restart of the runtime; a `reset` Input returns to its `default` at Stop. While the project is stopped, a `kept` Input takes the value into the store and carries it at the next Play, and `graph` shows it; a `reset` Input is refused as `playback_stopped`. Until a write, the Input carries its config's `default`, if it declares one. `graph` renders the value an Input carries now, or carries at the next Play while stopped, under its node's `graph_interface` component, as `current_value`. Array, Channels and Records Inputs are written; an Image or Packets Input runs nothing and is refused. Refused for an Input no top-level `GraphInput` declares, and a value its family does not take. A refusal because the Input's node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `import_project`: import a project from an archive into a folder that does not exist yet.
pub struct ImportProject;

impl ControlOperation for ImportProject {
    type Request = ImportProjectRequest;
    type Result = ImportProjectToolResult;
    const METHOD_NAME: &'static str = "import_project";
    const ONE_LINE_DOC: &'static str =
        "Import a project from an archive into a folder that does not exist yet.";
    const AGENT_USAGE_NOTE: &'static str = "Make a new project in `project_directory` from the `.zip` of a project at `source`, as a Marketplace entry or a GitHub archive is handed over. Its files are unpacked as `add_dependency` copies a zip — every file its history would record and every asset, its `imports/` included, never a hidden folder such as its `.tatolab/`, a cache, a Python virtual environment or a symbolic link — under a new project UUID of its own, so the same zip imported twice makes two projects, and committed by `author` as the first commit of a history of its own. Nothing reaches the network and nothing runs: open it with `open_project`, and `start_playback` plays it. Answers the new `project_directory`, its `project` UUID, its `name` and the `commit`. Refused, nothing left behind, for a relative path, a `source` that is a folder or no zip archive, a zip holding no `tatolab-project.json` or no project UUID in it, naming what is missing, or a `project_directory` that already exists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `logs`: read a project's log records after a cursor, oldest first.
pub struct Logs;

impl ControlOperation for Logs {
    type Request = LogsRequest;
    type Result = LogsToolResult;
    const METHOD_NAME: &'static str = "logs";
    const ONE_LINE_DOC: &'static str = "Read a project's log records after a cursor, oldest first.";
    const AGENT_USAGE_NOTE: &'static str = "Read a project's log records after the cursor `after`, oldest first, each as the project's JSONL log on disk holds it: every stream of the project and every load of it writes that one log, each record stamped with its `stream` — none for a load's own lines, uv's among them — across the runtime's runs, whether the stream is still recorded or not. `stream` narrows to one stream's records, and `action` to the records of one stream action — every record its load wrote, uv's among them, and the records of the streams it started. Pass the result's `next_after` back as `after` to read on; `records_no_longer_held` is true when retention deleted the segment `after` named, so the page resumes at the oldest record held. A project that has logged nothing answers no records.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `open_viewer`: open a viewer of a node's output port, held open for the connection that opened
/// it.
pub struct OpenViewer;

impl ControlOperation for OpenViewer {
    type Request = OpenViewerRequest;
    type Result = OpenViewerToolResult;
    const METHOD_NAME: &'static str = "open_viewer";
    const ONE_LINE_DOC: &'static str =
        "Open a viewer of a node's output port, held open for the connection that opened it.";
    const AGENT_USAGE_NOTE: &'static str = "Open a viewer of any output port of a running node, adding no node and changing no graph: the runtime holds the port's channel open and answers the `viewer` id, the `iceoryx2_domain_root`, the `channel` whose reserved subscriber slot the viewer takes from its own process, and the `wake_service` the node notifies on each write, whose one listener the viewer takes so it waits for a message rather than reading on a timer. A viewer reads the latest message the node wrote and skips what it falls behind on, so it never slows the stream; an Image message names a pooled frame rather than carrying its pixels, and the viewer imports that frame's surface from the `surface_share_address` the way a helper process does. The viewer stays open until `close_viewer` or the end of the connection that opened it, so it is opened only over a connection that stays open: `/mcp/stdio` or `/control/ws`. Refused for a node that is not running, with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `close_viewer`: close a viewer this connection opened.
pub struct CloseViewer;

impl ControlOperation for CloseViewer {
    type Request = CloseViewerRequest;
    type Result = CloseViewerToolResult;
    const METHOD_NAME: &'static str = "close_viewer";
    const ONE_LINE_DOC: &'static str = "Close a viewer this connection opened.";
    const AGENT_USAGE_NOTE: &'static str =
        "Close a viewer this connection opened, letting go of its port's channel.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `set_stream_clock_scale`: run a started stream's scaled clock at another scale from now on.
pub struct SetStreamClockScale;

impl ControlOperation for SetStreamClockScale {
    type Request = SetStreamClockScaleRequest;
    type Result = SetStreamClockScaleToolResult;
    const METHOD_NAME: &'static str = "set_stream_clock_scale";
    const ONE_LINE_DOC: &'static str =
        "Run a started stream's scaled clock at another scale from now on.";
    const AGENT_USAGE_NOTE: &'static str = "Run a started stream's scaled clock at `scale` from now on: its stream time carries on from where it stands, never jumping or running backwards, and every node's `ctx.time`, write stamps and continuous pace follow the new scale. Above 1 the clock runs in steps of its `step_ns`, paced at the scale, behind the step barrier every node runs on; back at or under 1 it runs free again. Refused for a stream on the real or a stepped clock, and for a scale above 1 while the stream holds a node that paces itself (execution = manual) and is not paced by real time. Live only: the stream's next start runs the clock its graph document names.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `create_project`: queue the creation of a project from the starter or a sample.
pub struct CreateProject;

impl ControlOperation for CreateProject {
    type Request = CreateProjectRequest;
    type Result = CreateProjectToolResult;
    const METHOD_NAME: &'static str = "create_project";
    const ONE_LINE_DOC: &'static str =
        "Queue the creation of a project from the starter or a sample.";
    const AGENT_USAGE_NOTE: &'static str = "Create a project — the one `tatolab new` creates: the starter, its `tatolab-project.json`, one stream asset `streams/main.graph.json` holding the test pattern's one source, `pyproject.toml`, `.python-version` and `.gitignore`, or, given a `sample` that `list_samples` lists, a copy of that sample project under a new project UUID — committed as the first commit of the project's history, in `project_directory`, or with none in a folder `<adjective>-<noun>` drawn under `~/streams`, and queue its open, answering at once with the action's id as `action`, the stream id its `main` runs under as `stream`, and the resolved `project_directory`. The action writes the project's files, then opens it as `open_project` does — reads its graph documents, makes its `.venv`, however long uv takes, and holds its stream stopped until `start_playback` — the project named after its folder and its `main` stream shown under the folder's name. A directory already holding a file the create writes fails the action with nothing written. Follow it with `describe_stream_action` and `logs`; the project is recorded and re-loaded whenever the runtime starts, so a later session finds it in `list_streams`. Refused only for a `sample` the runtime unit does not carry, naming the ones it does, a relative `project_directory`, one that cannot be created, or, with none, a `HOME` that is unset or not absolute or a draw that finds no free folder.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_samples`: list the sample projects a new project can be created from.
pub struct ListSamples;

impl ControlOperation for ListSamples {
    type Request = ListSamplesRequest;
    type Result = ListSamplesToolResult;
    const METHOD_NAME: &'static str = "list_samples";
    const ONE_LINE_DOC: &'static str =
        "List the sample projects a new project can be created from.";
    const AGENT_USAGE_NOTE: &'static str = "List the sample projects the runtime unit carries at its own version, each by the `name` `create_project`'s `sample` takes, its `title` and a `summary` of what it shows, in name order: none for a runtime run outside a runtime unit.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `refresh_fragments`: refresh, as one transaction, every fragment whose graph generator a changed
/// module defines.
pub struct RefreshFragments;

impl ControlOperation for RefreshFragments {
    type Request = RefreshFragmentsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "refresh_fragments";
    const ONE_LINE_DOC: &'static str = "Refresh, as one transaction, every fragment whose graph generator a changed module defines.";
    const AGENT_USAGE_NOTE: &'static str = "Refresh, as one transaction, every fragment of the project's loaded streams whose graph generator, or a generator nested in it, is defined in a module one of `changed_files` holds: each is run again with the promoted settings its fragment keeps, as `import_graph_generator` refreshes it, each node keeping its UUID by name. Answers as `transact` does; with no such fragment, it answers no operation and records no transaction. A generator that raises refuses the whole refresh with its traceback, nothing changed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `undo`: undo an author's latest transaction in a project that is not undone yet, as a new
/// transaction.
pub struct Undo;

impl ControlOperation for Undo {
    type Request = UndoOrRedoRequest;
    type Result = ProjectTransactionOnTheWire;
    const METHOD_NAME: &'static str = "undo";
    const ONE_LINE_DOC: &'static str = "Undo an author's latest transaction in a project that is not undone yet, as a new transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Undo the latest transaction by `author` in the project that is not undone yet, applying its inverse as a new transaction: its graph edits to the running streams and their graph documents, its asset changes and file writes to the project. Only the author's own transactions are undone, so two authors' interleaved edits are each undone by their own author. An edit to a stream not loaded is applied to its graph document alone, and the stream of a stream document the undo takes out is unloaded and forgotten. Refused when the author has nothing left to undo, or when what it would undo no longer applies — a node another author removed since, or a stream document it would take out that was edited after it was written. Answers the transaction applied, with `undoes` naming the one it reversed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `redo`: redo what an author's latest undo in a project reversed, as a new transaction.
pub struct Redo;

impl ControlOperation for Redo {
    type Request = UndoOrRedoRequest;
    type Result = ProjectTransactionOnTheWire;
    const METHOD_NAME: &'static str = "redo";
    const ONE_LINE_DOC: &'static str =
        "Redo what an author's latest undo in a project reversed, as a new transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Redo what `author`'s latest undo in the project reversed, as a new transaction; a new edit by the author after the undo leaves nothing to redo. Answers the transaction applied, with `redoes` naming the one it reapplied.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// Every operation the registry holds, in the order the contract file lists them.
pub const CONTROL_OPERATIONS: &[ControlOperationDeclaration] = &[
    ControlOperationDeclaration::of::<Discover>(),
    ControlOperationDeclaration::of::<SubscribeProjectChanges>(),
    ControlOperationDeclaration::of::<SubscribeStreamGraphChanges>(),
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
            Self::InvalidParams { method_name, why } => {
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
    serde_json::from_value(params).map_err(|parse_failure| {
        ControlOperationCallParseRefusal::InvalidParams {
            method_name: Operation::METHOD_NAME,
            why: parse_failure.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_registered_operation_has_a_unique_method_name() {
        let method_names: BTreeSet<&str> = CONTROL_OPERATIONS
            .iter()
            .map(|declaration| declaration.method_name)
            .collect();
        assert_eq!(method_names.len(), CONTROL_OPERATIONS.len());
    }

    #[test]
    fn every_registered_operation_is_documented_versioned_and_served() {
        for declaration in CONTROL_OPERATIONS {
            assert!(
                !declaration.one_line_doc.is_empty(),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.one_line_doc.contains('\n'),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.agent_usage_note.is_empty(),
                "{}",
                declaration.method_name
            );
            assert!(
                (1..=CONTROL_CONTRACT_VERSION).contains(&declaration.since_contract_version),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.exposures.is_empty(),
                "{}",
                declaration.method_name
            );
        }
    }

    #[test]
    fn every_registered_method_name_parses_to_the_call_of_that_name_or_is_refused_for_its_params() {
        for declaration in CONTROL_OPERATIONS {
            match ControlOperationCall::parse(declaration.method_name, None) {
                Ok(call) => assert_eq!(call.method_name(), declaration.method_name),
                Err(ControlOperationCallParseRefusal::InvalidParams { method_name, .. }) => {
                    assert_eq!(method_name, declaration.method_name)
                }
                Err(refusal) => panic!("{refusal}"),
            }
        }
    }

    #[test]
    fn an_unknown_method_and_unknown_params_are_refused_by_name() {
        assert_eq!(
            ControlOperationCall::parse("expose_port", None).unwrap_err(),
            ControlOperationCallParseRefusal::NoSuchMethod {
                method_name: "expose_port".to_owned()
            }
        );
        let refusal =
            ControlOperationCall::parse("list_streams", Some(serde_json::json!({ "stream": "x" })))
                .unwrap_err();
        assert!(matches!(
            refusal,
            ControlOperationCallParseRefusal::InvalidParams {
                method_name: "list_streams",
                ..
            }
        ));
    }
}
