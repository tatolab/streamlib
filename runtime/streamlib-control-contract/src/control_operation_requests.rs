// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params each registered operation takes, and the argument types several share.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib_project_repository::{
    DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION, DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION,
    DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION, EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION, GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION, GROUP_ARGUMENT_DESCRIPTION,
    GROUP_COLOUR_ARGUMENT_DESCRIPTION, GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION,
    GROUP_TITLE_ARGUMENT_DESCRIPTION, IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION, NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION,
    NOTE_ARGUMENT_DESCRIPTION, NOTE_POSITION_ARGUMENT_DESCRIPTION, NOTE_SIZE_ARGUMENT_DESCRIPTION,
    NOTE_TEXT_ARGUMENT_DESCRIPTION, ProjectAssetKind, StreamGraphAction, StreamGraphActionLinkEnd,
    StudioCanvasPair,
};
use uuid::Uuid;

/// What a graph edit's `mode` argument says.
pub const GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION: &str = "`commit`, the default: the edit is applied to the running stream, written to the stream's graph document and recorded as one commit on the project's history, so it is kept on the next load and undo reverses it. `live`: applied to the running stream alone, gone on the next load, never in history. Send a continuous gesture as one transaction when it ends.";

/// What a change's `author` argument says.
pub const AUTHOR_ARGUMENT_DESCRIPTION: &str = "Who the change is by, as history records it and undo follows it. Omit it to be the agent the MCP client named in its `clientInfo`.";

/// What a change's `idempotency_key` argument says.
pub const IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION: &str = "A key the caller mints once per user gesture, such as a slider drag committed on release. Sent again under a key the project's history holds, the action applies nothing and answers the transaction the key first applied. Omit it to apply the action each time it is sent. A `live` edit is never recorded, so its key is not kept.";

/// What `graph`'s `stream` argument says.
pub const GRAPH_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream whose graph to export, by the stream id `list_streams` reports. Omit it for every loaded stream's graph under the machine's name.";

/// What `project_directory` says where it names the project an operation acts on.
pub const PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str = "The project, by absolute path: a folder holding `tatolab-project.json` and its stream assets' graph documents.";

/// What `action` says where it names a stream action.
pub const STREAM_ACTION_ARGUMENT_DESCRIPTION: &str =
    "The stream action, by the action id a tool that queues one answered.";

/// Whether a graph edit is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphEditMode {
    /// Applied, written to the graph document and recorded in history.
    #[default]
    Commit,
    /// Applied to the running stream alone.
    Live,
}

impl GraphEditMode {
    /// The mode as the wire spells it.
    pub fn on_the_wire(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Live => "live",
        }
    }
}

/// Who a change is by, as a caller names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTransactionAuthorKindArgument {
    /// A person, through the studio or the CLI.
    Person,
    /// An agent.
    Agent,
}

/// A change's author as a caller names it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectTransactionAuthorArgument {
    /// Whether a person or an agent made the change.
    #[schemars(description = "`person` or `agent`.")]
    pub kind: ProjectTransactionAuthorKindArgument,
    /// The author's name.
    #[schemars(description = "The author's name as history shows it.")]
    pub name: String,
}

/// `describe_runtime`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeRuntimeRequest {}

/// `list_streams`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListStreamsRequest {}

/// `graph`'s params.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphRequest {
    /// The stream whose graph to export; every loaded stream's when absent.
    #[schemars(description = GRAPH_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Option<String>,
}

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

/// `discover`'s params.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverRequest {
    /// The contract version the client was built against; another version than the runtime's is
    /// refused naming both.
    pub contract_version: Option<u32>,
}

/// `subscribe_project_changes`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeProjectChangesRequest {
    /// The projects, by UUID, whose changes the connection is sent; refused when empty.
    pub projects: Vec<Uuid>,
}

