// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime's MCP server, served by the official SDK, `rmcp`.
//!
//! [`LocalApiMcpServerHandler`] is the runtime's whole MCP surface. Its tools are generated from
//! the control operation registry's agent surface ([`crate::mcp_agent_tools`]): the operations
//! the registry lists as tools of their own, and `list_operation_groups`,
//! `describe_operations` and `call_operation`, which reach every other operation. Beside them
//! it serves the node catalog and the live graph as resources ([`crate::mcp_resources`]) and
//! recipes over those operations as prompts ([`crate::mcp_prompts`]). `rmcp` owns the
//! protocol; this module owns only what the runtime says through it.
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

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ResultType, ServerCapabilities, ServerConfig, SubscriptionFilter, Tool,
};
use rmcp::prompt_handler;
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
    DescribeStreamActionToolResult, DescribeStreamActionToolResultStream, ProjectVenvSyncOnTheWire,
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

/// The guidance `server/discover` hands an agent before its first call.
pub(crate) const LOCAL_API_MCP_SERVER_INSTRUCTIONS: &str = "StreamLib's runtime control plane: this machine's one runtime, which loads any number of streams, each a stream asset of a project: its graph document, `streams/<name>.graph.json`, which holds each node by the UUID the runtime gave it. Every name in backticks below that is not in the tool list is an operation called through `call_operation`, with its name as `operation` and what it takes as `params`: `list_operation_groups` lists every operation by group, and `describe_operations` states each one's params and result. A project is open or closed, and an open project's playback is stopped, playing or paused; its graph reads and edits whatever its playback does. `open_project` opens a project from its absolute `project_directory`, and `reload_project` softly re-reads an open one from disk, keeping its playback: each answers at once with an `action` id while the project's own queue reads each stream asset's graph document, makes its `.venv` when it does not run, however long uv takes, and loads each stream — an open holding each stopped — one action at a time per project, projects side by side; follow it with `describe_stream_action` until its `state` is `done`, `failed` or `cancelled` — its `venv_sync` naming each package uv downloads, builds and installs as it goes — or live through the resource `streamlib://stream-action/<action>`, and every action queued or under way, whoever queued it, through `streamlib://stream-actions`, each of which a `subscriptions/listen` naming it over `/mcp/stdio` is sent `notifications/resources/updated` for as an action it covers changes; end it early with `cancel_stream_action`, and read what it wrote with `logs` given its `action`; `list_assets` narrowed to the `kind` `stream` lists the project's stream assets, running none of its code. `create_project` creates a project — the one `tatolab new` creates, the starter or a copy of a sample project `list_samples` lists — in an absolute `project_directory`, or with none in a new folder `<adjective>-<noun>` under `~/streams`, and queues its open the same way, answering its `action`, the `stream` id its `main` runs under, and the `project_directory`; the stream is shown under the folder's name, which its graph document writes as its `display_name`. `list_streams` lists every stream, each by its stream id with its display name and location, running, stopped, paused, closed, failed or `preparing` — a stream an action is working on, with the `action` and its `step` — a stream implicated in the runtime's last two crashes in a row is failed, carries why in `failed_because`, and is held stopped at the start from its document with none of its node code read, each node drawn as saved, until `reload_project` tries it for real; any other failed stream is held stopped from its document at the start, as at an open. Each stream a load takes in is listed under its stream id, `<project>-<stream>-<hash>`: the project's name, the stream asset's name and a hash over the project's UUID and the stream asset's UUID, kept while the stream is recorded, so moving the project or renaming its document keeps it. Reloaded, a project keeps each stream under the same id: its graph brought to its document in place while the project's node code is unchanged, and loaded again otherwise. A stream is shown under the `display_name` its graph document writes, or the document's file name while it writes none. A load describes each node type its graph names in the stream's own interpreter, which imports that node code, and never fails for what a document holds: a stream whose graph document does not load, or whose load is refused, is listed failed with the reason in `failed_because` until a reload of the fixed document replaces it; one whose nodes validate and some of whose links do not is listed failed and held stopped, each link its validation refuses kept as drawn and never wired, reading `error` in `graph` with the reason in `error_reason`, so `start_playback` refuses it until an edit takes each out or fixes it — an edit keeps those links and refuses a new one; one whose start is refused, or whose project's `.venv` is not made, is listed failed and held stopped, its graph there to read and edit, and `start_playback` tries it again; one whose node code does not describe a type its graph names is listed failed and held stopped from its document, each node of that type held as written with its saved config and ports and reading `Error`, so its graph reads and edits while `start_playback` refuses it until a reload of the fixed code replaces it; and a stream whose document is gone is unloaded and forgotten; only the caller's own mistakes — a directory holding no `tatolab-project.json`, an id another stream holds — fail the action. A project plays and stops as a whole, never one stream of it: every stream a load takes in is recorded, and an open project is re-loaded, held stopped or playing as it was, whenever the runtime starts, until `close_project` or `remove_project` ends it; closing the connection that carried the call unloads nothing. `start_playback` queues the playback of an open project, each stream starting fresh from the graph it holds, reading no document; `stop_playback` queues its end — every processor instance, device claim, function-node state and the clock torn down — keeping each stream's graph loaded to read and edit and recording each stopped; `close_project` queues the end of its playback and the unload of every stream, recording each closed and deleting nothing; a verb refuses a project in a state it does not fit and names the verb that does — `open_project` on an open project names `reload_project`, `start_playback` on a paused one `resume_playback`, any playback verb on a closed one `open_project`; `pause_playback` pauses every loaded stream of a project at once, never waiting behind an action, each listed `paused` with its processors holding their state, a stream a reload loads meanwhile starting paused, each scaled or stepped stream clock held where it stands, until `resume_playback` resumes them and compiles the graph edits made meanwhile — recorded, so a runtime restart re-loads the project paused; `remove_project` queues the unload and forgetting of every stream of a project — each answering at once with its `action`, which `describe_stream_action` follows, and each on its project's queue; `set_stream_clock_scale` runs a started stream's scaled clock at a new `scale` live, its stream time carrying on from where it stands. `set_stream_clock_scale` names a stream by its stream id with a required `stream`, never by its display name; a graph edit names it by its stream asset's UUID, the `stream_asset` `list_streams` reports under its `location`, which is the same on every machine the project is on; `graph` names one optionally, and without it returns every loaded stream's graph under `machine`, this machine's name; `logs` takes the project's absolute `project_directory` and names a stream or a stream action optionally. `describe_runtime` names no stream and answers this runtime's `version`, its `build_id`, `up_for_ms` since its engine started (a restart resets it) and `runtime_log_directory`, the folder of its own log, `null` when it keeps none. Observe a stream with `graph` (nodes by UUID and name, their types and port names, links, and each node's `metrics`: the bags each output port wrote and each inbound link dropped) and `logs` (the project's one log, every stream's records and its runs' venv lines, read from disk so they outlive the stream and the runtime's restarts: pass the result's `next_after` back as `after` to read on). What an output port carries is read from outside its stream in two ways. `open_viewer` holds a node's output port open for a viewer on this machine, naming the node by the `id` `graph` lists, and answers the channel the viewer reads it from with no copy, until `close_viewer` or the connection's end; a node that does not run is refused with `not_running_because` in the result's `structuredContent.refusal` — `playback_stopped`, `playback_paused`, `preparing` or `node_failed` — so a stopped project is told apart from a broken one. A stream sends a port off itself through the `tatolab-moq` gateway nodes: a `MoqTrackSend` linked from the port publishes it as a track at the relay a `MoqGateway` node joins, which any MoQ client reads, and another stream reads that track through a `MoqTrackReceive`. Change its graph with `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group`, `add_note`, `set_note_fields` and `remove_note`, or several of them as one `transact`, each naming a node by its UUID, which `add_node` answers and `graph` lists as `id`, and each one transaction by its `author`, applied whole or refused whole in the runtime's own words: with `mode` `commit`, the default, it is applied to the running stream, written to the stream's graph document and recorded as one commit on the project's history, so it is kept on the next load; with `mode` `live`, applied to the running stream alone. `import_graph_generator` runs one of the project's `@graph_generator` functions, `<module>:<function>`, in the stream's own interpreter and places what it builds as a fragment node, the nodes and links it built stored inside it; run again under the name of a fragment the graph holds, it refreshes that fragment, each node keeping its UUID by name; `refresh_fragments` refreshes, as one transaction, every fragment of a project's loaded streams whose generator a changed module defines, which `tatolab dev` asks for on each save of a `.py` file. `collapse_selection_into_subgraph` saves nodes of a stream's level as a new stream of the project and places it by reference where they were, each link that crossed the selection carried through a `GraphInput` or `GraphOutput` of the new stream; an edit's `level` steps inside a placed stream and is made on that stream's document, which every stream placing it takes, and a stream another stream of the project places runs only where it is placed; `unlock_subgraph` turns a subgraph's nodes back into plain nodes of the stream — a generator's output, which stops being refreshed, or a copy of a placed stream's. `add_group` makes a labelled group of nodes — a title, a description and a colour, listed under `graph`'s `groups` and joined by a node holding its UUID as `studio.group` — which changes nothing that runs; `set_group_fields` edits one and `remove_group` takes one out, its nodes left ungrouped. `add_note` places a canvas note — text at a position and size, listed under `graph`'s `studio.notes` — beside the nodes, which changes nothing that runs; `set_note_fields` edits one and `remove_note` takes one out. A node's `studio` and the graph's are the studio contract, versioned by `studio.contract`: a node's takes `position`, `group`, `comment`, `colour`, `collapsed` and a client's own `metadata`, and a key of the wrong shape or outside it is refused by name. `undo` reverses an author's latest transaction in a project as a new transaction and `redo` reapplies it, each reversing only that author's own. A config value `{\"asset\": \"<uuid>\"}` references an asset the project must hold, which cannot then be deleted. A Python class written to a module the stream's own interpreter can import — a file in the stream's project, or a package installed in its venv — is added by its `module:ClassName` path, and a class of an import's own node code by `imports/<folder>/module:ClassName`, imported from that folder ahead of the project's own code, and runs in its own processor interpreter; a link is spliced in by connecting the new node on both sides, then disconnecting the link it replaces. Read `graph` first for UUIDs and port names, and again afterwards to confirm a link's state is `wired` and the node is `Running`. A `connect` onto a node in a helper process returns before that helper has opened its port, so its link reads `pending` until the helper answers and then `wired`; a link that reads `error` carries the refusing end's own reason in `error_reason` and will never carry a bag — read the reason, `disconnect` it, and fix what it names. A Python node whose helper process died on its own reads `Restarting` while a new helper process, with a new instance of its class, starts in its place after a backoff, and its `components.helper_process_restarts` counts the restarts and carries `last_exit_reason`; one whose helper died five times in a row, each within a minute of starting, is left `Error`, and `reload_project` starts it afresh. A node that reads `Error` carries why in `components.failed_because`, in the words of what failed it — a `start()` that raised among them; a node that has not failed carries `null` there. A node whose `process()` raised, or returned an error, and has not run cleanly since keeps running and carries what it raised in `components.process_raising_because` — the first raise of the run, in its own words, and for a Python node where in its code — and `null` once a call runs cleanly again. A node that reports itself not ready — a setting with nothing sensible to default to left unset, a file it names missing or unreadable — keeps running, its stream with it, and carries why in `components.not_ready_because`, in its own words, and `null` once it reports itself ready; meanwhile the runtime sends a transparent frame on each Image output of a Python node and nothing on its other outputs. Both ends of a `connect` are ports in the one stream it names; a stream reads another stream's track at a relay, on this machine or another, through a `MoqTrackReceive` node. A project's assets — media, scenes and data such as an FMU or a LUT, each a file beside its sidecar `<file>.asset.json`, which holds the UUID the runtime made at its import and keeps across every relocation, and streams, each its graph document alone with its UUID inside it — are read with `list_assets` and `describe_asset`, which also answer what the runtime read from each asset's bytes, its `media_facts` and `preview`, and `read_asset_preview` answers its preview picture; they are changed with `import_asset`, `relocate_asset`, `delete_asset`, `set_asset_settings` and `replace_asset`, which puts new bytes at an asset's path keeping its UUID, and a stream is created, deleted and given its display name with `create_stream`, `delete_stream` and `set_stream_display_name`, naming it by its stream asset's UUID — a created stream loaded, a deleted one unloaded and a named one shown under its new name at once while its project runs — each naming the project by its absolute `project_directory`, a folder holding `tatolab-project.json`, and an asset by its UUID; each change is one transaction by its `author`, applied whole or refused whole in the runtime's own words, and one commit on the project's own history at `.tatolab/history.git`; an asset's bytes are the one file at its path, which a node reads with `asset_content_path` and history never holds: history records a media, scene or data asset by its sidecar alone, so an undo of a delete brings its bytes back only while its file is still at its path. Any other file of a project — its node code, its `stream.py`, a view's sources — is read with `list_project_files` and `read_project_file` and written with `write_project_file`, one transaction an undo reverses, never inside `.tatolab/` or outside the project; `create_node_file` creates a new Python node file from the runtime's node template, a node type of the project with no edits, named after the file, and answers the `node_type` `add_node` takes. A project's change feed is the resource `list_assets` names as `transactions_resource_uri`, `streamlib://project/<project>/transactions`, holding its newest transactions with their author, operations and inverse, each edit and redo with its `undo_state` — `applied`, `undone_redoable` or `undone_not_redoable` — as its author's `undo` and `redo` stand now; name it in `subscriptions/listen` over `/mcp/stdio` to be sent `notifications/resources/updated` each time a transaction applies, then read it. The resource `streamlib://node-catalog` lists the native types every stream can add, then each loaded stream's Python types under that stream, each with its config schema and ports; `streamlib://graph` is every loaded stream's live graph. A project is renamed with `rename_project`, described with `set_project_description` and given a cover — a media asset it holds, by UUID — with `set_project_cover`, each one transaction on its history that undoes like any edit, its folder and UUID untouched; `list_assets` serves them with `project_last_edited_at`, when its history last changed. `duplicate_project` copies a project with its whole history into a new folder under a new UUID, and `delete_project` queues, like `remove_project`, the unload and forgetting of every stream of a project and then moves its folder to the Trash. Another project is imported with `add_dependency`: the project a folder or a `.zip` on this machine holds — a GitHub archive downloaded first — is copied whole into `imports/<folder>/` and recorded in `tatolab-project.json` with the `version` its copy carries, and nothing reaches the network. The copy is this project's own: its assets are referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read from its folder, which a node reads with `asset_content_path(asset, from_project)`; `use_dependency_stream` places one of its streams in a loaded stream's graph as a fragment referencing it, which every load places as the stream's graph document is on disk, each node type the import's own node code defines led by its folder, `imports/<folder>/<module>:<class>`, so it runs that code from there; to edit an import, open `imports/<folder>/` as a project of its own, whose transactions also commit on this project's history. `update_dependency` copies an import again as one transaction, listing the files of its folder edited since its copy and overwriting them only with `overwrite_edited_files`; `list_dependencies` lists each import with its edited files and, with `check_for_newer`, the version its source holds now; `remove_dependency` stops importing a project no node references any longer. `list_project_node_types` lists the node types and graph generators a project offers — its own node code outside `imports/`, each import's and each installed distribution's `tatolab.nodes` — read from source without running any of it, a module that does not parse listed with why and where while the rest still lists. `list_project_views` lists the Tato Studio views a project and its imports bring — each folder of their `views/`, by its `view_type` `<project name>:<view folder>`, with what its `tatolab_view.json` declares — reading each manifest and running nothing, a folder whose manifest does not read listed with why it does not open. Each such view that opens and whose `placement` is `node` is a node type of every stream the project runs, named by its `view_type`, whose ports are the view's declared inputs and outputs and whose config is the view's settings, listed in the node catalog with the manifest's description; a stream holds one node of each view, and a view that does not open is no type. A panel reads a view node's inputs with `open_viewer` on the port linked into each, and `write_studio_view_output` hands the node a value for one of its Array, Channels or Records outputs, which it publishes there as live data: the last write wins, and nothing is recorded. The prompts are step-by-step recipes over these tools, each rendered against the `stream` it names: inserting a node into a link, fanning an output to another consumer, and showing a channel on a virtual camera.";

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
    prompt_router: Arc<PromptRouter<Self>>,
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
        let project_repositories_open_in_this_runtime = Arc::new(
            ProjectRepositoriesOpenInThisRuntime::locking_python_requirements_with(Arc::new(
                move |project_directory| {
                    runtime_locking_python_requirements
                        .lock_the_python_requirements_of_the_project_in(project_directory)
                },
            ))
            .reading_the_core_project_the_runtime_carries_at(Arc::new(move || {
                runtime_carrying_the_core_nodes_project
                    .the_core_nodes_project_directory_this_runtime_carries()
            })),
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
            prompt_router: Arc::new(Self::prompt_router()),
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
            Err(ControlOperationRefusal::RefusedAction(refusal)) => Ok(
                crate::mcp_agent_tools::a_tool_result_of_the_refusal(*refusal),
            ),
            Err(ControlOperationRefusal::Failed(why)) => Err(why),
            Err(ControlOperationRefusal::ContractVersionMismatch(mismatch)) => {
                Err(format!("{tool_name} failed: {mismatch}"))
            }
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

/// A tool's answer: its result, or the message an `isError` result carries.
pub(crate) type ToolCallAnswer = Result<CallToolResult, String>;

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

#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for LocalApiMcpServerHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
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
                Err(why) => CallToolResult::error(vec![ContentBlock::text(why)]).into(),
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
    let tool_result_json = serde_json::to_value(tool_result)
        .map_err(|e| format!("{tool_name} result serialization failed: {e}"))?;
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

fn json_text_content_block(value: &Value) -> ContentBlock {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ContentBlock::text(text)
}

#[cfg(test)]
pub(crate) mod tests;
