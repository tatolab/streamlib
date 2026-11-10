// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph tools: every edit of a stream's graph a transaction, committed to the graph
//! document and the project's history or applied live to the running stream alone, its nodes
//! by UUID, and each author's undo and redo.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamLocation,
};
use streamlib_project_repository::{
    AppliedProjectTransaction, EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GROUP_ARGUMENT_DESCRIPTION, GROUP_COLOUR_ARGUMENT_DESCRIPTION,
    GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION, GROUP_TITLE_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION, ProjectAction, ProjectActionIdempotencyKey,
    ProjectOperation, ProjectRepositoriesOpenInThisRuntime, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionLineage,
    ProjectTransactionReversal, STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE,
    StreamGraphAction, StreamGraphActionLinkEnd, StreamGraphDocument, StreamGraphDocumentGroup,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphFragmentPlacement, StreamGraphGroupFields, StreamGraphOperation,
    StreamGraphOperationOnAStream, fragment_with_each_node_uuid_kept_by_its_name,
    graph_document_holds_a_stream_fragment, the_fragment_a_stream_fragment_places,
    the_stream_document_of_an_import,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphLinkEndOnTheWire, GraphTransactionAddedGroup, GraphTransactionAddedNode,
    GraphTransactionConnectedLink, GraphTransactionToolResult, ProjectActionRefusalOnTheWire,
    RefusedGraphLinkOnTheWire,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION,
    ProjectTransactionAuthorToolArgument,
    bring_the_loaded_streams_in_line_with_the_stream_documents_of, the_author_of_a_change,
    the_idempotency_key_a_caller_sent,
};
use crate::mcp_project_change_feed::{
    ProjectActionTheRuntimeRefused, project_transaction_on_the_wire,
};

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

