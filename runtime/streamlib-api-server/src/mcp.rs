// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's MCP server, served by the official SDK, `rmcp`.
//!
//! [`LocalApiMcpServerHandler`] is the runtime's whole MCP surface: its tools
//! are the control vocabulary — the observation verbs `graph`, `logs` and
//! `describe_runtime`; the graph-mutation verbs `add_node`, `remove_node`,
//! `connect` and `disconnect`, each naming a node by its name; and the stream
//! actions `run_stream`, `run_project`, `describe_stream_action`,
//! `cancel_stream_action`, `list_project_streams`, `stop_stream`, `start_stream`, `remove_project`,
//! `list_streams` and `expose_port`; and the relay verbs `add_relay`,
//! `configure_relay`, `remove_relay` and `list_relays` — and beside them it
//! serves the node catalog, the live graph and the relay-adapter catalog as
//! resources ([`crate::mcp_resources`]) and
//! recipes over those tools as prompts ([`crate::mcp_prompts`]). `rmcp` owns
//! the protocol; this module owns only what the runtime says through it.
//!
//! The runtime loads any number of streams. Every tool that reads or changes
//! one stream takes a required `stream`, refused naming the loaded streams
//! when it is not loaded; `graph` takes it optionally and renders every loaded
//! stream without it. `list_streams` names no stream.
//!
//! A mutation tool answers when the engine accepted the change into its graph;
//! the wiring itself commits on the engine's own compile task, whose failure
//! `graph` and `logs` show rather than this call.
//!
//! A stream action blocks — a load runs the project's compile and describes
//! its types — so each runs on a blocking task. Every stream `run_stream`
//! loads is recorded, whichever connection carried the call, and outlives it.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities,
    ServerConfig, SubscriptionFilter,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use rmcp::{prompt_handler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::{Error, Result as StreamlibResult};
use streamlib::sdk::graph::{
    InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, OutputPortExposureLevel,
};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{
    LoadedStreamTag, OperationsOnTheStreamsLoadedInThisRuntime, ProjectStreamsListing,
    RunProjectRequest, RunStreamRequest, RuntimeOperations, StreamAction, StreamActionKind,
    StreamActionState, StreamActionStep, StreamListing, StreamListingState,
    StreamLoadedOrFailedByARun, StreamLocation, StreamStateAfterItsRun,
    WhatTheLastCompileOfTheProjectGave,
};
use streamlib_relay_adapter_sdk::{RelayInstanceControlRefusal, RelayInstancesOfThisMachine};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AddRelayToolResult, CancelStreamActionToolResult, ConfigureRelayToolResult,
    DescribeStreamActionToolResult, DescribeStreamActionToolResultStream, ExposePortLevel,
    ExposePortToolResult, ListProjectStreamsToolResult, ListProjectStreamsToolResultStream,
    ListRelaysToolResult, ListStreamsToolResult, ListStreamsToolResultStream, ListedStreamState,
    LoadedStreamInstance, LogsToolResult, RemoveProjectToolResult, RemoveRelayToolResult,
    RenameStreamToolResult, RunProjectToolResult, RunStreamToolResult, RunStreamToolResultStream,
    RunStreamToolResultStreamState, StartStreamToolResult, StopStreamToolResult,
    StreamActionKindOnTheWire, StreamActionStateOnTheWire, StreamActionStepOnTheWire,
    StreamLocationOnTheWire,
};
use streamlib_runtime_client_contract::project_log_records::{
    ProjectLogRecordsPage, project_log_records_after,
};
use tokio_util::sync::CancellationToken;

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

/// The guidance `server/discover` hands an agent before its first call.
pub(crate) const LOCAL_API_MCP_SERVER_INSTRUCTIONS: &str = "StreamLib's runtime control plane: this machine's one runtime, which loads any number of streams, each a project's stream function compiled in that project's own interpreter. `run_project` runs a project from its absolute `project_directory`: it answers at once with an `action` id while the project's own queue makes its `.venv`, compiles every `@stream` in its `stream.py` and starts each — however long uv takes — one action at a time per project, projects side by side; follow it with `describe_stream_action` until its `state` is `done`, `failed` or `cancelled`, end it early with `cancel_stream_action`, and read what it wrote with `logs` given its `action`; `list_project_streams` lists the project's `@stream` functions as its last compile named them, running none of its code. `list_streams` lists every stream, each by its stream id with its display name and location, running, stopped, failed or `preparing` — a stream an action is working on, with the `action` and its `step` — a stream implicated in the runtime's last two crashes in a row, or one that could not re-load at its start, is failed, carries why in `failed_because`, and is skipped at the start. `run_stream` compiles and loads one from an absolute `project_directory` — `stream_function` a target (`stream.py:main`, or none for every `@stream` in `stream.py`, each its own stream), `name` to show it under another display name — and answers its stream id, `<folder>-<function>-<hash>`, computed from its location: the resolved project directory and the function's `module:qualname`. Run again, the same location replaces its stream under the same id. A load is never refused for what the project's code does: a stream whose function raises, whose load or start is refused, or whose project's `stream.py` does not import or defines no `@stream` is returned in `streams` with `state` `failed` and its traceback or reason in `failed_because`, listed failed until a load of the fixed code replaces it or `start_stream` compiles it again; only the caller's own mistakes — a directory that is not a project, a function nothing defines or records, an id another location holds — are refused. Every stream it loads is recorded and re-loaded whenever the runtime starts, until `stop_stream` or `remove_project` ends it: closing the connection that carried the call unloads nothing. `stop_stream` queues the unload of a stream that records it stopped, `start_stream` queues a load of a stopped one and a retry of a failed one, and `remove_project` queues the unload and forgetting of every stream of a project — each answering at once with its `action`, which `describe_stream_action` follows, and each on its stream's project queue; `rename_stream` shows a stream under another display name — recorded at once, never waiting behind an action, and never changing its id, address or broadcast — and `expose_port` puts an output port at `internal`, `private` or `public` live, cutting off at once every reader the level no longer allows, and records the level on the stream's record. Every tool about one stream — `add_node`, `remove_node`, `connect`, `disconnect` and the stream actions — names it by its stream id with a required `stream`, never by its display name; `graph` names one optionally, and without it returns every loaded stream's graph under `machine`, this machine's name; `logs` takes the project's absolute `project_directory` and names a stream or a stream action optionally. `describe_runtime` names no stream and answers this runtime's `version`, its `build_id`, `up_for_ms` since its engine started (a restart resets it) and `runtime_log_directory`, the folder of its own log, `null` when it keeps none. Observe a stream with `graph` (nodes by name, their types and port names, links, and each node's `metrics`: the bags each output port wrote and each inbound link dropped) and `logs` (the project's one log, every stream's records and its runs' venv and compile lines, read from disk so they outlive the stream and the runtime's restarts: pass the result's `next_after` back as `after` to read on). What an output port carries is read from outside its stream only by exposing it with `expose_port`: an exposed port is the track `<node>/<port>` of the broadcast `<machine>/<stream id>.hang` at the local relay — a private one at its loopback listener alone — and a public one at every relay the machine has joined, which any MoQ client reads. The relays are `list_relays`'s: the local relay's two built-in instances and any added with `add_relay` through an adapter the resource `streamlib://relay-adapter-catalog` lists with its settings; `configure_relay` replaces settings, a secret among them kept in the OS secret store and only ever listed as `set` or `unset`, and `remove_relay` forgets an added one. Change its live graph with `add_node`, `connect`, `disconnect` and `remove_node`, each naming a node by its name: a Python class written to a module the stream's own interpreter can import — a file in the stream's project, or a package installed in its venv — is added by its `module:ClassName` path and runs in its own processor interpreter; a link is spliced in by connecting the new node on both sides, then disconnecting the link it replaces. Read `graph` first for names and port names, and again afterwards to confirm a link's state is `wired` and the node is `Running`. A `connect` onto a node in a helper process returns before that helper has opened its port, so its link reads `pending` until the helper answers and then `wired`; a link that reads `error` carries the refusing end's own reason in `error_reason` and will never carry a bag — read the reason, `disconnect` it, and fix what it names. Both ends of a `connect` are ports in the one stream it names; a stream reads another stream's port, on this machine or another, through a `RelayTrackSource` node. The resource `streamlib://node-catalog` lists the native types every stream can add, then each loaded stream's Python types under that stream, each with its config schema and ports; `streamlib://graph` is every loaded stream's live graph. The prompts are step-by-step recipes over these tools, each rendered against the `stream` it names: inserting a node into a link, fanning an output to another consumer, and showing a channel on a virtual camera.";

/// How many log records one `logs` call returns when the caller names no
/// `count`.
const DEFAULT_LOGS_RECORD_COUNT: u32 = 256;

/// The most log records one `logs` call returns: as many as a stream's log
/// route holds in memory.
const MAX_LOGS_RECORD_COUNT: u32 = 4096;

/// What every required `stream` argument says.
const STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream this call is about, by the stream id `run_stream`, `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";

/// What every `project_directory` argument naming one project says.
const PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str = "The project, by absolute path: a folder holding `pyproject.toml` and a `stream.py` defining its `@stream` functions.";

/// What `graph`'s optional `stream` argument says.
const GRAPH_STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream whose graph to export, by the stream id `list_streams` reports. Omit it for every loaded stream's graph under the machine's name.";

/// What a stream action's `stream` argument says.
const STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION: &str =
    "The stream, by the stream id `list_streams` reports: loaded, or recorded and not loaded.";

/// Which local API connection a handler serves, and so which MCP protocol
/// versions it answers with.
#[derive(Clone, Copy)]
pub(crate) enum LocalApiMcpConnection {
    /// A `POST /mcp` request, which ends with its answer.
    OneShotStreamableHttpRequest,
    /// One `/mcp/stdio` upgraded connection.
    McpStdio,
}

/// The runtime's MCP server handler: its tools, resources and prompts over the
/// streams the runtime loads, for one connection.
#[derive(Clone)]
pub(crate) struct LocalApiMcpServerHandler {
    pub(crate) operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
    local_api_stopping_token: CancellationToken,
    mcp_connection: LocalApiMcpConnection,
    tool_router: Arc<ToolRouter<Self>>,
    prompt_router: Arc<PromptRouter<Self>>,
}

impl LocalApiMcpServerHandler {
    /// A handler for one-shot `POST /mcp` requests. `local_api_stopping_token`
    /// ends every held `subscriptions/listen` with its final result, so the
    /// server's graceful shutdown never waits on a host that holds one open.
    pub(crate) fn new(
        operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
        local_api_stopping_token: CancellationToken,
    ) -> Self {
        Self {
            operations_on_the_loaded_streams,
            relay_instances,
            local_api_stopping_token,
            mcp_connection: LocalApiMcpConnection::OneShotStreamableHttpRequest,
            tool_router: Arc::new(Self::tool_router()),
            prompt_router: Arc::new(Self::prompt_router()),
        }
    }

    /// This handler for one `/mcp/stdio` connection just upgraded.
    pub(crate) fn for_one_mcp_stdio_connection(&self) -> Self {
        Self {
            mcp_connection: LocalApiMcpConnection::McpStdio,
            ..self.clone()
        }
    }

    /// The operations on the stream a `tool` call names, or the refusal —
    /// naming the loaded streams — its caller reads.
    fn the_stream_a_tool_call_names(
        &self,
        tool: &str,
        stream: &str,
    ) -> Result<Arc<dyn RuntimeOperations>, String> {
        self.operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(stream)
            .map_err(|refusal| format!("{tool} failed: {refusal}"))
    }

    /// Run the blocking stream action `action` over the runtime's streams on a
    /// blocking task; a refusal is the engine's own words.
    async fn a_stream_action_off_the_async_runtime<StreamActionOutcome: Send + 'static>(
        &self,
        action: impl FnOnce(
            &dyn OperationsOnTheStreamsLoadedInThisRuntime,
        ) -> StreamlibResult<StreamActionOutcome>
        + Send
        + 'static,
    ) -> Result<StreamActionOutcome, String> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || action(operations_on_the_loaded_streams.as_ref()))
            .await
            .map_err(|join_failure| format!("the stream action did not finish: {join_failure}"))?
            .map_err(|refusal: Error| refusal.to_string())
    }
}

impl LocalApiMcpServerHandler {
    /// Run `call` on the machine's relay instances on a blocking task; a
    /// refusal is the runtime's own words.
    async fn a_relay_call_off_the_async_runtime<RelayCallOutcome: Send + 'static>(
        &self,
        call: impl FnOnce(
            &dyn RelayInstancesOfThisMachine,
        ) -> Result<RelayCallOutcome, RelayInstanceControlRefusal>
        + Send
        + 'static,
    ) -> Result<RelayCallOutcome, String> {
        let relay_instances = Arc::clone(&self.relay_instances);
        tokio::task::spawn_blocking(move || call(relay_instances.as_ref()))
            .await
            .map_err(|join_failure| format!("the relay call did not finish: {join_failure}"))?
            .map_err(|refusal| refusal.to_string())
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

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct GraphToolArguments {
    #[schemars(description = GRAPH_STREAM_ARGUMENT_DESCRIPTION)]
    stream: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct LogsToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Only the records of this stream, by its stream id. Omit it for every record of the project, its loads' among them."
    )]
    stream: Option<String>,
    #[schemars(description = "Only the records of this stream action, by its action id.")]
    action: Option<String>,
    #[schemars(
        description = "Return the records after this opaque cursor, the previous result's `next_after`. Omit it for the oldest the project's log holds."
    )]
    after: Option<String>,
    #[schemars(
        range(min = 1),
        description = "The most records to return. Defaults to 256; a larger value than 4096 is clamped to it."
    )]
    count: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddNodeToolArguments {
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[serde(rename = "type")]
    #[schemars(
        description = "The node's class import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`."
    )]
    processor_class_import_path: String,
    #[schemars(
        description = "The node's configuration, as the keys its config schema declares. Omit for none."
    )]
    config: Option<Map<String, Value>>,
    #[schemars(
        description = "The node's name: cast to lowercase URL-safe (`Front Camera` becomes `front-camera`), and refused when a node already has that name. Omit it to take the class's short name, cast, with the next free `-2`, `-3` … when one is taken. The name is the node's part of its ports' addresses."
    )]
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveNodeToolArguments {
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = "A node's name, as `graph` or `add_node` reported it.")]
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ConnectToolArguments {
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = "The source node's name.")]
    from_node: String,
    #[schemars(description = "The source's output port name.")]
    from_port: String,
    #[schemars(description = "The destination node's name.")]
    to_node: String,
    #[schemars(description = "The destination's input port name.")]
    to_port: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DisconnectToolArguments {
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = "A link id `graph` or `connect` reported.")]
    link_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RunStreamToolArguments {
    #[schemars(
        description = "The project whose stream function to run, by absolute path. Its `.venv/bin/python` compiles the function; when that interpreter does not run, the runtime makes the `.venv` first with the uv it carries."
    )]
    project_directory: PathBuf,
    #[schemars(
        description = "The stream function as a target — `stream.py:main`, `x.py`, `mod.sub:fn`. Omit it to run every `@stream` in the project's `stream.py`, each its own stream; one that `stream.py` no longer defines fails its recorded stream, naming the function. A function the project no longer defines fails its recorded stream; one nothing defines or records is refused."
    )]
    stream_function: Option<String>,
    #[schemars(
        description = "The display name to show the stream under, in place of the function's own. The stream id comes from the location alone."
    )]
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RunProjectToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveProjectToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DescribeStreamActionToolArguments {
    #[schemars(description = "The stream action, by the action id `run_project` answered.")]
    action: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CancelStreamActionToolArguments {
    #[schemars(
        description = "The stream action, by the action id `run_project`, `start_stream`, `stop_stream` or `remove_project` answered."
    )]
    action: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListProjectStreamsToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct StreamActionToolArguments {
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RenameStreamToolArguments {
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(
        description = "The label to show the stream under: any text with something to show, never cast and never unique. The stream id, its address and its broadcast stay as they are."
    )]
    display_name: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListStreamsToolArguments {}

/// An output port's exposure level as `expose_port` takes it.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum OutputPortExposureLevelToolArgument {
    /// Read by the stream's own nodes alone.
    Internal,
    /// Read by any other stream, and any code, on this machine as well.
    Private,
    /// Read off this machine as well.
    Public,
}

