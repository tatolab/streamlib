// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations that edit a stream's graph, and of undo and redo.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib_project_repository::{
    EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION, GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GROUP_ARGUMENT_DESCRIPTION, GROUP_COLOUR_ARGUMENT_DESCRIPTION,
    GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION, GROUP_TITLE_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION, NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION,
    NOTE_ARGUMENT_DESCRIPTION, NOTE_POSITION_ARGUMENT_DESCRIPTION, NOTE_SIZE_ARGUMENT_DESCRIPTION,
    NOTE_TEXT_ARGUMENT_DESCRIPTION, StreamGraphAction, StreamGraphActionLinkEnd, StudioCanvasPair,
};
use uuid::Uuid;

use super::{
    AUTHOR_ARGUMENT_DESCRIPTION, GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION, GraphEditMode,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
};

/// `transact`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactRequest {
    /// The operations, applied in order and whole or not at all.
    #[schemars(
        description = "The operations, applied in order and whole or not at all, each tagged by its `operation` — `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group`, `add_note`, `set_note_fields`, `remove_note`, `use_dependency_stream`, `collapse_selection_into_subgraph` or `unlock_subgraph` — with the arguments that operation's own tool takes. Every `stream` they name is a stream of one project."
    )]
    pub operations: Vec<StreamGraphAction>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `add_node`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddNodeRequest {
    /// The stream asset the node is added to.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node's type by its import path.
    #[serde(rename = "type")]
    #[schemars(
        description = "The node's type by its import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`, as `graph` reports it and `streamlib://node-catalog` lists it."
    )]
    pub node_type: String,
    /// The node's name.
    #[schemars(
        description = "The node's name, a label: cast to lowercase URL-safe (`Front Camera` becomes `front-camera`) and refused when another node has it. Omit it for the type's short name, cast, with the next free `-2`, `-3` …"
    )]
    pub name: Option<String>,
    /// The node's config.
    #[schemars(
        description = "The node's config, as the keys its type's schema declares. Omit for none. A `GraphInput`, `GraphOutput` or `SetVariable` whose config names no `port` or `variable` takes the next free `input1`, `output1` or `var1` on its level; a `GetVariable` naming no `variable` reads the variable of its level's last `SetVariable`. A `GraphInput` or `GraphOutput` naming no `family` takes the family of the first port linked to it, and runs nothing until one is."
    )]
    pub config: Option<Map<String, Value>>,
    /// The value on each unlinked input.
    #[schemars(description = "The value on each unlinked input, by input name.")]
    pub pins: Option<Map<String, Value>>,
    /// The config fields the node also takes as inputs.
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    pub exposed_config_fields: Option<Vec<String>>,
    /// What Tato Studio draws the node with.
    #[schemars(description = NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION)]
    pub studio: Option<Map<String, Value>>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// The params of an edit naming one link: `connect`'s, and `disconnect`'s.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneLinkGraphEditRequest {
    /// The stream asset the link is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The output port the link carries from.
    #[schemars(
        description = "The output port the link carries from: a node's UUID and its output port's name."
    )]
    pub source: StreamGraphActionLinkEnd,
    /// The input port the link carries into.
    #[schemars(
        description = "The input port the link carries into: a node's UUID and its input port's name."
    )]
    pub target: StreamGraphActionLinkEnd,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `remove_node`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneNodeGraphEditRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_node_config`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetNodeConfigRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// The node's new config, as the keys its type's schema declares.
    #[schemars(
        description = "The node's new config, as the keys its type's schema declares; it replaces the old one whole."
    )]
    pub config: Map<String, Value>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_pin_value`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPinValueRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// The input's name, as `graph` lists it under the node's `ports.inputs`.
    #[schemars(
        description = "The input's name, as `graph` lists it under the node's `ports.inputs`."
    )]
    pub input: String,
    /// The value the input holds while no link feeds it.
    #[schemars(
        description = "The value the input holds while no link feeds it. Omit it, or send `null`, to take the value off."
    )]
    pub value: Option<Value>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `rename_node`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameNodeRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// The node's new name.
    #[schemars(
        description = "The node's new name: cast to lowercase URL-safe, refused when another node has it. The node keeps its UUID."
    )]
    pub name: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_studio_fields`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStudioFieldsRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// The node's studio fields.
    #[schemars(description = NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION)]
    pub studio: Map<String, Value>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `add_group`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddGroupRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The group's title.
    #[schemars(description = GROUP_TITLE_ARGUMENT_DESCRIPTION)]
    pub title: String,
    /// The group's description.
    #[schemars(description = GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION)]
    pub description: Option<String>,
    /// The group's colour.
    #[schemars(description = GROUP_COLOUR_ARGUMENT_DESCRIPTION)]
    pub colour: Option<String>,
    /// The nodes that join it, each by the UUID `graph` lists as its `id`.
    #[schemars(
        description = "The nodes that join it, each by the UUID `graph` lists as its `id`; a node of another group is refused."
    )]
    pub nodes: Vec<String>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_group_fields`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetGroupFieldsRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The group, by its UUID.
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    pub group: String,
    /// The group's new title.
    #[schemars(description = "The group's new title; kept when omitted.")]
    pub title: Option<String>,
    /// What the group's nodes do.
    #[schemars(description = "What the group's nodes do; kept when omitted, cleared by `\"\"`.")]
    pub description: Option<String>,
    /// The group's new colour.
    #[schemars(description = "The group's new colour; kept when omitted, cleared by `\"\"`.")]
    pub colour: Option<String>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `add_note`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddNoteRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The note's text.
    #[schemars(description = NOTE_TEXT_ARGUMENT_DESCRIPTION)]
    pub text: String,
    /// The note's position.
    #[schemars(description = NOTE_POSITION_ARGUMENT_DESCRIPTION, with = "[f64; 2]")]
    pub position: StudioCanvasPair,
    /// The note's size.
    #[schemars(description = NOTE_SIZE_ARGUMENT_DESCRIPTION, with = "[f64; 2]")]
    pub size: StudioCanvasPair,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_note_fields`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetNoteFieldsRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The note, by its UUID.
    #[schemars(description = NOTE_ARGUMENT_DESCRIPTION)]
    pub note: String,
    /// What the note says.
    #[schemars(description = "What the note says; kept when omitted.")]
    pub text: Option<String>,
    /// Where the note's top-left corner sits, `[x, y]`.
    #[schemars(
        description = "Where the note's top-left corner sits, `[x, y]`; kept when omitted.",
        with = "Option<[f64; 2]>"
    )]
    pub position: Option<StudioCanvasPair>,
    /// The note's `[width, height]`.
    #[schemars(
        description = "The note's `[width, height]`; kept when omitted.",
        with = "Option<[f64; 2]>"
    )]
    pub size: Option<StudioCanvasPair>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `remove_note`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveNoteRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The note, by its UUID.
    #[schemars(description = NOTE_ARGUMENT_DESCRIPTION)]
    pub note: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `remove_group`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveGroupRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The group, by its UUID.
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    pub group: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `collapse_selection_into_subgraph`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollapseSelectionIntoSubgraphRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The nodes collapsed, each by its UUID, all of the level `level` names and none of that level's own `GraphInput` or `GraphOutput` nodes.
    #[schemars(
        description = "The nodes collapsed, each by its UUID, all of the level `level` names and none of that level's own `GraphInput` or `GraphOutput` nodes."
    )]
    pub nodes: Vec<String>,
    /// The subgraph node's name, and the new stream's.
    #[schemars(
        description = "The subgraph node's name, cast to lowercase URL-safe, and the new stream's, `streams/<name>.graph.json`; refused when a node outside the selection or a stream of the project has it."
    )]
    pub name: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `unlock_subgraph`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnlockSubgraphRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The subgraph node, by its UUID.
    #[schemars(
        description = "The subgraph node, by its UUID: one holding a graph generator's output, or one placing a stream, whose nodes come back as a copy under new UUIDs."
    )]
    pub fragment: String,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `import_graph_generator`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportGraphGeneratorRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The graph generator, by its import path.
    #[schemars(description = IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION)]
    pub generator: String,
    /// The fragment node's name.
    #[schemars(description = IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION)]
    pub name: Option<String>,
    /// The generator's config.
    #[schemars(description = IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION)]
    pub config: Option<Map<String, Value>>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_exposed_config_fields`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetExposedConfigFieldsRequest {
    /// The stream asset the edit is in.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The subgraph the edit is inside, by the fragment nodes leading to it; the stream's own
    /// level when omitted.
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    pub level: Option<Vec<Uuid>>,
    /// The node, by its UUID.
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    pub node: String,
    /// The config fields the node also takes as inputs.
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    pub exposed_config_fields: Vec<String>,
    /// Whether the edit is kept.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `write_graph_input`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteGraphInputRequest {
    /// The stream id of a loaded stream.
    #[schemars(description = "The stream id of a loaded stream.")]
    pub stream: String,
    /// The Input's name, as a top-level `GraphInput`'s config declares it under `port`, cast as
    /// every exposed name is.
    #[schemars(
        description = "The Input's name, as a top-level `GraphInput`'s config declares it under `port`, cast as every exposed name is."
    )]
    pub input: String,
    /// The value, as the Input's family carries it.
    #[schemars(
        description = "The value, as the Input's family carries it: a Records Input takes a JSON object; an Array Input a number or a list of numbers, nested to any depth, as its `values` attribute of float64, or an object of such attributes by name; a Channels Input a number or a bool, as one control value, or `{\"samples\": [[...], ...], \"sample_rate\"}`, float32 channel by channel."
    )]
    pub value: serde_json::Value,
}

/// The params of `undo` and `redo`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UndoOrRedoRequest {
    /// The project, by absolute path.
    #[schemars(
        description = "The project, by absolute path: a folder holding `tatolab-project.json`."
    )]
    pub project_directory: PathBuf,
    /// Whose edits.
    #[schemars(
        description = "Whose edits: an undo or a redo reverses only its own author's. Omit it to be the agent the MCP client named in its `clientInfo`."
    )]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}