/// The node, link and field `action` names, as its caller named them.
fn what_a_graph_action_names(action: &StreamGraphAction) -> WhatAGraphOperationNames {
    let names_a_node = |node: &str| WhatAGraphOperationNames {
        node: Some(node.to_owned()),
        ..WhatAGraphOperationNames::default()
    };
    match action {
        StreamGraphAction::RemoveNode { node, .. }
        | StreamGraphAction::SetNodeConfig { node, .. }
        | StreamGraphAction::RenameNode { node, .. }
        | StreamGraphAction::SetStudioFields { node, .. }
        | StreamGraphAction::SetExposedConfigFields { node, .. } => names_a_node(node),
        StreamGraphAction::SetPinValue { node, input, .. } => WhatAGraphOperationNames {
            field: Some(input.clone()),
            ..names_a_node(node)
        },
        StreamGraphAction::Connect { source, target, .. }
        | StreamGraphAction::Disconnect { source, target, .. } => WhatAGraphOperationNames {
            link: Some(RefusedGraphLinkOnTheWire {
                source: GraphLinkEndOnTheWire {
                    node: source.node.clone(),
                    port: source.port.clone(),
                },
                target: GraphLinkEndOnTheWire {
                    node: target.node.clone(),
                    port: target.port.clone(),
                },
            }),
            ..WhatAGraphOperationNames::default()
        },
        StreamGraphAction::AddNode { label, name, .. }
        | StreamGraphAction::ImportGraphGenerator { label, name, .. }
        | StreamGraphAction::UseDependencyStream { label, name, .. } => WhatAGraphOperationNames {
            node: label.clone().or_else(|| name.clone()),
            ..WhatAGraphOperationNames::default()
        },
        StreamGraphAction::AddGroup { .. }
        | StreamGraphAction::SetGroupFields { .. }
        | StreamGraphAction::RemoveGroup { .. } => WhatAGraphOperationNames::default(),
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct TransactToolArguments {
    #[schemars(
        description = "The operations, applied in order and whole or not at all, each tagged by its `operation` — `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group` or `use_dependency_stream` — with the arguments that operation's own tool takes. Every `stream` they name is a stream of one project."
    )]
    operations: Vec<StreamGraphAction>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OneNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OneLinkToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(
        description = "The output port the link carries from: a node's UUID and its output port's name."
    )]
    source: StreamGraphActionLinkEnd,
    #[schemars(
        description = "The input port the link carries into: a node's UUID and its input port's name."
    )]
    target: StreamGraphActionLinkEnd,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetNodeConfigToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetPinValueToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RenameNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetStudioFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetGroupFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    group: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ImportGraphGeneratorToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetExposedConfigFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    exposed_config_fields: Vec<String>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
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
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[tool_router(router = stream_graph_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Apply operations on the graphs of one project's loaded streams as one transaction by its `author`: applied in order, whole or not at all, to each running stream, and with `mode` `commit`, the default, written to each stream's graph document and recorded as one commit on the project's history, which `undo` reverses as one. Answers `mode`, the `transaction` and `commit` recording a committed edit, each operation as applied, `added_nodes` with each new node's UUID and the name it took, and `connected_links` with each new link's `state` — `pending`, `wired`, or `error` with its `error_reason`. Refused whole, in the runtime's own words, with nothing changed. A config value `{\"asset\": \"<uuid>\"}` must name an asset the project holds. An `add_node` may give its node an `as`, which later operations of the same transaction name the node by. The transaction is announced on the project's change feed, the resource `list_assets` names as `transactions_resource_uri`."
    )]
    async fn transact(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(TransactToolArguments {
            operations,
            mode,
            author,
            idempotency_key,
        }): Parameters<TransactToolArguments>,
    ) -> ToolCallAnswer {
        self.one_graph_transaction(
            "transact",
            operations,
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Run one of the project's `@graph_generator` functions in the stream's own interpreter and place what it builds in a loaded stream's graph as a fragment node, as one transaction: its generator as `type`, its promoted settings as `config`, and the nodes and links it built stored under `fragment`, each node with a UUID of its own. Run again under the name of a fragment the graph holds, it refreshes that fragment with the fresh run: each node it builds keeps the UUID of the node of the same name it built before, so links into the fragment and the graph's history hold. A generator that raises, refuses, or is not found is refused with its traceback, nothing changed. Answers as `transact` does, a new fragment node under `added_nodes`; undo restores the fragment as it was."
    )]
    async fn import_graph_generator(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(ImportGraphGeneratorToolArguments {
            stream,
            generator,
            name,
            config,
            mode,
            author,
            idempotency_key,
        }): Parameters<ImportGraphGeneratorToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::ImportGraphGenerator {
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
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Add a node to a loaded stream's graph by its type's import path, as one transaction, the runtime making the node's UUID. A Python type is named `module:QualifiedClassName` and must be importable by the stream's own interpreter — a module in the stream's project, or a package installed in its venv: the runtime never imports it, and describes it in that interpreter the first time it is added; each add spawns it in its own processor interpreter, which imports the module fresh. The type's port declaration is read when it is first described and kept; to change a type's ports, add it under a new class name."
    )]
    async fn add_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
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
            idempotency_key,
        }): Parameters<AddNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::AddNode {
            stream,
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            label: None,
        };
        self.one_graph_transaction(
            "add_node",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Remove a node from a loaded stream's graph by its UUID, as one transaction, stopping it; its links go with it, and undo brings both back."
    )]
    async fn remove_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(OneNodeToolArguments {
            stream,
            node,
            mode,
            author,
            idempotency_key,
        }): Parameters<OneNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RemoveNode { stream, node };
        self.one_graph_transaction(
            "remove_node",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`."
    )]
    async fn connect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(OneLinkToolArguments {
            stream,
            source,
            target,
            mode,
            author,
            idempotency_key,
        }): Parameters<OneLinkToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::Connect {
            stream,
            source,
            target,
        };
        self.one_graph_transaction(
            "connect",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Take out the link between an output port and an input port in one loaded stream, as one transaction, each end a node's UUID and a port name."
    )]
    async fn disconnect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(OneLinkToolArguments {
            stream,
            source,
            target,
            mode,
            author,
            idempotency_key,
        }): Parameters<OneLinkToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::Disconnect {
            stream,
            source,
            target,
        };
        self.one_graph_transaction(
            "disconnect",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Replace a node's config whole, as one transaction, applied to the running node at the stream's next commit. A config value `{\"asset\": \"<uuid>\"}` references an asset of the project, which then cannot be deleted while the reference stands."
    )]
    async fn set_node_config(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetNodeConfigToolArguments {
            stream,
            node,
            config,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetNodeConfigToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetNodeConfig {
            stream,
            node,
            config,
        };
        self.one_graph_transaction(
            "set_node_config",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Set the value one of a node's inputs holds while no link feeds it, as one transaction; the value is saved in the graph document as a link is."
    )]
    async fn set_pin_value(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetPinValueToolArguments {
            stream,
            node,
            input,
            value,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetPinValueToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetPinValue {
            stream,
            node,
            input,
            value,
        };
        self.one_graph_transaction(
            "set_pin_value",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Give a node another name, a label, as one transaction; the node keeps its UUID, which every address names it by. Refused while another node's config names it by its name."
    )]
    async fn rename_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RenameNodeToolArguments {
            stream,
            node,
            name,
            mode,
            author,
            idempotency_key,
        }): Parameters<RenameNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RenameNode { stream, node, name };
        self.one_graph_transaction(
            "rename_node",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Replace what the studio draws a node with — canvas position, group, comment, colour — as one transaction."
    )]
    async fn set_studio_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetStudioFieldsToolArguments {
            stream,
            node,
            studio,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetStudioFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetStudioFields {
            stream,
            node,
            studio,
        };
        self.one_graph_transaction(
            "set_studio_fields",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Replace the config fields a node takes as inputs, as one transaction. Each is a number or bool field of the node's config schema and becomes an input of the field's name: linked, it drives the field, a function node reading it at each evaluation and a process node taking each new value as a live config update; unlinked, it reads the node's config. A field still linked cannot be taken off."
    )]
    async fn set_exposed_config_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetExposedConfigFieldsToolArguments {
            stream,
            node,
            exposed_config_fields,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetExposedConfigFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetExposedConfigFields {
            stream,
            node,
            exposed_config_fields,
        };
        self.one_graph_transaction(
            "set_exposed_config_fields",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Make a labelled group of nodes of a loaded stream's own level, as one transaction: a title, a description and a colour Tato Studio frames the nodes with, so the next reader knows what they do. It changes nothing that runs. Each node joins it by holding the group's UUID as its `studio.group`; a node of another group is refused, and a node leaves or joins later through `set_studio_fields`. Answers as `transact` does, the new group's UUID under `added_groups`."
    )]
    async fn add_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(AddGroupToolArguments {
            stream,
            title,
            description,
            colour,
            nodes,
            mode,
            author,
            idempotency_key,
        }): Parameters<AddGroupToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::AddGroup {
            stream,
            title,
            description,
            colour,
            nodes,
        };
        self.one_graph_transaction(
            "add_group",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Set a labelled group's title, description or colour, as one transaction; a field omitted is kept. Its nodes stay as they are."
    )]
    async fn set_group_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetGroupFieldsToolArguments {
            stream,
            group,
            title,
            description,
            colour,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetGroupFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetGroupFields {
            stream,
            group,
            title,
            description,
            colour,
        };
        self.one_graph_transaction(
            "set_group_fields",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Take out a labelled group, as one transaction: its nodes stay, ungrouped. Undo brings the group back with every node that belonged to it."
    )]
    async fn remove_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RemoveGroupToolArguments {
            stream,
            group,
            mode,
            author,
            idempotency_key,
        }): Parameters<RemoveGroupToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RemoveGroup { stream, group };
        self.one_graph_transaction(
            "remove_group",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Refresh, as one transaction, every fragment of the project's loaded streams whose graph generator, or a generator nested in it, is defined in a module one of `changed_files` holds: each is run again with the promoted settings its fragment keeps, as `import_graph_generator` refreshes it, each node keeping its UUID by name. Answers as `transact` does; with no such fragment, it answers no operation and records no transaction. A generator that raises refuses the whole refresh with its traceback, nothing changed."
    )]
    async fn refresh_fragments(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RefreshFragmentsToolArguments {
            project_directory,
            changed_files,
            mode,
            author,
            idempotency_key,
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
        self.one_graph_transaction(
            "refresh_fragments",
            refreshes,
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Undo the latest transaction by `author` in the project that is not undone yet, applying its inverse as a new transaction: its graph edits to the running streams and their graph documents, its asset changes and file writes to the project. Only the author's own transactions are undone, so two authors' interleaved edits are each undone by their own author. An edit to a stream not loaded is applied to its graph document alone, and the stream of a stream document the undo takes out is unloaded and forgotten. Refused when the author has nothing left to undo, or when what it would undo no longer applies — a node another author removed since, or a stream document it would take out that was edited after it was written. Answers the transaction applied, with `undoes` naming the one it reversed."
    )]
    async fn undo(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(UndoOrRedoToolArguments {
            project_directory,
            author,
            idempotency_key,
        }): Parameters<UndoOrRedoToolArguments>,
    ) -> ToolCallAnswer {
        self.an_undo_or_redo(
            "undo",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectRepository::undo_by,
        )
        .await
    }

    #[tool(
        description = "Redo what `author`'s latest undo in the project reversed, as a new transaction; a new edit by the author after the undo leaves nothing to redo. Answers the transaction applied, with `redoes` naming the one it reapplied."
    )]
    async fn redo(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(UndoOrRedoToolArguments {
            project_directory,
            author,
            idempotency_key,
        }): Parameters<UndoOrRedoToolArguments>,
    ) -> ToolCallAnswer {
        self.an_undo_or_redo(
            "redo",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectRepository::redo_by,
        )
        .await
    }
}

