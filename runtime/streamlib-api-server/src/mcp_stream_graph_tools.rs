// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph tools: every edit of a stream's graph a transaction, committed to the graph
//! document and the project's history or applied live to the running stream alone, its nodes
//! by UUID, and each author's undo and redo.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Peer, RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamLocation,
};
use streamlib_project_repository::{
    GRAPH_DOCUMENT_FILE_SUFFIX, ProjectAssetKind, ProjectAssetOperation,
    ProjectDependencyOperation, ProjectOperation, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepository, ProjectStreamDocumentOperation, ProjectTransactionAuthor,
    ProjectTransactionLineage, ProjectTransactionReversal, STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY,
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentGroup,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphFragmentPlacement, StreamGraphGroupFields, StreamGraphOperation,
    StreamGraphOperationOnAStream, a_graph_document_as_a_fragment_run,
    asset_references_made_references_into_the_dependency,
    fragment_with_each_node_uuid_kept_by_its_name, the_refresh_of_each_fragment_holding_a_stream,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphLinkEndOnTheWire, GraphTransactionAddedGroup, GraphTransactionAddedNode,
    GraphTransactionConnectedLink, GraphTransactionToolResult,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorToolArgument, the_author_of_a_change,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;

pub(crate) const GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream the edit is to, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";
const GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION: &str = "The node, by the UUID `graph` lists as its `id` and `add_node` answers; in `transact`, also the `as` of a node an earlier `add_node` of the same transaction adds.";
const IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION: &str = "The graph generator by its import path, `<module>:<function>`, a `@graph_generator` function the stream's own interpreter imports from its project, e.g. `generators.looks:soft_look`.";
const IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION: &str = "The fragment node's name, cast to lowercase URL-safe; the generator's own name when omitted. A top-level fragment of that name is refreshed, and any other node of that name refuses the import.";
const IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION: &str = "The settings the generator promotes, by name, which it runs with and the fragment node keeps as its `config`. Omit for the generator's defaults.";
const GROUP_ARGUMENT_DESCRIPTION: &str =
    "The labelled group, by the UUID `graph` lists under `groups` and `add_group` answers.";
const GROUP_TITLE_ARGUMENT_DESCRIPTION: &str =
    "The group's title, which Tato Studio draws on its frame.";
const GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION: &str =
    "What the group's nodes do, for the next person who reads the graph.";
const GROUP_COLOUR_ARGUMENT_DESCRIPTION: &str =
    "The colour Tato Studio draws the group's frame with, e.g. `#3b82f6`.";
pub(crate) const DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION: &str = "The project depended on, by the UUID `add_dependency` answers and `tatolab-project.json` lists under `dependencies`.";
pub(crate) const DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION: &str = "The depended-on project's stream, by its stream asset's UUID, the `asset` its graph document holds.";
pub(crate) const DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION: &str = "The fragment node's name, cast to lowercase URL-safe; the stream's own name when omitted. A top-level fragment of that name holding the same stream is refreshed from the pin.";
const GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION: &str = "`commit`, the default: the edit is applied to the running stream, written to the stream's graph document and recorded as one commit on the project's history, so it is kept on the next load and undo reverses it. `live`: applied to the running stream alone, gone on the next load, never in history. Send a continuous gesture as one transaction when it ends.";

/// Whether a graph edit is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphEditMode {
    /// Applied, written to the graph document and recorded in history.
    #[default]
    Commit,
    /// Applied to the running stream alone.
    Live,
}

impl GraphEditMode {
    fn on_the_wire(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Live => "live",
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphLinkEndToolArgument {
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = "The port's name, as `graph` lists it under the node's `ports`.")]
    port: String,
}

/// One operation of a graph transaction, as a caller names it.
#[derive(Clone, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StreamGraphOperationToolArgument {
    /// Place a node, answering the UUID the runtime makes for it.
    AddNode {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[serde(rename = "type")]
        #[schemars(
            description = "The node's type by its import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`, as `graph` reports it and `streamlib://node-catalog` lists it."
        )]
        node_type: String,
        #[schemars(
            description = "The node's name, a label: cast to lowercase URL-safe and refused when another node has it. Omit it for the type's short name, cast, with the next free `-2`, `-3` …"
        )]
        name: Option<String>,
        #[schemars(description = "The node's config, as the keys its type's schema declares.")]
        config: Option<Map<String, Value>>,
        #[schemars(description = "The value on each unlinked input, by input name.")]
        pins: Option<Map<String, Value>>,
        #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
        exposed_config_fields: Option<Vec<String>>,
        #[schemars(
            description = "What the studio draws the node with: canvas position, group, comment, colour."
        )]
        studio: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the node by, before its UUID is known."
        )]
        label: Option<String>,
    },
    /// Take a node out, with every link it has.
    RemoveNode {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
    },
    /// Link an output port to an input port.
    Connect {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = "The output port the link carries from.")]
        source: GraphLinkEndToolArgument,
        #[schemars(description = "The input port the link carries into.")]
        target: GraphLinkEndToolArgument,
    },
    /// Take out the link between two ports.
    Disconnect {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = "The output port the link carries from.")]
        source: GraphLinkEndToolArgument,
        #[schemars(description = "The input port the link carries into.")]
        target: GraphLinkEndToolArgument,
    },
    /// Replace a node's config whole.
    SetNodeConfig {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
        #[schemars(description = "The node's new config, as the keys its type's schema declares.")]
        config: Map<String, Value>,
    },
    /// Set the value on one of a node's inputs.
    SetPinValue {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
        #[schemars(
            description = "The input's name, as `graph` lists it under the node's `ports.inputs`."
        )]
        input: String,
        #[schemars(
            description = "The value the input holds while no link feeds it. Omit it, or send `null`, to take the value off."
        )]
        value: Option<Value>,
    },
    /// Give a node another name.
    RenameNode {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
        #[schemars(
            description = "The node's new name: cast to lowercase URL-safe, refused when another node has it."
        )]
        name: String,
    },
    /// Replace a node's studio fields whole.
    SetStudioFields {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
        #[schemars(
            description = "What the studio draws the node with: canvas position, group, comment, colour."
        )]
        studio: Map<String, Value>,
    },
    /// Run one of the project's graph generators and place what it builds as a fragment, or
    /// refresh the fragment of that name with a fresh run.
    ImportGraphGenerator {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION)]
        generator: String,
        #[schemars(description = IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION)]
        name: Option<String>,
        #[schemars(description = IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION)]
        config: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
        )]
        label: Option<String>,
    },
    /// Replace the config fields a node takes as inputs whole.
    SetExposedConfigFields {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
        node: String,
        #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
        exposed_config_fields: Vec<String>,
    },
    /// Make a labelled group of nodes of the stream's own level.
    AddGroup {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION)]
        title: String,
        #[schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION)]
        description: Option<String>,
        #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
        colour: Option<String>,
        #[schemars(
            description = "The nodes that join it, each as `node` names one; a node of another group is refused."
        )]
        nodes: Vec<String>,
    },
    /// Set a labelled group's title, description or colour.
    SetGroupFields {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
        group: String,
        #[schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION)]
        title: Option<String>,
        #[schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION)]
        description: Option<String>,
        #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
        colour: Option<String>,
    },
    /// Place a stream of a project this one depends on, at its pin, as a fragment node, or
    /// refresh the fragment of that name from the pin.
    UseDependencyStream {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
        project: String,
        #[schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION)]
        dependency_stream: String,
        #[schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION)]
        name: Option<String>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
        )]
        label: Option<String>,
    },
    /// Take out a labelled group, its nodes left ungrouped.
    RemoveGroup {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
        group: String,
    },
}

/// What a node's `exposed_config_fields` are, as every tool that takes them says.
const EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION: &str = "The config fields the node takes as inputs too, each a number or bool field of its config schema, shown as an input of the field's name. A link into one drives the field: a function node reads it at each evaluation, a process node takes each new value as a live config update. Unlinked, it reads the node's config.";