/// `subscribe_stream_graph_changes`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeStreamGraphChangesRequest {
    /// The loaded streams, by stream id, whose graph changes the connection is sent; refused when
    /// empty or when one is not loaded.
    pub streams: Vec<String>,
}

/// `subscribe_project_log_records`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeProjectLogRecordsRequest {
    /// The projects, by absolute directory, whose new log records the connection is told of;
    /// refused when empty, or when one is relative or names no folder on this machine.
    pub project_directories: Vec<PathBuf>,
}

/// `subscribe_stream_action_changes`' params, which name nothing.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeStreamActionChangesRequest {}

/// The params of an operation on one project, named by its directory.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneProjectByItsDirectoryRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// The params of an operation on one stream action, named by its id.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneStreamActionRequest {
    /// The stream action's id.
    #[schemars(description = STREAM_ACTION_ARGUMENT_DESCRIPTION)]
    pub action: String,
}

/// `rename_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameProjectRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The project's new name.
    #[schemars(description = "The project's new name; never blank.")]
    pub name: String,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_project_description`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetProjectDescriptionRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// What the project is.
    #[schemars(description = "What the project is; `null` or blank clears it.")]
    pub description: Option<String>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_project_cover`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetProjectCoverRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The media asset the project is shown by.
    #[schemars(
        description = "The media asset the project is shown by, by the UUID `import_asset` or `list_assets` answered; `null` clears it."
    )]
    pub cover: Option<String>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `duplicate_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateProjectRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the duplicate goes.
    #[schemars(
        description = "Where the duplicate goes, by absolute path: a folder that does not exist yet, outside the project."
    )]
    pub into: PathBuf,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
}

/// What `asset_project_directory` says.
pub const ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project, by absolute path: a folder holding `tatolab-project.json`.";

/// What `asset` says.
pub const ASSET_ARGUMENT_DESCRIPTION: &str =
    "The asset, by the UUID `import_asset` or `list_assets` answered.";

/// What `stream_asset` says.
pub const STREAM_ASSET_ARGUMENT_DESCRIPTION: &str = "The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports as its location's `stream_asset`.";

/// What `stream_display_name` says.
pub const STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION: &str =
    "The name the stream is shown under, written in its graph document; refused when blank.";

/// What an asset is, as a caller names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetKindArgument {
    /// Video, audio or an image.
    Media,
    /// USD, glTF and other 3D scenes.
    Scene,
    /// Everything else a node reads: an FMU, a LUT, model weights, a font.
    Data,
    /// A stream's graph document; listed, never imported.
    Stream,
}

/// `list_assets`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAssetsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Only the assets of this kind.
    #[schemars(description = "Only the assets of this kind. Omit it for every asset.")]
    pub kind: Option<ProjectAssetKindArgument>,
}

/// `describe_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
}

/// `read_asset_preview`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadAssetPreviewRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
}