impl LocalApiMcpServerHandler {
    pub(crate) async fn one_graph_transaction(
        &self,
        tool_name: &'static str,
        operations: Vec<StreamGraphAction>,
        mode: Option<GraphEditMode>,
        author: Option<ProjectTransactionAuthorToolArgument>,
        idempotency_key: Option<String>,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, tool_call_request_context);
        let mode = mode.unwrap_or_default();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let project_actions_refused =
            Arc::clone(&self.project_actions_refused_since_the_runtime_started);
        let async_runtime = tokio::runtime::Handle::current();
        let applied = tokio::task::spawn_blocking(move || {
            let in_flight = GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            };
            let operations_as_requested: Vec<Value> = operations
                .iter()
                .map(|operation| serde_json::to_value(operation).unwrap_or_default())
                .collect();
            let stream_assets_named: Vec<Uuid> =
                operations.iter().map(StreamGraphAction::stream).collect();
            let (idempotency_key, applied) =
                match the_idempotency_key_a_caller_sent(idempotency_key) {
                    Ok(idempotency_key) => {
                        let applied =
                            in_flight.apply(operations, mode, &author, idempotency_key.as_ref());
                        (idempotency_key, applied)
                    }
                    Err(refusal) => (None, Err(GraphTransactionRefusal::from(refusal))),
                };
            applied.map_err(|refusal| {
                let refusal = refusal.into_the_refusal_on_the_wire(tool_name);
                if let Some(project) =
                    in_flight.the_project_of_the_first_stream_asset_of(&stream_assets_named)
                {
                    project_actions_refused.keep_and_announce(
                        &project_repositories,
                        project,
                        ProjectActionTheRuntimeRefused {
                            author: &author,
                            mode: mode.on_the_wire(),
                            operations_as_requested,
                            idempotency_key: idempotency_key.as_ref(),
                            refusal: refusal.clone(),
                        },
                    );
                }
                Box::new(refusal)
            })
        })
        .await
        .map_err(|join_failure| {
            format!("{tool_name} failed: the transaction did not finish: {join_failure}")
        })?;
        match applied {
            Ok(applied) => serialized_tool_result(tool_name, &applied),
            Err(refusal) => Ok(a_tool_result_of_the_refusal(*refusal)),
        }
    }

    async fn an_undo_or_redo(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
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
        let project_actions_refused =
            Arc::clone(&self.project_actions_refused_since_the_runtime_started);
        let async_runtime = tokio::runtime::Handle::current();
        let applied = tokio::task::spawn_blocking(move || {
            let idempotency_key_as_sent = idempotency_key.clone();
            GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            }
            .apply_a_reversal(&project_directory, &author, idempotency_key, reversal_of)
            .map_err(|refusal| {
                let refusal = GraphTransactionRefusal::of_the_whole_transaction(refusal)
                    .into_the_refusal_on_the_wire(tool_name);
                if let Ok(project_repository) =
                    project_repositories.the_project_repository_at(&project_directory)
                {
                    let project = project_repository.lock().project_file().project;
                    let idempotency_key =
                        the_idempotency_key_a_caller_sent(idempotency_key_as_sent)
                            .ok()
                            .flatten();
                    project_actions_refused.keep_and_announce(
                        &project_repositories,
                        project,
                        ProjectActionTheRuntimeRefused {
                            author: &author,
                            mode: GraphEditMode::Commit.on_the_wire(),
                            operations_as_requested: Vec::new(),
                            idempotency_key: idempotency_key.as_ref(),
                            refusal: refusal.clone(),
                        },
                    );
                }
                Box::new(refusal)
            })
        })
        .await
        .map_err(|join_failure| {
            format!("{tool_name} failed: the transaction did not finish: {join_failure}")
        })?;
        match applied {
            Ok(applied) => serialized_tool_result(tool_name, &applied),
            Err(refusal) => Ok(a_tool_result_of_the_refusal(*refusal)),
        }
    }
}