impl From<OutputPortExposureLevelToolArgument> for OutputPortExposureLevel {
    fn from(level: OutputPortExposureLevelToolArgument) -> Self {
        match level {
            OutputPortExposureLevelToolArgument::Internal => OutputPortExposureLevel::Internal,
            OutputPortExposureLevelToolArgument::Private => OutputPortExposureLevel::Private,
            OutputPortExposureLevelToolArgument::Public => OutputPortExposureLevel::Public,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ExposePortToolArguments {
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = "The node's name, as `graph` lists it.")]
    node: String,
    #[schemars(description = "The node's output port, as `graph` lists it under `ports.outputs`.")]
    port: String,
    #[schemars(
        description = "`internal` for the stream's own nodes alone, `private` for any reader on this machine, `public` for readers off it as well."
    )]
    level: OutputPortExposureLevelToolArgument,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddRelayToolArguments {
    #[schemars(
        description = "The adapter's kind, as the resource `streamlib://relay-adapter-catalog` lists it."
    )]
    adapter: String,
    #[schemars(
        description = "The instance's settings, keyed as the adapter's `settings` in the catalog name them; each secret among them is kept in the OS secret store, never recorded. Absent is no settings."
    )]
    settings: Option<Map<String, Value>>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ConfigureRelayToolArguments {
    #[schemars(description = "The relay instance, as `list_relays` names it.")]
    relay_id: String,
    #[schemars(
        description = "The settings to replace, each by name; a setting left out keeps its value, and `null` returns one to its default. A secret given here replaces the one held."
    )]
    settings: Map<String, Value>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveRelayToolArguments {
    #[schemars(description = "The relay instance, as `list_relays` names it.")]
    relay_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListRelaysToolArguments {}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DescribeRuntimeToolArguments {}

/// A tool's answer: its result, or the message an `isError` result carries.
type ToolCallAnswer = Result<CallToolResult, String>;

// ============================================================================
// Tools
// ============================================================================

#[tool_router]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Export a loaded stream's current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, the ports it exposes, with each node's and link's live state and counters beside them, and this machine's name (`machine`), the first part of `<machine>/<stream id>/<node>/<port>`. Without `stream`, `{machine, streams}` holds every loaded stream's graph."
    )]
    async fn graph(
        &self,
        Parameters(GraphToolArguments { stream }): Parameters<GraphToolArguments>,
    ) -> ToolCallAnswer {
        let graph = match stream {
            Some(stream) => self
                .the_stream_a_tool_call_names("graph", &stream)?
                .to_json_async()
                .await
                .map_err(|e| format!("graph export failed: {e}"))?,
            None => {
                crate::handlers::machine_wide_graph_json(&self.operations_on_the_loaded_streams)
                    .await
                    .map_err(|e| format!("graph export failed: {e}"))?
            }
        };
        Ok(json_text_tool_result(&graph))
    }

    #[tool(
        description = "Read a project's log records after the cursor `after`, oldest first, each as the project's JSONL log on disk holds it: every stream of the project and every load of it writes that one log, each record stamped with its `stream` — none for a load's own lines, uv's and the compile's among them — across the runtime's runs, whether the stream is still recorded or not. `stream` narrows to one stream's records, and `action` to the records of one stream action — every record its load wrote, uv's and the compile's among them, and the records of the streams it started. Pass the result's `next_after` back as `after` to read on; `records_no_longer_held` is true when retention deleted the segment `after` named, so the page resumes at the oldest record held. A project that has logged nothing answers no records."
    )]
    async fn logs(
        &self,
        Parameters(LogsToolArguments {
            project_directory,
            stream,
            action,
            after,
            count,
        }): Parameters<LogsToolArguments>,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let max_count = count
            .unwrap_or(DEFAULT_LOGS_RECORD_COUNT)
            .clamp(1, MAX_LOGS_RECORD_COUNT) as usize;
        let read_project_directory = project_directory.clone();
        let ProjectLogRecordsPage {
            records,
            next_after,
            records_no_longer_held,
        } = tokio::task::spawn_blocking(move || {
            project_log_records_after(
                &read_project_directory,
                stream.as_deref(),
                action.as_deref(),
                after.as_deref(),
                max_count,
            )
        })
        .await
        .map_err(|join_failure| format!("logs failed: the read did not finish: {join_failure}"))?
        .map_err(|not_read| format!("logs failed: {not_read}"))?;
        serialized_tool_result(
            "logs",
            &LogsToolResult {
                project_directory,
                records,
                next_after,
                records_no_longer_held,
            },
        )
    }

    #[tool(
        description = "Add a node to the running graph by its class import path — the `type` string `graph` reports for every node and `streamlib://node-catalog` lists. A Python class is named `module:QualifiedClassName` and must be importable by the stream's own interpreter (a module in the stream's project, or a package installed in its venv): the runtime never imports it, and describes it in that interpreter the first time it is added; a built-in is named by the `type` an existing node of that kind shows. Returns the name the node received, which `connect` and `remove_node` take, once the engine has spawned it — a Python class in its own processor interpreter, which imports the module fresh, so edited code is picked up by every new add. The class's port declaration is read when it is first described and kept; to change a class's ports, add it under a new class name. Read `graph` to see its state and ports."
    )]
    async fn add_node(
        &self,
        Parameters(arguments): Parameters<AddNodeToolArguments>,
    ) -> ToolCallAnswer {
        let stream_operations = self.the_stream_a_tool_call_names("add_node", &arguments.stream)?;
        let processor_class_import_path =
            ProcessorClassImportPath::new(&arguments.processor_class_import_path)
                .map_err(|e| format!("add_node `type`: {e}"))?;
        // Absent is an empty object: a config struct deserializes from `{}`.
        let config = Value::Object(arguments.config.unwrap_or_default());
        let mut spec = ProcessorSpec::new(processor_class_import_path, config);
        spec.display_name = arguments.name;

        let added = stream_operations
            .add_processor_async(spec)
            .await
            .map_err(|e| format!("add_node failed: {e}"))?;
        Ok(json_text_tool_result(&json!({ "name": added.name })))
    }

    #[tool(
        description = "Remove a node from the running graph by name, stopping it. Its links go with it."
    )]
    async fn remove_node(
        &self,
        Parameters(RemoveNodeToolArguments { stream, name }): Parameters<RemoveNodeToolArguments>,
    ) -> ToolCallAnswer {
        let stream_operations = self.the_stream_a_tool_call_names("remove_node", &stream)?;
        let node = stream_operations
            .the_node_named(&name)
            .map_err(|e| format!("remove_node failed: {e}"))?;
        stream_operations
            .remove_processor_async(node.processor_id)
            .await
            .map_err(|e| format!("remove_node failed: {e}"))?;
        Ok(json_text_tool_result(&json!({ "removed_name": node.name })))
    }

    #[tool(
        description = "Link an output port to an input port in one loaded stream, and answer the new link's `link_id` and `state` — with `error_reason` when the link will carry nothing. Each end is a node's name and a port name — the names `graph` lists for each node and under its `outputs` and `inputs`. A stream reads another stream's port through a `RelayTrackSource` node."
    )]
    async fn connect(
        &self,
        Parameters(arguments): Parameters<ConnectToolArguments>,
    ) -> ToolCallAnswer {
        let ConnectToolArguments {
            stream,
            from_node,
            from_port,
            to_node,
            to_port,
        } = arguments;
        fn connect_failed(refusal: impl std::fmt::Display) -> String {
            format!("connect failed: {refusal}")
        }
        let stream_operations = self.the_stream_a_tool_call_names("connect", &stream)?;
        let from = stream_operations
            .the_node_named(&from_node)
            .map(|node| OutputLinkPortRef::new(node.processor_id, from_port))
            .map_err(connect_failed)?;
        let to = stream_operations
            .the_node_named(&to_node)
            .map(|node| InputLinkPortRef::new(node.processor_id, to_port))
            .map_err(connect_failed)?;
        let link_id = stream_operations
            .connect_async(from, to)
            .await
            .map_err(connect_failed)?;
        let how_the_graph_reads_it =
            how_the_graph_reads_one_link(&stream_operations, &link_id).await;
        let mut answer = json!({
            "link_id": link_id.as_str(),
            "state": how_the_graph_reads_it.state,
        });
        if let Some(reason) = how_the_graph_reads_it.error_reason {
            answer["error_reason"] = json!(reason);
        }
        Ok(json_text_tool_result(&answer))
    }

    #[tool(
        description = "Remove a link from one loaded stream's running graph by the id `graph` or `connect` reported."
    )]
    async fn disconnect(
        &self,
        Parameters(DisconnectToolArguments { stream, link_id }): Parameters<
            DisconnectToolArguments,
        >,
    ) -> ToolCallAnswer {
        self.the_stream_a_tool_call_names("disconnect", &stream)?
            .disconnect_async(LinkUniqueId::from(link_id.as_str()))
            .await
            .map_err(|e| format!("disconnect failed: {e}"))?;
        Ok(json_text_tool_result(
            &json!({ "disconnected_link_id": link_id }),
        ))
    }

    #[tool(
        description = "Make the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run, compile a project's stream function — or, with none named, every `@stream` in its `stream.py` — in that interpreter, describe its Python types, then load and start the stream under its stream id, `<folder>-<function>-<hash>`, which the runtime computes from its location — the resolved project directory and the function's `module:qualname`. The answer lists in `streams` each stream the load took in, by `stream`, beside its `display_name` (the function's name, or `name`), its `location` and its `state`: `running`, with `stream_instance` naming this load and its `node_count`; or `failed`, with `failed_because`. A failed stream is returned, never refused: a function that raises carries its traceback, a load or start that is refused its reason, and a `.venv` uv cannot make — uv's own words — or a `stream.py` that does not import or defines no `@stream` fails every stream recorded from the project — or, with none, a stream `<folder>-<hash>` shown under the folder's name, which the next load that defines a stream replaces. Fix the code and run again, or `start_stream` it. Refused, with nothing changed: a project the compile refuses, a function nothing defines or records, and an id another location holds, named with both. The stream is recorded, re-loaded whenever the runtime starts until `stop_stream` or `remove_project` ends it — a failed one waits for `start_stream` — and outlives the connection that carried this call. A run of a recorded stream's location replaces it, keeping its display name unless `name` is given and the owner's exposure levels; the running stream is unloaded once its replacement is recorded, and never put back. `compile_warnings` holds each line the compile wrote to its standard error — the cross-floor check's warnings among them — for the caller to show its user."
    )]
    async fn run_stream(
        &self,
        Parameters(RunStreamToolArguments {
            project_directory,
            stream_function,
            name,
        }): Parameters<RunStreamToolArguments>,
    ) -> ToolCallAnswer {
        if !project_directory.is_absolute() {
            return Err(format!(
                "`project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            ));
        }
        let run = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.run_stream(RunStreamRequest {
                    project_directory,
                    stream_function,
                    display_name: name,
                })
            })
            .await?;
        serialized_tool_result(
            "run_stream",
            &RunStreamToolResult {
                project_directory: run.project_directory,
                streams: run
                    .streams
                    .into_iter()
                    .map(run_stream_tool_result_stream_of)
                    .collect(),
                compile_warnings: run.compile_warnings,
            },
        )
    }

    #[tool(
        description = "Queue a run of the project in `project_directory` on its own stream action queue and answer at once with the action's id as `action`: the run makes the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run — however long uv takes — compiles every `@stream` in its `stream.py`, then loads and starts each as `run_stream` does, recording each stream running or failed. One project's actions run one at a time, in the order they were queued; different projects' run side by side. Follow the action with `describe_stream_action`, its streams with `list_streams` — `preparing`, with the action and its `step`, until it ends — and its records with `logs` given `action`. A directory that does not exist or is no project fails the action, never the call; refused only for a `project_directory` that is not absolute."
    )]
    async fn run_project(
        &self,
        Parameters(RunProjectToolArguments { project_directory }): Parameters<
            RunProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams
                    .run_project(RunProjectRequest { project_directory })
            })
            .await?;
        serialized_tool_result(
            "run_project",
            &RunProjectToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Describe the stream action `action`: its project, `kind` (`load`), `state` — `queued` behind the project's earlier actions, `in_progress` on its `step` (`making_the_venv`, `compiling`, `starting`), `done`, `failed` with `failed_because`, or `cancelled` by `cancel_stream_action` — and `streams`, each stream its compile named by `stream` id and `display_name`, with its `state` `running` or `failed` once the action took it in. The runtime holds every queued and running action and the last 64 finished ones of each project; an older one is refused, its records still in the project's `logs`."
    )]
    async fn describe_stream_action(
        &self,
        Parameters(DescribeStreamActionToolArguments { action }): Parameters<
            DescribeStreamActionToolArguments,
        >,
    ) -> ToolCallAnswer {
        let stream_action = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.describe_stream_action(&action)
            })
            .await?;
        serialized_tool_result(
            "describe_stream_action",
            &describe_stream_action_tool_result_of(stream_action),
        )
    }

    #[tool(
        description = "Cancel the stream action `action`, the only way an action ends early; nothing replaces or edits a queued one. A queued action is dropped from its project's queue, its pending mark taken off each stream's record so the record stays as it was, and answers `cancelled`. A running `load`, `start` or `reload_at_start` answers `in_progress` and stops at the step it is on — a running uv sync or compile is killed and a half-made `.venv` removed — then ends `cancelled`: a stream it was loading is recorded failed, and the streams it had not reached stay as recorded. Refused for a running `stop` or `remove`, whose unload has begun and cannot be cut short, which ends `done`; for an action already finished; and for one the runtime does not hold."
    )]
    async fn cancel_stream_action(
        &self,
        Parameters(CancelStreamActionToolArguments { action }): Parameters<
            CancelStreamActionToolArguments,
        >,
    ) -> ToolCallAnswer {
        let stream_action = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.cancel_stream_action(&action)
            })
            .await?;
        serialized_tool_result(
            "cancel_stream_action",
            &CancelStreamActionToolResult {
                action: stream_action.action_id,
                state: stream_action_state_on_the_wire(stream_action.state),
            },
        )
    }

    #[tool(
        description = "List the project's `@stream` functions as the last compile of its `stream.py` named them, in definition order, running none of its code: each by `stream_function`, its docstring as `description`, and its `stream` id and `state` when the runtime holds a stream of it. `import_failed_because` holds the traceback when `stream.py` did not import. While a `run_project` action is queued or under way, `action` and `step` name it, and `streams` stays what the compile before it named until its own compile ends. Refused for a project the runtime has neither compiled nor queued: `run_project` runs it."
    )]
    async fn list_project_streams(
        &self,
        Parameters(ListProjectStreamsToolArguments { project_directory }): Parameters<
            ListProjectStreamsToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let (project_streams, listings) = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                Ok((
                    operations_on_the_loaded_streams.list_project_streams(&project_directory)?,
                    operations_on_the_loaded_streams.list_streams(),
                ))
            })
            .await?;
        serialized_tool_result(
            "list_project_streams",
            &list_project_streams_tool_result_of(project_streams, &listings),
        )
    }

    #[tool(
        description = "Queue a stop of a stream on its project's stream action queue and answer at once with the action's id as `action` beside the stream's id as `stream`: once the project's earlier actions have run, the stop unloads the stream and records it stopped, so it stays unloaded across a runtime restart until `start_stream`. The stream is `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. A recorded stream carries the stop on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a stream stopped, or failed, once the actions queued before it run."
    )]
    async fn stop_stream(
        &self,
        Parameters(StreamActionToolArguments { stream }): Parameters<StreamActionToolArguments>,
    ) -> ToolCallAnswer {
        let queued = self
            .a_stream_action_off_the_async_runtime({
                let stream = stream.clone();
                move |operations_on_the_loaded_streams| {
                    operations_on_the_loaded_streams.stop_stream(&stream)
                }
            })
            .await?;
        serialized_tool_result(
            "stop_stream",
            &StopStreamToolResult {
                action: queued.action_id,
                stream,
            },
        )
    }

    #[tool(
        description = "Queue a start of a recorded stream that is not loaded — one `stop_stream` stopped, or a failed one, which this retries — on its project's stream action queue, and answer at once with the action's id as `action` beside the stream's id as `stream`. Once the project's earlier actions have run, the start makes the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run, then loads and starts the stream from the graph recorded when it was run, with the owner's exposure levels applied; once it starts it is neither stopped nor failed and its count of the runtime's crashes is reset. A failed stream whose retry is refused stays failed, for the retry's reason, and the action ends `done` with the stream `failed`. The stream is `preparing` in `list_streams` until it ends; follow it with `describe_stream_action`. Refused, with nothing queued: a stream loaded once the actions queued before it run, and an id no record holds."
    )]
    async fn start_stream(
        &self,
        Parameters(StreamActionToolArguments { stream }): Parameters<StreamActionToolArguments>,
    ) -> ToolCallAnswer {
        let queued = self
            .a_stream_action_off_the_async_runtime({
                let stream = stream.clone();
                move |operations_on_the_loaded_streams| {
                    operations_on_the_loaded_streams.start_stream(&stream)
                }
            })
            .await?;
        serialized_tool_result(
            "start_stream",
            &StartStreamToolResult {
                action: queued.action_id,
                stream,
            },
        )
    }

    #[tool(
        description = "Queue the removal of the whole project in `project_directory` on its stream action queue and answer at once with the action's id as `action`: once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, so the runtime no longer re-loads any of them, and deletes no file. No tool removes one stream: every `@stream` in `stream.py` runs, and a stream leaves its project when its function leaves `stream.py`. The project's streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute, and a project the runtime holds no stream of."
    )]
    async fn remove_project(
        &self,
        Parameters(RemoveProjectToolArguments { project_directory }): Parameters<
            RemoveProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.remove_project(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "remove_project",
            &RemoveProjectToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Show a stream under another display name. Its stream id, address and broadcast stay as they are, so no reader of its ports is cut off. A recorded stream's record is rewritten first, so a rename survives a runtime restart and a re-load that gives no `name`; a rename whose record cannot be written is refused with the stream shown as it was. Answers the stream id as `stream` and the `display_name` it is shown under now."
    )]
    async fn rename_stream(
        &self,
        Parameters(RenameStreamToolArguments {
            stream,
            display_name,
        }): Parameters<RenameStreamToolArguments>,
    ) -> ToolCallAnswer {
        let renamed = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.rename_stream(&stream, &display_name)
            })
            .await?;
        serialized_tool_result(
            "rename_stream",
            &RenameStreamToolResult {
                stream: renamed.stream_id,
                display_name: renamed.display_name,
            },
        )
    }

    #[tool(
        description = "List every stream the runtime holds: its stream id as `stream`, its `display_name`, its `location` — project directory and `module:qualname` stream function, `null` for a record an earlier runtime wrote — and its state: `running` (loaded, or recorded and re-loaded at the runtime's next start), `stopped`, `preparing` (an action that loads it — a `run_project` or a `start_stream` — is queued or under way on it, named by `action`, on its `step`: `making_the_venv`, `compiling` or `starting`, `null` while it waits), `stopping` (a `stop_stream`, or a `remove_project` of its project, is queued or under way on it, named by `action`, on the step `stopping`, `null` while it waits), or `failed` (implicated in the runtime's last two crashes in a row, or could not re-load at its start, and skipped at the start until `start_stream` retries it; or recorded by an earlier runtime, never loaded, with `failed_because` saying to run it again and `remove_project` forgetting it), with the node count of its loaded graph — `null` when it is not loaded — and `failed_because`, why a failed stream failed — `null` otherwise."
    )]
    async fn list_streams(
        &self,
        Parameters(ListStreamsToolArguments {}): Parameters<ListStreamsToolArguments>,
    ) -> ToolCallAnswer {
        let listings = self
            .a_stream_action_off_the_async_runtime(|operations_on_the_loaded_streams| {
                Ok(operations_on_the_loaded_streams.list_streams())
            })
            .await?;
        serialized_tool_result(
            "list_streams",
            &ListStreamsToolResult {
                streams: listings
                    .into_iter()
                    .map(|listing| ListStreamsToolResultStream {
                        stream: listing.stream_id,
                        display_name: listing.display_name,
                        location: listing.stream_location.map(stream_location_on_the_wire),
                        state: listed_stream_state_on_the_wire(listing.state),
                        node_count: listing.node_count,
                        failed_because: listing.failed_because,
                        step: listing
                            .stream_action_under_way
                            .as_ref()
                            .and_then(|working_on_it| working_on_it.step)
                            .map(stream_action_step_on_the_wire),
                        action: listing
                            .stream_action_under_way
                            .map(|working_on_it| working_on_it.action_id),
                    })
                    .collect(),
            },
        )
    }

    #[tool(
        description = "Put a stream's output port at `internal`, `private` or `public`. On a loaded stream it changes live: a reader the new level no longer allows is cut off at once, and nothing restarts. On a recorded stream — loaded or stopped — the level is recorded as the owner's ruling and wins over the level the stream function declares, through every restart; `recorded` says whether it was. On a loaded stream a restriction is recorded before it changes live, and refused with the live level unchanged when it cannot be recorded. `not_recorded_because`, present only when `recorded` is false, says why a level raised live could not be recorded, so a runtime restart puts back the level it had."
    )]
    async fn expose_port(
        &self,
        Parameters(ExposePortToolArguments {
            stream,
            node,
            port,
            level,
        }): Parameters<ExposePortToolArguments>,
    ) -> ToolCallAnswer {
        let exposed = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.expose_port(&stream, &node, &port, level.into())
            })
            .await?;
        serialized_tool_result(
            "expose_port",
            &ExposePortToolResult {
                stream: exposed.stream_id,
                node: exposed.node,
                port: exposed.port,
                level: expose_port_level_on_the_wire(exposed.level),
                recorded: exposed.recorded,
                not_recorded_because: exposed.ruling_not_recorded_because,
            },
        )
    }

    #[tool(
        description = "Add a relay instance of an adapter and join it: `adapter` is a kind the resource `streamlib://relay-adapter-catalog` lists, `settings` carries the settings that adapter lists, by name. Answers the `relay_id` the runtime gave it. Each secret among the settings is kept in the OS secret store and never shown again; the rest is recorded in `relays.json` and joined again at every start. An instance that cannot be joined is refused and nothing is recorded. The local relay is built in, so no `local` instance is added."
    )]
    async fn add_relay(
        &self,
        Parameters(AddRelayToolArguments { adapter, settings }): Parameters<AddRelayToolArguments>,
    ) -> ToolCallAnswer {
        let relay_id = self
            .a_relay_call_off_the_async_runtime(move |relay_instances| {
                relay_instances.add_relay(&adapter, settings.unwrap_or_default())
            })
            .await?;
        serialized_tool_result(
            "add_relay",
            &AddRelayToolResult {
                relay_id: relay_id.0,
            },
        )
    }

    #[tool(
        description = "Replace settings of a relay instance and join it again with them, or keep what it had when it cannot be joined. Only the settings named change; `null` returns one to its default; a secret named replaces the one held in the OS secret store."
    )]
    async fn configure_relay(
        &self,
        Parameters(ConfigureRelayToolArguments { relay_id, settings }): Parameters<
            ConfigureRelayToolArguments,
        >,
    ) -> ToolCallAnswer {
        let configured_relay_id = relay_id.clone();
        self.a_relay_call_off_the_async_runtime(move |relay_instances| {
            relay_instances.configure_relay(&relay_id, settings)
        })
        .await?;
        serialized_tool_result(
            "configure_relay",
            &ConfigureRelayToolResult {
                relay_id: configured_relay_id,
            },
        )
    }

    #[tool(
        description = "Leave a relay instance and forget it and its secrets. The local relay's built-in instances, `local-loopback` and `local-every-interface`, are never removed."
    )]
    async fn remove_relay(
        &self,
        Parameters(RemoveRelayToolArguments { relay_id }): Parameters<RemoveRelayToolArguments>,
    ) -> ToolCallAnswer {
        let removed_relay_id = relay_id.clone();
        self.a_relay_call_off_the_async_runtime(move |relay_instances| {
            relay_instances.remove_relay(&relay_id)
        })
        .await?;
        serialized_tool_result(
            "remove_relay",
            &RemoveRelayToolResult {
                relay_id: removed_relay_id,
            },
        )
    }

    #[tool(
        description = "List every relay instance the runtime holds, the built-in ones first: its `relay_id`, its `adapter`, whether it is `built_in`, its `settings` with every secret left out, each secret setting by name as `set` or `unset` and never its value, and its `connection_state` — `not_joined` (with `why` when known), `joining`, `joined`, or `lost` (with `why` and `rejoining_after_ms`)."
    )]
    async fn list_relays(
        &self,
        Parameters(ListRelaysToolArguments {}): Parameters<ListRelaysToolArguments>,
    ) -> ToolCallAnswer {
        let relays = self
            .a_relay_call_off_the_async_runtime(|relay_instances| Ok(relay_instances.list_relays()))
            .await?;
        serialized_tool_result("list_relays", &ListRelaysToolResult { relays })
    }

    #[tool(
        description = "Describe this runtime: its crate `version`, its `build_id` (`<version>+<git sha>.<per-build nonce>`, telling two builds of one version apart), `up_for_ms`, the monotonic milliseconds since its engine started — a restart resets it — and `runtime_log_directory`, the folder its own log is written to, `null` when it keeps none. Names no stream."
    )]
    async fn describe_runtime(
        &self,
        Parameters(DescribeRuntimeToolArguments {}): Parameters<DescribeRuntimeToolArguments>,
    ) -> ToolCallAnswer {
        serialized_tool_result(
            "describe_runtime",
            &self
                .operations_on_the_loaded_streams
                .describe_this_runtime(),
        )
    }
}

