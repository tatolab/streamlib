// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime's MCP server, served by the official SDK, `rmcp`.
//!
//! [`LocalApiMcpServerHandler`] is the runtime's whole MCP surface. Its tools are generated from
//! the control operation registry's agent surface ([`crate::mcp_agent_tools`]): the operations
//! the registry lists as tools of their own, and `list_operation_groups`,
//! `describe_operations` and `call_operation`, which reach every other operation. Beside them
//! it serves the node catalog and the live graph as resources ([`crate::mcp_resources`]).
//! `rmcp` owns the protocol; this module owns only what the runtime says through it.
//!
//! A graph edit answers when the engine accepted the change into its graph;
//! the wiring itself commits on the engine's own compile task, whose failure
//! `graph` and `logs` show rather than this call.
//!
//! Each call into the runtime runs on a blocking task. A stream action answers
//! at once, queued on its project's stream action queue; every stream a run
//! loads is recorded, whichever connection carried the call, and outlives it.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ResultType, ServerCapabilities, ServerConfig, SubscriptionFilter, Tool,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde::Serialize;
use serde_json::Value;
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, ProjectVenvSyncPackage, ProjectVenvSyncPackageKind,
    ProjectVenvSyncPackageState, ProjectVenvSyncPhase, ProjectVenvSyncProgress, StreamAction,
    StreamActionKind, StreamActionState,
};
use streamlib_control_contract::ControlOperationCall;
use streamlib_project_repository::ProjectRepositoriesOpenInThisRuntime;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, DescribeStreamActionToolResult,
    DescribeStreamActionToolResultStream, ProjectActionRefusalOnTheWire, ProjectVenvSyncOnTheWire,
    ProjectVenvSyncPackageKindOnTheWire, ProjectVenvSyncPackageOnTheWire,
    ProjectVenvSyncPackageStateOnTheWire, ProjectVenvSyncPhaseOnTheWire, StreamActionKindOnTheWire,
    StreamActionStateOnTheWire,
};
use tokio_util::sync::CancellationToken;

use crate::control_operation_dispatcher::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin, listed_stream_state_on_the_wire, stream_action_step_on_the_wire,
};
use crate::control_operations_on_loaded_streams::ViewersOneConnectionHoldsOpen;

/// The protocol revisions `POST /mcp` serves: the latest `rmcp` implements
/// alone, since the endpoint demands the per-request metadata no earlier
/// revision carries.
const MCP_PROTOCOL_VERSIONS_SERVED_OVER_POST_MCP: &[ProtocolVersion] = &[ProtocolVersion::LATEST];

/// The protocol revisions a `/mcp/stdio` connection serves: every one `rmcp`
/// implements, so a host that still opens with an `initialize` handshake is
/// answered at the newest revision it named that has one, and a host at the
/// latest revision discovers and picks the latest.
const MCP_PROTOCOL_VERSIONS_SERVED_OVER_MCP_STDIO: &[ProtocolVersion] =
    ProtocolVersion::KNOWN_VERSIONS;

/// Server identity carried in every result's `serverInfo`.
const MCP_SERVER_NAME: &str = env!("CARGO_PKG_NAME");

/// Server version carried in `serverInfo` — the runtime's crate version.
const MCP_SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) use streamlib_control_contract::LOCAL_API_MCP_SERVER_INSTRUCTIONS;

/// Which local API connection a handler serves, and so which MCP protocol
/// versions it answers with.
#[derive(Clone, Copy)]
pub(crate) enum LocalApiMcpConnection {
    /// A `POST /mcp` request, which ends with its answer.
    OneShotStreamableHttpRequest,
    /// One `/mcp/stdio` upgraded connection.
    McpStdio,
}