/// One loaded stream a transaction edits: its operations, where it lives, and its graph as the
/// transaction has staged it.
struct StreamATransactionEdits {
    stream_operations: Arc<dyn RuntimeOperations>,
    stream_location: Option<StreamLocation>,
    staged_graph: StreamGraphDocument,
}

/// The node, link and field one operation of a graph action names, as its caller named them.
#[derive(Default)]
pub(crate) struct WhatAGraphOperationNames {
    node: Option<String>,
    link: Option<RefusedGraphLinkOnTheWire>,
    field: Option<String>,
}

/// Why a graph action was refused, in the runtime's own words, and which of its operations
/// was refused when one was.
pub(crate) struct GraphTransactionRefusal {
    refused_operation_index: Option<usize>,
    what_the_refused_operation_names: Box<WhatAGraphOperationNames>,
    runtime_words: String,
}

impl GraphTransactionRefusal {
    fn of_the_operation(
        refused_operation_index: usize,
        what_the_refused_operation_names: WhatAGraphOperationNames,
        runtime_words: String,
    ) -> Self {
        Self {
            refused_operation_index: Some(refused_operation_index),
            what_the_refused_operation_names: Box::new(what_the_refused_operation_names),
            runtime_words,
        }
    }

    fn of_the_whole_transaction(runtime_words: String) -> Self {
        Self {
            refused_operation_index: None,
            what_the_refused_operation_names: Box::default(),
            runtime_words,
        }
    }

