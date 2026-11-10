// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The actions on a stream's graph every caller sends, and their argument descriptions.

#[cfg(feature = "mcp-tool-json-schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::StudioCanvasPair;

/// How a graph action's `stream` is described to its caller.
pub const GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION: &str = "The stream the edit is to, by its stream asset's UUID: the `asset` its graph document holds, which `list_streams` reports as its `location`'s `stream_asset`. A stream asset no loaded stream runs is refused naming the loaded ones.";
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

/// How a node's `studio` is described to its caller.
pub const NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION: &str = "What Tato Studio draws the node with, under the studio contract: `position` an `[x, y]` pair of numbers, `group` a labelled group's UUID, `comment` and `colour` strings, `collapsed` a bool, and anything else a client keeps under `metadata`, an object kept as given. A key of the wrong shape, or any other key, is refused by name.";
/// How a note action's `note` is described to its caller.
pub const NOTE_ARGUMENT_DESCRIPTION: &str =
    "The canvas note, by the UUID `graph` lists under `notes` and `add_note` answers.";
/// How a note's `text` is described to its caller.
pub const NOTE_TEXT_ARGUMENT_DESCRIPTION: &str =
    "What the note says, for the next person who reads the graph.";
/// How a note's `position` is described to its caller.
pub const NOTE_POSITION_ARGUMENT_DESCRIPTION: &str =
    "Where the note's top-left corner sits on the canvas, an `[x, y]` pair of numbers.";
/// How a note's `size` is described to its caller.
pub const NOTE_SIZE_ARGUMENT_DESCRIPTION: &str =
    "The note's width and height on the canvas, a `[width, height]` pair of numbers.";

/// One end of a link a graph action names: a node and one of its ports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp-tool-json-schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StreamGraphActionLinkEnd {
    #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
    pub node: String,
    #[cfg_attr(
        feature = "mcp-tool-json-schema",
        schemars(description = "The port's name, as `graph` lists it under the node's `ports`.")
    )]
    pub port: String,
}