/// The runtime's MCP server handler: its tools and resources over the
/// streams the runtime loads, for one connection.
#[derive(Clone)]
pub(crate) struct LocalApiMcpServerHandler {
    pub(crate) operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    local_api_stopping_token: CancellationToken,
    mcp_connection: LocalApiMcpConnection,
    viewers_this_connection_holds_open: Arc<ViewersOneConnectionHoldsOpen>,
    pub(crate) project_repositories_open_in_this_runtime: Arc<ProjectRepositoriesOpenInThisRuntime>,
    pub(crate) project_actions_refused_since_the_runtime_started:
        Arc<crate::mcp_project_change_feed::ProjectActionsRefusedSinceTheRuntimeStarted>,
    pub(crate) asset_media_facts_reads_in_this_runtime:
        Arc<crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime>,
    project_node_types_last_listed:
        Arc<crate::project_node_types_followed_for_changes::ProjectNodeTypesLastListed>,
    agent_tools_listed: Arc<Vec<Tool>>,
}

impl LocalApiMcpServerHandler {
    /// A handler for one-shot `POST /mcp` requests. `local_api_stopping_token`
    /// ends every held `subscriptions/listen` with its final result, so the
    /// server's graceful shutdown never waits on a host that holds one open.
    pub(crate) fn new(
        operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        local_api_stopping_token: CancellationToken,
    ) -> Self {
        let runtime_locking_python_requirements = Arc::clone(&operations_on_the_loaded_streams);
        let runtime_carrying_the_core_nodes_project = Arc::clone(&operations_on_the_loaded_streams);
        let project_repositories_open_in_this_runtime =
            ProjectRepositoriesOpenInThisRuntime::locking_python_requirements_with(Arc::new(
                move |project_directory| {
                    runtime_locking_python_requirements
                        .lock_the_python_requirements_of_the_project_in(project_directory)
                },
            ))
            .reading_the_core_project_the_runtime_carries_at(Arc::new(move || {
                runtime_carrying_the_core_nodes_project
                    .the_core_nodes_project_directory_this_runtime_carries()
            }));
        let project_repositories_open_in_this_runtime = Arc::new(
            match operations_on_the_loaded_streams.the_core_project_history_directory() {
                Some(core_project_history_directory) => project_repositories_open_in_this_runtime
                    .keeping_the_core_projects_history_in(core_project_history_directory),
                None => project_repositories_open_in_this_runtime,
            },
        );
        operations_on_the_loaded_streams.record_first_party_package_moves_in(Arc::new(
            crate::first_party_package_moves_recorded_in_the_open_projects::FirstPartyPackageMovesRecordedInTheOpenProjects {
                project_repositories_open_in_this_runtime: Arc::downgrade(
                    &project_repositories_open_in_this_runtime,
                ),
            },
        ));
        let asset_media_facts_reads_in_this_runtime = Arc::new(
            crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime::announcing_on(
                Arc::clone(&project_repositories_open_in_this_runtime),
                crate::asset_media_facts_reads::asset_media_facts_readers_beside_this_executable(),
            ),
        );
        Self {
            operations_on_the_loaded_streams,
            local_api_stopping_token,
            mcp_connection: LocalApiMcpConnection::OneShotStreamableHttpRequest,
            viewers_this_connection_holds_open: Arc::default(),
            project_repositories_open_in_this_runtime,
            project_actions_refused_since_the_runtime_started: Arc::default(),
            asset_media_facts_reads_in_this_runtime,
            project_node_types_last_listed: Arc::default(),
            agent_tools_listed: Arc::new(crate::mcp_agent_tools::the_agent_tools_listed_over_mcp()),
        }
    }

    /// This handler for one `/mcp/stdio` connection just upgraded.
    pub(crate) fn for_one_mcp_stdio_connection(&self) -> Self {
        Self {
            mcp_connection: LocalApiMcpConnection::McpStdio,
            viewers_this_connection_holds_open: Arc::default(),
            ..self.clone()
        }
    }

    /// The dispatcher over this handler's runtime services.
    pub(crate) fn control_operation_dispatcher(&self) -> ControlOperationDispatcher {
        ControlOperationDispatcher {
            operations_on_the_loaded_streams: Arc::clone(&self.operations_on_the_loaded_streams),
            project_repositories: Arc::clone(&self.project_repositories_open_in_this_runtime),
            project_actions_refused: Arc::clone(
                &self.project_actions_refused_since_the_runtime_started,
            ),
            asset_media_facts_reads: Arc::clone(&self.asset_media_facts_reads_in_this_runtime),
            project_node_types_last_listed: Arc::clone(&self.project_node_types_last_listed),
        }
    }

