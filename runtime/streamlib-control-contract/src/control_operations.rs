// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every operation the registry holds, the table the adapters iterate, and the one call type a
//! dispatcher matches on.

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CancelStreamActionToolResult, DescribeRuntimeToolResult, DescribeStreamActionToolResult,
    GraphTransactionToolResult, ListStreamsToolResult, PausePlaybackToolResult,
    ProjectStreamActionQueuedToolResult, RemoveProjectToolResult, ResumePlaybackToolResult,
};

use crate::control_operation::{
    ControlOperation, ControlOperationDeclaration, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    AddGroupRequest, AddNodeRequest, AddNoteRequest, CollapseSelectionIntoSubgraphRequest,
    DescribeRuntimeRequest, DiscoverRequest, GraphRequest, ImportGraphGeneratorRequest,
    ListStreamsRequest, OneLinkGraphEditRequest, OneNodeGraphEditRequest,
    OneProjectByItsDirectoryRequest, OneStreamActionRequest, RemoveGroupRequest, RemoveNoteRequest,
    RenameNodeRequest, SetExposedConfigFieldsRequest, SetGroupFieldsRequest, SetNodeConfigRequest,
    SetNoteFieldsRequest, SetPinValueRequest, SetStudioFieldsRequest,
    SubscribeProjectChangesRequest, TransactRequest, UnlockGeneratorFragmentRequest,
};
use crate::control_operation_results::{DiscoverResult, SubscribeProjectChangesResult};

/// The contract's version: bumped only by a breaking change to an operation already in it.
pub const CONTROL_CONTRACT_VERSION: u32 = 1;

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
}

/// `subscribe_project_changes`: the connection is sent each change on some projects' feeds.
pub struct SubscribeProjectChanges;

impl ControlOperation for SubscribeProjectChanges {
    type Request = SubscribeProjectChangesRequest;
    type Result = SubscribeProjectChangesResult;
    const METHOD_NAME: &'static str = "subscribe_project_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `project_changed` notification for each change on the named projects' feeds.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the change feeds of `projects`, by UUID: from then on, each transaction applied to one, each action the runtime refuses on one and each media facts read it finishes is sent as a `project_changed` notification, whoever made it, until the connection closes. Served only on a connection that stays open; refused for an empty `projects`. Answers every project the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
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
}

/// `connect`: one link between an output port and an input port.
pub struct Connect;

impl ControlOperation for Connect {
    type Request = OneLinkGraphEditRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "connect";
    const ONE_LINE_DOC: &'static str =
        "Link an output port to an input port in one loaded stream, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
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
}

/// `collapse_selection_into_subgraph`: collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction.
pub struct CollapseSelectionIntoSubgraph;