impl StreamGraphOperationToolArgument {
    fn stream(&self) -> &str {
        match self {
            Self::AddNode { stream, .. }
            | Self::RemoveNode { stream, .. }
            | Self::Connect { stream, .. }
            | Self::Disconnect { stream, .. }
            | Self::SetNodeConfig { stream, .. }
            | Self::SetPinValue { stream, .. }
            | Self::RenameNode { stream, .. }
            | Self::SetStudioFields { stream, .. }
            | Self::ImportGraphGenerator { stream, .. }
            | Self::SetExposedConfigFields { stream, .. }
            | Self::AddGroup { stream, .. }
            | Self::SetGroupFields { stream, .. }
            | Self::RemoveGroup { stream, .. }
            | Self::UseDependencyStream { stream, .. } => stream,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct TransactToolArguments {
    #[schemars(
        description = "The operations, applied in order and whole or not at all, each tagged by its `operation` — `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group` or `use_dependency_stream` — with the arguments that operation's own tool takes. Every `stream` they name is a stream of one project."
    )]
    operations: Vec<StreamGraphOperationToolArgument>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[serde(rename = "type")]
    #[schemars(
        description = "The node's type by its import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`, as `graph` reports it and `streamlib://node-catalog` lists it."
    )]
    node_type: String,
    #[schemars(
        description = "The node's name, a label: cast to lowercase URL-safe (`Front Camera` becomes `front-camera`) and refused when another node has it. Omit it for the type's short name, cast, with the next free `-2`, `-3` …"
    )]
    name: Option<String>,
    #[schemars(
        description = "The node's config, as the keys its type's schema declares. Omit for none."
    )]
    config: Option<Map<String, Value>>,
    #[schemars(description = "The value on each unlinked input, by input name.")]
    pins: Option<Map<String, Value>>,
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    exposed_config_fields: Option<Vec<String>>,
    #[schemars(
        description = "What the studio draws the node with: canvas position, group, comment, colour."
    )]
    studio: Option<Map<String, Value>>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OneNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OneLinkToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(
        description = "The output port the link carries from: a node's UUID and its output port's name."
    )]
    source: GraphLinkEndToolArgument,
    #[schemars(
        description = "The input port the link carries into: a node's UUID and its input port's name."
    )]
    target: GraphLinkEndToolArgument,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetNodeConfigToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "The node's new config, as the keys its type's schema declares; it replaces the old one whole."
    )]
    config: Map<String, Value>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetPinValueToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "The input's name, as `graph` lists it under the node's `ports.inputs`."
    )]
    input: String,
    #[schemars(
        description = "The value the input holds while no link feeds it. Omit it, or send `null`, to take the value off."
    )]
    value: Option<Value>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RenameNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "The node's new name: cast to lowercase URL-safe, refused when another node has it. The node keeps its UUID."
    )]
    name: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetStudioFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "What the studio draws the node with: canvas position, group, comment, colour. It replaces the old fields whole."
    )]
    studio: Map<String, Value>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION)]
    title: String,
    #[schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION)]
    description: Option<String>,
    #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
    colour: Option<String>,
    #[schemars(
        description = "The nodes that join it, each by the UUID `graph` lists as its `id`; a node of another group is refused."
    )]
    nodes: Vec<String>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetGroupFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    group: String,
    #[schemars(description = "The group's new title; kept when omitted.")]
    title: Option<String>,
    #[schemars(description = "What the group's nodes do; kept when omitted, cleared by `\"\"`.")]
    description: Option<String>,
    #[schemars(description = "The group's new colour; kept when omitted, cleared by `\"\"`.")]
    colour: Option<String>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    group: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ImportGraphGeneratorToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION)]
    generator: String,
    #[schemars(description = IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION)]
    name: Option<String>,
    #[schemars(description = IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION)]
    config: Option<Map<String, Value>>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetExposedConfigFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    exposed_config_fields: Vec<String>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RefreshFragmentsToolArguments {
    #[schemars(
        description = "The project, by absolute path: a folder holding `tatolab-project.json`."
    )]
    project_directory: PathBuf,
    #[schemars(
        description = "The project's Python files that changed, each by its path relative to the project directory or absolute inside it, e.g. `generators/looks.py`; each names the module it is imported as."
    )]
    changed_files: Vec<PathBuf>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct UndoOrRedoToolArguments {
    #[schemars(
        description = "The project, by absolute path: a folder holding `tatolab-project.json`."
    )]
    project_directory: PathBuf,
    #[schemars(
        description = "Whose edits: an undo or a redo reverses only its own author's. Omit it to be the agent the MCP client named in its `clientInfo`."
    )]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[tool_router(router = stream_graph_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Apply operations on the graphs of one project's loaded streams as one transaction by its `author`: applied in order, whole or not at all, to each running stream, and with `mode` `commit`, the default, written to each stream's graph document and recorded as one commit on the project's history, which `undo` reverses as one. Answers `mode`, the `transaction` and `commit` recording a committed edit, each operation as applied, `added_nodes` with each new node's UUID and the name it took, and `connected_links` with each new link's `state` — `pending`, `wired`, or `error` with its `error_reason`. Refused whole, in the runtime's own words, with nothing changed. A config value `{\"asset\": \"<uuid>\"}` must name an asset the project holds. An `add_node` may give its node an `as`, which later operations of the same transaction name the node by. The transaction is announced on the project's change feed, the resource `list_assets` names as `transactions_resource_uri`."
    )]
    async fn transact(
        &self,
        peer: Peer<RoleServer>,
        Parameters(TransactToolArguments {
            operations,
            mode,
            author,
        }): Parameters<TransactToolArguments>,
    ) -> ToolCallAnswer {
        self.one_graph_transaction("transact", operations, mode, author, &peer)
            .await
    }

    #[tool(
        description = "Run one of the project's `@graph_generator` functions in the stream's own interpreter and place what it builds in a loaded stream's graph as a fragment node, as one transaction: its generator as `type`, its promoted settings as `config`, and the nodes and links it built stored under `fragment`, each node with a UUID of its own. Run again under the name of a fragment the graph holds, it refreshes that fragment with the fresh run: each node it builds keeps the UUID of the node of the same name it built before, so links into the fragment and the graph's history hold. A generator that raises, refuses, or is not found is refused with its traceback, nothing changed. Answers as `transact` does, a new fragment node under `added_nodes`; undo restores the fragment as it was."
    )]
    async fn import_graph_generator(
        &self,
        peer: Peer<RoleServer>,
        Parameters(ImportGraphGeneratorToolArguments {
            stream,
            generator,
            name,
            config,
            mode,
            author,
        }): Parameters<ImportGraphGeneratorToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::ImportGraphGenerator {
            stream,
            generator,
            name,
            config,
            label: None,
        };
        self.one_graph_transaction(
            "import_graph_generator",
            vec![operation],
            mode,
            author,
            &peer,
        )
        .await
    }

    #[tool(
        description = "Add a node to a loaded stream's graph by its type's import path, as one transaction, the runtime making the node's UUID. A Python type is named `module:QualifiedClassName` and must be importable by the stream's own interpreter — a module in the stream's project, or a package installed in its venv: the runtime never imports it, and describes it in that interpreter the first time it is added; each add spawns it in its own processor interpreter, which imports the module fresh. The type's port declaration is read when it is first described and kept; to change a type's ports, add it under a new class name."
    )]
    async fn add_node(
        &self,
        peer: Peer<RoleServer>,
        Parameters(AddNodeToolArguments {
            stream,
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            mode,
            author,
        }): Parameters<AddNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::AddNode {
            stream,
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            label: None,
        };
        self.one_graph_transaction("add_node", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Remove a node from a loaded stream's graph by its UUID, as one transaction, stopping it; its links go with it, and undo brings both back."
    )]
    async fn remove_node(
        &self,
        peer: Peer<RoleServer>,
        Parameters(OneNodeToolArguments {
            stream,
            node,
            mode,
            author,
        }): Parameters<OneNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::RemoveNode { stream, node };
        self.one_graph_transaction("remove_node", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`."
    )]
    async fn connect(
        &self,
        peer: Peer<RoleServer>,
        Parameters(OneLinkToolArguments {
            stream,
            source,
            target,
            mode,
            author,
        }): Parameters<OneLinkToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::Connect {
            stream,
            source,
            target,
        };
        self.one_graph_transaction("connect", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Take out the link between an output port and an input port in one loaded stream, as one transaction, each end a node's UUID and a port name."
    )]
    async fn disconnect(
        &self,
        peer: Peer<RoleServer>,
        Parameters(OneLinkToolArguments {
            stream,
            source,
            target,
            mode,
            author,
        }): Parameters<OneLinkToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::Disconnect {
            stream,
            source,
            target,
        };
        self.one_graph_transaction("disconnect", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit. A config value `{\"asset\": \"<uuid>\"}` references an asset of the project, which then cannot be deleted while the reference stands."
    )]
    async fn set_node_config(
        &self,
        peer: Peer<RoleServer>,
        Parameters(SetNodeConfigToolArguments {
            stream,
            node,
            config,
            mode,
            author,
        }): Parameters<SetNodeConfigToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::SetNodeConfig {
            stream,
            node,
            config,
        };
        self.one_graph_transaction("set_node_config", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Set the value one of a node's inputs holds while no link feeds it, as one transaction; the value is saved in the graph document as a link is."
    )]
    async fn set_pin_value(
        &self,
        peer: Peer<RoleServer>,
        Parameters(SetPinValueToolArguments {
            stream,
            node,
            input,
            value,
            mode,
            author,
        }): Parameters<SetPinValueToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::SetPinValue {
            stream,
            node,
            input,
            value,
        };
        self.one_graph_transaction("set_pin_value", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Give a node another name, a label, as one transaction; the node keeps its UUID, which every address names it by. Refused while another node's config names it by its name."
    )]
    async fn rename_node(
        &self,
        peer: Peer<RoleServer>,
        Parameters(RenameNodeToolArguments {
            stream,
            node,
            name,
            mode,
            author,
        }): Parameters<RenameNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::RenameNode { stream, node, name };
        self.one_graph_transaction("rename_node", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Replace what the studio draws a node with — canvas position, group, comment, colour — as one transaction."
    )]
    async fn set_studio_fields(
        &self,
        peer: Peer<RoleServer>,
        Parameters(SetStudioFieldsToolArguments {
            stream,
            node,
            studio,
            mode,
            author,
        }): Parameters<SetStudioFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::SetStudioFields {
            stream,
            node,
            studio,
        };
        self.one_graph_transaction("set_studio_fields", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Replace the config fields a node takes as inputs, as one transaction. Each is a number or bool field of the node's config schema and becomes an input of the field's name: linked, it drives the field, a function node reading it at each evaluation and a process node taking each new value as a live config update; unlinked, it reads the node's config. A field still linked cannot be taken off."
    )]
    async fn set_exposed_config_fields(
        &self,
        peer: Peer<RoleServer>,
        Parameters(SetExposedConfigFieldsToolArguments {
            stream,
            node,
            exposed_config_fields,
            mode,
            author,
        }): Parameters<SetExposedConfigFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::SetExposedConfigFields {
            stream,
            node,
            exposed_config_fields,
        };
        self.one_graph_transaction(
            "set_exposed_config_fields",
            vec![operation],
            mode,
            author,
            &peer,
        )
        .await
    }

    #[tool(
        description = "Make a labelled group of nodes of a loaded stream's own level, as one transaction: a title, a description and a colour Tato Studio frames the nodes with, so the next reader knows what they do. It changes nothing that runs. Each node joins it by holding the group's UUID as its `studio.group`; a node of another group is refused, and a node leaves or joins later through `set_studio_fields`. Answers as `transact` does, the new group's UUID under `added_groups`."
    )]
    async fn add_group(
        &self,
        peer: Peer<RoleServer>,
        Parameters(AddGroupToolArguments {
            stream,
            title,
            description,
            colour,
            nodes,
            mode,
            author,
        }): Parameters<AddGroupToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::AddGroup {
            stream,
            title,
            description,
            colour,
            nodes,
        };
        self.one_graph_transaction("add_group", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Set a labelled group's title, description or colour, as one transaction; a field omitted is kept. Its nodes stay as they are."
    )]
    async fn set_group_fields(
        &self,
        peer: Peer<RoleServer>,
        Parameters(SetGroupFieldsToolArguments {
            stream,
            group,
            title,
            description,
            colour,
            mode,
            author,
        }): Parameters<SetGroupFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::SetGroupFields {
            stream,
            group,
            title,
            description,
            colour,
        };
        self.one_graph_transaction("set_group_fields", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Take out a labelled group, as one transaction: its nodes stay, ungrouped. Undo brings the group back with every node that belonged to it."
    )]
    async fn remove_group(
        &self,
        peer: Peer<RoleServer>,
        Parameters(RemoveGroupToolArguments {
            stream,
            group,
            mode,
            author,
        }): Parameters<RemoveGroupToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::RemoveGroup { stream, group };
        self.one_graph_transaction("remove_group", vec![operation], mode, author, &peer)
            .await
    }

    #[tool(
        description = "Refresh, as one transaction, every fragment of the project's loaded streams whose graph generator, or a generator nested in it, is defined in a module one of `changed_files` holds: each is run again with the promoted settings its fragment keeps, as `import_graph_generator` refreshes it, each node keeping its UUID by name. Answers as `transact` does; with no such fragment, it answers no operation and records no transaction. A generator that raises refuses the whole refresh with its traceback, nothing changed."
    )]
    async fn refresh_fragments(
        &self,
        peer: Peer<RoleServer>,
        Parameters(RefreshFragmentsToolArguments {
            project_directory,
            changed_files,
            mode,
            author,
        }): Parameters<RefreshFragmentsToolArguments>,
    ) -> ToolCallAnswer {
        if !project_directory.is_absolute() {
            return Err(format!(
                "refresh_fragments failed: `project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            ));
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let refreshes = tokio::task::spawn_blocking(move || {
            the_refresh_of_each_fragment_a_changed_module_defines(
                operations_on_the_loaded_streams.as_ref(),
                &project_repositories,
                &project_directory,
                &changed_files,
            )
        })
        .await
        .map_err(|join_failure| {
            format!("refresh_fragments failed: the refresh did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("refresh_fragments failed: {refusal}"))?;
        if refreshes.is_empty() {
            return serialized_tool_result(
                "refresh_fragments",
                &GraphTransactionToolResult {
                    mode: mode.unwrap_or_default().on_the_wire().to_owned(),
                    transaction: None,
                    commit: None,
                    operations: Vec::new(),
                    added_nodes: Vec::new(),
                    connected_links: Vec::new(),
                    added_groups: Vec::new(),
                },
            );
        }
        self.one_graph_transaction("refresh_fragments", refreshes, mode, author, &peer)
            .await
    }

    #[tool(
        description = "Undo the latest transaction by `author` in the project that is not undone yet, applying its inverse as a new transaction: its graph edits to the running streams and their graph documents, its asset changes to the project. Only the author's own transactions are undone, so two authors' interleaved edits are each undone by their own author. An edit to a stream not loaded is applied to its graph document alone, and the stream of a stream document the undo takes out is unloaded and forgotten. Refused when the author has nothing left to undo, or when what it would undo no longer applies — a node another author removed since, or a stream document it would take out that was edited after it was written. Answers the transaction applied, with `undoes` naming the one it reversed."
    )]
    async fn undo(
        &self,
        peer: Peer<RoleServer>,
        Parameters(UndoOrRedoToolArguments {
            project_directory,
            author,
        }): Parameters<UndoOrRedoToolArguments>,
    ) -> ToolCallAnswer {
        self.an_undo_or_redo(
            "undo",
            project_directory,
            the_author_of_a_change(author, &peer),
            ProjectRepository::undo_by,
        )
        .await
    }

    #[tool(
        description = "Redo what `author`'s latest undo in the project reversed, as a new transaction; a new edit by the author after the undo leaves nothing to redo. Answers the transaction applied, with `redoes` naming the one it reapplied."
    )]
    async fn redo(
        &self,
        peer: Peer<RoleServer>,
        Parameters(UndoOrRedoToolArguments {
            project_directory,
            author,
        }): Parameters<UndoOrRedoToolArguments>,
    ) -> ToolCallAnswer {
        self.an_undo_or_redo(
            "redo",
            project_directory,
            the_author_of_a_change(author, &peer),
            ProjectRepository::redo_by,
        )
        .await
    }
}

impl LocalApiMcpServerHandler {
    pub(crate) async fn one_graph_transaction(
        &self,
        tool_name: &'static str,
        operations: Vec<StreamGraphOperationToolArgument>,
        mode: Option<GraphEditMode>,
        author: Option<ProjectTransactionAuthorToolArgument>,
        peer: &Peer<RoleServer>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, peer);
        let mode = mode.unwrap_or_default();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let async_runtime = tokio::runtime::Handle::current();
        let applied = tokio::task::spawn_blocking(move || {
            GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            }
            .apply(operations, Vec::new(), mode, &author)
        })
        .await
        .map_err(|join_failure| {
            format!("{tool_name} failed: the transaction did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("{tool_name} failed: {refusal}"))?;
        serialized_tool_result(tool_name, &applied)
    }

    async fn an_undo_or_redo(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        reversal_of: fn(
            &ProjectRepository,
            &ProjectTransactionAuthor,
        ) -> Result<
            ProjectTransactionReversal,
            streamlib_project_repository::ProjectRepositoryRefusal,
        >,
    ) -> ToolCallAnswer {
        if !project_directory.is_absolute() {
            return Err(format!(
                "{tool_name} failed: `project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            ));
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let async_runtime = tokio::runtime::Handle::current();
        let applied = tokio::task::spawn_blocking(move || {
            GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            }
            .apply_a_reversal(&project_directory, &author, reversal_of)
        })
        .await
        .map_err(|join_failure| {
            format!("{tool_name} failed: the transaction did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("{tool_name} failed: {refusal}"))?;
        serialized_tool_result(tool_name, &applied)
    }
}

/// One loaded stream a transaction edits: its operations, where it lives, and its graph as the
/// transaction has staged it.
struct StreamATransactionEdits {
    stream_operations: Arc<dyn RuntimeOperations>,
    stream_location: Option<StreamLocation>,
    staged_graph: StreamGraphDocument,
}

/// One operation of a graph transaction on a loaded stream: as a caller names it, or prepared
/// whole from the graph document while the project's one writer is held.
pub(crate) enum GraphTransactionStep {
    /// An operation as a caller names it, read against the graph as the transaction stages it.
    Requested(StreamGraphOperationToolArgument),
    /// An operation on the loaded stream `stream`, with everything its inverse needs.
    Prepared {
        stream: String,
        graph_operation: StreamGraphOperation,
    },
}

impl GraphTransactionStep {
    fn stream(&self) -> &str {
        match self {
            Self::Requested(request) => request.stream(),
            Self::Prepared { stream, .. } => stream,
        }
    }
}

/// What a graph transaction works through on its blocking task.
pub(crate) struct GraphTransactionInFlight<'transaction> {
    pub(crate) operations_on_the_loaded_streams:
        &'transaction dyn OperationsOnTheStreamsLoadedInThisRuntime,
    pub(crate) project_repositories: &'transaction ProjectRepositoriesOpenInThisRuntime,
    pub(crate) async_runtime: &'transaction tokio::runtime::Handle,
}

impl GraphTransactionInFlight<'_> {
    /// Apply `requests` as one transaction by `author`, after `project_operations_first`: a
    /// dependency pin they move is the pin a `use_dependency_stream` of the same transaction
    /// reads its stream at.
    pub(crate) fn apply(
        &self,
        requests: Vec<StreamGraphOperationToolArgument>,
        project_operations_first: Vec<ProjectOperation>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
    ) -> Result<GraphTransactionToolResult, String> {
        if requests.is_empty() {
            return Err("a transaction holds at least one operation".to_owned());
        }
        if mode == GraphEditMode::Live && !project_operations_first.is_empty() {
            return Err(
                "a change to the project beside its graphs is made only in a committed \
                 transaction; send `mode` `commit`"
                    .to_owned(),
            );
        }
        let stream_locations = self.the_location_of_each_loaded_stream();
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for request in &requests {
            if !stream_ids_in_order
                .iter()
                .any(|seen| seen == request.stream())
            {
                stream_ids_in_order.push(request.stream().to_owned());
            }
        }
        let project_repository =
            match mode {
                GraphEditMode::Live => None,
                GraphEditMode::Commit => Some(self.the_one_project_the_streams_belong_to(
                    &stream_ids_in_order,
                    &stream_locations,
                )?),
            };
        let mut project_repository_held = project_repository.as_ref().map(|held| held.lock());
        self.apply_the_steps(
            requests
                .into_iter()
                .map(GraphTransactionStep::Requested)
                .collect(),
            project_operations_first,
            mode,
            author,
            &stream_locations,
            project_repository_held.as_deref_mut(),
        )
    }

    /// Apply, as one committed transaction by `author`, what `prepare` reads from the project
    /// at `project_directory` and the stream id of each of its loaded streams by stream asset,
    /// its one writer held from the read to the commit so no other transaction lands between.
    pub(crate) fn apply_what_the_held_project_prepares(
        &self,
        project_directory: &std::path::Path,
        author: &ProjectTransactionAuthor,
        prepare: impl FnOnce(
            &ProjectRepository,
            &HashMap<Uuid, String>,
        )
            -> Result<(Vec<GraphTransactionStep>, Vec<ProjectOperation>), String>,
    ) -> Result<GraphTransactionToolResult, String> {
        let project_repository = self
            .project_repositories
            .the_project_repository_at(project_directory)
            .map_err(|refusal| refusal.to_string())?;
        let mut project_repository_held = project_repository.lock();
        let stream_locations = self.the_location_of_each_loaded_stream();
        let project = project_repository_held.project_file().project;
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let (steps, project_operations_first) =
            prepare(&project_repository_held, &stream_id_of_each_stream_asset)?;
        self.apply_the_steps(
            steps,
            project_operations_first,
            GraphEditMode::Commit,
            author,
            &stream_locations,
            Some(&mut *project_repository_held),
        )
    }

    /// The location of each loaded stream by its stream id, `None` for one of no project.
    fn the_location_of_each_loaded_stream(&self) -> HashMap<String, Option<StreamLocation>> {
        self.operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .map(|listing| (listing.stream_id, listing.stream_location))
            .collect()
    }

    /// The stream id of each of the project `project`'s loaded streams, by its stream asset.
    pub(crate) fn the_stream_id_of_each_loaded_stream_asset(
        &self,
        project: Uuid,
    ) -> HashMap<Uuid, String> {
        self.operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                (stream_location.project == project)
                    .then_some((stream_location.stream_asset, listing.stream_id))
            })
            .collect()
    }

    /// Apply `steps` as one transaction by `author`, after `project_operations_first`, to the
    /// project `project_repository_held` holds when the transaction is committed.
    fn apply_the_steps(
        &self,
        steps: Vec<GraphTransactionStep>,
        project_operations_first: Vec<ProjectOperation>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        stream_locations: &HashMap<String, Option<StreamLocation>>,
        project_repository_held: Option<&mut ProjectRepository>,
    ) -> Result<GraphTransactionToolResult, String> {
        let pin_moved_in_this_transaction: HashMap<Uuid, String> =
            project_operations_first
                .iter()
                .filter_map(|project_operation| match project_operation {
                    ProjectOperation::Dependency(
                        ProjectDependencyOperation::MoveDependencyPin { project, to, .. },
                    ) => Some((*project, to.clone())),
                    _ => None,
                })
                .collect();
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for step in &steps {
            if !stream_ids_in_order.iter().any(|seen| seen == step.stream()) {
                stream_ids_in_order.push(step.stream().to_owned());
            }
        }

        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        for stream_id in &stream_ids_in_order {
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)
                .map_err(|refusal| refusal.to_string())?;
            let stream_location = stream_locations.get(stream_id).cloned().flatten();
            let staged_graph = match (&project_repository_held, &stream_location) {
                (Some(project_repository), Some(stream_location)) => {
                    if stream_location.project != project_repository.project_file().project {
                        return Err(format!(
                            "a transaction edits the streams of one project, and `{stream_id}` \
                             is of the project {}, not {}",
                            stream_location.project,
                            project_repository.project_file().project
                        ));
                    }
                    project_repository
                        .stream_graph_document(stream_location.stream_asset)
                        .map_err(|refusal| refusal.to_string())?
                }
                _ => StreamGraphDocument::from_value(
                    self.async_runtime
                        .block_on(stream_operations.to_json_async())
                        .map_err(|refusal| refusal.to_string())?,
                )?,
            };
            streams.insert(
                stream_id.clone(),
                StreamATransactionEdits {
                    stream_operations,
                    stream_location,
                    staged_graph,
                },
            );
        }

        let mut node_by_label: HashMap<String, Uuid> = HashMap::new();
        let mut labels_of_added_nodes: HashMap<Uuid, String> = HashMap::new();
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        let project_repository_read_during_the_requests = project_repository_held.as_deref();
        for step in steps {
            let stream_id = step.stream().to_owned();
            let applied = streams
                .get_mut(&stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))
                .and_then(|stream| {
                    let request = match step {
                        GraphTransactionStep::Requested(request) => request,
                        GraphTransactionStep::Prepared {
                            graph_operation, ..
                        } => {
                            return self
                                .async_runtime
                                .block_on(
                                    stream
                                        .stream_operations
                                        .apply_a_stream_graph_operation_live(graph_operation),
                                )
                                .map_err(|refusal| refusal.to_string());
                        }
                    };
                    let label = match &request {
                        StreamGraphOperationToolArgument::AddNode { label, .. }
                        | StreamGraphOperationToolArgument::ImportGraphGenerator {
                            label, ..
                        }
                        | StreamGraphOperationToolArgument::UseDependencyStream {
                            label, ..
                        } => label.clone(),
                        _ => None,
                    };
                    let run_the_graph_generator =
                        |generator: &str,
                         fragment_name: Option<&str>,
                         promoted_settings: &Map<String, Value>| {
                            self.operations_on_the_loaded_streams
                                .run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
                                    &stream_id,
                                    generator,
                                    fragment_name,
                                    promoted_settings,
                                )
                                .map_err(|refusal| refusal.to_string())
                        };
                    let read_the_dependency_stream = |project: Uuid, dependency_stream: Uuid| {
                        let project_repository = project_repository_read_during_the_requests
                            .ok_or_else(|| {
                                "a dependency's stream is read from the project's own record of \
                                 it; use it with `mode` `commit`"
                                    .to_owned()
                            })?;
                        the_dependency_stream_at_its_pin(
                            project_repository,
                            project,
                            dependency_stream,
                            pin_moved_in_this_transaction.get(&project).map(String::as_str),
                        )
                    };
                    let operation = graph_operation_for_the_request(
                        request,
                        &stream.staged_graph,
                        &mut node_by_label,
                        &run_the_graph_generator,
                        &read_the_dependency_stream,
                    )?;
                    let applied = self
                        .async_runtime
                        .block_on(
                            stream
                                .stream_operations
                                .apply_a_stream_graph_operation_live(operation),
                        )
                        .map_err(|refusal| refusal.to_string())?;
                    if let (Some(label), StreamGraphOperation::AddNode { node, .. }) =
                        (label, &applied)
                    {
                        labels_of_added_nodes.insert(node.id, label);
                    }
                    Ok(applied)
                });
            let applied = match applied {
                Ok(applied) => applied,
                Err(refusal) => {
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(refusal);
                }
            };
            let stream = streams
                .get_mut(&stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
            let staged = stream.staged_graph.apply(&applied);
            applied_live.push((stream_id.clone(), applied));
            if let Err(why) = staged {
                self.reverse_what_was_applied_live(&streams, applied_live);
                return Err(format!(
                    "stream `{stream_id}`'s graph document does not hold what its running graph \
                     does: {why}"
                ));
            }
        }

        let mut refreshes_of_streams_not_loaded = Vec::new();
        if let Some(project_repository) = project_repository_held.as_deref() {
            match self.refresh_each_fragment_holding_an_edited_stream(
                project_repository,
                &project_operations_first,
                &mut streams,
                &mut applied_live,
            ) {
                Ok(refreshes) => refreshes_of_streams_not_loaded = refreshes,
                Err(refusal) => {
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(refusal);
                }
            }
        }

        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(project_repository) => {
                let project_operations = project_operations_first
                    .into_iter()
                    .chain(
                        applied_live
                            .iter()
                            .filter_map(|(stream_id, graph_operation)| {
                                let stream_location =
                                    streams.get(stream_id)?.stream_location.as_ref()?;
                                Some(ProjectOperation::StreamGraph(
                                    StreamGraphOperationOnAStream {
                                        stream: stream_location.stream_asset,
                                        graph_operation: graph_operation.clone(),
                                    },
                                ))
                            }),
                    )
                    .chain(refreshes_of_streams_not_loaded)
                    .collect();
                match project_repository.apply_project_operations(
                    author,
                    project_operations,
                    ProjectTransactionLineage::NewEdit,
                ) {
                    Ok(applied) => {
                        self.project_repositories
                            .announce_applied_transaction(project_repository, applied.clone());
                        (
                            Some(applied.transaction.transaction.to_string()),
                            Some(applied.commit),
                        )
                    }
                    Err(refusal) => {
                        self.reverse_what_was_applied_live(&streams, applied_live);
                        return Err(refusal.to_string());
                    }
                }
            }
        };

        Ok(self.the_answer_of_a_graph_transaction(
            mode,
            transaction,
            commit,
            &streams,
            applied_live,
            &labels_of_added_nodes,
        ))
    }

    /// Refresh each top-level fragment of the project's streams that holds one of its own
    /// streams the transaction changes, `{"stream": {"asset"}}`, from that stream as the
    /// transaction leaves it, each holder once every stream it holds has settled: a loaded
    /// stream's live and staged beside the transaction's edits, answering one not loaded as the
    /// project operations that refresh its graph document. A stream the transaction changes is
    /// one a step edited live or one of `project_operations_first` edits in its graph document.
    /// Refused when the holders the change reaches hold one another in a cycle.
    fn refresh_each_fragment_holding_an_edited_stream(
        &self,
        project_repository: &ProjectRepository,
        project_operations_first: &[ProjectOperation],
        streams: &mut BTreeMap<String, StreamATransactionEdits>,
        applied_live: &mut Vec<(String, StreamGraphOperation)>,
    ) -> Result<Vec<ProjectOperation>, String> {
        let project = project_repository.project_file().project;
        let loaded_stream_id_of_each_stream_asset: HashMap<Uuid, (String, StreamLocation)> = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                (stream_location.project == project).then_some((
                    stream_location.stream_asset,
                    (listing.stream_id, stream_location),
                ))
            })
            .collect();

        let mut changed_stream_assets: BTreeSet<Uuid> = BTreeSet::new();
        for (stream_id, _) in applied_live.iter() {
            if let Some(stream_location) = streams
                .get(stream_id)
                .and_then(|stream| stream.stream_location.as_ref())
            {
                changed_stream_assets.insert(stream_location.stream_asset);
            }
        }
        let mut documents_of_streams_not_loaded: BTreeMap<Uuid, StreamGraphDocument> =
            BTreeMap::new();
        for project_operation in project_operations_first {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = project_operation
            else {
                continue;
            };
            if loaded_stream_id_of_each_stream_asset.contains_key(stream) {
                continue;
            }
            let document = match documents_of_streams_not_loaded.entry(*stream) {
                std::collections::btree_map::Entry::Occupied(read) => read.into_mut(),
                std::collections::btree_map::Entry::Vacant(unread) => unread.insert(
                    project_repository
                        .stream_graph_document(*stream)
                        .map_err(|refusal| refusal.to_string())?,
                ),
            };
            document.apply(graph_operation)?;
            changed_stream_assets.insert(*stream);
        }
        if changed_stream_assets.is_empty() {
            return Ok(Vec::new());
        }

        let mut documents_of_loaded_streams_outside_the_transaction: BTreeMap<
            Uuid,
            StreamGraphDocument,
        > = BTreeMap::new();
        let stream_id_in_the_transaction_of =
            |stream_asset: Uuid, streams: &BTreeMap<String, StreamATransactionEdits>| {
                streams
                    .iter()
                    .find(|(_, stream)| {
                        stream
                            .stream_location
                            .as_ref()
                            .map(|location| location.stream_asset)
                            == Some(stream_asset)
                    })
                    .map(|(stream_id, _)| stream_id.clone())
            };
        let mut local_streams_each_stream_holds: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for asset in project_repository
            .assets()
            .map_err(|refusal| refusal.to_string())?
            .into_iter()
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)
        {
            let stream_asset = asset.asset;
            let document = match stream_id_in_the_transaction_of(stream_asset, streams) {
                Some(stream_id) => streams[&stream_id].staged_graph.clone(),
                None => match documents_of_streams_not_loaded.get(&stream_asset) {
                    Some(document) => document.clone(),
                    None => match project_repository.stream_graph_document(stream_asset) {
                        Ok(document) => document,
                        Err(refusal) => {
                            tracing::warn!(
                                %stream_asset,
                                %refusal,
                                "a stream whose graph document cannot be read is not refreshed \
                                 from the streams it holds"
                            );
                            continue;
                        }
                    },
                },
            };
            let held_local_streams: BTreeSet<Uuid> = document
                .fragment_nodes()
                .into_iter()
                .filter(|fragment_node| fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE)
                .filter_map(|fragment_node| {
                    let held_stream = fragment_node
                        .config
                        .get(STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY)?
                        .as_object()?;
                    if held_stream.len() != 1 {
                        return None;
                    }
                    Uuid::parse_str(held_stream.get("asset")?.as_str()?).ok()
                })
                .filter(|held| *held != stream_asset)
                .collect();
            local_streams_each_stream_holds.insert(stream_asset, held_local_streams);
            if loaded_stream_id_of_each_stream_asset.contains_key(&stream_asset) {
                documents_of_loaded_streams_outside_the_transaction.insert(stream_asset, document);
            } else {
                documents_of_streams_not_loaded
                    .entry(stream_asset)
                    .or_insert(document);
            }
        }

        let mut holders_of_each_stream: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for (holder, held_streams) in &local_streams_each_stream_holds {
            for held in held_streams {
                holders_of_each_stream
                    .entry(*held)
                    .or_default()
                    .insert(*holder);
            }
        }
        let mut streams_the_change_reaches: BTreeSet<Uuid> = changed_stream_assets.clone();
        let mut unvisited: Vec<Uuid> = changed_stream_assets.iter().copied().collect();
        while let Some(stream_asset) = unvisited.pop() {
            for holder in holders_of_each_stream
                .get(&stream_asset)
                .into_iter()
                .flatten()
            {
                if streams_the_change_reaches.insert(*holder) {
                    unvisited.push(*holder);
                }
            }
        }
        let mut unsettled_held_streams_of_each_reached_stream: BTreeMap<Uuid, usize> =
            streams_the_change_reaches
                .iter()
                .map(|stream_asset| {
                    let unsettled = local_streams_each_stream_holds
                        .get(stream_asset)
                        .into_iter()
                        .flatten()
                        .filter(|held| streams_the_change_reaches.contains(held))
                        .count();
                    (*stream_asset, unsettled)
                })
                .collect();
        let mut settled_in_order: Vec<Uuid> = Vec::new();
        let mut ready: Vec<Uuid> = unsettled_held_streams_of_each_reached_stream
            .iter()
            .filter(|(_, unsettled)| **unsettled == 0)
            .map(|(stream_asset, _)| *stream_asset)
            .collect();
        while let Some(stream_asset) = ready.pop() {
            settled_in_order.push(stream_asset);
            for holder in holders_of_each_stream
                .get(&stream_asset)
                .into_iter()
                .flatten()
            {
                if let Some(unsettled) =
                    unsettled_held_streams_of_each_reached_stream.get_mut(holder)
                {
                    *unsettled -= 1;
                    if *unsettled == 0 {
                        ready.push(*holder);
                    }
                }
            }
        }
        if settled_in_order.len() != streams_the_change_reaches.len() {
            let streams_in_a_cycle: Vec<String> = unsettled_held_streams_of_each_reached_stream
                .iter()
                .filter(|(_, unsettled)| **unsettled > 0)
                .map(|(stream_asset, _)| stream_asset.to_string())
                .collect();
            return Err(format!(
                "the streams {} hold one another as fragments in a cycle, so no order refreshes \
                 each from the streams it holds",
                streams_in_a_cycle.join(", ")
            ));
        }

        let mut refreshes_of_streams_not_loaded = Vec::new();
        for holder in settled_in_order {
            let held_changed_streams: Vec<Uuid> = local_streams_each_stream_holds
                .get(&holder)
                .into_iter()
                .flatten()
                .filter(|held| changed_stream_assets.contains(held))
                .copied()
                .collect();
            let mut holder_changed = false;
            for held in held_changed_streams {
                let held_document = match stream_id_in_the_transaction_of(held, streams) {
                    Some(stream_id) => streams[&stream_id].staged_graph.to_value(),
                    None => documents_of_streams_not_loaded
                        .get(&held)
                        .or_else(|| documents_of_loaded_streams_outside_the_transaction.get(&held))
                        .ok_or_else(|| format!("stream `{held}`'s graph document was not read"))?
                        .to_value(),
                };
                let held_stream = json!({ "asset": held.to_string() });
                match loaded_stream_id_of_each_stream_asset.get(&holder) {
                    Some((stream_id, stream_location)) => {
                        let holder_document = match streams.get(stream_id) {
                            Some(stream) => &stream.staged_graph,
                            None => documents_of_loaded_streams_outside_the_transaction
                                .get(&holder)
                                .ok_or_else(|| {
                                    format!("stream `{holder}`'s graph document was not read")
                                })?,
                        };
                        let refreshes = the_refresh_of_each_fragment_holding_a_stream(
                            holder_document,
                            &held_stream,
                            &held_document,
                        )?;
                        if refreshes.is_empty() {
                            continue;
                        }
                        if !streams.contains_key(stream_id) {
                            let staged_graph = documents_of_loaded_streams_outside_the_transaction
                                .remove(&holder)
                                .ok_or_else(|| {
                                    format!("stream `{holder}`'s graph document was not read")
                                })?;
                            streams.insert(
                                stream_id.clone(),
                                StreamATransactionEdits {
                                    stream_operations: self
                                        .operations_on_the_loaded_streams
                                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                                        .map_err(|refusal| refusal.to_string())?,
                                    stream_location: Some(stream_location.clone()),
                                    staged_graph,
                                },
                            );
                        }
                        let stream = streams
                            .get_mut(stream_id)
                            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
                        for refresh in refreshes {
                            let applied = self
                                .async_runtime
                                .block_on(
                                    stream
                                        .stream_operations
                                        .apply_a_stream_graph_operation_live(refresh),
                                )
                                .map_err(|refusal| refusal.to_string())?;
                            stream.staged_graph.apply(&applied)?;
                            applied_live.push((stream_id.clone(), applied));
                        }
                        holder_changed = true;
                    }
                    None => {
                        let holder_document = documents_of_streams_not_loaded
                            .get_mut(&holder)
                            .ok_or_else(|| {
                                format!("stream `{holder}`'s graph document was not read")
                            })?;
                        let refreshes = the_refresh_of_each_fragment_holding_a_stream(
                            holder_document,
                            &held_stream,
                            &held_document,
                        )?;
                        for refresh in refreshes {
                            holder_document.apply(&refresh)?;
                            refreshes_of_streams_not_loaded.push(ProjectOperation::StreamGraph(
                                StreamGraphOperationOnAStream {
                                    stream: holder,
                                    graph_operation: refresh,
                                },
                            ));
                            holder_changed = true;
                        }
                    }
                }
            }
            if holder_changed {
                changed_stream_assets.insert(holder);
            }
        }
        Ok(refreshes_of_streams_not_loaded)
    }

    /// The project every stream of `stream_ids` belongs to, its one writer, refused for a
    /// stream with no project and for streams of two projects.
    fn the_one_project_the_streams_belong_to(
        &self,
        stream_ids: &[String],
        stream_locations: &HashMap<String, Option<StreamLocation>>,
    ) -> Result<streamlib_project_repository::ProjectRepositoryOneWriterAtATime, String> {
        let mut project: Option<&StreamLocation> = None;
        for stream_id in stream_ids {
            let stream_location = match stream_locations.get(stream_id) {
                None => {
                    return Err(self
                        .operations_on_the_loaded_streams
                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                        .err()
                        .map_or_else(
                            || format!("stream `{stream_id}` is not loaded"),
                            |refusal| refusal.to_string(),
                        ));
                }
                Some(None) => {
                    return Err(format!(
                        "stream `{stream_id}` belongs to no project, so no graph document keeps \
                         its edits; edit it with `mode` `live`"
                    ));
                }
                Some(Some(stream_location)) => stream_location,
            };
            match project {
                Some(first) if first.project != stream_location.project => {
                    return Err(format!(
                        "a transaction edits the streams of one project, and `{stream_id}` is \
                         of the project {} while `{}` is of {}",
                        stream_location.project, stream_ids[0], first.project
                    ));
                }
                _ => project = Some(stream_location),
            }
        }
        let project = project.ok_or("a transaction holds at least one operation")?;
        self.project_repositories
            .the_project_repository_at(&project.project_directory)
            .map_err(|refusal| refusal.to_string())
    }

    /// Apply the undo or redo `reversal_of` names for `author` in the project at
    /// `project_directory`: its graph edits to the running streams, then the whole of it to
    /// the project, recorded as a new transaction.
    fn apply_a_reversal(
        &self,
        project_directory: &std::path::Path,
        author: &ProjectTransactionAuthor,
        reversal_of: fn(
            &ProjectRepository,
            &ProjectTransactionAuthor,
        ) -> Result<
            ProjectTransactionReversal,
            streamlib_project_repository::ProjectRepositoryRefusal,
        >,
    ) -> Result<
        streamlib_runtime_client_contract::local_api_wire_contract::ProjectTransactionOnTheWire,
        String,
    > {
        let project_repository = self
            .project_repositories
            .the_project_repository_at(project_directory)
            .map_err(|refusal| refusal.to_string())?;
        let mut project_repository = project_repository.lock();
        let reversal =
            reversal_of(&project_repository, author).map_err(|refusal| refusal.to_string())?;
        let project = project_repository.project_file().project;
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        for operation in &reversal.operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = operation
            else {
                continue;
            };
            let Some(stream_id) = stream_id_of_each_stream_asset.get(stream) else {
                continue;
            };
            let mut applied_to_the_running_stream = || {
                let stream_operations = match streams.get(stream_id) {
                    Some(held) => Arc::clone(&held.stream_operations),
                    None => {
                        let stream_operations = self
                            .operations_on_the_loaded_streams
                            .runtime_operations_of_the_stream_a_call_names(stream_id)
                            .map_err(|refusal| refusal.to_string())?;
                        streams.insert(
                            stream_id.clone(),
                            StreamATransactionEdits {
                                stream_operations: Arc::clone(&stream_operations),
                                stream_location: None,
                                staged_graph: StreamGraphDocument::from_value(Value::Object(
                                    Map::new(),
                                ))?,
                            },
                        );
                        stream_operations
                    }
                };
                self.async_runtime
                    .block_on(
                        stream_operations
                            .apply_a_stream_graph_operation_live(graph_operation.clone()),
                    )
                    .map_err(|refusal| refusal.to_string())
                    .map(|applied| (stream_id.clone(), applied))
            };
            let applied = applied_to_the_running_stream();
            match applied {
                Ok(applied) => applied_live.push(applied),
                Err(refusal) => {
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(refusal);
                }
            }
        }
        match project_repository.apply_project_operations(
            author,
            reversal.operations,
            reversal.lineage,
        ) {
            Ok(applied) => {
                self.project_repositories
                    .announce_applied_transaction(&project_repository, applied.clone());
                drop(project_repository);
                self.forget_the_stream_of_each_stream_asset_taken_out(project, &applied)?;
                Ok(project_transaction_on_the_wire(applied))
            }
            Err(refusal) => {
                self.reverse_what_was_applied_live(&streams, applied_live);
                Err(refusal.to_string())
            }
        }
    }

    /// Unload and forget the stream of each stream asset `applied` took out of the project
    /// `project`, so no stream runs a graph document the project no longer holds.
    fn forget_the_stream_of_each_stream_asset_taken_out(
        &self,
        project: Uuid,
        applied: &streamlib_project_repository::AppliedProjectTransaction,
    ) -> Result<(), String> {
        for operation in &applied.transaction.operations {
            let stream_asset = match operation {
                ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::DeleteStreamDocument { stream, .. },
                ) => *stream,
                ProjectOperation::Asset(ProjectAssetOperation::DeleteAsset { asset })
                    if asset.kind == ProjectAssetKind::Stream =>
                {
                    asset.asset
                }
                _ => continue,
            };
            self.operations_on_the_loaded_streams
                .forget_the_stream_of_a_deleted_stream_asset(project, stream_asset)
                .map_err(|not_forgotten| {
                    format!(
                        "the transaction {} applied, and the stream of the stream asset \
                         {stream_asset} it took out was not unloaded: {not_forgotten}",
                        applied.transaction.transaction
                    )
                })?;
        }
        Ok(())
    }

    /// Apply the inverse of each operation of `applied_live`, newest first, to the running
    /// stream it was applied to, so a refused transaction leaves the streams as they were.
    fn reverse_what_was_applied_live(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, StreamGraphOperation)>,
    ) {
        for (stream_id, applied) in applied_live.into_iter().rev() {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            if let Err(not_reversed) = self.async_runtime.block_on(
                stream
                    .stream_operations
                    .apply_a_stream_graph_operation_live(applied.inverse()),
            ) {
                tracing::error!(
                    stream = %stream_id,
                    %not_reversed,
                    "could not reverse part of a refused graph transaction on its running stream"
                );
            }
        }
    }

    fn the_answer_of_a_graph_transaction(
        &self,
        mode: GraphEditMode,
        transaction: Option<String>,
        commit: Option<String>,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, StreamGraphOperation)>,
        labels_of_added_nodes: &HashMap<Uuid, String>,
    ) -> GraphTransactionToolResult {
        let mut added_nodes = Vec::new();
        let mut added_groups = Vec::new();
        let mut connected_links = Vec::new();
        let mut graph_render_of_each_stream: HashMap<String, Option<Value>> = HashMap::new();
        let mut operations = Vec::with_capacity(applied_live.len());
        for (stream_id, applied) in applied_live {
            match &applied {
                StreamGraphOperation::AddNode { node, .. } => {
                    added_nodes.push(GraphTransactionAddedNode {
                        stream: stream_id.clone(),
                        node: node.id.to_string(),
                        name: node.name.clone(),
                        label: labels_of_added_nodes.get(&node.id).cloned(),
                    });
                }
                StreamGraphOperation::AddGroup { group, .. } => {
                    added_groups.push(GraphTransactionAddedGroup {
                        stream: stream_id.clone(),
                        group: group.id.to_string(),
                        title: group.fields.title.clone(),
                    });
                }
                StreamGraphOperation::Connect { link } => {
                    let graph_render = graph_render_of_each_stream
                        .entry(stream_id.clone())
                        .or_insert_with(|| {
                            let stream = streams.get(&stream_id)?;
                            match self
                                .async_runtime
                                .block_on(stream.stream_operations.to_json_async())
                            {
                                Ok(graph_render) => Some(graph_render),
                                Err(unreadable) => {
                                    tracing::warn!(
                                        stream = %stream_id,
                                        "a link was connected and its stream's graph could not \
                                         be read to say what state it is in: {unreadable}"
                                    );
                                    None
                                }
                            }
                        });
                    let (state, error_reason) = graph_render
                        .as_ref()
                        .map(|graph_render| how_the_graph_reads_the_link(graph_render, link))
                        .unwrap_or_default();
                    connected_links.push(GraphTransactionConnectedLink {
                        stream: stream_id.clone(),
                        source: link_end_on_the_wire(&link.source),
                        target: link_end_on_the_wire(&link.target),
                        state,
                        error_reason,
                    });
                }
                _ => {}
            }
            let mut operation = serde_json::to_value(&applied).unwrap_or_default();
            if let Value::Object(operation) = &mut operation {
                operation.insert("stream".to_owned(), Value::String(stream_id));
            }
            operations.push(operation);
        }
        GraphTransactionToolResult {
            mode: mode.on_the_wire().to_owned(),
            transaction,
            commit,
            operations,
            added_nodes,
            connected_links,
            added_groups,
        }
    }
}