    /// Run `call` through the dispatcher and answer it as its tool does.
    pub(crate) async fn a_dispatched_control_operation(
        &self,
        call: ControlOperationCall,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> ToolCallAnswer {
        let method_name = call.method_name();
        let request_origin = ControlRequestOrigin {
            viewers_held_open: matches!(self.mcp_connection, LocalApiMcpConnection::McpStdio)
                .then(|| Arc::clone(&self.viewers_this_connection_holds_open)),
            ..control_request_origin_of_an_mcp_tool_call(tool_call_request_context)
        };
        let answered = self
            .control_operation_dispatcher()
            .dispatch(call, &request_origin)
            .await;
        self.the_tool_result_of_a_control_operation(method_name, answered)
    }

    /// A dispatched operation's answer as the tool `tool_name` answers it.
    pub(crate) fn the_tool_result_of_a_control_operation(
        &self,
        tool_name: &str,
        answered: Result<ControlOperationAnswer, ControlOperationRefusal>,
    ) -> ToolCallAnswer {
        match answered {
            Ok(answer) => serialized_tool_result(tool_name, &answer),
            Err(refusal) => Err(Box::new(refusal.into_the_refusal_on_the_wire(tool_name))),
        }
    }
}

/// Where an MCP tool call came from: the name its client gave in `clientInfo`.
pub(crate) fn control_request_origin_of_an_mcp_tool_call(
    tool_call_request_context: &RequestContext<RoleServer>,
) -> ControlRequestOrigin {
    ControlRequestOrigin {
        client_name: tool_call_request_context
            .client_info()
            .map(|client| client.name),
        subscriptions: None,
        viewers_held_open: None,
    }
}

/// `/mcp`'s service: `rmcp`'s Streamable HTTP transport over one
/// [`LocalApiMcpServerHandler`], stateless, serving only
/// [`MCP_PROTOCOL_VERSIONS_SERVED_OVER_POST_MCP`].
pub(crate) fn local_api_mcp_streamable_http_service(
    handler: LocalApiMcpServerHandler,
) -> StreamableHttpService<LocalApiMcpServerHandler, NeverSessionManager> {
    StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_stateless_protocol_metadata_required(true)
            .with_json_response(true)
            // The socket's file mode is the gate; DNS rebinding needs a TCP
            // port a browser can reach, and the local API has none.
            .disable_allowed_hosts(),
    )
}

// ============================================================================
// Tool arguments
// ============================================================================

/// A tool's answer: its result, or the refusal an `isError` result carries.
pub(crate) type ToolCallAnswer = Result<CallToolResult, Box<ProjectActionRefusalOnTheWire>>;

/// A stream action's state as the local API spells it.
pub(crate) fn stream_action_state_on_the_wire(
    state: StreamActionState,
) -> StreamActionStateOnTheWire {
    match state {
        StreamActionState::Queued => StreamActionStateOnTheWire::Queued,
        StreamActionState::InProgress => StreamActionStateOnTheWire::InProgress,
        StreamActionState::Done => StreamActionStateOnTheWire::Done,
        StreamActionState::Failed => StreamActionStateOnTheWire::Failed,
        StreamActionState::Cancelled => StreamActionStateOnTheWire::Cancelled,
    }
}

/// A project's venv sync as the local API spells it.
fn project_venv_sync_on_the_wire(venv_sync: ProjectVenvSyncProgress) -> ProjectVenvSyncOnTheWire {
    ProjectVenvSyncOnTheWire {
        phase: match venv_sync.phase {
            ProjectVenvSyncPhase::Resolving => ProjectVenvSyncPhaseOnTheWire::Resolving,
            ProjectVenvSyncPhase::Downloading => ProjectVenvSyncPhaseOnTheWire::Downloading,
            ProjectVenvSyncPhase::Building => ProjectVenvSyncPhaseOnTheWire::Building,
            ProjectVenvSyncPhase::Installing => ProjectVenvSyncPhaseOnTheWire::Installing,
            ProjectVenvSyncPhase::Installed => ProjectVenvSyncPhaseOnTheWire::Installed,
        },
        packages_resolved: venv_sync.packages_resolved,
        packages_prepared: venv_sync.packages_prepared,
        packages_installed: venv_sync.packages_installed,
        downloads_started: venv_sync.downloads_started(),
        downloads_done: venv_sync.downloads_done(),
        packages: venv_sync
            .packages
            .into_iter()
            .map(project_venv_sync_package_on_the_wire)
            .collect(),
    }
}