impl ControlOperation for CollapseSelectionIntoSubgraph {
    type Request = CollapseSelectionIntoSubgraphRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "collapse_selection_into_subgraph";
    const ONE_LINE_DOC: &'static str = "Collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction: an authored fragment holding the nodes and the links between them, which no refresh touches and whose inside is edited like any graph. Each link that crossed the selection is carried through a `GraphInput` or `GraphOutput` inside it, named `<node>-<port>` and of the family the running stream declares at the link's source, so the stream runs the same nodes, each under the subgraph's name (`sub/blur`). Answers as `transact` does, the subgraph node under `added_nodes`; undo gives back the exact earlier graph document.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `unlock_generator_fragment`: unlock a fragment node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.
pub struct UnlockGeneratorFragment;

impl ControlOperation for UnlockGeneratorFragment {
    type Request = UnlockGeneratorFragmentRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "unlock_generator_fragment";
    const ONE_LINE_DOC: &'static str = "Unlock a fragment node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Unlock a fragment node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction: its nodes and the links between them become plain nodes of its level, its `GraphInput` and `GraphOutput` nodes go, and each link into or out of one of its ports is carried straight to the node it fed or read. A graph generator's output unlocked this way is no longer refreshed. Refused for a placed stream, and for a node of the fragment named as a node of the stream is; undo gives back the exact earlier graph document.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
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
}

/// `describe_stream_action`: describe one stream action by its id: its state, its step and each stream it reached.
pub struct DescribeStreamAction;

impl ControlOperation for DescribeStreamAction {
    type Request = OneStreamActionRequest;
    type Result = DescribeStreamActionToolResult;
    const METHOD_NAME: &'static str = "describe_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Describe one stream action by its id: its state, its step and each stream it reached.";
    const AGENT_USAGE_NOTE: &'static str = "Describe the stream action `action`: its project, `kind` (`open`, `load`, `start_playback`, `stop`, `close`, `remove`, `delete` or `reload_at_start`), `state` — `queued` behind the project's earlier actions, `in_progress` on its `step` (`reading_the_graph_documents`, `making_the_venv`, `starting`), `done`, `failed` with `failed_because`, or `cancelled` by `cancel_stream_action` — its `venv_sync`, what its latest uv sync of the project's `.venv` has done, read from uv's own lines from the moment uv starts and kept once it ends, `null` for an action that has not synced: its `phase` (`resolving`, `downloading`, `building`, `installing`, `installed`), `packages_resolved`, `packages_prepared` and `packages_installed` as uv counts them, `downloads_started` and `downloads_done` of the downloads uv names — only those over about a MiB — and under `packages` each package uv named, first named first, by `name`, `version` once uv names it, `kind` (`distribution` or `python_interpreter`), `state` (`downloading`, `downloaded`, `building`, `built`, `installed`, `uninstalled`) and `download_size_bytes` as uv states it; uv names no byte of a download as it arrives — and `streams`, each stream its project's graph documents named by `stream` id and `display_name`, with its `state` `running`, `stopped` or `failed` once the action took it in. Followed live as the resource `streamlib://stream-action/<action>`, which a `subscriptions/listen` naming it is sent `notifications/resources/updated` for each time the action changes. The runtime holds every queued and running action and the last 64 finished ones of each project; an older one is refused, its records still in the project's `logs`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `cancel_stream_action`: cancel one stream action that is still queued or under way.
pub struct CancelStreamAction;

impl ControlOperation for CancelStreamAction {
    type Request = OneStreamActionRequest;
    type Result = CancelStreamActionToolResult;
    const METHOD_NAME: &'static str = "cancel_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Cancel one stream action that is still queued or under way.";
    const AGENT_USAGE_NOTE: &'static str = "Cancel the stream action `action`, the only way an action ends early; nothing replaces or edits a queued one. A queued action is dropped from its project's queue, its pending mark taken off each stream's record so the record stays as it was, and answers `cancelled`. A running `open`, `load` or `reload_at_start` answers `in_progress` and stops at the step it is on — a running uv sync is killed and a half-made `.venv` removed — then ends `cancelled`: a stream it was loading is recorded failed, and the streams it had not reached stay as recorded. Refused for a running `stop`, `close` or `remove`, whose unload has begun and cannot be cut short, which ends `done`; for an action already finished; and for one the runtime does not hold.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// Every operation the registry holds, in the order the contract file lists them.
pub const CONTROL_OPERATIONS: &[ControlOperationDeclaration] = &[
    ControlOperationDeclaration::of::<Discover>(),
    ControlOperationDeclaration::of::<SubscribeProjectChanges>(),
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
    ControlOperationDeclaration::of::<UnlockGeneratorFragment>(),
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
];

/// One call of a registered operation with its parsed params; a dispatcher matches on it
/// exhaustively, so an operation added here and not served is a compile error.
#[derive(Debug)]
pub enum ControlOperationCall {
    /// A `discover` call.
    Discover(DiscoverRequest),
    /// A `subscribe_project_changes` call.
    SubscribeProjectChanges(SubscribeProjectChangesRequest),
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
    /// A `unlock_generator_fragment` call.
    UnlockGeneratorFragment(UnlockGeneratorFragmentRequest),
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
            UnlockGeneratorFragment::METHOD_NAME => {
                the_request_of::<UnlockGeneratorFragment>(params).map(Self::UnlockGeneratorFragment)
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
            Self::UnlockGeneratorFragment(_) => UnlockGeneratorFragment::METHOD_NAME,
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