/// One action on a stream's graph, as every caller sends it and history records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp-tool-json-schema", derive(JsonSchema))]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamGraphAction {
    /// Place a node, answering the UUID the runtime makes for it.
    AddNode {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[serde(rename = "type")]
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The node's type by its import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`, as `graph` reports it and `streamlib://node-catalog` lists it."
            )
        )]
        node_type: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The node's name, a label: cast to lowercase URL-safe and refused when another node has it. Omit it for the type's short name, cast, with the next free `-2`, `-3` …"
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The node's config, as the keys its type's schema declares.")
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Map<String, Value>>,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The value on each unlinked input, by input name.")
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pins: Option<Map<String, Value>>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_config_fields: Option<Vec<String>>,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        studio: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "A label later operations of the same transaction name the node by, before its UUID is known."
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Take a node out, with every link it has.
    RemoveNode {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
    },
    /// Link an output port to an input port.
    Connect {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The output port the link carries from.")
        )]
        source: StreamGraphActionLinkEnd,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The input port the link carries into.")
        )]
        target: StreamGraphActionLinkEnd,
    },
    /// Take out the link between two ports.
    Disconnect {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The output port the link carries from.")
        )]
        source: StreamGraphActionLinkEnd,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(description = "The input port the link carries into.")
        )]
        target: StreamGraphActionLinkEnd,
    },
    /// Replace a node's config whole.
    SetNodeConfig {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The node's new config, as the keys its type's schema declares."
            )
        )]
        config: Map<String, Value>,
    },
    /// Set the value on one of a node's inputs.
    SetPinValue {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The input's name, as `graph` lists it under the node's `ports.inputs`."
            )
        )]
        input: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The value the input holds while no link feeds it. Omit it, or send `null`, to take the value off."
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
    /// Give a node another name.
    RenameNode {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The node's new name: cast to lowercase URL-safe, refused when another node has it."
            )
        )]
        name: String,
    },
    /// Replace a node's studio fields whole.
    SetStudioFields {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION
            )
        )]
        studio: Map<String, Value>,
    },
    /// Run one of the project's graph generators and place what it builds as a fragment, or
    /// refresh the fragment of that name with a fresh run.
    ImportGraphGenerator {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION))]
        generator: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Map<String, Value>>,
        #[serde(rename = "as")]
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Replace the config fields a node takes as inputs whole.
    SetExposedConfigFields {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION))]
        node: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION))]
        exposed_config_fields: Vec<String>,
    },
    /// Make a labelled group of nodes of the stream's own level.
    AddGroup {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION))]
        title: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<String>,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The nodes that join it, each as `node` names one; a node of another group is refused."
            )
        )]
        nodes: Vec<String>,
    },
    /// Set a labelled group's title, description or colour.
    SetGroupFields {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_ARGUMENT_DESCRIPTION))]
        group: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<String>,
    },
    /// Place a stream of an imported project as a fragment node referencing it.
    UseDependencyStream {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION))]
        project: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION))]
        dependency_stream: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(rename = "as")]
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "A label later operations of the same transaction name the fragment node by, before its UUID is known."
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Move nodes of the stream's own level, and the links between them, into one new
    /// authored fragment, each link that crossed the selection carried through a `GraphInput`
    /// or `GraphOutput` inside it.
    CollapseSelectionIntoSubgraph {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The nodes collapsed, each as `node` names one, all of the stream's own level and none of its own `GraphInput` or `GraphOutput` nodes."
            )
        )]
        nodes: Vec<String>,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The subgraph node's name, lowercase URL-safe, refused when a node outside the selection has it."
            )
        )]
        name: String,
        #[serde(rename = "as")]
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "A label later operations of the same transaction name the subgraph node by, before its UUID is known."
            )
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Replace a fragment node of the stream's own level with the nodes and links it places,
    /// as plain nodes no refresh reaches.
    UnlockGeneratorFragment {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(
            feature = "mcp-tool-json-schema",
            schemars(
                description = "The fragment node, by its UUID: a graph generator's or a collapsed selection's, never a placed stream's."
            )
        )]
        fragment: String,
    },
    /// Place a canvas note on the stream's graph, answering the UUID the runtime makes for it.
    AddNote {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_TEXT_ARGUMENT_DESCRIPTION))]
        text: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_POSITION_ARGUMENT_DESCRIPTION, with = "[f64; 2]"))]
        position: StudioCanvasPair,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_SIZE_ARGUMENT_DESCRIPTION, with = "[f64; 2]"))]
        size: StudioCanvasPair,
    },
    /// Set a canvas note's text, position or size.
    SetNoteFields {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_ARGUMENT_DESCRIPTION))]
        note: String,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_TEXT_ARGUMENT_DESCRIPTION))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_POSITION_ARGUMENT_DESCRIPTION, with = "Option<[f64; 2]>"))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<StudioCanvasPair>,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_SIZE_ARGUMENT_DESCRIPTION, with = "Option<[f64; 2]>"))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<StudioCanvasPair>,
    },
    /// Take a canvas note off the stream's graph.
    RemoveNote {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = NOTE_ARGUMENT_DESCRIPTION))]
        note: String,
    },
    /// Take out a labelled group, its nodes left ungrouped.
    RemoveGroup {
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION))]
        stream: Uuid,
        #[cfg_attr(feature = "mcp-tool-json-schema", schemars(description = GROUP_ARGUMENT_DESCRIPTION))]
        group: String,
    },
}

/// What a node's `exposed_config_fields` are, as every tool that takes them says.
pub const EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION: &str = "The config fields the node takes as inputs too, each a number or bool field of its config schema, shown as an input of the field's name. A link into one drives the field: a function node reads it at each evaluation, a process node takes each new value as a live config update. Unlinked, it reads the node's config.";

impl StreamGraphAction {
    /// The stream asset whose graph the action is to.
    pub fn stream(&self) -> Uuid {
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
            | Self::UseDependencyStream { stream, .. }
            | Self::CollapseSelectionIntoSubgraph { stream, .. }
            | Self::UnlockGeneratorFragment { stream, .. }
            | Self::AddNote { stream, .. }
            | Self::SetNoteFields { stream, .. }
            | Self::RemoveNote { stream, .. } => *stream,
        }
    }
}