/// `import_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the bytes come from.
    #[schemars(
        description = "Where the bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    pub source: String,
    /// Where the asset sits in the project, `/`-separated and relative to the project directory,
    /// such as `media/intro.mp4`.
    #[schemars(
        description = "Where the asset sits in the project, `/`-separated and relative to the project directory, such as `media/intro.mp4`. Omit it to place the file in its kind's folder — `media/`, `scenes/` or `data/` — under `file_name`, numbered `<name>-2.<extension>`, `-3`, … past a name already taken there."
    )]
    pub asset_path: Option<String>,
    /// The asset's file name in its kind's folder when `asset_path` is omitted, such as
    /// `intro.mp4`.
    #[schemars(
        description = "The asset's file name in its kind's folder when `asset_path` is omitted, such as `intro.mp4`. Omit it for the source file's own name; an upload, which carries no name, needs it."
    )]
    pub file_name: Option<String>,
    /// `media`, `scene` or `data`.
    #[schemars(
        description = "`media`, `scene` or `data`. Omit it for the kind the file's extension implies, `data` when it implies none."
    )]
    pub kind: Option<ProjectAssetKindArgument>,
    /// The settings the import takes, kept in the asset's sidecar.
    #[schemars(description = "The settings the import takes, kept in the asset's sidecar.")]
    pub import_settings: Option<Map<String, Value>>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `relocate_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelocateAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// The asset's new path in the project, `/`-separated and relative to the project directory,
    /// such as `archive/opening.mp4`.
    #[schemars(
        description = "The asset's new path in the project, `/`-separated and relative to the project directory, such as `archive/opening.mp4`; its folders are made when missing."
    )]
    pub to_path: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `delete_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_asset_settings`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAssetSettingsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// The settings that replace the asset's import settings whole.
    #[schemars(description = "The settings that replace the asset's import settings whole.")]
    pub import_settings: Map<String, Value>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `replace_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// Where the new bytes come from.
    #[schemars(
        description = "Where the new bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    pub source: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `create_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateStreamRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the stream's graph document sits in the project, `/`-separated and relative to the
    /// project directory and ending `.graph.json`, such as `streams/preview.graph.json`.
    #[schemars(
        description = "Where the stream's graph document sits in the project, `/`-separated and relative to the project directory and ending `.graph.json`, such as `streams/preview.graph.json`."
    )]
    pub path: String,
    /// The name the stream is shown under, written in its graph document.
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    pub display_name: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `delete_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteStreamRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports
    /// as its location's `stream_asset`.
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    pub stream_asset: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_stream_display_name`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStreamDisplayNameRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports
    /// as its location's `stream_asset`.
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    pub stream_asset: String,
    /// The name the stream is shown under, written in its graph document.
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    pub display_name: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

impl From<ProjectAssetKindArgument> for ProjectAssetKind {
    fn from(kind: ProjectAssetKindArgument) -> Self {
        match kind {
            ProjectAssetKindArgument::Media => Self::Media,
            ProjectAssetKindArgument::Scene => Self::Scene,
            ProjectAssetKindArgument::Data => Self::Data,
            ProjectAssetKindArgument::Stream => Self::Stream,
        }
    }
}

/// What `importing_project_directory` says.
pub const IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project that imports, by absolute path: a folder holding `tatolab-project.json`.";

/// What `import_source` says.
pub const IMPORT_SOURCE_ARGUMENT_DESCRIPTION: &str = "Where the project is copied from, on this machine, by absolute path: its project folder, or a `.zip` of one, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder.";

/// `add_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the project is copied from, on this machine, by absolute path.
    #[schemars(description = IMPORT_SOURCE_ARGUMENT_DESCRIPTION)]
    pub source: String,
    /// The folder under `imports/` its copy takes.
    #[schemars(
        description = "The folder under `imports/` its copy takes; the imported project's name, lowercase with each run of other characters made `-`, when omitted."
    )]
    pub folder: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `update_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// Where to copy it from this time, as `add_dependency` takes it.
    #[schemars(
        description = "Where to copy it from this time, as `add_dependency` takes it; the `source` it was last copied from when omitted."
    )]
    pub source: Option<String>,
    /// Overwrite the files of its folder edited since its copy, which an update without it lists
    /// under `edited_files` and leaves as they are.
    #[schemars(
        description = "Overwrite the files of its folder edited since its copy, which an update without it lists under `edited_files` and leaves as they are."
    )]
    pub overwrite_edited_files: Option<bool>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `list_dependencies`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDependenciesRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Read each import's `source` and answer the version it holds now beside the copy's.
    #[schemars(
        description = "Read each import's `source` and answer the version it holds now beside the copy's."
    )]
    pub check_for_newer: Option<bool>,
}

/// `remove_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `use_dependency_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseDependencyStreamRequest {
    /// The stream the edit is to, by its stream asset's UUID.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: Uuid,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// The imported project's stream, by its stream asset's UUID, the `asset` its graph document
    /// under `imports/<folder>/` holds.
    #[schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION)]
    pub dependency_stream: String,
    /// The fragment node's name, cast to lowercase URL-safe.
    #[schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION)]
    pub name: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// What `project_file_path` says.
pub const PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION: &str = "The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`; refused outside the project, inside `.tatolab/`, or through a symbolic link.";