/// A run of one of a stream's graph generators, `(generator, fragment name, promoted settings)`,
/// answering the fragment node it placed or the refusal.
type GraphGeneratorRunOfTheStream<'stream> =
    dyn Fn(&str, Option<&str>, &Map<String, Value>) -> Result<Value, String> + 'stream;

/// A read of a depended-on project's stream at its pin, `(project, stream asset)`, answering
/// its graph document and the name its file gives it.
type DependencyStreamReadAtItsPin<'repository> =
    dyn Fn(Uuid, Uuid) -> Result<(Value, String), String> + 'repository;

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
fn graph_operation_for_the_request(
    request: StreamGraphOperationToolArgument,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    run_the_graph_generator: &GraphGeneratorRunOfTheStream<'_>,
    read_the_dependency_stream: &DependencyStreamReadAtItsPin<'_>,
) -> Result<StreamGraphOperation, String> {
    let node_named = |node: &str, node_by_label: &HashMap<String, Uuid>| -> Result<Uuid, String> {
        node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())
            .ok_or_else(|| {
                format!(
                    "`{node}` is neither a node's UUID nor the `as` of a node an earlier \
                     `add_node` of this transaction adds"
                )
            })
    };
    let link_between = |source: &GraphLinkEndToolArgument,
                        target: &GraphLinkEndToolArgument,
                        node_by_label: &HashMap<String, Uuid>|
     -> Result<StreamGraphDocumentLink, String> {
        Ok(StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: node_named(&source.node, node_by_label)?,
                port: source.port.clone(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: node_named(&target.node, node_by_label)?,
                port: target.port.clone(),
            },
        })
    };
    let label_the_node = |label: Option<String>,
                          node: Uuid,
                          node_by_label: &mut HashMap<String, Uuid>|
     -> Result<(), String> {
        match label {
            Some(label) if node_by_label.insert(label.clone(), node).is_some() => Err(format!(
                "two operations of this transaction both name a node `as` `{label}`"
            )),
            _ => Ok(()),
        }
    };
    match request {
        StreamGraphOperationToolArgument::ImportGraphGenerator {
            generator,
            name,
            config,
            label,
            ..
        } => {
            let placed: StreamGraphFragmentNodeAGeneratorRunPlaced = serde_json::from_value(
                run_the_graph_generator(&generator, name.as_deref(), &config.unwrap_or_default())?,
            )
            .map_err(|unread| {
                format!("graph generator `{generator}`'s run placed no fragment node: {unread}")
            })?;
            let operation = match staged_graph.node_named(&placed.name) {
                None => {
                    let node = Uuid::new_v4();
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        None,
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, node, node_by_label)?;
                    StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: node,
                            name: placed.name,
                            node_type: placed.generator,
                            config: placed.config,
                            pins: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Map::new(),
                            fragment: Some(Box::new(fragment)),
                        },
                        links: Vec::new(),
                    }
                }
                Some(StreamGraphDocumentNode {
                    id,
                    node_type,
                    config,
                    fragment: Some(previous_fragment),
                    ..
                }) => {
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        Some(&previous_fragment),
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, id, node_by_label)?;
                    StreamGraphOperation::RefreshFragment {
                        node: id,
                        from: StreamGraphFragmentPlacement {
                            generator: node_type,
                            config,
                            fragment: *previous_fragment,
                        },
                        to: StreamGraphFragmentPlacement {
                            generator: placed.generator,
                            config: placed.config,
                            fragment,
                        },
                    }
                }
                Some(StreamGraphDocumentNode { id, node_type, .. }) => {
                    return Err(format!(
                        "node {id} `{}` is a `{node_type}`, no fragment, so no graph \
                         generator's run refreshes it; give the import another `name`",
                        placed.name
                    ));
                }
            };
            Ok(operation)
        }
        StreamGraphOperationToolArgument::AddNode {
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            label,
            ..
        } => {
            let node = Uuid::new_v4();
            label_the_node(label, node, node_by_label)?;
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name: name.unwrap_or_default(),
                    node_type,
                    config: Value::Object(config.unwrap_or_default()),
                    pins: pins.unwrap_or_default(),
                    exposed_config_fields: exposed_config_fields.unwrap_or_default(),
                    studio: studio.unwrap_or_default(),
                    fragment: None,
                },
                links: Vec::new(),
            })
        }
        StreamGraphOperationToolArgument::RemoveNode { node, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RemoveNode {
                node: staged_graph.node(node)?,
                links: staged_graph.links_touching(node),
            })
        }
        StreamGraphOperationToolArgument::Connect { source, target, .. } => {
            Ok(StreamGraphOperation::Connect {
                link: link_between(&source, &target, node_by_label)?,
            })
        }
        StreamGraphOperationToolArgument::Disconnect { source, target, .. } => {
            Ok(StreamGraphOperation::Disconnect {
                link: link_between(&source, &target, node_by_label)?,
            })
        }
        StreamGraphOperationToolArgument::SetNodeConfig { node, config, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetNodeConfig {
                node,
                from: staged_graph.node(node)?.config,
                to: Value::Object(config),
            })
        }
        StreamGraphOperationToolArgument::SetPinValue {
            node, input, value, ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetPinValue {
                node,
                from: staged_graph.node(node)?.pins.get(&input).cloned(),
                input,
                to: value.filter(|value| !value.is_null()),
            })
        }
        StreamGraphOperationToolArgument::RenameNode { node, name, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RenameNode {
                node,
                from: staged_graph.node(node)?.name,
                to: name,
            })
        }
        StreamGraphOperationToolArgument::SetStudioFields { node, studio, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetStudioFields {
                node,
                from: staged_graph.node(node)?.studio,
                to: studio,
            })
        }
        StreamGraphOperationToolArgument::SetExposedConfigFields {
            node,
            exposed_config_fields,
            ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetExposedConfigFields {
                node,
                from: staged_graph.node(node)?.exposed_config_fields,
                to: exposed_config_fields,
            })
        }
        StreamGraphOperationToolArgument::AddGroup {
            title,
            description,
            colour,
            nodes,
            ..
        } => {
            if title.is_empty() {
                return Err("a group needs a `title`".to_owned());
            }
            let nodes = nodes
                .iter()
                .map(|node| node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, String>>()?;
            Ok(StreamGraphOperation::AddGroup {
                group: StreamGraphDocumentGroup {
                    id: Uuid::new_v4(),
                    fields: StreamGraphGroupFields {
                        title,
                        description: description.unwrap_or_default(),
                        colour: colour.unwrap_or_default(),
                    },
                },
                nodes,
            })
        }
        StreamGraphOperationToolArgument::SetGroupFields {
            group,
            title,
            description,
            colour,
            ..
        } => {
            let from = staged_graph.group(the_group_named(&group)?)?;
            if title.as_deref() == Some("") {
                return Err("a group needs a `title`".to_owned());
            }
            let to = StreamGraphGroupFields {
                title: title.unwrap_or_else(|| from.fields.title.clone()),
                description: description.unwrap_or_else(|| from.fields.description.clone()),
                colour: colour.unwrap_or_else(|| from.fields.colour.clone()),
            };
            Ok(StreamGraphOperation::SetGroupFields {
                group: from.id,
                from: from.fields,
                to,
            })
        }
        StreamGraphOperationToolArgument::UseDependencyStream {
            project,
            dependency_stream,
            name,
            label,
            ..
        } => {
            let project = Uuid::parse_str(&project)
                .map_err(|_| format!("`{project}` is no project's UUID"))?;
            let dependency_stream = Uuid::parse_str(&dependency_stream)
                .map_err(|_| format!("`{dependency_stream}` is no stream asset's UUID"))?;
            let (graph_document, stream_name) =
                read_the_dependency_stream(project, dependency_stream)?;
            let mut fragment_run = a_graph_document_as_a_fragment_run(&graph_document)?;
            asset_references_made_references_into_the_dependency(&mut fragment_run, project);
            let name = name.unwrap_or(stream_name);
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the fragment name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            let config = json!({ STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: {
                "asset": dependency_stream.to_string(),
                "from": project.to_string(),
            }});
            match staged_graph.node_named(&name) {
                None => {
                    let node = Uuid::new_v4();
                    label_the_node(label, node, node_by_label)?;
                    Ok(StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: node,
                            name,
                            node_type: STREAM_FRAGMENT_NODE_TYPE.to_owned(),
                            config,
                            pins: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Map::new(),
                            fragment: Some(Box::new(
                                fragment_with_each_node_uuid_kept_by_its_name(
                                    &fragment_run,
                                    None,
                                    &mut Uuid::new_v4,
                                )?,
                            )),
                        },
                        links: Vec::new(),
                    })
                }
                Some(StreamGraphDocumentNode {
                    id,
                    node_type,
                    config: held_config,
                    fragment: Some(previous_fragment),
                    ..
                }) if node_type == STREAM_FRAGMENT_NODE_TYPE && held_config == config => {
                    label_the_node(label, id, node_by_label)?;
                    Ok(StreamGraphOperation::RefreshFragment {
                        node: id,
                        from: StreamGraphFragmentPlacement {
                            generator: node_type.clone(),
                            config: held_config,
                            fragment: (*previous_fragment).clone(),
                        },
                        to: StreamGraphFragmentPlacement {
                            generator: node_type,
                            config,
                            fragment: fragment_with_each_node_uuid_kept_by_its_name(
                                &fragment_run,
                                Some(&previous_fragment),
                                &mut Uuid::new_v4,
                            )?,
                        },
                    })
                }
                Some(StreamGraphDocumentNode { id, node_type, .. }) => Err(format!(
                    "node {id} `{name}` is a `{node_type}`, not this dependency's stream, so \
                     it is not refreshed from it; give the stream another `name`"
                )),
            }
        }
        StreamGraphOperationToolArgument::RemoveGroup { group, .. } => {
            let group = staged_graph.group(the_group_named(&group)?)?;
            Ok(StreamGraphOperation::RemoveGroup {
                nodes: staged_graph.nodes_of_the_group(group.id),
                group,
            })
        }
    }
}