/// One package of a project's venv sync as the local API spells it.
fn project_venv_sync_package_on_the_wire(
    package: ProjectVenvSyncPackage,
) -> ProjectVenvSyncPackageOnTheWire {
    ProjectVenvSyncPackageOnTheWire {
        name: package.name,
        version: package.version,
        kind: match package.kind {
            ProjectVenvSyncPackageKind::Distribution => {
                ProjectVenvSyncPackageKindOnTheWire::Distribution
            }
            ProjectVenvSyncPackageKind::PythonInterpreter => {
                ProjectVenvSyncPackageKindOnTheWire::PythonInterpreter
            }
        },
        state: match package.state {
            ProjectVenvSyncPackageState::Downloading => {
                ProjectVenvSyncPackageStateOnTheWire::Downloading
            }
            ProjectVenvSyncPackageState::Downloaded => {
                ProjectVenvSyncPackageStateOnTheWire::Downloaded
            }
            ProjectVenvSyncPackageState::Building => ProjectVenvSyncPackageStateOnTheWire::Building,
            ProjectVenvSyncPackageState::Built => ProjectVenvSyncPackageStateOnTheWire::Built,
            ProjectVenvSyncPackageState::Installed => {
                ProjectVenvSyncPackageStateOnTheWire::Installed
            }
            ProjectVenvSyncPackageState::Uninstalled => {
                ProjectVenvSyncPackageStateOnTheWire::Uninstalled
            }
        },
        download_size_bytes: package.download_size_bytes,
    }
}

/// A stream action as `describe_stream_action` answers it.
pub(crate) fn describe_stream_action_tool_result_of(
    stream_action: StreamAction,
) -> DescribeStreamActionToolResult {
    DescribeStreamActionToolResult {
        action: stream_action.action_id,
        project_directory: stream_action.project_directory,
        kind: match stream_action.kind {
            StreamActionKind::Open => StreamActionKindOnTheWire::Open,
            StreamActionKind::Load => StreamActionKindOnTheWire::Load,
            StreamActionKind::StartPlayback => StreamActionKindOnTheWire::StartPlayback,
            StreamActionKind::Stop => StreamActionKindOnTheWire::Stop,
            StreamActionKind::Close => StreamActionKindOnTheWire::Close,
            StreamActionKind::Remove => StreamActionKindOnTheWire::Remove,
            StreamActionKind::Delete => StreamActionKindOnTheWire::Delete,
            StreamActionKind::ReloadAtStart => StreamActionKindOnTheWire::ReloadAtStart,
            StreamActionKind::MakeTheVenvCurrent => StreamActionKindOnTheWire::MakeVenvCurrent,
        },
        state: stream_action_state_on_the_wire(stream_action.state),
        step: stream_action.step.map(stream_action_step_on_the_wire),
        venv_sync: stream_action.venv_sync.map(project_venv_sync_on_the_wire),
        streams: stream_action
            .streams
            .into_iter()
            .map(|stream| DescribeStreamActionToolResultStream {
                stream: stream.stream_id,
                display_name: stream.display_name,
                state: stream.state.map(listed_stream_state_on_the_wire),
            })
            .collect(),
        failed_because: stream_action.failed_because,
    }
}

