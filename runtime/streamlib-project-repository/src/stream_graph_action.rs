// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The actions on a stream's graph every caller sends, and their argument descriptions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How a graph action's `stream` is described to its caller.
pub const GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream the edit is to, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";
/// How a graph action's `node` is described to its caller.
pub const GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION: &str = "The node, by the UUID `graph` lists as its `id` and `add_node` answers; in `transact`, also the `as` of a node an earlier `add_node` of the same transaction adds.";
/// How `import_graph_generator`'s `generator` is described to its caller.
pub const IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION: &str = "The graph generator by its import path, `<module>:<function>`, a `@graph_generator` function the stream's own interpreter imports from its project, e.g. `generators.looks:soft_look`.";
/// How `import_graph_generator`'s `name` is described to its caller.
pub const IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION: &str = "The fragment node's name, cast to lowercase URL-safe; the generator's own name when omitted. A top-level fragment of that name is refreshed, and any other node of that name refuses the import.";
/// How `import_graph_generator`'s `config` is described to its caller.
pub const IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION: &str = "The settings the generator promotes, by name, which it runs with and the fragment node keeps as its `config`. Omit for the generator's defaults.";
/// How a group action's `group` is described to its caller.
pub const GROUP_ARGUMENT_DESCRIPTION: &str =
    "The labelled group, by the UUID `graph` lists under `groups` and `add_group` answers.";
/// How a group's `title` is described to its caller.
pub const GROUP_TITLE_ARGUMENT_DESCRIPTION: &str =
    "The group's title, which Tato Studio draws on its frame.";
/// How a group's `description` is described to its caller.
pub const GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION: &str =
    "What the group's nodes do, for the next person who reads the graph.";
/// How a group's `colour` is described to its caller.
pub const GROUP_COLOUR_ARGUMENT_DESCRIPTION: &str =
    "The colour Tato Studio draws the group's frame with, e.g. `#3b82f6`.";
/// How `use_dependency_stream`'s `project` is described to its caller.
pub const DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION: &str = "The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists under `dependencies`.";
/// How `use_dependency_stream`'s `dependency_stream` is described to its caller.
pub const DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION: &str = "The imported project's stream, by its stream asset's UUID, the `asset` its graph document under `imports/<folder>/` holds.";
/// How `use_dependency_stream`'s `name` is described to its caller.
pub const DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION: &str = "The fragment node's name, cast to lowercase URL-safe; the stream's own name when omitted. Any node of that name already in the graph refuses the placement.";

/// One end of a link a graph action names: a node and one of its ports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamGraphActionLinkEnd {
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    #[schemars(description = "The port's name, as `graph` lists it under the node's `ports`.")]
    pub port: String,
}

/// One action on a stream's graph, as every caller sends it and history records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamGraphAction {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[schemars(description = "The node's config, as the keys its type's schema declares.")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Map<String, Value>>,
        #[schemars(description = "The value on each unlinked input, by input name.")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pins: Option<Map<String, Value>>,
        #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_config_fields: Option<Vec<String>>,
        #[schemars(
            description = "What the studio draws the node with: canvas position, group, comment, colour."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        studio: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the node by, before its UUID is known."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        source: StreamGraphActionLinkEnd,
        #[schemars(description = "The input port the link carries into.")]
        target: StreamGraphActionLinkEnd,
    },
    /// Take out the link between two ports.
    Disconnect {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = "The output port the link carries from.")]
        source: StreamGraphActionLinkEnd,
        #[schemars(description = "The input port the link carries into.")]
        target: StreamGraphActionLinkEnd,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[schemars(description = IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<String>,
    },
    /// Place a stream of an imported project as a fragment node referencing it.
    UseDependencyStream {
        #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
        stream: String,
        #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
        project: String,
        #[schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION)]
        dependency_stream: String,
        #[schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(rename = "as")]
        #[schemars(
            description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
pub const EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION: &str = "The config fields the node takes as inputs too, each a number or bool field of its config schema, shown as an input of the field's name. A link into one drives the field: a function node reads it at each evaluation, a process node takes each new value as a live config update. Unlinked, it reads the node's config.";

impl StreamGraphAction {
    /// The stream the action is to, as its caller named it.
    pub fn stream(&self) -> &str {
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