/// The graph document of the depended-on `project`'s stream asset `dependency_stream` and the
/// name its file gives it, read at `pin_moved_to` when the transaction moves the pin, else at
/// the pin `tatolab-project.json` holds.
fn the_dependency_stream_at_its_pin(
    project_repository: &ProjectRepository,
    project: Uuid,
    dependency_stream: Uuid,
    pin_moved_to: Option<&str>,
) -> Result<(Value, String), String> {
    let commit = match pin_moved_to {
        Some(commit) => commit.to_owned(),
        None => {
            project_repository
                .dependency(project)
                .map_err(|refusal| refusal.to_string())?
                .revision
        }
    };
    let (path, graph_document) = project_repository
        .dependency_stream_at_commit(project, dependency_stream, &commit)
        .map_err(|refusal| refusal.to_string())?;
    let file_name = path.file_name();
    let stream_name = file_name
        .strip_suffix(GRAPH_DOCUMENT_FILE_SUFFIX)
        .unwrap_or(file_name)
        .to_owned();
    Ok((graph_document, stream_name))
}

fn the_group_named(group: &str) -> Result<Uuid, String> {
    Uuid::parse_str(group).map_err(|_| {
        format!("`{group}` is no group's UUID; `graph` lists each group's under `groups`")
    })
}

