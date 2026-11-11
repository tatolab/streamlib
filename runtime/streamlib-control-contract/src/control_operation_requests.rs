// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The params each registered operation takes, and the argument types several share.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib_project_repository::{
    EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION, StreamGraphAction, StreamGraphActionLinkEnd,
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
        description = "The operations, applied in order and whole or not at all, each tagged by its `operation` — `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group`, `add_note`, `set_note_fields`, `remove_note`, `use_dependency_stream`, `collapse_selection_into_subgraph` or `unlock_generator_fragment` — with the arguments that operation's own tool takes. Every `stream` they name is a stream of one project."
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
        description = "The node's config, as the keys its type's schema declares. Omit for none."
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