fn listed_stream_state_on_the_wire(state: StreamListingState) -> ListedStreamState {
    match state {
        StreamListingState::Running => ListedStreamState::Running,
        StreamListingState::Stopped => ListedStreamState::Stopped,
        StreamListingState::Failed => ListedStreamState::Failed,
        StreamListingState::Preparing => ListedStreamState::Preparing,
        StreamListingState::Stopping => ListedStreamState::Stopping,
    }
}

fn stream_action_step_on_the_wire(step: StreamActionStep) -> StreamActionStepOnTheWire {
    match step {
        StreamActionStep::MakingTheVenv => StreamActionStepOnTheWire::MakingTheVenv,
        StreamActionStep::Compiling => StreamActionStepOnTheWire::Compiling,
        StreamActionStep::Starting => StreamActionStepOnTheWire::Starting,
        StreamActionStep::Stopping => StreamActionStepOnTheWire::Stopping,
    }
}

/// A stream action's state as the local API spells it.
fn stream_action_state_on_the_wire(state: StreamActionState) -> StreamActionStateOnTheWire {
    match state {
        StreamActionState::Queued => StreamActionStateOnTheWire::Queued,
        StreamActionState::InProgress => StreamActionStateOnTheWire::InProgress,
        StreamActionState::Done => StreamActionStateOnTheWire::Done,
        StreamActionState::Failed => StreamActionStateOnTheWire::Failed,
        StreamActionState::Cancelled => StreamActionStateOnTheWire::Cancelled,
    }
}