/// The module a project's Python file at `changed_file` is imported as: its path from the
/// project directory, `/` read as `.`, a package's `__init__.py` as the package; `None` for a
/// file that is no `.py` or lies outside the project.
fn the_module_a_project_file_is_imported_as(
    project_directory: &std::path::Path,
    changed_file: &std::path::Path,
) -> Option<String> {
    let relative_file = if changed_file.is_absolute() {
        changed_file.strip_prefix(project_directory).ok()?
    } else {
        changed_file
    };
    if relative_file.extension()? != "py" {
        return None;
    }
    let mut module_parts: Vec<String> = relative_file
        .with_extension("")
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if module_parts.last().is_some_and(|last| last == "__init__") {
        module_parts.pop();
    }
    (!module_parts.is_empty()).then(|| module_parts.join("."))
}

/// An `import_graph_generator` for each top-level fragment of each loaded stream of the project
/// at `project_directory` whose generator, or one nested in it, a module of `changed_files`
/// defines, run with the promoted settings the fragment keeps.
fn the_refresh_of_each_fragment_a_changed_module_defines(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    project_directory: &std::path::Path,
    changed_files: &[PathBuf],
) -> Result<Vec<StreamGraphOperationToolArgument>, String> {
    let changed_modules: std::collections::HashSet<String> = changed_files
        .iter()
        .filter_map(|changed_file| {
            the_module_a_project_file_is_imported_as(project_directory, changed_file)
        })
        .collect();
    if changed_modules.is_empty() {
        return Ok(Vec::new());
    }
    let project_repository = project_repositories
        .the_project_repository_at(project_directory)
        .map_err(|refusal| refusal.to_string())?;
    let project_repository = project_repository.lock();
    let project = project_repository.project_file().project;
    let mut refreshes = Vec::new();
    for listing in operations_on_the_loaded_streams.list_streams() {
        let Some(stream_location) = listing.stream_location.filter(|stream_location| {
            stream_location.project == project && listing.node_count.is_some()
        }) else {
            continue;
        };
        let staged_graph = project_repository
            .stream_graph_document(stream_location.stream_asset)
            .map_err(|refusal| refusal.to_string())?;
        for fragment_node in staged_graph.fragment_nodes() {
            if fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE {
                continue;
            }
            let defined_in_a_changed_module = fragment_node
                .graph_generators_a_refresh_runs()
                .iter()
                .any(|generator| {
                    generator
                        .split_once(':')
                        .is_some_and(|(module, _)| changed_modules.contains(module))
                });
            if !defined_in_a_changed_module {
                continue;
            }
            refreshes.push(StreamGraphOperationToolArgument::ImportGraphGenerator {
                stream: listing.stream_id.clone(),
                generator: fragment_node.node_type,
                name: Some(fragment_node.name),
                config: match fragment_node.config {
                    Value::Object(promoted_settings) => Some(promoted_settings),
                    _ => None,
                },
                label: None,
            });
        }
    }
    Ok(refreshes)
}

