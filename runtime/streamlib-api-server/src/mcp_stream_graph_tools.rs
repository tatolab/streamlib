// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph tools: every edit of a stream's graph a transaction, committed to the graph
//! document and the project's history or applied live to the running stream alone, its nodes
//! by UUID, and each author's undo and redo.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Peer, RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamLocation,
};
use streamlib_project_repository::{
    ProjectOperation, ProjectRepositoriesOpenInThisRuntime, ProjectRepository,
    ProjectTransactionAuthor, ProjectTransactionLineage, ProjectTransactionReversal,
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphOperation, StreamGraphOperationOnAStream,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphLinkEndOnTheWire, GraphTransactionAddedNode, GraphTransactionConnectedLink,
    GraphTransactionToolResult,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorToolArgument, the_author_of_a_change,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;

const GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream the edit is to, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";
const GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION: &str = "The node, by the UUID `graph` lists as its `id` and `add_node` answers; in `transact`, also the `as` of a node an earlier `add_node` of the same transaction adds.";
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
}

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
            | Self::SetStudioFields { stream, .. } => stream,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct TransactToolArguments {
    #[schemars(
        description = "The operations, applied in order and whole or not at all, each tagged by its `operation` — `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node` or `set_studio_fields` — with the arguments that operation's own tool takes. Every `stream` they name is a stream of one project."
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
        description = "Undo the latest transaction by `author` in the project that is not undone yet, applying its inverse as a new transaction: its graph edits to the running streams and their graph documents, its asset changes to the project. Only the author's own transactions are undone, so two authors' interleaved edits are each undone by their own author. Refused when the author has nothing left to undo, or when what it would undo no longer applies — a node another author removed since. Answers the transaction applied, with `undoes` naming the one it reversed."
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
    async fn one_graph_transaction(
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
            .apply(operations, mode, &author)
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

/// What a graph transaction works through on its blocking task.
struct GraphTransactionInFlight<'transaction> {
    operations_on_the_loaded_streams: &'transaction dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project_repositories: &'transaction ProjectRepositoriesOpenInThisRuntime,
    async_runtime: &'transaction tokio::runtime::Handle,
}

impl GraphTransactionInFlight<'_> {
    fn apply(
        &self,
        requests: Vec<StreamGraphOperationToolArgument>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
    ) -> Result<GraphTransactionToolResult, String> {
        if requests.is_empty() {
            return Err("a transaction holds at least one operation".to_owned());
        }
        let stream_locations: HashMap<String, Option<StreamLocation>> = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .map(|listing| (listing.stream_id, listing.stream_location))
            .collect();
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

        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        for stream_id in &stream_ids_in_order {
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)
                .map_err(|refusal| refusal.to_string())?;
            let stream_location = stream_locations.get(stream_id).cloned().flatten();
            let staged_graph = match (&project_repository_held, &stream_location) {
                (Some(project_repository), Some(stream_location)) => project_repository
                    .stream_graph_document(stream_location.stream_asset)
                    .map_err(|refusal| refusal.to_string())?,
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
        for request in requests {
            let stream_id = request.stream().to_owned();
            let applied = streams
                .get_mut(&stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))
                .and_then(|stream| {
                    let label = match &request {
                        StreamGraphOperationToolArgument::AddNode { label, .. } => label.clone(),
                        _ => None,
                    };
                    let operation = graph_operation_for_the_request(
                        request,
                        &stream.staged_graph,
                        &mut node_by_label,
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

        let (transaction, commit) = match project_repository_held.as_mut() {
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
        drop(project_repository_held);

        Ok(self.the_answer_of_a_graph_transaction(
            mode,
            transaction,
            commit,
            &streams,
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
        let stream_id_of_each_stream_asset: HashMap<Uuid, String> = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                (stream_location.project == project)
                    .then_some((stream_location.stream_asset, listing.stream_id))
            })
            .collect();
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
            let applied = stream_id_of_each_stream_asset
                .get(stream)
                .ok_or_else(|| {
                    format!(
                        "the stream asset {stream} it edits is not loaded; run the project, \
                         then try again"
                    )
                })
                .and_then(|stream_id| {
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
                });
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
                Ok(project_transaction_on_the_wire(applied))
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
        }
    }
}

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
fn graph_operation_for_the_request(
    request: StreamGraphOperationToolArgument,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
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
    match request {
        StreamGraphOperationToolArgument::AddNode {
            node_type,
            name,
            config,
            pins,
            studio,
            label,
            ..
        } => {
            let node = Uuid::new_v4();
            if let Some(label) = label
                && node_by_label.insert(label.clone(), node).is_some()
            {
                return Err(format!(
                    "two `add_node`s of this transaction are both `as` `{label}`"
                ));
            }
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name: name.unwrap_or_default(),
                    node_type,
                    config: Value::Object(config.unwrap_or_default()),
                    pins: pins.unwrap_or_default(),
                    studio: studio.unwrap_or_default(),
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
    }
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