/// `list_project_files`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectFilesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// A folder of the project, `/`-separated and relative to the project directory, to list alone.
    #[schemars(
        description = "A folder of the project, `/`-separated and relative to the project directory, to list alone; the whole project when omitted."
    )]
    pub under: Option<String>,
}

/// `read_project_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadProjectFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`.
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    pub path: String,
}

/// `write_project_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteProjectFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`.
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    pub path: String,
    /// The file's whole new content as UTF-8 text.
    #[schemars(
        description = "The file's whole new content as UTF-8 text. Send this or `content_base64`, never both."
    )]
    pub content: Option<String>,
    /// The file's whole new content as standard base64 with padding, for bytes that are not text.
    #[schemars(
        description = "The file's whole new content as standard base64 with padding, for bytes that are not text. Send this or `content`, never both."
    )]
    pub content_base64: Option<String>,
    /// The `content_sha256` `read_project_file` or the last `write_project_file` answered for the
    /// file.
    #[schemars(
        description = "The `content_sha256` `read_project_file` or the last `write_project_file` answered for the file: the write is refused, and the file left as it is, when the file no longer holds those bytes. Omitted, the write goes over whatever the file holds."
    )]
    pub expected_sha256: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `create_node_file`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNodeFileRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The new Python file, `/`-separated and relative to the project directory, such as
    /// `nodes/edge_glow.py`.
    #[schemars(
        description = "The new Python file, `/`-separated and relative to the project directory, such as `nodes/edge_glow.py`: each folder and the file's name a Python identifier, the class named after the file (`EdgeGlow`). Refused where a file already is, outside the project, inside `.tatolab/` or `imports/`, or through a symbolic link."
    )]
    pub path: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `list_project_node_types`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectNodeTypesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `list_project_views`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectViewsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `write_studio_view_output`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteStudioViewOutputRequest {
    /// The stream id of the stream holding the view node.
    #[schemars(description = "The stream id of the stream holding the view node.")]
    pub stream: String,
    /// The view node's UUID, as `graph` lists it under `id`.
    #[schemars(description = "The view node's UUID, as `graph` lists it under `id`.")]
    pub node: String,
    /// The view node's output port the value is published on.
    #[schemars(description = "The view node's output port the value is published on.")]
    pub port: String,
    /// The value, as the port's family carries it.
    #[schemars(
        description = "The value, as the port's family carries it: a Records port takes a JSON object; an Array port a number or a list of numbers, nested to any depth, as its `values` attribute of float64, or an object of such attributes by name; a Channels port a number, as one control value, or `{\"samples\": [[...], ...], \"sample_rate\"}`, float32 channel by channel."
    )]
    pub value: serde_json::Value,
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

/// `import_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportProjectRequest {
    /// The `.zip` of a project on this machine, by absolute path, such as a GitHub archive
    /// downloaded first.
    #[schemars(
        description = "The `.zip` of a project on this machine, by absolute path, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder."
    )]
    pub source: String,
    /// Where the new project is made, by absolute path.
    #[schemars(
        description = "Where the new project is made, by absolute path: a folder that does not exist yet, its parent made when missing."
    )]
    pub project_directory: PathBuf,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
}

/// What every required `stream` argument says.
pub const STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream this call is about, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";

/// What a stream action's `stream` argument says.
pub const STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION: &str =
    "The stream, by the stream id `list_streams` reports: loaded, or recorded and not loaded.";