/// The fragment node one graph generator run places, as the run entry prints it: its nodes
/// and links by name.
#[derive(Deserialize)]
struct StreamGraphFragmentNodeAGeneratorRunPlaced {
    name: String,
    #[serde(rename = "type")]
    generator: String,
    config: Value,
    fragment: Value,
}

fn link_end_on_the_wire(end: &StreamGraphDocumentLinkEnd) -> GraphLinkEndOnTheWire {
    GraphLinkEndOnTheWire {
        node: end.node.to_string(),
        port: end.port.clone(),
    }
}

/// The `state` and `error_reason` the `graph` render `graph_render` gives `link`, whose ends it
/// names by node name.
fn how_the_graph_reads_the_link(
    graph_render: &Value,
    link: &StreamGraphDocumentLink,
) -> (Option<String>, Option<String>) {
    let node_name_of = |node: Uuid| {
        graph_render["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| rendered["id"].as_str() == Some(node.to_string().as_str()))
            .and_then(|rendered| rendered["name"].as_str())
            .map(str::to_owned)
    };
    let (Some(source_name), Some(target_name)) = (
        node_name_of(link.source.node),
        node_name_of(link.target.node),
    ) else {
        return (None, None);
    };
    let end_is = |end: &Value, name: &str, port: &str| {
        end["node"].as_str() == Some(name) && end["port"].as_str() == Some(port)
    };
    let Some(rendered_link) =
        graph_render["links"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| {
                end_is(&rendered["source"], &source_name, &link.source.port)
                    && end_is(&rendered["target"], &target_name, &link.target.port)
            })
    else {
        return (None, None);
    };
    let text_at = |key: &str| rendered_link[key].as_str().map(str::to_owned);
    (text_at("state"), text_at("error_reason"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::the_module_a_project_file_is_imported_as;

    #[test]
    fn a_project_file_is_imported_as_its_dotted_path_and_a_package_as_its_folder() {
        let project_directory = Path::new("/rig");
        for (changed_file, module) in [
            ("generators/looks.py", Some("generators.looks")),
            ("/rig/generators/looks.py", Some("generators.looks")),
            ("generators/__init__.py", Some("generators")),
            ("stream.py", Some("stream")),
            ("streams/main.graph.json", None),
            ("/elsewhere/looks.py", None),
        ] {
            assert_eq!(
                the_module_a_project_file_is_imported_as(
                    project_directory,
                    Path::new(changed_file)
                )
                .as_deref(),
                module,
                "{changed_file}"
            );
        }
    }
}