    /// The refusal of the action the tool `tool_name` was sent, as the wire carries it.
    fn into_the_refusal_on_the_wire(self, tool_name: &str) -> ProjectActionRefusalOnTheWire {
        let what_the_refused_operation_names = *self.what_the_refused_operation_names;
        ProjectActionRefusalOnTheWire {
            action: tool_name.to_owned(),
            refused_operation_index: self.refused_operation_index,
            refused_node: what_the_refused_operation_names.node,
            refused_link: what_the_refused_operation_names.link,
            refused_field: what_the_refused_operation_names.field,
            runtime_words: self.runtime_words,
        }
    }
}

/// `refusal` as its tool answers it: an error result whose text is the runtime's words and whose
/// `structuredContent` holds the refusal under `refusal`.
fn a_tool_result_of_the_refusal(refusal: ProjectActionRefusalOnTheWire) -> CallToolResult {
    let text = format!("{} failed: {}", refusal.action, refusal.runtime_words);
    let mut tool_result = CallToolResult::error(vec![ContentBlock::text(text)]);
    tool_result.structured_content = Some(json!({ "refusal": refusal }));
    tool_result
}

impl From<ProjectRepositoryRefusal> for GraphTransactionRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::of_the_whole_transaction(refusal.to_string())
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
    /// Apply `requests` as one transaction by `author`; committed under an `idempotency_key`
    /// the project's history already holds, it applies nothing and answers that transaction.
    pub(crate) fn apply(
        &self,
        requests: Vec<StreamGraphAction>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        if requests.is_empty() {
            return Err(GraphTransactionRefusal::of_the_whole_transaction(
                "a transaction holds at least one operation".to_owned(),
            ));
        }
        let stream_locations = self.the_location_of_each_loaded_stream();
        let stream_id_of_each_request = self.the_loaded_stream_each_request_names(&requests)?;
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for stream_id in &stream_id_of_each_request {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }
        let project_repository = match mode {
            GraphEditMode::Live => None,
            GraphEditMode::Commit => Some(
                self.the_one_project_the_streams_belong_to(&stream_ids_in_order, &stream_locations)
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            ),
        };
        let mut project_repository_held = project_repository.as_ref().map(|held| held.lock());
        if let Some(project_repository_held) = project_repository_held.as_deref()
            && let Some(applied_before) = project_repository_held
                .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(self.the_answer_of_a_graph_transaction_applied_before(
                project_repository_held.project_file().project,
                applied_before,
            ));
        }
        let applied = self.apply_the_requests(
            requests
                .into_iter()
                .zip(stream_id_of_each_request)
                .collect(),
            mode,
            author,
            idempotency_key,
            &stream_locations,
            project_repository_held.as_deref_mut(),
        )?;
        if let Some(project_repository_held) = project_repository_held {
            let edited_project_directory =
                project_repository_held.project_directory().to_path_buf();
            drop(project_repository_held);
            self.reconcile_each_loaded_stream_holding_a_stream_fragment(&edited_project_directory);
        }
        Ok(applied)
    }

    /// Bring each loaded stream whose graph holds a stream fragment, of the project at
    /// `edited_project_directory` or of a project enclosing it as an import, to the streams its
    /// fragments reference, as their graph documents are on disk now. A stream that refuses
    /// keeps its live graph as it was, and the refusal is logged.
    pub(crate) fn reconcile_each_loaded_stream_holding_a_stream_fragment(
        &self,
        edited_project_directory: &std::path::Path,
    ) {
        let loaded_streams_the_edit_can_reach = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                edited_project_directory
                    .starts_with(&stream_location.project_directory)
                    .then_some((stream_location.stream_asset, listing.stream_id))
            });
        for (stream_asset, stream_id) in loaded_streams_the_edit_can_reach {
            let stream_operations = match self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream_id)
            {
                Ok(stream_operations) => stream_operations,
                Err(_) => continue,
            };
            let holds_a_stream_fragment = self
                .async_runtime
                .block_on(stream_operations.to_json_async())
                .is_ok_and(|graph| graph_document_holds_a_stream_fragment(&graph));
            if !holds_a_stream_fragment {
                continue;
            }
            if let Err(refusal) = self.async_runtime.block_on(
                stream_operations.reconcile_each_stream_fragment_to_the_stream_it_references_live(),
            ) {
                tracing::warn!(
                    stream = %stream_id,
                    %stream_asset,
                    %refusal,
                    "a loaded stream's fragments were not brought to the streams they reference; \
                     its live graph is kept as it was until its next load"
                );
            }
        }
    }

    /// The project of the first of `stream_ids` that is a loaded stream of a project.
    fn the_project_of_the_first_stream_asset_of(&self, stream_assets: &[Uuid]) -> Option<Uuid> {
        let project_of_each_loaded_stream_asset: HashMap<Uuid, Uuid> = self
            .the_location_of_each_loaded_stream()
            .into_values()
            .flatten()
            .map(|stream_location| (stream_location.stream_asset, stream_location.project))
            .collect();
        stream_assets.iter().find_map(|stream_asset| {
            project_of_each_loaded_stream_asset
                .get(stream_asset)
                .copied()
        })
    }

    /// The stream id of the stream running each request's stream asset, in request order; a
    /// loaded one over one only recorded.
    fn the_loaded_stream_each_request_names(
        &self,
        requests: &[StreamGraphAction],
    ) -> Result<Vec<String>, GraphTransactionRefusal> {
        let mut listings_loaded_last = self.operations_on_the_loaded_streams.list_streams();
        listings_loaded_last.sort_by_key(|listing| listing.node_count.is_some());
        let stream_id_of_each_loaded_stream_asset: HashMap<Uuid, String> = listings_loaded_last
            .into_iter()
            .filter_map(|listing| Some((listing.stream_location?.stream_asset, listing.stream_id)))
            .collect();
        requests
            .iter()
            .enumerate()
            .map(|(request_index, request)| {
                stream_id_of_each_loaded_stream_asset
                    .get(&request.stream())
                    .cloned()
                    .ok_or_else(|| {
                        let mut loaded_stream_assets: Vec<String> =
                            stream_id_of_each_loaded_stream_asset
                                .iter()
                                .map(|(stream_asset, stream_id)| {
                                    format!("{stream_asset} (stream `{stream_id}`)")
                                })
                                .collect();
                        loaded_stream_assets.sort();
                        GraphTransactionRefusal::of_the_operation(
                            request_index,
                            what_a_graph_action_names(request),
                            format!(
                                "no stream the runtime holds runs the stream asset {}; the stream \
                                 assets it holds are: {}",
                                request.stream(),
                                if loaded_stream_assets.is_empty() {
                                    "none".to_owned()
                                } else {
                                    loaded_stream_assets.join(", ")
                                }
                            ),
                        )
                    })
            })
            .collect()
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

    /// Apply each request, to the loaded stream it names, as one transaction by `author`, to the
    /// project `project_repository_held` holds when the transaction is committed.
    fn apply_the_requests(
        &self,
        requests_and_the_stream_id_each_names: Vec<(StreamGraphAction, String)>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        stream_locations: &HashMap<String, Option<StreamLocation>>,
        project_repository_held: Option<&mut ProjectRepository>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        let actions_as_sent: Vec<ProjectAction> = requests_and_the_stream_id_each_names
            .iter()
            .map(|(request, _)| ProjectAction::StreamGraph(request.clone()))
            .collect();
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for (_, stream_id) in &requests_and_the_stream_id_each_names {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }

        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        for stream_id in &stream_ids_in_order {
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)
                .map_err(|refusal| {
                    GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
                })?;
            let stream_location = stream_locations.get(stream_id).cloned().flatten();
            let staged_graph = match (&project_repository_held, &stream_location) {
                (Some(project_repository), Some(stream_location)) => {
                    if stream_location.project != project_repository.project_file().project {
                        return Err(GraphTransactionRefusal::of_the_whole_transaction(format!(
                            "a transaction edits the streams of one project, and `{stream_id}` \
                             is of the project {}, not {}",
                            stream_location.project,
                            project_repository.project_file().project
                        )));
                    }
                    project_repository.stream_graph_document(stream_location.stream_asset)?
                }
                _ => StreamGraphDocument::from_value(
                    self.async_runtime
                        .block_on(stream_operations.to_json_async())
                        .map_err(|refusal| {
                            GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
                        })?,
                )
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
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
        for (request_index, (request, stream_id)) in requests_and_the_stream_id_each_names
            .into_iter()
            .enumerate()
        {
            let what_the_request_names = what_a_graph_action_names(&request);
            let applied = streams
                .get_mut(&stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))
                .and_then(|stream| {
                    let label = match &request {
                        StreamGraphAction::AddNode { label, .. }
                        | StreamGraphAction::ImportGraphGenerator {
                            label, ..
                        }
                        | StreamGraphAction::UseDependencyStream {
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
                    let place_the_imported_stream =
                        |fragment_node: Uuid, held_stream: &Value| {
                            let project_repository = project_repository_read_during_the_requests
                                .ok_or_else(|| {
                                    "an imported stream is placed from the project's \
                                     `imports/` folder; place it with `mode` `commit`"
                                        .to_owned()
                                })?;
                            the_fragment_an_imported_stream_places(
                                project_repository.project_directory(),
                                fragment_node,
                                held_stream,
                            )
                        };
                    let operation = graph_operation_for_the_request(
                        request,
                        &stream.staged_graph,
                        &mut node_by_label,
                        &run_the_graph_generator,
                        &place_the_imported_stream,
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
                    return Err(GraphTransactionRefusal::of_the_operation(
                        request_index,
                        what_the_request_names,
                        refusal,
                    ));
                }
            };
            let Some(stream) = streams.get_mut(&stream_id) else {
                return Err(GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_the_request_names,
                    format!("stream `{stream_id}` is not loaded"),
                ));
            };
            let staged = stream.staged_graph.apply(&applied);
            applied_live.push((stream_id.clone(), applied));
            if let Err(why) = staged {
                self.reverse_what_was_applied_live(&streams, applied_live);
                return Err(GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_the_request_names,
                    format!(
                        "stream `{stream_id}`'s graph document does not hold what its running \
                         graph does: {why}"
                    ),
                ));
            }
        }

        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(project_repository) => {
                let project_operations = applied_live
                    .iter()
                    .filter_map(|(stream_id, graph_operation)| {
                        let stream_location = streams.get(stream_id)?.stream_location.as_ref()?;
                        Some(ProjectOperation::StreamGraph(
                            StreamGraphOperationOnAStream {
                                stream: stream_location.stream_asset,
                                graph_operation: graph_operation.clone(),
                            },
                        ))
                    })
                    .collect();
                match project_repository.apply_project_operations(
                    author,
                    idempotency_key,
                    actions_as_sent,
                    project_operations,
                    ProjectTransactionLineage::NewEdit,
                ) {
                    Ok(outcome) => {
                        self.project_repositories
                            .announce_the_transaction_if_applied_now(project_repository, &outcome);
                        let applied = outcome.into_applied_transaction();
                        (
                            Some(applied.transaction.transaction.to_string()),
                            Some(applied.commit),
                        )
                    }
                    Err(refusal) => {
                        self.reverse_what_was_applied_live(&streams, applied_live);
                        return Err(refusal.into());
                    }
                }
            }
        };

        Ok(self.the_answer_of_a_graph_transaction(
            mode,
            transaction,
            commit,
            &|stream_id| {
                streams
                    .get(stream_id)
                    .map(|stream| Arc::clone(&stream.stream_operations))
            },
            applied_live,
            &labels_of_added_nodes,
        ))
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
        idempotency_key: Option<String>,
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
        let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
            .map_err(|refusal| refusal.to_string())?;
        if let Some(applied_before) = project_repository
            .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())
            .map_err(|refusal| refusal.to_string())?
        {
            let undo_state_of_each_transaction = project_repository
                .the_undo_state_of_each_transaction()
                .map_err(|refusal| refusal.to_string())?;
            return Ok(project_transaction_on_the_wire(
                applied_before,
                &undo_state_of_each_transaction,
            ));
        }
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
            idempotency_key.as_ref(),
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        ) {
            Ok(outcome) => {
                self.project_repositories
                    .announce_the_transaction_if_applied_now(&project_repository, &outcome);
                let applied = outcome.into_applied_transaction();
                let undo_state_of_each_transaction = project_repository
                    .the_undo_state_of_each_transaction()
                    .map_err(|refusal| refusal.to_string())?;
                let edited_project_directory = project_repository.project_directory().to_path_buf();
                drop(project_repository);
                bring_the_loaded_streams_in_line_with_the_stream_documents_of(
                    self.operations_on_the_loaded_streams,
                    project,
                    &edited_project_directory,
                    &applied.transaction,
                )
                .map_err(|not_in_line| {
                    format!(
                        "the transaction {} applied, and the loaded streams were not brought in \
                         line with it: {not_in_line}",
                        applied.transaction.transaction
                    )
                })?;
                self.reconcile_each_loaded_stream_holding_a_stream_fragment(
                    &edited_project_directory,
                );
                Ok(project_transaction_on_the_wire(
                    applied,
                    &undo_state_of_each_transaction,
                ))
            }
            Err(refusal) => {
                self.reverse_what_was_applied_live(&streams, applied_live);
                Err(refusal.to_string())
            }
        }
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

    /// The answer of `applied_before`, the transaction of the project `project` an action sent
    /// again under its key first applied, each edit of a loaded stream under its stream id.
    fn the_answer_of_a_graph_transaction_applied_before(
        &self,
        project: Uuid,
        applied_before: AppliedProjectTransaction,
    ) -> GraphTransactionToolResult {
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        for operation in applied_before.transaction.operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = operation
            else {
                continue;
            };
            let stream_id = stream_id_of_each_stream_asset
                .get(&stream)
                .cloned()
                .unwrap_or_else(|| stream.to_string());
            applied_live.push((stream_id, graph_operation));
        }
        self.the_answer_of_a_graph_transaction(
            GraphEditMode::Commit,
            Some(applied_before.transaction.transaction.to_string()),
            Some(applied_before.commit),
            &|stream_id| {
                self.operations_on_the_loaded_streams
                    .runtime_operations_of_the_stream_a_call_names(stream_id)
                    .ok()
            },
            applied_live,
            &HashMap::new(),
        )
    }

    fn the_answer_of_a_graph_transaction(
        &self,
        mode: GraphEditMode,
        transaction: Option<String>,
        commit: Option<String>,
        stream_operations_of: &dyn Fn(&str) -> Option<Arc<dyn RuntimeOperations>>,
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
                            let stream_operations = stream_operations_of(&stream_id)?;
                            match self
                                .async_runtime
                                .block_on(stream_operations.to_json_async())
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

/// The placement of an imported stream under the fragment node `(fragment node, held stream)`
/// names, `held_stream` its config's `stream`: the fragment it places and the stream's name.
type ImportedStreamPlacement<'repository> =
    dyn Fn(Uuid, &Value) -> Result<(Value, String), String> + 'repository;

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
fn graph_operation_for_the_request(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    run_the_graph_generator: &GraphGeneratorRunOfTheStream<'_>,
    place_the_imported_stream: &ImportedStreamPlacement<'_>,
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
    let link_between = |source: &StreamGraphActionLinkEnd,
                        target: &StreamGraphActionLinkEnd,
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
        StreamGraphAction::ImportGraphGenerator {
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
        StreamGraphAction::AddNode {
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
        StreamGraphAction::RemoveNode { node, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RemoveNode {
                node: staged_graph.node(node)?,
                links: staged_graph.links_touching(node),
            })
        }
        StreamGraphAction::Connect { source, target, .. } => Ok(StreamGraphOperation::Connect {
            link: link_between(&source, &target, node_by_label)?,
        }),
        StreamGraphAction::Disconnect { source, target, .. } => {
            Ok(StreamGraphOperation::Disconnect {
                link: link_between(&source, &target, node_by_label)?,
            })
        }
        StreamGraphAction::SetNodeConfig { node, config, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetNodeConfig {
                node,
                from: staged_graph.node(node)?.config,
                to: Value::Object(config),
            })
        }
        StreamGraphAction::SetPinValue {
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
        StreamGraphAction::RenameNode { node, name, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RenameNode {
                node,
                from: staged_graph.node(node)?.name,
                to: name,
            })
        }
        StreamGraphAction::SetStudioFields { node, studio, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetStudioFields {
                node,
                from: staged_graph.node(node)?.studio,
                to: studio,
            })
        }
        StreamGraphAction::SetExposedConfigFields {
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
        StreamGraphAction::AddGroup {
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
        StreamGraphAction::SetGroupFields {
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
        StreamGraphAction::UseDependencyStream {
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
            let config = json!({ STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: {
                "asset": dependency_stream.to_string(),
                "from": project.to_string(),
            }});
            let node = Uuid::new_v4();
            let (fragment, stream_name) =
                place_the_imported_stream(node, &config[STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY])?;
            let name = name.unwrap_or(stream_name);
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the fragment name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            if let Some(held) = staged_graph.node_named(&name) {
                return Err(
                    if held.node_type == STREAM_FRAGMENT_NODE_TYPE && held.config == config {
                        format!(
                            "node {} `{name}` already places this stream, and follows its import's \
                         folder as it is on disk, so there is nothing to place again",
                            held.id
                        )
                    } else {
                        format!(
                            "node {} `{name}` is a `{}`; give the stream another `name`",
                            held.id, held.node_type
                        )
                    },
                );
            }
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
                    fragment: Some(Box::new(fragment)),
                },
                links: Vec::new(),
            })
        }
        StreamGraphAction::RemoveGroup { group, .. } => {
            let group = staged_graph.group(the_group_named(&group)?)?;
            Ok(StreamGraphOperation::RemoveGroup {
                nodes: staged_graph.nodes_of_the_group(group.id),
                group,
            })
        }
    }
}

/// The fragment the stream fragment node `fragment_node` places for `held_stream`,
/// `{"asset", "from"}`, in a stream of the project at `project_directory`, and the name the
/// imported stream's file gives it.
fn the_fragment_an_imported_stream_places(
    project_directory: &std::path::Path,
    fragment_node: Uuid,
    held_stream: &Value,
) -> Result<(Value, String), String> {
    let from = held_stream["from"]
        .as_str()
        .and_then(|from| Uuid::parse_str(from).ok())
        .ok_or("an imported stream names the project it is imported from under `from`")?;
    let stream_asset = held_stream["asset"]
        .as_str()
        .and_then(|asset| Uuid::parse_str(asset).ok())
        .ok_or("an imported stream names its stream asset under `asset`")?;
    let (_, stream_name) = the_stream_document_of_an_import(project_directory, from, stream_asset)?;
    let fragment =
        the_fragment_a_stream_fragment_places(project_directory, fragment_node, held_stream)?;
    Ok((fragment, stream_name))
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
) -> Result<Vec<StreamGraphAction>, String> {
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
            refreshes.push(StreamGraphAction::ImportGraphGenerator {
                stream: stream_location.stream_asset,
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
