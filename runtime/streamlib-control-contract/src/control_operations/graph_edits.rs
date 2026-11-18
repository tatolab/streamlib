// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphTransactionToolResult, ProjectTransactionOnTheWire, WriteGraphInputToolResult,
};

use super::EVERY_SURFACE;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExposure, ControlOperationGroup, ControlOperationStability,
};
use crate::control_operation_requests::{
    AddGroupRequest, AddNodeRequest, AddNoteRequest, CollapseSelectionIntoSubgraphRequest,
    ImportGraphGeneratorRequest, OneLinkGraphEditRequest, OneNodeGraphEditRequest,
    RemoveGroupRequest, RemoveNoteRequest, RenameNodeRequest, SetExposedConfigFieldsRequest,
    SetGroupFieldsRequest, SetNodeConfigRequest, SetNodeInputValueRequest, SetNoteFieldsRequest,
    SetPinValueRequest, SetStudioFieldsRequest, TransactRequest, UndoOrRedoRequest,
    UnlockSubgraphRequest, WriteGraphInputRequest,
};

/// `transact`: several graph operations of one project as one transaction.
pub struct Transact;

impl ControlOperation for Transact {
    type Request = TransactRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "transact";
    const ONE_LINE_DOC: &'static str =
        "Apply operations on one project's stream graphs as one transaction, whole or not at all.";
    const AGENT_USAGE_NOTE: &'static str = "Apply operations on the graphs of one project's loaded streams as one transaction by its `author`: applied in order, whole or not at all, to each running stream, and with `mode` `commit`, the default, written to each stream's graph document and recorded as one commit on the project's history, which `undo` reverses as one. Answers `mode`, the `transaction` and `commit` recording a committed edit, each operation as applied, `added_nodes` with each new node's UUID and the name it took, and `connected_links` with each new link's `state` — `pending`, `wired`, or `error` with its `error_reason`. Refused whole, in the runtime's own words, with nothing changed. A `live` transaction sets each input of a node once, since each value set live is published as its own write: one setting the same input of the same node twice is refused with `refused` at its second set, before anything is applied. A config value `{\"asset\": \"<uuid>\"}` must name an asset the project holds. An `add_node` may give its node an `as`, which later operations of the same transaction name the node by. The transaction is announced on the project's change feed, the resource `list_assets` names as `transactions_resource_uri`.";
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
    const AGENT_USAGE_NOTE: &'static str = "Add a node to a loaded stream's graph by its type's import path, as one transaction, the runtime making the node's UUID. A Python type is named `module:QualifiedClassName` and must be importable by the stream's own interpreter — a module in the stream's project, or a package installed in its venv: the runtime never imports it, and describes it in that interpreter the first time it is added; each add spawns it in its own node interpreter, which imports the module fresh. The type's port declaration is read when it is first described and kept; to change a type's ports, add it under a new class name.";
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
    const AGENT_USAGE_NOTE: &'static str = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`. A link that would close a loop of function nodes with no `tatolab.stream:PreviousValue` node in it adds one and runs through it: the result names it under `added_nodes` and both of its links under `connected_links`. Two ends that both state a pin type connect when the input takes what the output sends: the same type; a Texture whose every pixel format is one the input lists, an input listing none taking any, and an output listing none connecting to an input that lists some; an encoded video or audio stream or an Asset whose every codec or media is one the input lists, an input listing none taking any, and an output listing none connecting only to an input that lists none; a Menu into a Menu, whatever options each lists; or a type the input reads from the output's for free: Boolean, Integer and Float among themselves, a Vector into a Vector or a Color, one number into a Vector or a Color, a Menu into a String. Any other pair is refused with `link_pin_types_do_not_connect`, naming both types and either the node or nodes to place between the two ends or that no node type converts between them. A link a saved graph document already holds that these rules refuse is kept when the stream loads, as long as its two ports' families match, and the runtime logs the refusal. A link into an input that is a setting is refused with `input_takes_no_wire`; set its value with `set_node_input_value`. These pin type rules judge a link in the stream's own graph; a link inside a placed stream, under a non-empty `level`, is not judged by them yet.";
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
    const ONE_LINE_DOC: &'static str = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit. Replaced by `set_node_input_value`.";
    const AGENT_USAGE_NOTE: &'static str = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit. A config value `{\"asset\": \"<uuid>\"}` references an asset of the project, which then cannot be deleted while the reference stands. Replaced by `set_node_input_value`, which sets one input or setting: this call is one such set for each key it names, and on a node type that still takes a config, one that takes off each config key it does not name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const REPLACED_BY: Option<&'static str> = Some(SetNodeInputValue::METHOD_NAME);
}

/// `set_pin_value`: set the value one of a node's inputs holds while no link feeds it, as one transaction.
pub struct SetPinValue;