impl ServerHandler for LocalApiMcpServerHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new(MCP_SERVER_NAME, MCP_SERVER_VERSION))
        .with_instructions(LOCAL_API_MCP_SERVER_INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        match self.mcp_connection {
            LocalApiMcpConnection::OneShotStreamableHttpRequest => {
                Cow::Borrowed(MCP_PROTOCOL_VERSIONS_SERVED_OVER_POST_MCP)
            }
            LocalApiMcpConnection::McpStdio => {
                Cow::Borrowed(MCP_PROTOCOL_VERSIONS_SERVED_OVER_MCP_STDIO)
            }
        }
    }

    /// No list ever changes under a host; a listen subscribes only to projects' change feeds
    /// and the stream action resources, and is held until it is cancelled.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let accepted_resource_subscriptions: Vec<String> =
            crate::mcp_project_change_feed::accepted_project_transactions_subscriptions(requested)
                .resource_subscriptions
                .unwrap_or_default()
                .into_iter()
                .chain(
                    crate::mcp_stream_action_resources::accepted_stream_action_subscriptions(
                        requested,
                    ),
                )
                .collect();
        if accepted_resource_subscriptions.is_empty() {
            return Some(SubscriptionFilter::default());
        }
        Some(
            SubscriptionFilter::builder()
                .resource_subscriptions(accepted_resource_subscriptions)
                .build(),
        )
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        tokio::join!(
            crate::mcp_project_change_feed::announce_project_transactions_until_the_listen_ends(
                &self.project_repositories_open_in_this_runtime,
                &context,
                &self.local_api_stopping_token,
            ),
            crate::mcp_stream_action_resources::announce_stream_action_changes_until_the_listen_ends(
                &self.operations_on_the_loaded_streams,
                &context,
                &self.local_api_stopping_token,
            ),
        );
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.agent_tools_listed.to_vec(),
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(CacheScope::Public),
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.agent_tools_listed
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        Ok(
            match self
                .the_answer_to_an_agent_tool_call(request, &context)
                .await?
            {
                Ok(tool_result) => tool_result.into(),
                Err(refusal) => {
                    crate::mcp_agent_tools::a_tool_result_of_the_refusal(*refusal).into()
                }
            },
        )
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            crate::mcp_project_change_feed::project_transactions_resource_template(),
            crate::mcp_stream_action_resources::stream_action_resource_template(),
        ]))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(crate::mcp_resources::resources_list_result())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if let Some(project) =
            crate::mcp_project_change_feed::the_project_a_transactions_resource_uri_names(
                &request.uri,
            )
        {
            return crate::mcp_project_change_feed::read_project_transactions_resource(
                Arc::clone(&self.project_repositories_open_in_this_runtime),
                Arc::clone(&self.project_actions_refused_since_the_runtime_started),
                Arc::clone(&self.asset_media_facts_reads_in_this_runtime),
                &request.uri,
                project,
            )
            .await
            .map(Into::into);
        }
        if let Some(read) = crate::mcp_stream_action_resources::read_stream_action_resource(
            &self.operations_on_the_loaded_streams,
            &request.uri,
        )
        .await
        {
            return read.map(Into::into);
        }
        crate::mcp_resources::read_resource(&self.operations_on_the_loaded_streams, &request.uri)
            .await
            .map(Into::into)
    }
}

// ============================================================================
// Result content
// ============================================================================

/// `tool_result` as `tool_name`'s successful result.
pub(crate) fn serialized_tool_result(
    tool_name: &str,
    tool_result: &impl Serialize,
) -> ToolCallAnswer {
    let tool_result_json = serde_json::to_value(tool_result).map_err(|e| {
        Box::new(ProjectActionRefusalOnTheWire::of_the_action(
            tool_name,
            ControlOperationRefusalCode::RuntimeFailed,
            format!("{tool_name} result serialization failed: {e}"),
        ))
    })?;
    Ok(json_text_tool_result(tool_result_json))
}

/// A successful tool result: the value as one pretty-JSON text block, and a JSON object also as
/// `structuredContent`, which a tool with an `outputSchema` answers.
fn json_text_tool_result(value: Value) -> CallToolResult {
    let mut tool_result = CallToolResult::success(vec![json_text_content_block(&value)]);
    if value.is_object() {
        tool_result.structured_content = Some(value);
    }
    tool_result
}

pub(crate) fn json_text_content_block(value: &Value) -> ContentBlock {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ContentBlock::text(text)
}

#[cfg(test)]
pub(crate) mod tests;