/// A stream action as `describe_stream_action` answers it.
fn describe_stream_action_tool_result_of(
    stream_action: StreamAction,
) -> DescribeStreamActionToolResult {
    DescribeStreamActionToolResult {
        action: stream_action.action_id,
        project_directory: stream_action.project_directory,
        kind: match stream_action.kind {
            StreamActionKind::Load => StreamActionKindOnTheWire::Load,
            StreamActionKind::Start => StreamActionKindOnTheWire::Start,
            StreamActionKind::Stop => StreamActionKindOnTheWire::Stop,
            StreamActionKind::Remove => StreamActionKindOnTheWire::Remove,
            StreamActionKind::ReloadAtStart => StreamActionKindOnTheWire::ReloadAtStart,
        },
        state: stream_action_state_on_the_wire(stream_action.state),
        step: stream_action.step.map(stream_action_step_on_the_wire),
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

/// A project's streams as `list_project_streams` answers them, each stream the runtime holds
/// named by its id and state from `listings`.
fn list_project_streams_tool_result_of(
    project_streams: ProjectStreamsListing,
    listings: &[StreamListing],
) -> ListProjectStreamsToolResult {
    let (streams, import_failed_because) = match project_streams.what_the_last_compile_gave {
        Some(WhatTheLastCompileOfTheProjectGave::Streams(streams)) => (
            streams
                .into_iter()
                .map(|stream| {
                    let stream_id = stream.stream_location.stream_id();
                    let listing = listings
                        .iter()
                        .find(|listing| listing.stream_id == stream_id);
                    ListProjectStreamsToolResultStream {
                        stream_function: stream.stream_location.stream_function,
                        description: stream.description,
                        stream: listing.map(|listing| listing.stream_id.clone()),
                        state: listing
                            .map(|listing| listed_stream_state_on_the_wire(listing.state)),
                    }
                })
                .collect(),
            None,
        ),
        Some(WhatTheLastCompileOfTheProjectGave::EntryDidNotImport(import_failed_because)) => {
            (Vec::new(), Some(import_failed_because))
        }
        Some(WhatTheLastCompileOfTheProjectGave::EntryDefinesNoStream(_)) | None => {
            (Vec::new(), None)
        }
    };
    let (action, step) = match project_streams.stream_action_under_way {
        Some(working_on_it) => (
            Some(working_on_it.action_id),
            working_on_it.step.map(stream_action_step_on_the_wire),
        ),
        None => (None, None),
    };
    ListProjectStreamsToolResult {
        project_directory: project_streams.project_directory,
        action,
        step,
        streams,
        import_failed_because,
    }
}

fn expose_port_level_on_the_wire(level: OutputPortExposureLevel) -> ExposePortLevel {
    match level {
        OutputPortExposureLevel::Internal => ExposePortLevel::Internal,
        OutputPortExposureLevel::Private => ExposePortLevel::Private,
        OutputPortExposureLevel::Public => ExposePortLevel::Public,
    }
}

/// Refuses a `project_directory` that is not absolute, since the runtime cannot see the caller's
/// working directory.
fn refuse_a_relative_project_directory(project_directory: &std::path::Path) -> Result<(), String> {
    if project_directory.is_absolute() {
        return Ok(());
    }
    Err(format!(
        "`project_directory` must be an absolute path; got `{}`",
        project_directory.display()
    ))
}

/// The instance a `run_stream` result names the load tagged `stream_tag` by.
fn the_instance_of_the_load_tagged(stream_tag: LoadedStreamTag) -> LoadedStreamInstance {
    LoadedStreamInstance(stream_tag.to_string())
}

/// One stream a `run_stream` load took in, as the local API reports it.
fn run_stream_tool_result_stream_of(
    stream: StreamLoadedOrFailedByARun,
) -> RunStreamToolResultStream {
    let (state, stream_instance, node_count, failed_because) = match stream.state {
        StreamStateAfterItsRun::Running {
            stream_tag,
            node_count,
        } => (
            RunStreamToolResultStreamState::Running,
            Some(the_instance_of_the_load_tagged(stream_tag)),
            Some(node_count),
            None,
        ),
        StreamStateAfterItsRun::Failed { failed_because } => (
            RunStreamToolResultStreamState::Failed,
            None,
            None,
            Some(failed_because),
        ),
    };
    RunStreamToolResultStream {
        stream: stream.stream_id,
        display_name: stream.display_name,
        state,
        stream_instance,
        location: stream_location_on_the_wire(stream.stream_location),
        node_count,
        replaced_a_recorded_stream: stream.replaced_a_recorded_stream,
        failed_because,
    }
}

/// Where a stream's code lives, as the local API reports it.
fn stream_location_on_the_wire(stream_location: StreamLocation) -> StreamLocationOnTheWire {
    StreamLocationOnTheWire {
        project_directory: stream_location.project_directory,
        stream_function: stream_location.stream_function,
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for LocalApiMcpServerHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
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

    /// No list ever changes under a host, so a listen is acknowledged with
    /// nothing subscribed and held until it is cancelled.
    fn accepted_subscription_filter(
        &self,
        _requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(SubscriptionFilter::default())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        tokio::select! {
            () = context.cancelled() => {}
            () = self.local_api_stopping_token.cancelled() => {}
        }
        Ok(())
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
        crate::mcp_resources::read_resource(
            &self.operations_on_the_loaded_streams,
            self.relay_instances.as_ref(),
            &request.uri,
        )
        .await
        .map(Into::into)
    }
}

/// What `graph` says about a link just connected, beside its id.
///
/// A struct rather than a bare `Option<String>` so a second reading can join it
/// without changing every call site.
#[derive(Default)]
struct HowTheGraphReadsOneLink {
    state: Option<String>,
    error_reason: Option<String>,
}

/// The state the new link reads, taken from the same `graph` a caller would
/// read next.
///
/// Read after the connect rather than returned by it: `connect_async` hands
/// back a link id, and everything else about the link is what `graph` renders.
/// A render that fails leaves the state unknown rather than failing a connect
/// that already took.
async fn how_the_graph_reads_one_link(
    stream_operations: &Arc<dyn RuntimeOperations>,
    link_id: &LinkUniqueId,
) -> HowTheGraphReadsOneLink {
    let graph = match stream_operations.to_json_async().await {
        Ok(graph) => graph,
        Err(unreadable) => {
            // Swallowing this would leave the caller a null state with nothing
            // to act on, and the operator nothing to re-derive it from.
            tracing::warn!(
                "a link was connected and its stream's graph could not be read to say what \
                 state it is in: {unreadable}"
            );
            return HowTheGraphReadsOneLink::default();
        }
    };
    let Some(link) = graph["links"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|link| link["id"].as_str() == Some(link_id.as_str()))
    else {
        return HowTheGraphReadsOneLink::default();
    };
    let text_at = |key: &str| link[key].as_str().map(str::to_string);
    HowTheGraphReadsOneLink {
        state: text_at("state"),
        error_reason: text_at("error_reason"),
    }
}

// ============================================================================
// Result content
// ============================================================================

/// `tool_result` as `tool_name`'s successful result.
fn serialized_tool_result(tool_name: &str, tool_result: &impl Serialize) -> ToolCallAnswer {
    let tool_result_json = serde_json::to_value(tool_result)
        .map_err(|e| format!("{tool_name} result serialization failed: {e}"))?;
    Ok(json_text_tool_result(&tool_result_json))
}

/// A successful tool result: the value as one pretty-JSON text block, the form
/// every tool here states a result a caller parses in.
fn json_text_tool_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![json_text_content_block(value)])
}

fn json_text_content_block(value: &Value) -> ContentBlock {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ContentBlock::text(text)
}

#[cfg(test)]
pub(crate) mod tests {
    //! MCP wire tests: an `rmcp` client drives the real `/mcp` endpoint that
    //! [`crate::handlers::build_router`] wires in, served on a real local API
    //! socket, through discovery, the tool catalog, and each tool through to
    //! the runtime. The router is the real one; only the backend is a stub — a
    //! runtime loading one stream, the stub's own `RuntimeOperations` — so the
    //! MCP → runtime seam is what's under test.
    //!
    //! The catalog assertions are two-sided on purpose — what is advertised,
    //! and what must never be again.

    use crate::control_plane_stub_support::{
        LocalApiServedOnAFreshSocket, STUB_MACHINE_NAME, STUB_STREAM_ID,
    };
    use parking_lot::Mutex;
    use rmcp::RoleClient;
    use rmcp::model::{
        CallToolRequestParams, ClientCapabilities, GetPromptRequestParams,
        ReadResourceRequestParams, RequestMetaObject,
    };
    use rmcp::service::{
        ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
        SubscriptionEnd,
    };
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};
    use streamlib::sdk::error::Result;
    use streamlib::sdk::runtime::{
        BoxFuture, OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations,
    };
    use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;

    use std::time::Duration;

    use super::*;

    /// Stub runtime answering the observation ops without a live engine.
    ///
    /// Every graph-mutating op records what it was handed and answers a fixed
    /// id, so a mutation tool's test asserts the op it reached and the
    /// arguments it carried.
    #[derive(Clone)]
    pub(crate) struct ControlPlaneMcpDispatchStubRuntime {
        exported_graph: Arc<Mutex<Value>>,
        recorded_graph_mutations: crate::control_plane_stub_support::RecordedGraphMutations,
        armed_add_processor_refusal: crate::control_plane_stub_support::ArmedAddProcessorRefusal,
        add_processor_answers_after: Option<Duration>,
    }

    impl ControlPlaneMcpDispatchStubRuntime {
        pub(crate) fn new() -> Self {
            Self {
                exported_graph: Arc::new(Mutex::new(json!({ "nodes": [], "links": [] }))),
                recorded_graph_mutations: Arc::new(Mutex::new(Vec::new())),
                armed_add_processor_refusal: Arc::new(Mutex::new(None)),
                add_processor_answers_after: None,
            }
        }

        /// A stub whose `add_node` refuses with `refusal`, standing in for an
        /// engine-side refusal the front end has to carry back.
        fn refusing_every_add_node(refusal: &str) -> Self {
            Self {
                armed_add_processor_refusal: Arc::new(Mutex::new(Some(refusal.to_string()))),
                ..Self::new()
            }
        }

        /// A stub whose `add_node` answers only once `answers_after` has
        /// passed, so it is the request still in flight.
        pub(crate) fn with_add_node_answering_after(answers_after: Duration) -> Self {
            Self {
                add_processor_answers_after: Some(answers_after),
                ..Self::new()
            }
        }
    }

    impl RuntimeOperations for ControlPlaneMcpDispatchStubRuntime {
        fn to_json_async(&self) -> BoxFuture<'_, Result<Value>> {
            let exported_graph = self.exported_graph.lock().clone();
            Box::pin(async move { Ok(exported_graph) })
        }
        crate::control_plane_stub_support::the_exposed_port_subscription_is_unreachable!("tool");
        crate::control_plane_stub_support::graph_mutation_ops_record_the_call!();
        fn to_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
    }

    crate::control_plane_stub_support::a_stub_runtime_loading_this_stub_as_its_only_stream!(
        ControlPlaneMcpDispatchStubRuntime
    );

    /// The control vocabulary. This is the whole of it —
    /// `tools/list` is asserted equal to this, not merely a superset.
    const CONTROL_TOOL_NAMES: &[&str] = &[
        "graph",
        "logs",
        "add_node",
        "remove_node",
        "connect",
        "disconnect",
        "run_stream",
        "run_project",
        "describe_stream_action",
        "cancel_stream_action",
        "list_project_streams",
        "stop_stream",
        "start_stream",
        "remove_project",
        "rename_stream",
        "list_streams",
        "expose_port",
        "add_relay",
        "configure_relay",
        "remove_relay",
        "list_relays",
        "describe_runtime",
    ];

    /// The tools that act on one stream, each naming it with a required
    /// `stream`.
    const TOOLS_THAT_REQUIRE_A_STREAM: &[&str] = &[
        "add_node",
        "remove_node",
        "connect",
        "disconnect",
        "stop_stream",
        "start_stream",
        "rename_stream",
        "expose_port",
    ];

    /// Exact, not a superset: the catalog IS the control vocabulary, so a
    /// tool appearing that is not in this list is a surface the plan does not
    /// grant.
    pub(crate) fn assert_names_exactly_the_control_vocabulary(
        mut advertised_tool_names: Vec<&str>,
    ) {
        advertised_tool_names.sort_unstable();
        let mut control_tool_names = CONTROL_TOOL_NAMES.to_vec();
        control_tool_names.sort_unstable();
        assert_eq!(
            advertised_tool_names, control_tool_names,
            "tools/list must advertise exactly the control vocabulary"
        );
    }

    /// The authority the MCP client names in `Host`; the socket path is the
    /// address.
    const LOCAL_API_MCP_URI: &str = "http://localhost/mcp";

    pub(crate) fn mcp_transport_to(
        served: &LocalApiServedOnAFreshSocket,
    ) -> StreamableHttpClientTransport<UnixSocketHttpClient> {
        mcp_transport_naming(served, LOCAL_API_MCP_URI)
    }

    fn mcp_transport_naming(
        served: &LocalApiServedOnAFreshSocket,
        local_api_mcp_uri: &str,
    ) -> StreamableHttpClientTransport<UnixSocketHttpClient> {
        StreamableHttpClientTransport::with_client(
            UnixSocketHttpClient::new(
                served.local_api_socket_path.to_str().unwrap(),
                local_api_mcp_uri,
            ),
            StreamableHttpClientTransportConfig::with_uri(local_api_mcp_uri.to_string()),
        )
    }

    /// An `rmcp` client at the latest revision, connected to the real router
    /// over `runtime`.
    async fn connected_mcp_client(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> (LocalApiServedOnAFreshSocket, RunningService<RoleClient, ()>) {
        let served = LocalApiServedOnAFreshSocket::over(runtime);
        let client = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the node answers `server/discover` at the latest revision");
        (served, client)
    }

    /// One `tools/call`: the result as it crossed the wire, or the protocol
    /// error that refused it.
    pub(crate) async fn tool_call_outcome(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        let request = CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        wire_outcome(client.call_tool(request).await)
    }

    /// A result or refusal as it crossed the wire.
    fn wire_outcome<T: serde::Serialize>(
        outcome: std::result::Result<T, ServiceError>,
    ) -> std::result::Result<Value, McpError> {
        match outcome {
            Ok(result) => Ok(serde_json::to_value(result).unwrap()),
            Err(ServiceError::McpError(refusal)) => Err(refusal),
            Err(other) => panic!("the request failed below the protocol: {other}"),
        }
    }

    /// The JSON a successful tool result states in its first text block.
    pub(crate) fn first_text_block_json(tool_result: &Value) -> Value {
        assert_eq!(tool_result["isError"], false, "{tool_result}");
        serde_json::from_str(
            tool_result["content"][0]["text"]
                .as_str()
                .expect("a text block"),
        )
        .expect("the text block is JSON")
    }

    pub(crate) async fn tool_call_result(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        tool_name: &str,
        arguments: Value,
    ) -> Value {
        tool_call_outcome(runtime, tool_name, arguments)
            .await
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal:?}"))
    }

    async fn tool_call_refusal(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        tool_name: &str,
        arguments: Value,
    ) -> McpError {
        match tool_call_outcome(runtime, tool_name, arguments).await {
            Ok(result) => panic!("`{tool_name}` answered a result: {result}"),
            Err(refusal) => refusal,
        }
    }

    pub(crate) async fn listed_tools(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> Vec<Value> {
        let (_served, client) = connected_mcp_client(runtime).await;
        client
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|tool| serde_json::to_value(tool).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn discover_over_post_mcp_answers_the_latest_revision_alone_with_the_tools_resources_and_prompts_capabilities()
     {
        let (_served, client) =
            connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

        let discovered = client
            .discover(RequestMetaObject::with_client_context(
                ProtocolVersion::LATEST,
                Implementation::new("discover-test", "0"),
                ClientCapabilities::default(),
            ))
            .await
            .unwrap();

        assert_eq!(discovered.supported_versions, [ProtocolVersion::LATEST]);
        assert_eq!(
            discovered.server_info().map(|server_info| server_info.name),
            Some(MCP_SERVER_NAME.to_string())
        );
        assert_eq!(
            discovered
                .server_info()
                .map(|server_info| server_info.version),
            Some(the_workspace_package_version())
        );
        assert!(discovered.capabilities.tools.is_some(), "{discovered:?}");
        assert!(
            discovered.capabilities.resources.is_some(),
            "{discovered:?}"
        );
        assert!(discovered.capabilities.prompts.is_some(), "{discovered:?}");
        assert_eq!(
            discovered.instructions.as_deref(),
            Some(LOCAL_API_MCP_SERVER_INSTRUCTIONS)
        );
    }

    /// A client that knows the handshake revisions only.
    pub(crate) struct HandshakeRevisionMcpClient {
        pub(crate) handshake_protocol_version: ProtocolVersion,
    }

    impl rmcp::ClientHandler for HandshakeRevisionMcpClient {
        fn get_info(&self) -> rmcp::model::ClientConfig {
            rmcp::model::ClientConfig::new(
                ClientCapabilities::default(),
                Implementation::new("handshake-test", "0"),
            )
            .with_protocol_version(self.handshake_protocol_version.clone())
        }
    }

    #[tokio::test]
    async fn an_initialize_handshake_over_post_mcp_is_refused_with_the_unsupported_version_error_naming_the_latest()
     {
        for handshake_protocol_version in [
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::LATEST_WITH_INITIALIZE,
        ] {
            let served = LocalApiServedOnAFreshSocket::over(Arc::new(
                ControlPlaneMcpDispatchStubRuntime::new(),
            ));
            let refusal = HandshakeRevisionMcpClient {
                handshake_protocol_version: handshake_protocol_version.clone(),
            }
            .serve_with_lifecycle(mcp_transport_to(&served), ClientLifecycleMode::Initialize)
            .await
            .err()
            .unwrap_or_else(|| panic!("{handshake_protocol_version} must not be served"));

            let ClientInitializeError::JsonRpcError(refusal) = refusal else {
                panic!("{handshake_protocol_version}: expected a JSON-RPC refusal, got {refusal}");
            };
            assert_eq!(
                refusal.code,
                rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
            );
            let data = refusal
                .data
                .expect("the refusal names the served revisions");
            assert_eq!(
                data["supported"],
                json!([ProtocolVersion::LATEST]),
                "{data}"
            );
            assert_eq!(
                data["requested"],
                json!(handshake_protocol_version),
                "{data}"
            );
        }
    }

    fn serve_one_mcp_tool_call() {
        let request_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds");
        request_runtime.block_on(async {
            tool_call_result(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                "graph",
                json!({}),
            )
            .await;
        });
    }

    fn rmcp_trace_targets_under(env_filter_directives: &str) -> Vec<&'static str> {
        crate::control_plane_stub_support::CapturedTracingTargets::captured_from_the_second_of_two_runs(
            env_filter_directives,
            serve_one_mcp_tool_call,
        )
        .into_iter()
        .filter(|target| target.starts_with("rmcp"))
        .collect()
    }

    /// At the engine's default filter a node's control plane adds nothing to
    /// the app's own log, MCP included.
    #[test]
    #[serial_test::serial]
    fn a_routine_mcp_request_says_nothing_at_the_engines_default_filter() {
        assert!(
            !rmcp_trace_targets_under("info").is_empty(),
            "the SDK speaks at info, so the default has to hold it"
        );
        let targets = rmcp_trace_targets_under(
            streamlib::sdk::logging::ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES,
        );
        assert!(
            targets.is_empty(),
            "an MCP request must be silent at the engine's default filter, got: {targets:?}"
        );
    }

    /// The settings that make the endpoint the latest revision's alone: no
    /// session, every request carrying its own revision, and no `Host`
    /// allowlist on a socket no browser can dial.
    #[test]
    fn the_endpoint_is_stateless_demands_per_request_metadata_and_admits_any_host() {
        let service = local_api_mcp_streamable_http_service(LocalApiMcpServerHandler::new(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            crate::control_plane_stub_support::RelayInstancesOfThisMachineStub::shared(),
            CancellationToken::new(),
        ));
        assert!(!service.config.legacy_session_mode);
        assert!(service.config.stateless_protocol_metadata_required);
        assert!(
            service.config.allowed_hosts.is_empty(),
            "{:?}",
            service.config.allowed_hosts
        );
    }

    #[tokio::test]
    async fn a_client_naming_any_host_over_the_socket_is_answered() {
        let served =
            LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
        let client = ()
            .serve_with_lifecycle(
                mcp_transport_naming(&served, "http://streamlib-node/mcp"),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the socket's file mode is the gate, not the Host header");
        assert!(client.list_all_tools().await.is_ok());
    }

    #[tokio::test]
    async fn a_client_speaking_only_a_handshake_revision_finds_no_compatible_revision_over_post_mcp()
     {
        let served =
            LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
        let refusal = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2025_06_18],
                },
            )
            .await
            .expect_err("a handshake revision must not be served");
        let ClientInitializeError::NoCompatibleProtocolVersion {
            server_supported, ..
        } = refusal
        else {
            panic!("expected the unsupported-version refusal, got {refusal}");
        };
        assert_eq!(server_supported, [ProtocolVersion::LATEST]);
    }

    /// A host that holds `subscriptions/listen` open must not hold the node's
    /// shutdown: stopping the local API ends the stream with its final result.
    #[tokio::test]
    async fn a_listen_stream_ends_gracefully_when_the_local_api_stops_serving() {
        let (mut served, client) =
            connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let mut subscription = client
            .listen(SubscriptionFilter::default())
            .await
            .expect("the node acknowledges a listen");
        assert_eq!(
            serde_json::to_value(subscription.acknowledged()).unwrap(),
            json!({}),
            "no list ever changes under a host, so nothing is subscribed"
        );

        served.stop_serving();

        let ended = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the stream ends once the local API stops");
        assert!(matches!(ended, Ok(None)), "{ended:?}");
        assert!(
            matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
            "the stream closes with the listen request's own final result"
        );
    }

    /// `[workspace.package] version` in the workspace manifest: the version every runtime crate
    /// takes.
    fn the_workspace_package_version() -> String {
        let workspace_manifest: toml::Table = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"),
        )
        .expect("the workspace manifest reads")
        .parse()
        .expect("the workspace manifest is TOML");
        workspace_manifest["workspace"]["package"]["version"]
            .as_str()
            .expect("the workspace manifest names a package version")
            .to_owned()
    }

    #[tokio::test]
    async fn describe_runtime_answers_the_runtimes_description_and_names_no_stream() {
        let described = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "describe_runtime",
            json!({}),
        )
        .await;
        let described: DescribeRuntimeToolResult =
            serde_json::from_value(first_text_block_json(&described)).unwrap();
        assert_eq!(
            described,
            crate::control_plane_stub_support::the_stub_runtimes_description()
        );
    }

    #[tokio::test]
    async fn tools_list_advertises_exactly_the_control_vocabulary() {
        let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        assert_names_exactly_the_control_vocabulary(
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect(),
        );
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool `{}` must declare an object inputSchema",
                tool["name"]
            );
        }
    }

    #[tokio::test]
    async fn tools_call_add_node_reaches_the_runtime_op_with_the_spec() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "stream": STUB_STREAM_ID,
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "config": { "strength": 0.5 },
                "name": "Gray"
            }),
        )
        .await;
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let stated: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            stated,
            json!({ "name": "gray" }),
            "the result is the name the node received, which `connect` and `remove_node` take"
        );
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("add_node must reach exactly one add op, recorded {recorded:?}");
        };
        assert_eq!(
            spec.name.as_str(),
            "processors.grayscale_effect:GrayscaleEffect"
        );
        assert_eq!(spec.config["strength"], 0.5);
        assert_eq!(spec.display_name.as_deref(), Some("Gray"));
    }

    /// What this locks is the door: an engine that refuses an add — a name
    /// already taken, an unknown class — reaches the MCP caller as a tool error
    /// carrying its own words, rather than as a success or a swallowed reason.
    #[tokio::test]
    async fn tools_call_add_node_carries_back_an_engine_refusal_in_its_own_words() {
        let refusal = "the node name `gray` is already taken in this graph";
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::refusing_every_add_node(
            refusal,
        ));

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "stream": STUB_STREAM_ID,
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "name": "gray"
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("add_node failed"), "{text}");
        assert!(
            text.contains(refusal),
            "the refusal must reach the caller in the engine's own words: {text}"
        );
    }

    /// The one setting [`AddNodeTestSourceTakingOneSetting`] takes.
    #[derive(
        Debug,
        Clone,
        Default,
        PartialEq,
        serde::Serialize,
        serde::Deserialize,
        streamlib::sdk::schemars::JsonSchema,
    )]
    #[schemars(crate = "streamlib::sdk::schemars")]
    #[serde(deny_unknown_fields)]
    pub struct AddNodeTestSourceTakingOneSettingConfig {
        #[serde(default)]
        pub frame_width: Option<u32>,
    }

    /// A native source whose config refuses every key but `frame_width`.
    #[streamlib::sdk::processor(
        execution = manual,
        config = crate::mcp::tests::AddNodeTestSourceTakingOneSettingConfig,
        output("video"),
    )]
    pub struct AddNodeTestSourceTakingOneSetting;

    impl streamlib::sdk::processors::ManualProcessor for AddNodeTestSourceTakingOneSetting::Processor {
        fn start(
            &mut self,
            _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
        ) -> Result<()> {
            Ok(())
        }
    }

    /// Against a real engine: a live add meets the refusal a load does, naming
    /// the node, its type and the setting, and adds nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn tools_call_add_node_refuses_a_setting_the_nodes_type_does_not_take_naming_it() {
        streamlib::sdk::processors::PROCESSOR_REGISTRY
            .register::<AddNodeTestSourceTakingOneSetting::Processor>();
        let source_type = AddNodeTestSourceTakingOneSetting::processor_class_import_path();
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runtime = streamlib::sdk::runtime::Runner::new().unwrap();
        let stream = runtime
            .load_an_empty_stream(
                streamlib::sdk::runtime::OptionsForLoadingOneStream::in_project_directory(
                    project_directory.path(),
                )
                .with_stream_id("main"),
            )
            .unwrap();

        let body = tool_call_result(
            Arc::clone(&runtime) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
            "add_node",
            json!({
                "stream": "main",
                "type": source_type.as_str(),
                "name": "front",
                "config": {"frame_widht": 640}
            }),
        )
        .await;

        assert_eq!(body["isError"], true, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("node `front`"), "{text}");
        assert!(text.contains(source_type.as_str()), "{text}");
        assert!(text.contains("`frame_widht`"), "{text}");
        let graph = stream.to_json().unwrap();
        assert_eq!(
            graph["nodes"],
            json!([]),
            "a refused add adds nothing: {graph}"
        );
    }

    #[tokio::test]
    async fn tools_call_add_node_without_config_or_name_sends_an_empty_object_and_answers_the_engines_name()
     {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({ "stream": STUB_STREAM_ID, "type": "tatolab.stream:CameraSource" }),
        )
        .await;
        let stated = first_text_block_json(&body);
        assert_eq!(
            stated["name"],
            crate::control_plane_stub_support::STUB_ADDED_NODE_NAME,
            "an unnamed node is answered with the name the engine gave it"
        );
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("expected one add op, recorded {recorded:?}");
        };
        assert_eq!(spec.config, json!({}));
        assert_eq!(spec.display_name, None);
    }

    /// Mental-revert: read unknown fields past and `display_name` is dropped,
    /// so the node takes the class's short name while the caller believes it
    /// named it.
    #[tokio::test]
    async fn tools_call_add_node_refuses_an_argument_it_does_not_take() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "stream": STUB_STREAM_ID,
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "display_name": "gray"
            }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("display_name"), "{text}");
        assert!(recorded.lock().is_empty(), "nothing was added");
    }

    /// The address `connect` hands the engine for a port on this node.
    #[tokio::test]
    async fn tools_call_connect_and_disconnect_reach_their_runtime_ops() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let connect_body = tool_call_result(
            runtime.clone(),
            "connect",
            json!({
                "stream": STUB_STREAM_ID, "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream"
            }),
        )
        .await;
        let stated = first_text_block_json(&connect_body);
        assert_eq!(
            stated["link_id"],
            crate::control_plane_stub_support::STUB_CREATED_LINK_ID
        );

        let disconnect_body = tool_call_result(
            runtime,
            "disconnect",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-9" }),
        )
        .await;
        assert_eq!(disconnect_body["isError"], false, "body={disconnect_body}");

        let recorded = recorded.lock();
        let [
            crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to),
            crate::control_plane_stub_support::RecordedGraphMutation::Disconnect(link_id),
        ] = &recorded[..]
        else {
            panic!("expected a connect then a disconnect, recorded {recorded:?}");
        };
        // Each end is the node the tool looked up by name.
        assert_eq!(from.processor_id().as_str(), "camera-id");
        assert_eq!(from.port_name(), "video");
        assert_eq!(to.processor_id().as_str(), "fx-id");
        assert_eq!(to.port_name(), "video_from_upstream");
        assert_eq!(link_id.as_str(), "link-9");
    }

    /// Each end's node is looked up by its name cast, so a caller that typed a
    /// name the way it reads still reaches the node `graph` lists; the port
    /// names reach the engine as given, and the engine casts them.
    #[tokio::test]
    async fn tools_call_connect_resolves_each_ends_node_by_its_cast_name() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = call_the_connect_tool(
            runtime,
            json!({
                "stream": STUB_STREAM_ID,
                "from_node": "Front Camera", "from_port": "Video",
                "to_node": "Gray FX", "to_port": "video_from_upstream",
            }),
        )
        .await;
        assert_eq!(body["isError"], false, "body={body}");

        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to)] =
            &recorded[..]
        else {
            panic!("expected one connect op, recorded {recorded:?}");
        };
        assert_eq!(from.processor_id().as_str(), "front-camera-id");
        assert_eq!(from.port_name(), "Video");
        assert_eq!(to.processor_id().as_str(), "gray-fx-id");
        assert_eq!(to.port_name(), "video_from_upstream");
    }

    /// Call `connect` with `arguments` and hand back the whole tool result.
    async fn call_the_connect_tool(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        tool_call_result(runtime, "connect", arguments).await
    }

    /// Call `disconnect` with `arguments` and hand back the whole tool result.
    async fn call_the_disconnect_tool(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        tool_call_result(runtime, "disconnect", arguments).await
    }

    /// Every end needs its node and its port, and a call missing one is
    /// refused naming the field rather than wired against a guess.
    #[tokio::test]
    async fn tools_call_connect_refuses_an_end_missing_its_node_or_port_naming_the_field() {
        for missing_field in ["from_node", "from_port", "to_node", "to_port"] {
            let mut arguments = json!({
                "stream": STUB_STREAM_ID, "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream",
            });
            arguments
                .as_object_mut()
                .unwrap()
                .remove(missing_field)
                .expect("the field this case removes");
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(missing_field),
                "{arguments} must be refused naming {missing_field}: {text}"
            );
            assert!(
                recorded_calls.lock().is_empty(),
                "a refused connect reaches no runtime op"
            );
        }
    }

    /// An end named by an argument `connect` does not take is refused naming
    /// it — the id and display-name spellings included, which a caller holding
    /// an older `graph` would still send.
    #[tokio::test]
    async fn tools_call_connect_refuses_an_argument_it_does_not_take_naming_it() {
        for unknown_field in [
            "from_processor_id",
            "from_processor_display_name",
            "to_processor_id",
            "to_processor_display_name",
        ] {
            let mut arguments = json!({
                "stream": STUB_STREAM_ID, "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream",
            });
            arguments[unknown_field] = json!("camera");
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(unknown_field),
                "{arguments} must be refused naming {unknown_field}: {text}"
            );
            assert!(
                recorded_calls.lock().is_empty(),
                "a refused connect reaches no runtime op"
            );
        }
    }

    /// A misspelled argument is refused rather than read past.
    #[tokio::test]
    async fn tools_call_connect_refuses_a_misspelled_argument_rather_than_reading_past_it() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);

        let body = call_the_connect_tool(
            runtime,
            json!({
                "stream": STUB_STREAM_ID,
                "from_node": "camera",
                "from_port": "video",
                "to_node": "fx",
                "to_prot": "video",
            }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("to_prot"), "{text}");
        assert!(recorded_calls.lock().is_empty(), "nothing was wired");
    }

    /// Both ends are nodes on this node's own graph, so a node it does not
    /// hold, at either end, is refused naming it before any runtime op is
    /// reached.
    #[tokio::test]
    async fn tools_call_connect_refuses_a_node_this_node_does_not_hold_at_either_end() {
        for arguments in [
            json!({
                "stream": STUB_STREAM_ID,
                "from_node": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME,
                "from_port": "video",
                "to_node": "fx", "to_port": "video",
            }),
            json!({
                "stream": STUB_STREAM_ID, "from_node": "camera", "from_port": "video",
                "to_node": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME,
                "to_port": "video",
            }),
        ] {
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);

            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(text.contains("connect failed"), "{text}");
            assert!(
                text.contains(crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME),
                "{text}"
            );
            assert!(recorded_calls.lock().is_empty(), "nothing was wired");
        }
    }

    /// `disconnect` takes its stream and a `link_id` and nothing else: a
    /// missing one, or any other argument, is refused naming it before any
    /// runtime op is reached.
    #[tokio::test]
    async fn tools_call_disconnect_takes_its_stream_and_a_link_id_alone() {
        for (arguments, what_the_refusal_must_name) in [
            (json!({ "stream": STUB_STREAM_ID }), "link_id"),
            (json!({ "link_id": "link-9" }), "stream"),
            (
                json!({
                    "stream": STUB_STREAM_ID,
                    "link_id": "link-9",
                    "machine": "studio-display",
                }),
                "machine",
            ),
        ] {
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_disconnect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(what_the_refusal_must_name),
                "{arguments} must be refused naming {what_the_refusal_must_name}: {text}"
            );
            assert!(recorded_calls.lock().is_empty());
        }
    }

    /// The result is the new link's id and how this node's own `graph` reads
    /// it, so an agent learns whether its change took without a second call.
    #[tokio::test]
    async fn tools_call_connect_states_the_link_id_and_the_links_state() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        *runtime.exported_graph.lock() = json!({
            "machine": crate::control_plane_stub_support::STUB_MACHINE_NAME,
            "links": [
                { "id": "some-other-link", "state": "wired" },
                { "id": crate::control_plane_stub_support::STUB_CREATED_LINK_ID,
                  "state": "pending" },
            ],
        });

        let body = call_the_connect_tool(
            runtime,
            json!({
                "stream": STUB_STREAM_ID,
                "from_node": "camerasource",
                "from_port": "video",
                "to_node": "fx",
                "to_port": "video_from_upstream",
            }),
        )
        .await;

        let stated = first_text_block_json(&body);
        assert_eq!(
            stated["link_id"],
            crate::control_plane_stub_support::STUB_CREATED_LINK_ID
        );
        assert_eq!(stated["state"], "pending");
        assert_eq!(
            stated.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["link_id", "state"]
        );
    }

    #[tokio::test]
    async fn tools_call_remove_node_resolves_the_name_and_reaches_the_runtime_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "remove_node",
            json!({ "stream": STUB_STREAM_ID, "name": "FX" }),
        )
        .await;
        let stated = first_text_block_json(&body);
        assert_eq!(
            stated,
            json!({ "removed_name": "fx" }),
            "the result names the node as `graph` listed it"
        );

        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::RemoveProcessor(id)] =
            &recorded[..]
        else {
            panic!("expected one remove op, recorded {recorded:?}");
        };
        assert_eq!(
            id.as_str(),
            format!(
                "fx{}",
                crate::control_plane_stub_support::STUB_NODE_ID_SUFFIX
            ),
            "the op is handed the id the runtime resolved the name to"
        );
    }

    #[tokio::test]
    async fn tools_call_remove_node_naming_an_absent_node_is_refused_naming_it() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "remove_node",
            json!({
                "stream": STUB_STREAM_ID,
                "name": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("remove_node failed"), "{text}");
        assert!(
            text.contains(crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME),
            "the refusal must name the node asked for: {text}"
        );
        assert!(recorded.lock().is_empty(), "nothing was removed");
    }

    #[tokio::test]
    async fn a_mutation_tool_with_malformed_arguments_is_a_tool_error_that_reaches_no_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "connect",
            json!({ "stream": STUB_STREAM_ID, "from_node": "camera" }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        assert!(
            recorded.lock().is_empty(),
            "a refused call must reach no runtime op"
        );
    }

    #[tokio::test]
    async fn tools_call_graph_naming_a_stream_returns_that_streams_graph() {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "graph",
            json!({ "stream": STUB_STREAM_ID }),
        )
        .await;
        let graph = first_text_block_json(&body);
        assert_eq!(graph, json!({ "nodes": [], "links": [] }));
    }

    /// Without `stream`, `graph` is every loaded stream's graph under the
    /// machine's name — what an agent reads first to learn what is loaded.
    #[tokio::test]
    async fn tools_call_graph_without_a_stream_renders_every_loaded_stream_under_the_machines_name()
    {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_nodes_graph();

        let body = tool_call_result(Arc::new(runtime), "graph", json!({})).await;

        assert_eq!(
            first_text_block_json(&body),
            json!({
                "machine": STUB_MACHINE_NAME,
                "streams": [two_linked_nodes_graph()],
            })
        );
        let machine_wide: streamlib::sdk::json_schema::MachineWideGraphResponse =
            serde_json::from_value(first_text_block_json(&body))
                .expect("the machine-wide graph parses as the schema the generator writes");
        assert_eq!(machine_wide.streams.len(), 1);
    }

    #[tokio::test]
    async fn tools_call_graph_naming_a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "graph",
            json!({ "stream": "elsewhere" }),
        )
        .await;
        assert_eq!(body["isError"], true, "{body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("elsewhere") && text.contains(STUB_STREAM_ID),
            "{text}"
        );
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_refused_as_invalid_params() {
        let refusal = tool_call_refusal(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "does_not_exist",
            json!({}),
        )
        .await;
        assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    /// `logs` reads a project's log from disk, answering where to read on from,
    /// and holds no record for a project that has written none.
    #[tokio::test]
    async fn tools_call_logs_answers_a_project_that_wrote_no_record_with_none() {
        let project_directory = tempfile::tempdir().unwrap();
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "logs",
            json!({
                "project_directory": project_directory.path(),
                "stream": STUB_STREAM_ID,
                "count": 16,
            }),
        )
        .await;

        assert_eq!(
            first_text_block_json(&body),
            json!({
                "project_directory": project_directory.path(),
                "records": [],
                "next_after": "1:0",
                "records_no_longer_held": false,
            })
        );
    }

    #[tokio::test]
    async fn tools_call_logs_refuses_a_call_naming_no_project_a_relative_one_or_a_malformed_cursor()
    {
        let project_directory = tempfile::tempdir().unwrap();
        for (arguments, what_the_refusal_must_name) in [
            (json!({ "stream": STUB_STREAM_ID }), "project_directory"),
            (json!({ "project_directory": "relative" }), "relative"),
            (
                json!({ "project_directory": project_directory.path(), "since": 3 }),
                "since",
            ),
            (
                json!({ "project_directory": project_directory.path(), "after": "seven" }),
                "seven",
            ),
        ] {
            let body = tool_call_result(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                "logs",
                arguments.clone(),
            )
            .await;
            assert_eq!(body["isError"], true, "{arguments}: {body}");
            let text = body["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains(what_the_refusal_must_name),
                "{arguments} must be refused naming {what_the_refusal_must_name}: {text}"
            );
        }
    }

    /// The words the stub runtime refuses every stream action in: a refusal
    /// carrying them reached the runtime, one without them was refused before.
    const THE_STUB_RUNTIMES_STREAM_ACTION_REFUSAL: &str = "takes no stream action";

    /// Every stream tool refuses an argument it does not take, a required one
    /// missing, a relative project directory and a level that is not one of
    /// the three, naming what is wrong, before any stream action runs.
    #[tokio::test]
    async fn every_stream_tool_refuses_malformed_arguments_before_any_stream_action() {
        let absolute_project_directory = "/home/someone/projects/camera";
        for (tool_name, arguments, what_the_refusal_must_name) in [
            (
                "run_stream",
                json!({ "project_directory": absolute_project_directory, "keep": true }),
                "keep",
            ),
            ("run_stream", json!({}), "project_directory"),
            (
                "run_stream",
                json!({ "project_directory": absolute_project_directory, "detach": true }),
                "detach",
            ),
            (
                "run_stream",
                json!({ "project_directory": "projects/camera" }),
                "absolute",
            ),
            (
                "run_stream",
                json!({ "project_directory": absolute_project_directory, "name": 7 }),
                "7",
            ),
            ("stop_stream", json!({}), "stream"),
            (
                "stop_stream",
                json!({ "stream": STUB_STREAM_ID, "reason": "done" }),
                "reason",
            ),
            ("start_stream", json!({}), "stream"),
            (
                "start_stream",
                json!({ "stream": STUB_STREAM_ID, "keep": true }),
                "keep",
            ),
            ("remove_project", json!({}), "project_directory"),
            (
                "remove_project",
                json!({ "project_directory": "relative/project" }),
                "absolute",
            ),
            (
                "remove_project",
                json!({ "project_directory": absolute_project_directory, "force": true }),
                "force",
            ),
            (
                "rename_stream",
                json!({ "stream": STUB_STREAM_ID }),
                "display_name",
            ),
            (
                "rename_stream",
                json!({ "display_name": "Desk camera" }),
                "stream",
            ),
            (
                "rename_stream",
                json!({ "stream": STUB_STREAM_ID, "display_name": "Desk", "name": "Desk" }),
                "name",
            ),
            (
                "list_streams",
                json!({ "stream": STUB_STREAM_ID }),
                "stream",
            ),
            (
                "expose_port",
                json!({ "stream": STUB_STREAM_ID, "node": "camera", "port": "video" }),
                "level",
            ),
            (
                "expose_port",
                json!({
                    "stream": STUB_STREAM_ID, "node": "camera", "port": "video",
                    "level": "everyone",
                }),
                "everyone",
            ),
            (
                "expose_port",
                json!({
                    "stream": STUB_STREAM_ID, "node": "camera", "port": "video",
                    "level": "public", "exposed": true,
                }),
                "exposed",
            ),
            (
                "expose_port",
                json!({ "node": "camera", "port": "video", "level": "public" }),
                "stream",
            ),
        ] {
            let body = tool_call_result(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                tool_name,
                arguments.clone(),
            )
            .await;
            assert_eq!(body["isError"], true, "{tool_name} {arguments}: {body}");
            let text = body["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains(what_the_refusal_must_name),
                "{tool_name} {arguments} must be refused naming {what_the_refusal_must_name}: \
                 {text}"
            );
            assert!(
                !text.contains(THE_STUB_RUNTIMES_STREAM_ACTION_REFUSAL),
                "{tool_name} {arguments} reached the runtime: {text}"
            );
        }
    }

    /// A stream action the runtime refuses reaches the caller as a tool error
    /// in the runtime's own words, nothing added.
    #[tokio::test]
    async fn a_stream_action_the_runtime_refuses_is_a_tool_error_in_its_own_words() {
        for (tool_name, arguments, action) in [
            (
                "run_stream",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "run_stream",
            ),
            ("stop_stream", json!({ "stream": "camera" }), "stop_stream"),
            (
                "start_stream",
                json!({ "stream": "camera" }),
                "start_stream",
            ),
            (
                "remove_project",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "remove_project",
            ),
            (
                "expose_port",
                json!({ "stream": "camera", "node": "source", "port": "video", "level": "private" }),
                "expose_port",
            ),
        ] {
            let body = tool_call_result(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                tool_name,
                arguments,
            )
            .await;
            assert_eq!(body["isError"], true, "{tool_name}: {body}");
            assert_eq!(
                body["content"][0]["text"],
                crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(action)
                    .to_string(),
                "{tool_name}"
            );
        }
    }

    #[tokio::test]
    async fn list_streams_answers_each_streams_id_display_name_location_state_node_count_and_failure()
     {
        for arguments in [json!({}), json!(null)] {
            let (_served, client) =
                connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
            let mut request = CallToolRequestParams::new("list_streams");
            if let Some(arguments) = arguments.as_object() {
                request = request.with_arguments(arguments.clone());
            }
            let body = wire_outcome(client.call_tool(request).await).unwrap();

            assert_eq!(
                first_text_block_json(&body),
                json!({
                    "streams": [{
                        "stream": STUB_STREAM_ID,
                        "display_name": STUB_STREAM_ID,
                        "location": null,
                        "state": "running",
                        "node_count": null,
                        "failed_because": null,
                        "action": null,
                        "step": null,
                    }]
                }),
                "arguments {arguments}"
            );
        }
    }

    /// The `stream` argument each tool advertises: required on every tool
    /// that acts on one stream, optional on `graph` and `logs`, and absent from the tools
    /// that name none.
    #[tokio::test]
    async fn every_tool_acting_on_one_stream_requires_its_stream_and_graph_takes_it_optionally() {
        let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        for tool in &tools {
            let tool_name = tool["name"].as_str().expect("a named tool");
            let advertises_stream = tool["inputSchema"]["properties"]["stream"].is_object();
            let requires_stream = tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|required| required.contains(&json!("stream")));
            let (must_advertise, must_require) = if TOOLS_THAT_REQUIRE_A_STREAM.contains(&tool_name)
            {
                (true, true)
            } else if tool_name == "graph" || tool_name == "logs" {
                (true, false)
            } else {
                (false, false)
            };
            assert_eq!(
                (advertises_stream, requires_stream),
                (must_advertise, must_require),
                "`{tool_name}`: {}",
                tool["inputSchema"]
            );
        }
        for tool_name in TOOLS_THAT_REQUIRE_A_STREAM {
            assert!(
                tools.iter().any(|tool| tool["name"] == *tool_name),
                "`{tool_name}` is listed"
            );
        }
    }

    /// `run_stream`'s, `expose_port`'s and `logs`' schemas say what each takes: a
    /// required absolute project directory and `keep`, one of the three
    /// levels, and a count with no maximum, since a larger one is clamped.
    #[tokio::test]
    async fn the_run_stream_expose_port_and_logs_schemas_state_what_each_takes() {
        let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let input_schema_of = |tool_name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == tool_name)
                .unwrap_or_else(|| panic!("the `{tool_name}` tool is listed"))["inputSchema"]
                .clone()
        };
        let required_of = |input_schema: &Value| {
            let mut required: Vec<String> = input_schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect();
            required.sort_unstable();
            required
        };

        let run_stream = input_schema_of("run_stream");
        assert_eq!(required_of(&run_stream), ["project_directory"]);
        let mut run_stream_arguments: Vec<&String> = run_stream["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        run_stream_arguments.sort_unstable();
        assert_eq!(
            run_stream_arguments,
            ["name", "project_directory", "stream_function"]
        );

        let expose_port = input_schema_of("expose_port");
        assert_eq!(
            required_of(&expose_port),
            ["level", "node", "port", "stream"]
        );
        let rendered_level = expose_port.to_string();
        for level in ["internal", "private", "public"] {
            assert!(rendered_level.contains(level), "{rendered_level}");
        }

        assert_eq!(
            required_of(&input_schema_of("list_streams")),
            Vec::<String>::new()
        );

        let logs_count = &input_schema_of("logs")["properties"]["count"];
        assert_eq!(logs_count["minimum"], json!(1), "{logs_count}");
        assert!(
            logs_count.get("maximum").is_none(),
            "a count past the most the runtime returns is clamped, never refused: {logs_count}"
        );
        assert!(
            logs_count["description"]
                .as_str()
                .is_some_and(|description| description.contains("clamped")),
            "{logs_count}"
        );
    }

    // ------------------------------------------------------------------------
    // Resources and prompts
    // ------------------------------------------------------------------------

    use streamlib::sdk::descriptors::{
        PortDescriptor, ProcessorClassShortName, ProcessorDescriptor,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    /// Two nodes and the one link between them, in the shape the engine's
    /// graph export takes, so the prompts parse what a real node answers.
    fn two_linked_nodes_graph() -> Value {
        json!({
            "stream": crate::control_plane_stub_support::STUB_STREAM_ID,
            "nodes": [
                {
                    "id": "PatternSourceId",
                    "type": "graph_probes:PatternSource",
                    "name": "pattern",
                    "ports": {
                        "inputs": [],
                        "outputs": [{ "name": "video", "description": "", "delivery_profile": null }]
                    },
                    "components": { "state": "Running" }
                },
                {
                    "id": "WindowSinkId",
                    "type": "graph_probes:WindowSink",
                    "name": "window",
                    "ports": {
                        "inputs": [{ "name": "video", "description": "", "delivery_profile": "newest" }],
                        "outputs": []
                    },
                    "components": { "state": "Running" }
                }
            ],
            "links": [{
                "id": "link-pattern-to-window",
                "source": { "node": "pattern", "port": "video" },
                "target": { "node": "window", "port": "video" },
                "state": "wired",
                "components": {}
            }],
            "exposed": [],
            "machine": "desk"
        })
    }

    /// The same graph after the helper that was to open the link refused it —
    /// what a live `connect` onto a helper-placed node leaves behind when
    /// that helper's port could not open.
    ///
    /// Kept beside the wired fixture rather than replacing its link: the
    /// prompts pick a link out of the graph they are rendered against, and an
    /// errored one has no business in the recipes' happy path.
    fn two_linked_nodes_graph_whose_link_a_helper_refused() -> Value {
        let mut graph = two_linked_nodes_graph();
        graph["links"][0]["state"] = json!("error");
        graph["links"][0]["error_reason"] = json!(
            "could not wire input port \"video\": BufferSizeExceedsMaxSupportedBufferSizeOfService"
        );
        graph
    }

    /// An agent is told to read a link's state, so a refused link has to reach
    /// it with the helper's own reason rather than a bare `error`.
    #[tokio::test]
    async fn tools_call_graph_carries_the_reason_a_helper_refused_a_link_for() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_nodes_graph_whose_link_a_helper_refused();

        let body = tool_call_result(
            Arc::new(runtime),
            "graph",
            json!({ "stream": STUB_STREAM_ID }),
        )
        .await;
        let text = body["content"][0]["text"].as_str().unwrap();
        let graph: Value = serde_json::from_str(text).unwrap();
        assert_eq!(graph["links"][0]["state"], "error");
        assert!(
            graph["links"][0]["error_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("BufferSizeExceeds")),
            "the helper's own reason is what tells an agent what to fix; got {graph}"
        );
    }

    /// The control vocabulary tells an agent to confirm `wired` in `graph`. A
    /// link onto a helper is not wired when `connect` returns, so the same text
    /// has to say what `pending` and `error` mean or the confirmation is a
    /// guess.
    #[tokio::test]
    async fn the_instructions_and_every_wiring_prompt_say_what_pending_and_error_mean() {
        register_a_virtual_camera_sink_probe_once();
        let mut texts = vec![(
            "instructions",
            LOCAL_API_MCP_SERVER_INSTRUCTIONS.to_string(),
        )];
        for (recipe, arguments) in [
            (
                "insert_node_between_linked_nodes",
                json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
            ),
        ] {
            texts.push((
                recipe,
                prompt_text(stub_serving_two_linked_nodes(), recipe, arguments).await,
            ));
        }
        for (name, text) in texts {
            assert!(
                text.contains("pending") && text.contains("error_reason"),
                "{name} tells an agent to confirm a link's state but not what a link left \
                 `pending` by a helper that has not answered, or refused with a reason, \
                 means:\n{text}"
            );
        }
    }

    fn stub_serving_two_linked_nodes() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_nodes_graph();
        Arc::new(runtime)
    }

    /// The registry is process-global and refuses a second registration of a
    /// path, so the probe standing in for the built-in registers once for the
    /// whole test binary.
    fn register_a_virtual_camera_sink_probe_once() {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("VirtualCameraSink").unwrap(),
                        ProcessorClassImportPath::new(
                            crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
                        )
                        .unwrap(),
                        "a virtual-camera probe",
                    )
                    .with_config_schema(
                        json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
                    )
                    .with_input(
                        PortDescriptor::new("video", "frames to present", true)
                            .with_delivery_profile("newest"),
                    ),
                )
                .expect("the virtual camera path is registered by this helper alone");
        });
    }

    async fn listed_resources_result(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_resources(None).await).unwrap()
    }

    async fn listed_resource_templates_result(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_resource_templates(None).await).unwrap()
    }

    async fn listed_prompts_result(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_prompts(None).await).unwrap()
    }

    async fn resource_read_outcome(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        uri: &str,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(
            client
                .read_resource(ReadResourceRequestParams::new(uri))
                .await,
        )
    }

    async fn prompt_outcome(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        prompt_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        let request = GetPromptRequestParams::new(prompt_name)
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        wire_outcome(client.get_prompt(request).await)
    }

    pub(crate) async fn resource_document(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        uri: &str,
    ) -> Value {
        let result = resource_read_outcome(runtime, uri)
            .await
            .unwrap_or_else(|refusal| panic!("reading `{uri}` was refused: {refusal:?}"));
        let contents = result["contents"].as_array().expect("a contents array");
        assert_eq!(contents.len(), 1, "one document per resource: {result}");
        assert_eq!(contents[0]["uri"], uri);
        assert_eq!(contents[0]["mimeType"], "application/json");
        serde_json::from_str(contents[0]["text"].as_str().expect("a text document"))
            .expect("the document is JSON")
    }

    pub(crate) async fn prompt_text(
        runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        prompt_name: &str,
        arguments: Value,
    ) -> String {
        let result = prompt_outcome(runtime, prompt_name, arguments)
            .await
            .unwrap_or_else(|refusal| panic!("`{prompt_name}` was refused: {refusal:?}"));
        let messages = result["messages"].as_array().expect("a messages array");
        assert_eq!(messages.len(), 1, "{result}");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"]["type"], "text");
        messages[0]["content"]["text"].as_str().unwrap().to_string()
    }

    /// The tools a recipe's numbered steps call, in order — what a client
    /// following the text step by step dispatches.
    fn tool_names_the_numbered_steps_call(prompt_text: &str) -> Vec<String> {
        prompt_text
            .lines()
            .filter_map(|line| {
                let (step_number, rest) = line.split_once(". `")?;
                if step_number.is_empty() || !step_number.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                rest.split_once('`')
                    .map(|(tool_name, _)| tool_name.to_string())
            })
            .collect()
    }

    fn served_tool_names() -> Vec<String> {
        LocalApiMcpServerHandler::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    #[tokio::test]
    async fn resources_list_names_the_node_catalog_the_live_graph_and_the_relay_adapter_catalog() {
        let result =
            listed_resources_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

        let uris: Vec<&str> = result["resources"]
            .as_array()
            .expect("a resources array")
            .iter()
            .map(|resource| resource["uri"].as_str().unwrap())
            .collect();
        assert_eq!(
            uris,
            [
                "streamlib://node-catalog",
                "streamlib://graph",
                "streamlib://relay-adapter-catalog"
            ]
        );
        for resource in result["resources"].as_array().unwrap() {
            assert_eq!(resource["mimeType"], "application/json", "{resource}");
            assert!(
                resource["description"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
        }

        let templates =
            listed_resource_templates_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()))
                .await;
        assert_eq!(templates["resourceTemplates"], json!([]), "{templates}");
    }

    /// Mental revert: render the catalog once at startup and cache it, and the
    /// second read misses the type registered after the first.
    #[tokio::test]
    async fn the_catalog_resource_renders_the_registry_as_it_stands_at_each_read() {
        let class_import_path =
            "streamlib_api_server::catalog_resource_probe::RegisteredBetweenReads";
        let entry_for_the_probe = |catalog: &Value| {
            catalog["nodes"]
                .as_array()
                .expect("a node type list")
                .iter()
                .find(|entry| entry["type"] == class_import_path)
                .cloned()
        };

        let before = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://node-catalog",
        )
        .await;
        assert!(entry_for_the_probe(&before).is_none());

        let config_schema = json!({
            "type": "object",
            "properties": { "strength": { "type": "number", "default": 0.5 } }
        });
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("RegisteredBetweenReads").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "registered after the first read",
                )
                .with_config_schema(config_schema.clone())
                .with_input(
                    PortDescriptor::new("video_from_upstream", "", true)
                        .with_delivery_profile("newest"),
                ),
            )
            .expect("the probe's path is registered by this test alone");

        let after = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://node-catalog",
        )
        .await;
        let entry = entry_for_the_probe(&after).expect("the type registered between reads");
        assert_eq!(entry["config_schema"], config_schema);
        assert_eq!(entry["inputs"][0]["name"], "video_from_upstream");
        assert_eq!(entry["inputs"][0]["delivery_profile"], "newest");
        assert_eq!(
            after["streams"],
            json!([{ "stream": STUB_STREAM_ID, "nodes": [] }]),
            "each loaded stream lists the types its own interpreter described"
        );
    }

    /// The graph resource is every loaded stream's graph under the runtime's
    /// name, rendered at each read.
    #[tokio::test]
    async fn the_graph_resource_renders_every_loaded_streams_graph_as_it_stands_at_each_read() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let exported_graph = runtime.exported_graph.clone();

        let before = resource_document(runtime.clone(), "streamlib://graph").await;
        assert_eq!(
            before,
            json!({
                "machine": STUB_MACHINE_NAME,
                "streams": [{ "nodes": [], "links": [] }],
            })
        );

        *exported_graph.lock() = two_linked_nodes_graph();
        let after = resource_document(runtime, "streamlib://graph").await;
        assert_eq!(
            after,
            json!({ "machine": STUB_MACHINE_NAME, "streams": [two_linked_nodes_graph()] })
        );
    }

    #[tokio::test]
    async fn reading_a_resource_the_node_does_not_serve_is_refused_naming_the_uri() {
        let refusal = resource_read_outcome(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://contracts",
        )
        .await
        .expect_err("an unserved uri is refused");
        assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            refusal.message.contains("streamlib://contracts"),
            "{refusal:?}"
        );
    }

    #[tokio::test]
    async fn prompts_list_names_the_three_recipes_and_their_arguments() {
        let result =
            listed_prompts_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let prompts = result["prompts"].as_array().expect("a prompts array");

        let described: Vec<(String, Vec<(String, bool)>)> = prompts
            .iter()
            .map(|prompt| {
                let arguments = prompt["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|argument| {
                        (
                            argument["name"].as_str().unwrap().to_string(),
                            argument["required"].as_bool().unwrap(),
                        )
                    })
                    .collect();
                (prompt["name"].as_str().unwrap().to_string(), arguments)
            })
            .collect();
        let owned = |name: &str, arguments: &[(&str, bool)]| {
            (
                name.to_string(),
                arguments
                    .iter()
                    .map(|(argument, required)| (argument.to_string(), *required))
                    .collect::<Vec<_>>(),
            )
        };
        let mut expected = vec![
            owned(
                "insert_node_between_linked_nodes",
                &[("stream", true), ("link_id", true), ("type", true)],
            ),
            owned(
                "fan_output_to_another_consumer",
                &[
                    ("stream", true),
                    ("from_node", true),
                    ("from_port", true),
                    ("type", true),
                ],
            ),
            owned(
                "show_channel_on_virtual_camera",
                &[
                    ("stream", true),
                    ("from_node", true),
                    ("from_port", true),
                    ("camera_name", false),
                ],
            ),
        ];
        let mut described = described;
        described.sort();
        expected.sort();
        assert_eq!(described, expected);
    }

    /// A prompt is a recipe over the tool set, never a verb of its own: every
    /// step of every recipe calls a tool `tools/list` serves.
    #[tokio::test]
    async fn every_step_of_every_prompt_calls_a_tool_the_node_serves() {
        register_a_virtual_camera_sink_probe_once();
        let served = served_tool_names();

        for (prompt_name, arguments) in [
            (
                "insert_node_between_linked_nodes",
                json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
            ),
        ] {
            let text = prompt_text(stub_serving_two_linked_nodes(), prompt_name, arguments).await;
            let called = tool_names_the_numbered_steps_call(&text);
            assert!(!called.is_empty(), "{prompt_name} lists no steps:\n{text}");
            for tool_name in &called {
                assert!(
                    served.contains(tool_name),
                    "{prompt_name} calls `{tool_name}`, which the node does not serve:\n{text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_insert_prompt_splices_the_named_link_in_an_order_that_never_leaves_it_unfed() {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        for named in [
            "`effects:Blur`",
            "`pattern`",
            "`window`",
            "`link-pattern-to-window`",
        ] {
            assert!(
                text.contains(named),
                "the recipe must name {named}:\n{text}"
            );
        }
    }

    /// The type an agent is about to add is described from the catalog as it
    /// is now, so the config keys it passes are the ones the node will take.
    #[tokio::test]
    async fn a_registered_types_catalog_entry_is_rendered_into_the_prompt() {
        let class_import_path = "streamlib_api_server::prompt_catalog_probe::GrayscaleEffect";
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("GrayscaleEffect").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "a prompt-rendering probe",
                )
                .with_config_schema(json!({
                    "type": "object",
                    "properties": { "grayscale_strength_probe_key": { "type": "number" } }
                })),
            )
            .expect("the probe's path is registered by this test alone");

        let registered = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": class_import_path }),
        )
        .await;
        assert!(
            registered.contains("grayscale_strength_probe_key"),
            "a registered type's config schema must reach the recipe:\n{registered}"
        );

        let unregistered = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "never_imported:Effect" }),
        )
        .await;
        assert!(
            unregistered.contains("not in this node's catalog yet"),
            "an unregistered type must be said to be absent, not described:\n{unregistered}"
        );
    }

    #[tokio::test]
    async fn the_virtual_camera_prompt_adds_the_registered_sink_on_its_own_input_port() {
        register_a_virtual_camera_sink_probe_once();

        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "camera_name": "Desk \"cam\"" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "connect", "graph"]
        );
        assert!(
            text.contains(crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH),
            "{text}"
        );
        assert!(text.contains("`to_port`: `video`"), "{text}");
        assert!(
            text.contains(r#"{"name":"Desk \"cam\""}"#),
            "the camera name must reach the config as escaped JSON:\n{text}"
        );
    }

    #[tokio::test]
    async fn a_prompt_request_naming_nothing_the_node_has_is_refused_by_name() {
        for (case, prompt_name, arguments, named) in [
            (
                "an unknown prompt",
                "delete_everything",
                json!({}),
                "delete_everything",
            ),
            (
                "a missing required argument",
                "insert_node_between_linked_nodes",
                json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window" }),
                "`type`",
            ),
            (
                "a missing stream",
                "show_channel_on_virtual_camera",
                json!({ "from_node": "pattern", "from_port": "video" }),
                "`stream`",
            ),
            (
                "a stream not loaded",
                "show_channel_on_virtual_camera",
                json!({ "stream": "elsewhere", "from_node": "pattern", "from_port": "video" }),
                STUB_STREAM_ID,
            ),
            (
                "a link the graph does not have",
                "insert_node_between_linked_nodes",
                json!({ "stream": STUB_STREAM_ID, "link_id": "no-such-link", "type": "effects:Blur" }),
                "no-such-link",
            ),
            (
                "a node the graph does not have",
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": "nosuchnode", "from_port": "video" }),
                "nosuchnode",
            ),
            (
                "a port that is not one of the node's outputs",
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": "window", "from_port": "video" }),
                "video",
            ),
            (
                "an argument that is not a string",
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": 7, "from_port": "video" }),
                "7",
            ),
            (
                "a misspelled argument",
                "show_channel_on_virtual_camera",
                json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "camera_nme": "desk" }),
                "camera_nme",
            ),
        ] {
            let refusal = prompt_outcome(stub_serving_two_linked_nodes(), prompt_name, arguments)
                .await
                .expect_err(case);
            assert_eq!(
                refusal.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{case}: {refusal:?}"
            );
            assert!(
                refusal.message.contains(named),
                "{case} must be refused naming `{named}`: {refusal:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_fan_prompt_adds_one_consumer_and_wires_it_to_the_named_port() {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "graph", "connect", "graph"]
        );
        assert!(
            text.contains("`from_node`: `pattern`, `from_port`: `video`"),
            "{text}"
        );
    }

    /// A consumer reading another profile than the ones an output port already
    /// feeds is fanned like any other. Mental revert: restore the refusal and
    /// the recipe is an error naming both profiles.
    #[tokio::test]
    async fn the_fan_prompt_wires_a_consumer_reading_another_profile_than_the_port_already_feeds() {
        let ordered_input_probe = register_a_sole_input_probe_once("ordered");

        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video", "type": ordered_input_probe }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "graph", "connect", "graph"]
        );
    }

    /// The virtual camera's `newest` input joins a port whatever its other
    /// consumers read. Mental revert: restore the refusal and a port already
    /// feeding an `ordered` consumer refuses the recipe.
    #[tokio::test]
    async fn the_virtual_camera_prompt_wires_onto_a_port_that_feeds_an_ordered_consumer() {
        register_a_virtual_camera_sink_probe_once();
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_nodes_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "show_channel_on_virtual_camera",
            json!({ "stream": STUB_STREAM_ID, "from_node": "pattern", "from_port": "video" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "connect", "graph"]
        );
    }

    /// A probe with one input declaring `delivery_profile`, registered once per
    /// profile for the test binary.
    fn register_a_sole_input_probe_once(delivery_profile: &'static str) -> String {
        static REGISTERED_PROFILES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        let class_import_path = format!(
            "streamlib_api_server::prompt_delivery_profile_probe::{delivery_profile}::SoleInputProbe"
        );
        let mut registered_profiles = REGISTERED_PROFILES.lock();
        if !registered_profiles.contains(&delivery_profile) {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("SoleInputProbe").unwrap(),
                        ProcessorClassImportPath::new(&class_import_path).unwrap(),
                        "a probe with one input",
                    )
                    .with_input(
                        PortDescriptor::new("bags_from_upstream", "", true)
                            .with_delivery_profile(delivery_profile),
                    )
                    .with_output(PortDescriptor::new(
                        "bags_to_downstream",
                        "",
                        true,
                    )),
                )
                .expect("each probe path is registered by this helper alone");
            registered_profiles.push(delivery_profile);
        }
        class_import_path
    }

    /// Consumers of one output port read it under whatever profiles they each
    /// declare, so a type reading another profile than the link it is spliced
    /// into still takes the zero-gap order: both new links first, the old one
    /// last. Mental revert: take the link out first again and the target sees a
    /// gap no refusal calls for.
    #[tokio::test]
    async fn inserting_a_type_that_reads_another_profile_than_the_link_connects_before_it_disconnects()
     {
        for (inserted_profile, replaced_profile) in [("newest", "ordered"), ("ordered", "newest")] {
            let inserted_probe = register_a_sole_input_probe_once(inserted_profile);
            let runtime = ControlPlaneMcpDispatchStubRuntime::new();
            let mut graph = two_linked_nodes_graph();
            graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!(replaced_profile);
            *runtime.exported_graph.lock() = graph;

            let text = prompt_text(
                Arc::new(runtime),
                "insert_node_between_linked_nodes",
                json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": inserted_probe }),
            )
            .await;

            assert_eq!(
                tool_names_the_numbered_steps_call(&text),
                [
                    "add_node",
                    "graph",
                    "connect",
                    "connect",
                    "disconnect",
                    "graph"
                ],
                "a `{inserted_profile}` type into a `{replaced_profile}` link:\n{text}"
            );
        }
    }

    /// A class the agent just wrote is not in the catalog until its first add,
    /// and nothing about the recipe waits on it: the order is the zero-gap one
    /// and no note asks the agent to read a profile before wiring.
    #[tokio::test]
    async fn an_insert_of_an_uncatalogued_type_connects_before_it_disconnects_with_no_profile_note()
    {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:WrittenJustNow" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        assert!(
            !text.contains("delivery profile") && !text.contains("remove_node"),
            "nothing about the insert depends on the new type's profile:\n{text}"
        );
    }

    #[tokio::test]
    async fn inserting_into_a_link_whose_target_takes_one_inbound_link_removes_the_link_first() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_nodes_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        graph["nodes"][1]["ports"]["inputs"][0]["audio_window"] =
            json!({ "resolved_from": "match_device" });
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "insert_node_between_linked_nodes",
            json!({ "stream": STUB_STREAM_ID, "link_id": "link-pattern-to-window", "type": "effects:NeverImported" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "disconnect",
                "connect",
                "connect",
                "graph"
            ]
        );
        assert!(text.contains("audio window contract"), "{text}");
    }

    #[test]
    fn each_exposure_level_reaches_the_wire_as_the_engine_spells_it() {
        for engine_level in [
            OutputPortExposureLevel::Internal,
            OutputPortExposureLevel::Private,
            OutputPortExposureLevel::Public,
        ] {
            assert_eq!(
                serde_json::to_value(expose_port_level_on_the_wire(engine_level)).unwrap(),
                serde_json::to_value(engine_level).unwrap()
            );
        }
    }

    #[test]
    fn each_listing_state_reaches_the_wire_under_its_own_name() {
        for (engine_state, wire_spelling) in [
            (StreamListingState::Running, "running"),
            (StreamListingState::Stopped, "stopped"),
            (StreamListingState::Failed, "failed"),
        ] {
            assert_eq!(
                serde_json::to_value(listed_stream_state_on_the_wire(engine_state)).unwrap(),
                json!(wire_spelling)
            );
        }
    }
}
