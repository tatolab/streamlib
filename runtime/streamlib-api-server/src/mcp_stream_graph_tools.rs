// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph tools: every edit of a stream's graph a transaction, committed to the graph
//! document and the project's history or applied live to the running stream alone, its nodes
//! by UUID, and each author's undo and redo.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::error::{Error, WhyANodeOfAStreamIsNotRunning};
use streamlib_control_contract::{
    AUTHOR_ARGUMENT_DESCRIPTION, AddNode, AddNodeRequest, Connect, ControlOperation,
    ControlOperationCall, GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION, GraphEditMode,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, OneLinkGraphEditRequest,
    ProjectTransactionAuthorArgument, Transact, TransactRequest,
};
use streamlib_project_repository::{
    EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION, GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GROUP_ARGUMENT_DESCRIPTION, GROUP_COLOUR_ARGUMENT_DESCRIPTION,
    GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION, GROUP_TITLE_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION, NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION,
    NOTE_ARGUMENT_DESCRIPTION, NOTE_POSITION_ARGUMENT_DESCRIPTION, NOTE_SIZE_ARGUMENT_DESCRIPTION,
    NOTE_TEXT_ARGUMENT_DESCRIPTION, ProjectRepository, ProjectTransactionAuthor,
    ProjectTransactionReversal, StreamGraphAction, StudioCanvasPair,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphTransactionToolResult, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::ControlRequestOrigin;
use crate::mcp::control_request_origin_of_an_mcp_tool_call;
use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{the_author_of_a_change, the_idempotency_key_a_caller_sent};
use crate::mcp_project_change_feed::ProjectActionTheRuntimeRefused;
use crate::stream_graph_transaction_executor::{
    GraphTransactionRefusal, StreamGraphTransactionExecutor,
    the_refresh_of_each_fragment_a_changed_module_defines,
};

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OneNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetNodeConfigToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "The node's new config, as the keys its type's schema declares; it replaces the old one whole."
    )]
    config: Map<String, Value>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetPinValueToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
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
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RenameNodeToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(
        description = "The node's new name: cast to lowercase URL-safe, refused when another node has it. The node keeps its UUID."
    )]
    name: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetStudioFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = NODE_STUDIO_FIELDS_ARGUMENT_DESCRIPTION)]
    studio: Map<String, Value>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
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
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetGroupFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
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
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddNoteToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = NOTE_TEXT_ARGUMENT_DESCRIPTION)]
    text: String,
    #[schemars(description = NOTE_POSITION_ARGUMENT_DESCRIPTION, with = "[f64; 2]")]
    position: StudioCanvasPair,
    #[schemars(description = NOTE_SIZE_ARGUMENT_DESCRIPTION, with = "[f64; 2]")]
    size: StudioCanvasPair,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetNoteFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = NOTE_ARGUMENT_DESCRIPTION)]
    note: String,
    #[schemars(description = "What the note says; kept when omitted.")]
    text: Option<String>,
    #[schemars(
        description = "Where the note's top-left corner sits, `[x, y]`; kept when omitted.",
        with = "Option<[f64; 2]>"
    )]
    position: Option<StudioCanvasPair>,
    #[schemars(
        description = "The note's `[width, height]`; kept when omitted.",
        with = "Option<[f64; 2]>"
    )]
    size: Option<StudioCanvasPair>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveNoteToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = NOTE_ARGUMENT_DESCRIPTION)]
    note: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveGroupToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GROUP_ARGUMENT_DESCRIPTION)]
    group: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CollapseSelectionIntoSubgraphToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(
        description = "The nodes collapsed, each by its UUID, all of the level `level` names and none of that level's own `GraphInput` or `GraphOutput` nodes."
    )]
    nodes: Vec<String>,
    #[schemars(
        description = "The subgraph node's name, cast to lowercase URL-safe and refused when a node outside the selection has it."
    )]
    name: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct UnlockGeneratorFragmentToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(
        description = "The fragment node, by its UUID: a graph generator's or a collapsed selection's, never a placed stream's."
    )]
    fragment: String,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
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
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetExposedConfigFieldsToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Uuid,
    #[schemars(description = GRAPH_EDIT_LEVEL_ARGUMENT_DESCRIPTION)]
    level: Option<Vec<Uuid>>,
    #[schemars(description = GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION)]
    node: String,
    #[schemars(description = EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION)]
    exposed_config_fields: Vec<String>,
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    mode: Option<GraphEditMode>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
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
    author: Option<ProjectTransactionAuthorArgument>,
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
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[tool_router(router = stream_graph_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = Transact::AGENT_USAGE_NOTE)]
    async fn transact(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<TransactRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Transact(request),
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

    #[tool(description = AddNode::AGENT_USAGE_NOTE)]
    async fn add_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddNodeRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddNode(request),
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
            level,
            node,
            mode,
            author,
            idempotency_key,
        }): Parameters<OneNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RemoveNode {
            stream,
            level: level.unwrap_or_default(),
            node,
        };
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

    #[tool(description = Connect::AGENT_USAGE_NOTE)]
    async fn connect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneLinkGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Connect(request),
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
        Parameters(OneLinkGraphEditRequest {
            stream,
            level,
            source,
            target,
            mode,
            author,
            idempotency_key,
        }): Parameters<OneLinkGraphEditRequest>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::Disconnect {
            stream,
            level: level.unwrap_or_default(),
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
            level,
            node,
            config,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetNodeConfigToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetNodeConfig {
            stream,
            level: level.unwrap_or_default(),
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
            level,
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
            level: level.unwrap_or_default(),
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
            level,
            node,
            name,
            mode,
            author,
            idempotency_key,
        }): Parameters<RenameNodeToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RenameNode {
            stream,
            level: level.unwrap_or_default(),
            node,
            name,
        };
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
        description = "Replace what Tato Studio draws a node with — canvas position, group, comment, colour, collapsed, and a client's own `metadata` — as one transaction, the old fields replaced whole. The fields are held to the studio contract: a key of the wrong shape, or any key outside it, is refused by name. It changes nothing that runs."
    )]
    async fn set_studio_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetStudioFieldsToolArguments {
            stream,
            level,
            node,
            studio,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetStudioFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetStudioFields {
            stream,
            level: level.unwrap_or_default(),
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
            level,
            node,
            exposed_config_fields,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetExposedConfigFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetExposedConfigFields {
            stream,
            level: level.unwrap_or_default(),
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
        description = "Make a labelled group of nodes of one level of a loaded stream, its own or the subgraph `level` names, as one transaction: a title, a description and a colour Tato Studio frames the nodes with, so the next reader knows what they do. It changes nothing that runs. Each node joins it by holding the group's UUID as its `studio.group`; a node of another group is refused, and a node leaves or joins later through `set_studio_fields`. Answers as `transact` does, the new group's UUID under `added_groups`."
    )]
    async fn add_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(AddGroupToolArguments {
            stream,
            level,
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
            level: level.unwrap_or_default(),
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
            level,
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
            level: level.unwrap_or_default(),
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
        description = "Collapse nodes of one level of a loaded stream, its own or the subgraph `level` names, into one subgraph node, as one transaction: an authored fragment holding the nodes and the links between them, which no refresh touches and whose inside is edited like any graph. Each link that crossed the selection is carried through a `GraphInput` or `GraphOutput` inside it, named `<node>-<port>` and of the family the running stream declares at the link's source, so the stream runs the same nodes, each under the subgraph's name (`sub/blur`). Answers as `transact` does, the subgraph node under `added_nodes`; undo gives back the exact earlier graph document."
    )]
    async fn collapse_selection_into_subgraph(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(CollapseSelectionIntoSubgraphToolArguments {
            stream,
            level,
            nodes,
            name,
            mode,
            author,
            idempotency_key,
        }): Parameters<CollapseSelectionIntoSubgraphToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::CollapseSelectionIntoSubgraph {
            stream,
            level: level.unwrap_or_default(),
            nodes,
            name,
            label: None,
        };
        self.one_graph_transaction(
            "collapse_selection_into_subgraph",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Unlock a fragment node of one level of a loaded stream, its own or the subgraph `level` names, as one transaction: its nodes and the links between them become plain nodes of its level, its `GraphInput` and `GraphOutput` nodes go, and each link into or out of one of its ports is carried straight to the node it fed or read. A graph generator's output unlocked this way is no longer refreshed. Refused for a placed stream, and for a node of the fragment named as a node of the stream is; undo gives back the exact earlier graph document."
    )]
    async fn unlock_generator_fragment(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(UnlockGeneratorFragmentToolArguments {
            stream,
            level,
            fragment,
            mode,
            author,
            idempotency_key,
        }): Parameters<UnlockGeneratorFragmentToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::UnlockGeneratorFragment {
            stream,
            level: level.unwrap_or_default(),
            fragment,
        };
        self.one_graph_transaction(
            "unlock_generator_fragment",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Place a canvas note on a loaded stream's graph, as one transaction: text at a position and size Tato Studio draws it at, beside the nodes rather than on one. It changes nothing that runs. Answers as `transact` does, the new note's UUID under `added_notes`."
    )]
    async fn add_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(AddNoteToolArguments {
            stream,
            text,
            position,
            size,
            mode,
            author,
            idempotency_key,
        }): Parameters<AddNoteToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::AddNote {
            stream,
            text,
            position,
            size,
        };
        self.one_graph_transaction(
            "add_note",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Set a canvas note's text, position or size, as one transaction; a field omitted is kept."
    )]
    async fn set_note_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetNoteFieldsToolArguments {
            stream,
            note,
            text,
            position,
            size,
            mode,
            author,
            idempotency_key,
        }): Parameters<SetNoteFieldsToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::SetNoteFields {
            stream,
            note,
            text,
            position,
            size,
        };
        self.one_graph_transaction(
            "set_note_fields",
            vec![operation],
            mode,
            author,
            idempotency_key,
            &tool_call_request_context,
        )
        .await
    }

    #[tool(
        description = "Take a canvas note off a loaded stream's graph, as one transaction. Undo puts it back as it was."
    )]
    async fn remove_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RemoveNoteToolArguments {
            stream,
            note,
            mode,
            author,
            idempotency_key,
        }): Parameters<RemoveNoteToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RemoveNote { stream, note };
        self.one_graph_transaction(
            "remove_note",
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
            level,
            group,
            mode,
            author,
            idempotency_key,
        }): Parameters<RemoveGroupToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphAction::RemoveGroup {
            stream,
            level: level.unwrap_or_default(),
            group,
        };
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
                    added_notes: Vec::new(),
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
        author: Option<ProjectTransactionAuthorArgument>,
        idempotency_key: Option<String>,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> ToolCallAnswer {
        let request_origin: ControlRequestOrigin =
            control_request_origin_of_an_mcp_tool_call(tool_call_request_context);
        let applied = self
            .control_operation_dispatcher()
            .apply_a_graph_transaction(
                tool_name,
                operations,
                mode,
                author,
                idempotency_key,
                &request_origin,
            )
            .await;
        self.the_tool_result_of_a_control_operation(
            tool_name,
            applied
                .map(crate::control_operation_dispatcher::ControlOperationAnswer::GraphTransaction),
        )
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
            StreamGraphTransactionExecutor {
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

/// `refusal` as its tool answers it: an error result whose text is the runtime's words and whose
/// `structuredContent` holds the refusal under `refusal`.
pub(crate) fn a_tool_result_of_the_refusal(
    refusal: ProjectActionRefusalOnTheWire,
) -> CallToolResult {
    let text = format!("{} failed: {}", refusal.action, refusal.runtime_words);
    let mut tool_result = CallToolResult::error(vec![ContentBlock::text(text)]);
    tool_result.structured_content = Some(json!({ "refusal": refusal }));
    tool_result
}

/// The answer of the tool `tool_name` refused for `refusal`: a structured refusal naming
/// `not_running_because` when the node it reached for does not run, and its words otherwise.
pub(crate) fn the_answer_to_a_refused_reach_for_a_node(
    tool_name: &str,
    refusal: Error,
) -> ToolCallAnswer {
    match refusal {
        Error::NodeOfAStreamIsNotRunning {
            not_running_because,
            runtime_words,
        } => Ok(a_tool_result_of_the_refusal(
            ProjectActionRefusalOnTheWire {
                action: tool_name.to_owned(),
                refused_operation_index: None,
                refused_node: None,
                refused_link: None,
                refused_field: None,
                not_running_because: Some(match not_running_because {
                    WhyANodeOfAStreamIsNotRunning::PlaybackStopped => {
                        NodeNotRunningBecauseOnTheWire::PlaybackStopped
                    }
                    WhyANodeOfAStreamIsNotRunning::PlaybackPaused => {
                        NodeNotRunningBecauseOnTheWire::PlaybackPaused
                    }
                    WhyANodeOfAStreamIsNotRunning::Preparing => {
                        NodeNotRunningBecauseOnTheWire::Preparing
                    }
                    WhyANodeOfAStreamIsNotRunning::NodeFailed => {
                        NodeNotRunningBecauseOnTheWire::NodeFailed
                    }
                }),
                runtime_words,
            },
        )),
        refusal => Err(format!("{tool_name} failed: {refusal}")),
    }
}