/// `logs`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsRequest {
    /// The project, by absolute path.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Only the records of this stream, by its stream id.
    #[schemars(
        description = "Only the records of this stream, by its stream id. Omit it for every record of the project, its loads' among them."
    )]
    pub stream: Option<String>,
    /// Only the records of this stream action, by its action id.
    #[schemars(description = "Only the records of this stream action, by its action id.")]
    pub action: Option<String>,
    /// Return the records after this opaque cursor, the previous result's `next_after`.
    #[schemars(
        description = "Return the records after this opaque cursor, the previous result's `next_after`. Omit it for the oldest the project's log holds."
    )]
    pub after: Option<String>,
    /// Return no record at or past this opaque cursor.
    #[schemars(
        description = "Return no record at or past this opaque cursor, a `next_after` a result or a `project_log_records_written` notification answered: a page that reaches it answers it as `next_after`. Omit it to read to the log's end."
    )]
    pub until: Option<String>,
    /// The most records to return.
    #[schemars(
        range(min = 1),
        description = "The most records to return. Defaults to 256; a larger value than 4096 is clamped to it."
    )]
    pub count: Option<u32>,
}

/// `set_stream_clock_scale`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStreamClockScaleRequest {
    /// The stream, by the stream id `list_streams` reports.
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: String,
    /// Stream seconds per machine second from now on.
    #[schemars(
        description = "Stream seconds per machine second from now on: 0.5 runs the stream at half speed, 1 at real time. A finite number greater than zero, at most 1."
    )]
    pub scale: f64,
}

/// `open_viewer`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenViewerRequest {
    /// The loaded stream this call is about, by the stream id `list_streams` and `graph` report.
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: String,
    /// The node's id, as `graph` lists it under `id`.
    #[schemars(
        description = "The node's id, as `graph` lists it under `id`; its name is a label, not its identity."
    )]
    pub node_id: String,
    /// The node's output port, as `graph` lists it under `ports.outputs`.
    #[schemars(description = "The node's output port, as `graph` lists it under `ports.outputs`.")]
    pub port: String,
    /// The process id of the viewer that imports the port's Image frames.
    #[schemars(
        description = "The process id of the viewer that imports the port's Image frames. On macOS the surface-sharing service admits only that process, a running process of the runtime's user, to new connections while the viewer stays open, and without it the answer names no `surface_share_address`; Linux admits whoever may open its socket."
    )]
    #[serde(default)]
    pub viewer_process_id: Option<u32>,
}

/// `close_viewer`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseViewerRequest {
    /// The viewer, by the id `open_viewer` answered.
    #[schemars(description = "The viewer, by the id `open_viewer` answered.")]
    pub viewer: String,
}

/// `list_samples`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSamplesRequest {}

/// `create_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectRequest {
    /// Where to create the project, by absolute path.
    #[schemars(
        description = "Where to create the project, by absolute path: created when missing, and holding none of the files a new project starts with. Omit it to create the project in a folder named `<adjective>-<noun>`, drawn at random, under `~/streams`."
    )]
    #[serde(default)]
    pub project_directory: Option<PathBuf>,
    /// The `name` of a sample project `list_samples` lists, to copy into the new project instead of
    /// the starter.
    #[schemars(
        description = "The `name` of a sample project `list_samples` lists, to copy into the new project instead of the starter. Omit it for the starter."
    )]
    #[serde(default)]
    pub sample: Option<String>,
}

/// `refresh_fragments`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshFragmentsRequest {
    /// The project, by absolute path.
    #[schemars(
        description = "The project, by absolute path: a folder holding `tatolab-project.json`."
    )]
    pub project_directory: PathBuf,
    /// The project's Python files that changed, each by its path relative to the project directory
    /// or absolute inside it, e.g.
    #[schemars(
        description = "The project's Python files that changed, each by its path relative to the project directory or absolute inside it, e.g. `generators/looks.py`; each names the module it is imported as."
    )]
    pub changed_files: Vec<PathBuf>,
    /// `commit`, the default.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
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
