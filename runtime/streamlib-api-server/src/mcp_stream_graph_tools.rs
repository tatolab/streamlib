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
use serde_json::json;
use streamlib::sdk::error::{Error, WhyANodeOfAStreamIsNotRunning};
use streamlib_control_contract::{
    AUTHOR_ARGUMENT_DESCRIPTION, AddGroup, AddGroupRequest, AddNode, AddNodeRequest, AddNote,
    AddNoteRequest, CollapseSelectionIntoSubgraph, CollapseSelectionIntoSubgraphRequest, Connect,
    ControlOperation, ControlOperationCall, Disconnect, GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION,
    GraphEditMode, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ImportGraphGenerator,
    ImportGraphGeneratorRequest, OneLinkGraphEditRequest, OneNodeGraphEditRequest,
    ProjectTransactionAuthorArgument, RemoveGroup, RemoveGroupRequest, RemoveNode, RemoveNote,
    RemoveNoteRequest, RenameNode, RenameNodeRequest, SetExposedConfigFields,
    SetExposedConfigFieldsRequest, SetGroupFields, SetGroupFieldsRequest, SetNodeConfig,
    SetNodeConfigRequest, SetNoteFields, SetNoteFieldsRequest, SetPinValue, SetPinValueRequest,
    SetStudioFields, SetStudioFieldsRequest, Transact, TransactRequest, UnlockGeneratorFragment,
    UnlockGeneratorFragmentRequest,
};
use streamlib_project_repository::{
    ProjectRepository, ProjectTransactionAuthor, ProjectTransactionReversal, StreamGraphAction,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphTransactionToolResult, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
};

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

    #[tool(description = ImportGraphGenerator::AGENT_USAGE_NOTE)]
    async fn import_graph_generator(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ImportGraphGeneratorRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ImportGraphGenerator(request),
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

    #[tool(description = RemoveNode::AGENT_USAGE_NOTE)]
    async fn remove_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneNodeGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveNode(request),
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

    #[tool(description = Disconnect::AGENT_USAGE_NOTE)]
    async fn disconnect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneLinkGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Disconnect(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetNodeConfig::AGENT_USAGE_NOTE)]
    async fn set_node_config(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetNodeConfigRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetNodeConfig(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetPinValue::AGENT_USAGE_NOTE)]
    async fn set_pin_value(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetPinValueRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetPinValue(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RenameNode::AGENT_USAGE_NOTE)]
    async fn rename_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RenameNodeRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RenameNode(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetStudioFields::AGENT_USAGE_NOTE)]
    async fn set_studio_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetStudioFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetStudioFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetExposedConfigFields::AGENT_USAGE_NOTE)]
    async fn set_exposed_config_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetExposedConfigFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetExposedConfigFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = AddGroup::AGENT_USAGE_NOTE)]
    async fn add_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddGroupRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddGroup(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetGroupFields::AGENT_USAGE_NOTE)]
    async fn set_group_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetGroupFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetGroupFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = CollapseSelectionIntoSubgraph::AGENT_USAGE_NOTE)]
    async fn collapse_selection_into_subgraph(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<CollapseSelectionIntoSubgraphRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::CollapseSelectionIntoSubgraph(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = UnlockGeneratorFragment::AGENT_USAGE_NOTE)]
    async fn unlock_generator_fragment(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UnlockGeneratorFragmentRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::UnlockGeneratorFragment(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = AddNote::AGENT_USAGE_NOTE)]
    async fn add_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddNoteRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddNote(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetNoteFields::AGENT_USAGE_NOTE)]
    async fn set_note_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetNoteFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetNoteFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveNote::AGENT_USAGE_NOTE)]
    async fn remove_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RemoveNoteRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveNote(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveGroup::AGENT_USAGE_NOTE)]
    async fn remove_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RemoveGroupRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveGroup(request),
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