impl ControlOperation for SetPinValue {
    type Request = SetPinValueRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_pin_value";
    const ONE_LINE_DOC: &'static str = "Set the value one of a node's inputs holds while no link feeds it, as one transaction. Replaced by `set_node_input_value`.";
    const AGENT_USAGE_NOTE: &'static str = "Set the value one of a node's inputs holds while no link feeds it, as one transaction; the value is saved in the graph document as a link is. Replaced by `set_node_input_value`, which this call is one set of.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const REPLACED_BY: Option<&'static str> = Some(SetNodeInputValue::METHOD_NAME);
}

/// `set_node_input_value`: set the value one input of one node holds while no link feeds it,
/// saved or live, as one transaction.
pub struct SetNodeInputValue;

impl ControlOperation for SetNodeInputValue {
    type Request = SetNodeInputValueRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "set_node_input_value";
    const ONE_LINE_DOC: &'static str = "Set the value one input of one node holds while no link feeds it, saved or live, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Set the value one input of one node holds while no link feeds it, as one transaction, on every kind of node: an input `graph` lists under the node's `ports.inputs`, or a setting it holds among its `input_values`. With `mode` `commit`, the default, the value is saved in the graph document, set on the running node and recorded as one commit that undo reverses to the value it replaced; with `mode` `live` it is set on the running node alone. A live value follows the lifetime its input declares: on a `kept` input, the default, it outlives Stop, the project's close and a restart of the runtime, and is held while the stream is stopped for its next Play; on a `reset` input it is taken off at Stop, so the input reads its saved value again, and a live set on it while the stream is stopped is refused with `node_not_running` and `not_running_because` `playback_stopped`. Omit `value`, or send `null`, to take the value off, so the input reads its declared default. The value is checked before anything is set: an input the node does not have is refused with `port_not_found`, an input that takes only a link with `input_takes_no_value`, and a value of another type or outside its range with `input_value_not_valid`, never clamped; each refusal names the node and the input. A set on an input a link feeds is kept and answered under `overridden_by_links`, since the link's messages are what the node reads. A config field takes a link once `set_exposed_config_fields` exposes it as an input. Omit `node` to set one of the stream's own inputs at the stream's own level: `input` names the port a top-level `GraphInput` declares, the set is made on that node's `value` input, which takes a value of the family the `GraphInput` declares, a number within its `range`, and lasts as its `lifetime` says; a name no `GraphInput` declares is refused with `port_not_found`, naming the stream's inputs. It is also an operation of `transact`, where a `live` transaction sets each input of a node once: a second live set of the same input is refused with `refused`, since each value set live is published as its own write.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
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
    const AGENT_USAGE_NOTE: &'static str = "Replace the config fields a node takes as inputs, as one transaction. Each is a number or bool field of the node's config schema and becomes an input of the field's name: linked, it drives the field, a function node reading it at each evaluation and a process node taking each new value as a live config update; unlinked, it reads the node's config. A field still linked cannot be taken off. On a node type that takes no config it records nothing, and refuses a name that is not one of the node's inputs. It makes a config field linkable; `set_node_input_value` sets a field's value.";
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

/// `write_graph_input`: write a value onto one of a loaded stream's graph inputs.
pub struct WriteGraphInput;

impl ControlOperation for WriteGraphInput {
    type Request = WriteGraphInputRequest;
    type Result = WriteGraphInputToolResult;
    const METHOD_NAME: &'static str = "write_graph_input";
    const ONE_LINE_DOC: &'static str = "Write a value onto one of a loaded stream's graph inputs. Replaced by `set_node_input_value`.";
    const AGENT_USAGE_NOTE: &'static str = "Hand a value to the Input `input` of the stream `stream`: the stream's top-level `GraphInput` declaring it publishes the value on its `value` output to every node it is linked to, stamped with stream time now, and each reads it on its next process; every write is published, the same value written again as much as a new one, and a node linked later reads the last value set. A write is live data: many clients may write one Input and the last write wins, and nothing is recorded in the graph document or the project's history. An Input whose config's `lifetime` is `kept`, the default, keeps the last value in the project's store, so it carries it again after a Stop, a reload and a restart of the runtime; a `reset` Input returns to its saved value, its `default`, at Stop. While the project is stopped, a `kept` Input takes the value and carries it at the next Play, and `graph` shows it; a `reset` Input is refused as `playback_stopped`. Until a write, the Input carries its saved value, if it holds one. `graph` renders the value an Input carries now, or carries at the next Play while stopped, under its node's `graph_interface` component, as `current_value`. Array, Channels and Records Inputs are written; an Image or Packets Input runs nothing and is refused. Refused for an Input no top-level `GraphInput` declares, and a value its family does not take. A refusal because the Input's node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`. Replaced by `set_node_input_value`, which this call is one `live` set of, naming no node and the Input as `input`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const REPLACED_BY: Option<&'static str> = Some(SetNodeInputValue::METHOD_NAME);
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
