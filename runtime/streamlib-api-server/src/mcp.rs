// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's MCP server, served by the official SDK, `rmcp`.
//!
//! [`LocalApiMcpServerHandler`] is the runtime's whole MCP surface: its tools
//! are the control vocabulary — the observation verbs `graph`, `logs` and
//! `describe_runtime`; the graph-mutation verbs `add_node`, `remove_node`,
//! `connect` and `disconnect`, each naming a node by its name; and the stream
//! actions `open_project`, `reload_project`, `close_project`,
//! `start_playback`, `stop_playback`, `pause_playback`, `resume_playback`,
//! `describe_stream_action`, `cancel_stream_action`, `remove_project`,
//! `list_streams` and `set_stream_clock_scale`; and the viewer verbs
//! `open_viewer` and `close_viewer` — and beside them it serves the node
//! catalog and the live graph as resources ([`crate::mcp_resources`]) and
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
//! Each call into the runtime runs on a blocking task. A stream action answers
//! at once, queued on its project's stream action queue; every stream a run
//! loads is recorded, whichever connection carried the call, and outlives it.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ServerCapabilities, ServerConfig, SubscriptionFilter,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use rmcp::{prompt_handler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamlib::sdk::error::{Error, PrivacyGatedCaptureDevice, Result as StreamlibResult};
use streamlib::sdk::runtime::{
    CreateProjectRequest, OperationsOnTheStreamsLoadedInThisRuntime, OutputPortViewerHeldOpen,
    ProjectOpenOrReloadRequest, ProjectVenvSyncPackage, ProjectVenvSyncPackageKind,
    ProjectVenvSyncPackageState, ProjectVenvSyncPhase, ProjectVenvSyncProgress, RuntimeOperations,
    StreamAction, StreamActionKind, StreamActionState, StreamActionStep, StreamListingState,
    StreamLocation,
};
use streamlib_project_repository::ProjectRepositoriesOpenInThisRuntime;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CancelStreamActionToolResult, CaptureDeviceOnTheWire, CloseViewerToolResult,
    CreateProjectToolResult, DescribeStreamActionToolResult, DescribeStreamActionToolResultStream,
    ListStreamsToolResult, ListStreamsToolResultStream, ListedStreamState, LogsToolResult,
    OpenViewerToolResult, PausePlaybackToolResult, ProjectStreamActionQueuedToolResult,
    ProjectVenvSyncOnTheWire, ProjectVenvSyncPackageKindOnTheWire, ProjectVenvSyncPackageOnTheWire,
    ProjectVenvSyncPackageStateOnTheWire, ProjectVenvSyncPhaseOnTheWire, RemoveProjectToolResult,
    ResumePlaybackToolResult, SetStreamClockScaleToolResult, StreamActionKindOnTheWire,
    StreamActionStateOnTheWire, StreamActionStepOnTheWire, StreamLocationOnTheWire,
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
pub(crate) const LOCAL_API_MCP_SERVER_INSTRUCTIONS: &str = "StreamLib's runtime control plane: this machine's one runtime, which loads any number of streams, each a stream asset of a project: its graph document, `streams/<name>.graph.json`, which holds each node by the UUID the runtime gave it. A project is open or closed, and an open project's playback is stopped, playing or paused; its graph reads and edits whatever its playback does. `open_project` opens a project from its absolute `project_directory`, and `reload_project` softly re-reads an open one from disk, keeping its playback: each answers at once with an `action` id while the project's own queue reads each stream asset's graph document, makes its `.venv` when it does not run, however long uv takes, and loads each stream — an open holding each stopped — one action at a time per project, projects side by side; follow it with `describe_stream_action` until its `state` is `done`, `failed` or `cancelled` — its `venv_sync` naming each package uv downloads, builds and installs as it goes — or live through the resource `streamlib://stream-action/<action>`, and every action queued or under way, whoever queued it, through `streamlib://stream-actions`, each of which a `subscriptions/listen` naming it over `/mcp/stdio` is sent `notifications/resources/updated` for as an action it covers changes; end it early with `cancel_stream_action`, and read what it wrote with `logs` given its `action`; `list_assets` narrowed to the `kind` `stream` lists the project's stream assets, running none of its code. `create_project` creates a project — the one `tatolab new` creates — in an absolute `project_directory`, or with none in a new folder `<adjective>-<noun>` under `~/streams`, and queues its open the same way, answering its `action`, the `stream` id its `main` runs under, and the `project_directory`; the stream is shown under the folder's name, which its graph document writes as its `display_name`. `list_streams` lists every stream, each by its stream id with its display name and location, running, stopped, paused, closed, failed or `preparing` — a stream an action is working on, with the `action` and its `step` — a stream implicated in the runtime's last two crashes in a row, or one that could not re-load at its start, is failed, carries why in `failed_because`, and is skipped at the start. Each stream a load takes in is listed under its stream id, `<project>-<stream>-<hash>`: the project's name, the stream asset's name and a hash over the project's UUID and the stream asset's UUID, kept while the stream is recorded, so moving the project or renaming its document keeps it. Reloaded, a project replaces its streams under the same ids. A stream is shown under the `display_name` its graph document writes, or the document's file name while it writes none. A load runs no project code and never fails for what a document holds: a stream whose graph document does not load, or whose load or start is refused, is listed failed with the reason in `failed_because` until a reload of the fixed document replaces it, and a stream whose document is gone is unloaded and forgotten; only the caller's own mistakes — a directory holding no `tatolab-project.json`, an id another stream holds — fail the action. A project plays and stops as a whole, never one stream of it: every stream a load takes in is recorded, and an open project is re-loaded, held stopped or playing as it was, whenever the runtime starts, until `close_project` or `remove_project` ends it; closing the connection that carried the call unloads nothing. `start_playback` queues the playback of an open project, each stream starting fresh from the graph it holds, reading no document; `stop_playback` queues its end — every processor instance, device claim, function-node state and the clock torn down — keeping each stream's graph loaded to read and edit and recording each stopped; `close_project` queues the end of its playback and the unload of every stream, recording each closed and deleting nothing; a verb refuses a project in a state it does not fit and names the verb that does — `open_project` on an open project names `reload_project`, `start_playback` on a paused one `resume_playback`, any playback verb on a closed one `open_project`; `pause_playback` pauses every loaded stream of a project at once, never waiting behind an action, each listed `paused` with its processors holding their state, a stream a reload loads meanwhile starting paused, until `resume_playback` resumes them and compiles the graph edits made meanwhile — live state, so a runtime restart re-loads the project playing; `remove_project` queues the unload and forgetting of every stream of a project — each answering at once with its `action`, which `describe_stream_action` follows, and each on its project's queue; `set_stream_clock_scale` runs a started stream's scaled clock at a new `scale` live, its stream time carrying on from where it stands. `set_stream_clock_scale` names a stream by its stream id with a required `stream`, never by its display name; a graph edit names it by its stream asset's UUID, the `stream_asset` `list_streams` reports under its `location`, which is the same on every machine the project is on; `graph` names one optionally, and without it returns every loaded stream's graph under `machine`, this machine's name; `logs` takes the project's absolute `project_directory` and names a stream or a stream action optionally. `describe_runtime` names no stream and answers this runtime's `version`, its `build_id`, `up_for_ms` since its engine started (a restart resets it) and `runtime_log_directory`, the folder of its own log, `null` when it keeps none. Observe a stream with `graph` (nodes by UUID and name, their types and port names, links, and each node's `metrics`: the bags each output port wrote and each inbound link dropped) and `logs` (the project's one log, every stream's records and its runs' venv lines, read from disk so they outlive the stream and the runtime's restarts: pass the result's `next_after` back as `after` to read on). What an output port carries is read from outside its stream in two ways. `open_viewer` holds a node's output port open for a viewer on this machine, naming the node by the `id` `graph` lists, and answers the channel the viewer reads it from with no copy, until `close_viewer` or the connection's end. A stream sends a port off itself through the `tatolab-moq` gateway nodes: a `MoqTrackSend` linked from the port publishes it as a track at the relay a `MoqGateway` node joins, which any MoQ client reads, and another stream reads that track through a `MoqTrackReceive`. Change its graph with `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, `set_exposed_config_fields`, `import_graph_generator`, `add_group`, `set_group_fields`, `remove_group`, `add_note`, `set_note_fields` and `remove_note`, or several of them as one `transact`, each naming a node by its UUID, which `add_node` answers and `graph` lists as `id`, and each one transaction by its `author`, applied whole or refused whole in the runtime's own words: with `mode` `commit`, the default, it is applied to the running stream, written to the stream's graph document and recorded as one commit on the project's history, so it is kept on the next load; with `mode` `live`, applied to the running stream alone. `import_graph_generator` runs one of the project's `@graph_generator` functions, `<module>:<function>`, in the stream's own interpreter and places what it builds as a fragment node, the nodes and links it built stored inside it; run again under the name of a fragment the graph holds, it refreshes that fragment, each node keeping its UUID by name; `refresh_fragments` refreshes, as one transaction, every fragment of a project's loaded streams whose generator a changed module defines, which `tatolab dev` asks for on each save of a `.py` file. `collapse_selection_into_subgraph` moves nodes of a stream's own level into one subgraph node, an authored fragment no refresh touches, each link that crossed the selection carried through a `GraphInput` or `GraphOutput` inside it; `unlock_generator_fragment` turns a fragment's nodes back into plain nodes of the stream, so a generator's output stops being refreshed. `add_group` makes a labelled group of nodes — a title, a description and a colour, listed under `graph`'s `groups` and joined by a node holding its UUID as `studio.group` — which changes nothing that runs; `set_group_fields` edits one and `remove_group` takes one out, its nodes left ungrouped. `add_note` places a canvas note — text at a position and size, listed under `graph`'s `studio.notes` — beside the nodes, which changes nothing that runs; `set_note_fields` edits one and `remove_note` takes one out. A node's `studio` and the graph's are the studio contract, versioned by `studio.contract`: a node's takes `position`, `group`, `comment`, `colour`, `collapsed` and a client's own `metadata`, and a key of the wrong shape or outside it is refused by name. `undo` reverses an author's latest transaction in a project as a new transaction and `redo` reapplies it, each reversing only that author's own. A config value `{\"asset\": \"<uuid>\"}` references an asset the project must hold, which cannot then be deleted. A Python class written to a module the stream's own interpreter can import — a file in the stream's project, or a package installed in its venv — is added by its `module:ClassName` path, and a class of an import's own node code by `imports/<folder>/module:ClassName`, imported from that folder ahead of the project's own code, and runs in its own processor interpreter; a link is spliced in by connecting the new node on both sides, then disconnecting the link it replaces. Read `graph` first for UUIDs and port names, and again afterwards to confirm a link's state is `wired` and the node is `Running`. A `connect` onto a node in a helper process returns before that helper has opened its port, so its link reads `pending` until the helper answers and then `wired`; a link that reads `error` carries the refusing end's own reason in `error_reason` and will never carry a bag — read the reason, `disconnect` it, and fix what it names. A Python node whose helper process died on its own reads `Restarting` while a new helper process, with a new instance of its class, starts in its place after a backoff, and its `components.helper_process_restarts` counts the restarts and carries `last_exit_reason`; one whose helper died five times in a row, each within a minute of starting, is left `Error`, and `reload_project` starts it afresh. A node that reads `Error` carries why in `components.failed_because`, in the words of what failed it — a `start()` that raised among them; a node that has not failed carries `null` there. A node whose `process()` raised, or returned an error, and has not run cleanly since keeps running and carries what it raised in `components.process_raising_because` — the first raise of the run, in its own words, and for a Python node where in its code — and `null` once a call runs cleanly again. Both ends of a `connect` are ports in the one stream it names; a stream reads another stream's track at a relay, on this machine or another, through a `MoqTrackReceive` node. A project's assets — media, scenes and data such as an FMU or a LUT, each a file beside its sidecar `<file>.asset.json`, which holds the UUID the runtime made at its import and keeps across every relocation, and streams, each its graph document alone with its UUID inside it — are read with `list_assets` and `describe_asset`, which also answer what the runtime read from each asset's bytes, its `media_facts` and `preview`, and `read_asset_preview` answers its preview picture; they are changed with `import_asset`, `relocate_asset`, `delete_asset`, `set_asset_settings` and `replace_asset`, which puts new bytes at an asset's path keeping its UUID, and a stream is created, deleted and given its display name with `create_stream`, `delete_stream` and `set_stream_display_name`, naming it by its stream asset's UUID — a created stream loaded, a deleted one unloaded and a named one shown under its new name at once while its project runs — each naming the project by its absolute `project_directory`, a folder holding `tatolab-project.json`, and an asset by its UUID; each change is one transaction by its `author`, applied whole or refused whole in the runtime's own words, and one commit on the project's own history at `.tatolab/history.git`; an asset's bytes are the one file at its path, which a node reads with `asset_content_path` and history never holds: history records a media, scene or data asset by its sidecar alone, so an undo of a delete brings its bytes back only while its file is still at its path. Any other file of a project — its node code, its `stream.py`, a view's sources — is read with `list_project_files` and `read_project_file` and written with `write_project_file`, one transaction an undo reverses, never inside `.tatolab/` or outside the project; `create_node_file` creates a new Python node file from the runtime's node template, a node type of the project with no edits, named after the file, and answers the `node_type` `add_node` takes. A project's change feed is the resource `list_assets` names as `transactions_resource_uri`, `streamlib://project/<project>/transactions`, holding its newest transactions with their author, operations and inverse, each edit and redo with its `undo_state` — `applied`, `undone_redoable` or `undone_not_redoable` — as its author's `undo` and `redo` stand now; name it in `subscriptions/listen` over `/mcp/stdio` to be sent `notifications/resources/updated` each time a transaction applies, then read it. The resource `streamlib://node-catalog` lists the native types every stream can add, then each loaded stream's Python types under that stream, each with its config schema and ports; `streamlib://graph` is every loaded stream's live graph. A project is renamed with `rename_project`, described with `set_project_description` and given a cover — a media asset it holds, by UUID — with `set_project_cover`, each one transaction on its history that undoes like any edit, its folder and UUID untouched; `list_assets` serves them with `project_last_edited_at`, when its history last changed. `duplicate_project` copies a project with its whole history into a new folder under a new UUID, and `delete_project` queues, like `remove_project`, the unload and forgetting of every stream of a project and then moves its folder to the Trash. Another project is imported with `add_dependency`: the project a folder or a `.zip` on this machine holds — a GitHub archive downloaded first — is copied whole into `imports/<folder>/` and recorded in `tatolab-project.json` with the `version` its copy carries, and nothing reaches the network. The copy is this project's own: its assets are referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read from its folder, which a node reads with `asset_content_path(asset, from_project)`; `use_dependency_stream` places one of its streams in a loaded stream's graph as a fragment referencing it, which every load places as the stream's graph document is on disk, each node type the import's own node code defines led by its folder, `imports/<folder>/<module>:<class>`, so it runs that code from there; to edit an import, open `imports/<folder>/` as a project of its own, whose transactions also commit on this project's history. `update_dependency` copies an import again as one transaction, listing the files of its folder edited since its copy and overwriting them only with `overwrite_edited_files`; `list_dependencies` lists each import with its edited files and, with `check_for_newer`, the version its source holds now; `remove_dependency` stops importing a project no node references any longer. `list_project_node_types` lists the node types and graph generators a project offers — its own node code outside `imports/`, each import's and each installed distribution's `tatolab.nodes` — read from source without running any of it, a module that does not parse listed with why and where while the rest still lists. `list_project_views` lists the Tato Studio views a project and its imports bring — each folder of their `views/`, by its `view_type` `<project name>:<view folder>`, with what its `tatolab_view.json` declares — reading each manifest and running nothing, a folder whose manifest does not read listed with why it does not open. Each such view that opens and whose `placement` is `node` is a node type of every stream the project runs, named by its `view_type`, whose ports are the view's declared inputs and outputs and whose config is the view's settings, listed in the node catalog with the manifest's description; a stream holds one node of each view, and a view that does not open is no type. A panel reads a view node's inputs with `open_viewer` on the port linked into each, and `write_studio_view_output` hands the node a value for one of its Array, Channels or Records outputs, which it publishes there as live data: the last write wins, and nothing is recorded. The prompts are step-by-step recipes over these tools, each rendered against the `stream` it names: inserting a node into a link, fanning an output to another consumer, and showing a channel on a virtual camera.";

/// How many log records one `logs` call returns when the caller names no
/// `count`.
const DEFAULT_LOGS_RECORD_COUNT: u32 = 256;

/// The most log records one `logs` call returns: as many as a stream's log
/// route holds in memory.
const MAX_LOGS_RECORD_COUNT: u32 = 4096;

/// What every required `stream` argument says.
const STREAM_ARGUMENT_DESCRIPTION: &str = "The loaded stream this call is about, by the stream id `list_streams` and `graph` report. A stream not loaded is refused naming the loaded ones.";

/// What every `project_directory` argument naming one project says.
const PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str = "The project, by absolute path: a folder holding `tatolab-project.json` and its stream assets' graph documents.";

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

/// The viewers one `/mcp/stdio` connection opened, by id, each holding its
/// port's channel open until it is closed or the connection ends.
#[derive(Default)]
pub(crate) struct ViewersOneLocalApiConnectionHoldsOpen {
    viewers_by_id: parking_lot::Mutex<std::collections::BTreeMap<String, OutputPortViewerHeldOpen>>,
}

impl Drop for ViewersOneLocalApiConnectionHoldsOpen {
    /// Releasing a port's channel may wait on its helper process, so the
    /// viewers a connection left open are released off the async workers.
    fn drop(&mut self) {
        let viewers_left_open = std::mem::take(self.viewers_by_id.get_mut());
        if viewers_left_open.is_empty() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(async_runtime) => {
                drop(async_runtime.spawn_blocking(move || drop(viewers_left_open)))
            }
            Err(_) => drop(viewers_left_open),
        }
    }
}

/// The runtime's MCP server handler: its tools, resources and prompts over the
/// streams the runtime loads, for one connection.
#[derive(Clone)]
pub(crate) struct LocalApiMcpServerHandler {
    pub(crate) operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    local_api_stopping_token: CancellationToken,
    mcp_connection: LocalApiMcpConnection,
    viewers_this_connection_holds_open: Arc<ViewersOneLocalApiConnectionHoldsOpen>,
    pub(crate) project_repositories_open_in_this_runtime: Arc<ProjectRepositoriesOpenInThisRuntime>,
    pub(crate) project_actions_refused_since_the_runtime_started:
        Arc<crate::mcp_project_change_feed::ProjectActionsRefusedSinceTheRuntimeStarted>,
    pub(crate) asset_media_facts_reads_in_this_runtime:
        Arc<crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime>,
    tool_router: Arc<ToolRouter<Self>>,
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
        let project_repositories_open_in_this_runtime = Arc::new(
            ProjectRepositoriesOpenInThisRuntime::locking_python_requirements_with(Arc::new(
                move |project_directory| {
                    runtime_locking_python_requirements
                        .lock_the_python_requirements_of_the_project_in(project_directory)
                },
            )),
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
            tool_router: Arc::new(
                Self::tool_router()
                    + Self::project_archive_tool_router()
                    + Self::project_asset_tool_router()
                    + Self::project_dependency_tool_router()
                    + Self::project_file_tool_router()
                    + Self::project_node_type_tool_router()
                    + Self::project_tool_router()
                    + Self::project_view_tool_router()
                    + Self::stream_graph_tool_router(),
            ),
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

    /// The operations on the stream a `tool` call names, or the refusal —
    /// naming the loaded streams — its caller reads.
    pub(crate) fn the_stream_a_tool_call_names(
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
    pub(crate) async fn a_stream_action_off_the_async_runtime<
        StreamActionOutcome: Send + 'static,
    >(
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
struct OpenProjectToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct StartPlaybackToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReloadProjectToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CreateProjectToolArguments {
    #[schemars(
        description = "Where to create the project, by absolute path: created when missing, and holding none of the files a new project starts with. Omit it to create the project in a folder named `<adjective>-<noun>`, drawn at random, under `~/streams`."
    )]
    #[serde(default)]
    project_directory: Option<PathBuf>,
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
    #[schemars(
        description = "The stream action, by the action id a tool that queues one answered."
    )]
    action: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CancelStreamActionToolArguments {
    #[schemars(
        description = "The stream action, by the action id a tool that queues one answered."
    )]
    action: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct StopPlaybackToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CloseProjectToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct PausePlaybackToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ResumePlaybackToolArguments {
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListStreamsToolArguments {}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetStreamClockScaleToolArguments {
    #[schemars(description = STREAM_ACTION_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(
        description = "Stream seconds per machine second from now on: 0.5 runs the stream at half speed, 1 at real time. A finite number greater than zero, at most 1."
    )]
    scale: f64,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct OpenViewerToolArguments {
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(
        description = "The node's id, as `graph` lists it under `id`; its name is a label, not its identity."
    )]
    node_id: String,
    #[schemars(description = "The node's output port, as `graph` lists it under `ports.outputs`.")]
    port: String,
    #[schemars(
        description = "The process id of the viewer that imports the port's Image frames. On macOS the surface-sharing service admits only that process, a running process of the runtime's user, to new connections while the viewer stays open, and without it the answer names no `surface_share_address`; Linux admits whoever may open its socket."
    )]
    #[serde(default)]
    viewer_process_id: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CloseViewerToolArguments {
    #[schemars(description = "The viewer, by the id `open_viewer` answered.")]
    viewer: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DescribeRuntimeToolArguments {}

/// A tool's answer: its result, or the message an `isError` result carries.
pub(crate) type ToolCallAnswer = Result<CallToolResult, String>;

// ============================================================================
// Tools
// ============================================================================

#[tool_router]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Export a loaded stream's current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, with each node's and link's live state and counters beside them, and this machine's name (`machine`), the first part of `<machine>/<stream id>/<node>/<port>`. A fragment — a graph generator's stored output — renders as one node of `kind` `fragment` whose ports are its Graph Input and Output nodes' and whose `fragment` holds its own nodes and links, a fragment inside it rendered the same way; its nodes run under their path of names, `look/soft/blur`. Without `stream`, `{machine, streams}` holds every loaded stream's graph."
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
        description = "Read a project's log records after the cursor `after`, oldest first, each as the project's JSONL log on disk holds it: every stream of the project and every load of it writes that one log, each record stamped with its `stream` — none for a load's own lines, uv's among them — across the runtime's runs, whether the stream is still recorded or not. `stream` narrows to one stream's records, and `action` to the records of one stream action — every record its load wrote, uv's among them, and the records of the streams it started. Pass the result's `next_after` back as `after` to read on; `records_no_longer_held` is true when retention deleted the segment `after` named, so the page resumes at the oldest record held. A project that has logged nothing answers no records."
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
        description = "Open the project in `project_directory`: queue its open on its own stream action queue and answer at once with the action's id as `action`. The open reads its `tatolab-project.json` and each stream asset's graph document, makes the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run — however long uv takes — then loads each stream under its stream id, `<project>-<stream>-<hash>`, the hash over the project's UUID and the stream asset's UUID, and holds it with its playback stopped, recording each stream stopped or failed: a graph document that does not load, or a load that is refused, with its reason, and a `.venv` uv cannot make — uv's own words — failing every stream of the project. It starts no node. Once it is done, `graph`, every graph edit and the node catalog answer for each stream, and `start_playback` plays the project. The project stays open, held stopped or playing, across the runtime's restarts until `close_project`. It opens a project that was never opened, one `close_project` closed, and one whose every stream failed, retrying them. One project's actions run one at a time, in the order they were queued; different projects' run side by side. Follow the action with `describe_stream_action`, its streams with `list_streams` — `preparing`, with the action and its `step`, until it ends — and its records with `logs` given `action`. A directory that does not exist or holds no `tatolab-project.json` fails the action, never the call. Refused, with nothing queued: a `project_directory` that is not absolute, and a project open once its queued actions run, which `reload_project` refreshes."
    )]
    async fn open_project(
        &self,
        Parameters(OpenProjectToolArguments { project_directory }): Parameters<
            OpenProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams
                    .open_project(ProjectOpenOrReloadRequest { project_directory })
            })
            .await?;
        serialized_tool_result(
            "open_project",
            &ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Start the playback of the open project in `project_directory`: queue it on the project's stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, each stream the project holds stopped starts fresh from the graph it holds, edits made while stopped included, reading no graph document and making no `.venv`: every node instance is made anew, so only the state a node persists itself carries over from an earlier playback. Each stream is recorded running, so the runtime plays the project again across its restarts until `stop_playback`. A project plays or stops as a whole: no tool plays or stops one of its streams. Follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute; a project that is not open, which `open_project` opens; a paused project, which `resume_playback` resumes; and a project that plays once its queued actions run, which `stop_playback` stops."
    )]
    async fn start_playback(
        &self,
        Parameters(StartPlaybackToolArguments { project_directory }): Parameters<
            StartPlaybackToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.start_playback(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "start_playback",
            &ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Softly reload the open project in `project_directory` from disk: queue the load on its stream action queue and answer at once with the action's id as `action`. It re-reads every stream asset's graph document and replaces each stream under its same id, keeping its display name — a stream whose graph document is gone is unloaded and forgotten, and a failed stream is retried — and makes the project's `.venv` only when its `.venv/bin/python` does not run. The project keeps its playback: a playing project's streams start playing, a paused one's start paused, and a stopped one's are held stopped. `tatolab dev` asks for it on each save. A hard reload is `close_project` then `open_project`. Follow it as `open_project`'s open. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open, which `open_project` opens."
    )]
    async fn reload_project(
        &self,
        Parameters(ReloadProjectToolArguments { project_directory }): Parameters<
            ReloadProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams
                    .reload_project(ProjectOpenOrReloadRequest { project_directory })
            })
            .await?;
        serialized_tool_result(
            "reload_project",
            &ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Create a project — the one `tatolab new` creates: its `tatolab-project.json`, one stream asset `streams/main.graph.json` running the sample animation through a GPU effect, the node modules it uses, `pyproject.toml`, `.python-version` and `.gitignore`, committed as the first commit of the project's history — in `project_directory`, or with none in a folder `<adjective>-<noun>` drawn under `~/streams`, and queue its open, answering at once with the action's id as `action`, the stream id its `main` runs under as `stream`, and the resolved `project_directory`. The action writes the project's files, then opens it as `open_project` does — reads its graph documents, makes its `.venv`, however long uv takes, and holds its stream stopped until `start_playback` — showing its stream under the folder's name. A directory already holding a file a new project starts with fails the action with nothing written. Follow it with `describe_stream_action` and `logs`; the project is recorded and re-loaded whenever the runtime starts, so a later session finds it in `list_streams`. Refused only for a relative `project_directory`, one that cannot be created, or, with none, a `HOME` that is unset or not absolute or a draw that finds no free folder."
    )]
    async fn create_project(
        &self,
        Parameters(CreateProjectToolArguments { project_directory }): Parameters<
            CreateProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        if let Some(project_directory) = &project_directory {
            refuse_a_relative_project_directory(project_directory)?;
        }
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams
                    .create_project(CreateProjectRequest { project_directory })
            })
            .await?;
        serialized_tool_result(
            "create_project",
            &CreateProjectToolResult {
                action: queued.action_id,
                stream: queued.stream_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Describe the stream action `action`: its project, `kind` (`open`, `load`, `start_playback`, `stop`, `close`, `remove`, `delete` or `reload_at_start`), `state` — `queued` behind the project's earlier actions, `in_progress` on its `step` (`reading_the_graph_documents`, `making_the_venv`, `starting`), `done`, `failed` with `failed_because`, or `cancelled` by `cancel_stream_action` — its `venv_sync`, what its latest uv sync of the project's `.venv` has done, read from uv's own lines from the moment uv starts and kept once it ends, `null` for an action that has not synced: its `phase` (`resolving`, `downloading`, `building`, `installing`, `installed`), `packages_resolved`, `packages_prepared` and `packages_installed` as uv counts them, `downloads_started` and `downloads_done` of the downloads uv names — only those over about a MiB — and under `packages` each package uv named, first named first, by `name`, `version` once uv names it, `kind` (`distribution` or `python_interpreter`), `state` (`downloading`, `downloaded`, `building`, `built`, `installed`, `uninstalled`) and `download_size_bytes` as uv states it; uv names no byte of a download as it arrives — and `streams`, each stream its project's graph documents named by `stream` id and `display_name`, with its `state` `running`, `stopped` or `failed` once the action took it in. Followed live as the resource `streamlib://stream-action/<action>`, which a `subscriptions/listen` naming it is sent `notifications/resources/updated` for each time the action changes. The runtime holds every queued and running action and the last 64 finished ones of each project; an older one is refused, its records still in the project's `logs`."
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
        description = "Cancel the stream action `action`, the only way an action ends early; nothing replaces or edits a queued one. A queued action is dropped from its project's queue, its pending mark taken off each stream's record so the record stays as it was, and answers `cancelled`. A running `open`, `load` or `reload_at_start` answers `in_progress` and stops at the step it is on — a running uv sync is killed and a half-made `.venv` removed — then ends `cancelled`: a stream it was loading is recorded failed, and the streams it had not reached stay as recorded. Refused for a running `stop`, `close` or `remove`, whose unload has begun and cannot be cut short, which ends `done`; for an action already finished; and for one the runtime does not hold."
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
        description = "Stop the playback of the whole project in `project_directory`: queue the stop on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it ends the playback of every loaded stream of the project — every processor instance, device claim, function-node state and the clock torn down — keeps each stream's graph loaded, so `graph`, every graph edit and the node catalog still answer for it, and records each stopped, so the runtime holds the project open and stopped across its restarts until `start_playback` plays it again from the graph it holds. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Each recorded stream carries the stop on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, a project whose removal is queued, and a project none of whose streams plays once the actions queued before it run."
    )]
    async fn stop_playback(
        &self,
        Parameters(StopPlaybackToolArguments { project_directory }): Parameters<
            StopPlaybackToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.stop_playback(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "stop_playback",
            &ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Close the open project in `project_directory`: queue the close on its stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, it ends the playback of every stream of the project, unloads each, forgets its function-node state, and records each closed, so the runtime leaves the project unloaded across its restarts until `open_project` opens it again. It deletes nothing: the project's files, its `.venv` and what uv synced into it stay. Its streams are `stopping` in `list_streams` until it ends, then `closed`; follow it with `describe_stream_action`. Each recorded stream carries the close on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open once its queued actions run."
    )]
    async fn close_project(
        &self,
        Parameters(CloseProjectToolArguments { project_directory }): Parameters<
            CloseProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.close_project(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "close_project",
            &ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }

    #[tool(
        description = "Pause the playback of the whole project in `project_directory` at once, never waiting behind an action on its queue, and answer the `project_directory` and, as `streams`, the id of each loaded stream this call paused. Every processor of each loaded stream holds its state and processes nothing; the streams stay loaded and are `paused` in `list_streams`, and a graph edit made meanwhile is applied and compiles on `resume_playback`. The project stays paused until `resume_playback`, `stop_playback`, `close_project` or `remove_project`: a stream a `reload_project` loads meanwhile, the one `tatolab dev` asks for on a save among them, starts paused. Pause is live state, never recorded, so a runtime restart re-loads the project playing. Pausing a paused project pauses nothing more. Refused: a `project_directory` that is not absolute, and a project none of whose streams plays — none loaded, or each held stopped, which `start_playback` plays — and whose last action queued is not a `start_playback`."
    )]
    async fn pause_playback(
        &self,
        Parameters(PausePlaybackToolArguments { project_directory }): Parameters<
            PausePlaybackToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let paused = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.pause_playback(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "pause_playback",
            &PausePlaybackToolResult {
                project_directory: paused.project_directory,
                streams: paused.stream_ids,
            },
        )
    }

    #[tool(
        description = "Resume the paused playback of the project in `project_directory` at once and answer the `project_directory` and, as `streams`, the id of each stream this call resumed: every processor carries on from the state it held, and each stream compiles the graph edits made while it was paused, a compile that fails logged in the project's `logs` as a live edit's is. Refused: a `project_directory` that is not absolute, and a project that is not paused."
    )]
    async fn resume_playback(
        &self,
        Parameters(ResumePlaybackToolArguments { project_directory }): Parameters<
            ResumePlaybackToolArguments,
        >,
    ) -> ToolCallAnswer {
        refuse_a_relative_project_directory(&project_directory)?;
        let resumed = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.resume_playback(&project_directory)
            })
            .await?;
        serialized_tool_result(
            "resume_playback",
            &ResumePlaybackToolResult {
                project_directory: resumed.project_directory,
                streams: resumed.stream_ids,
            },
        )
    }

    #[tool(
        description = "Queue the removal of the whole project in `project_directory` on its stream action queue and answer at once with the action's id as `action`: once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, so the runtime no longer re-loads any of them, and deletes no file. `delete_asset` on a stream asset unloads and forgets that one stream. The project's streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute, and a project the runtime holds no stream of."
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
        description = "List every stream the runtime holds: its stream id as `stream`, its `display_name`, its `location` — its project directory, its project's UUID as `project` and its stream asset's UUID as `stream_asset`, `null` for a record an earlier runtime wrote — and its state: `running` (playing, or recorded playing and re-loaded playing at the runtime's next start), `stopped` (open with its playback stopped, its graph loaded to read and edit while its node count is set, until `start_playback`), `paused` (open with its playback paused by `pause_playback`, until `resume_playback`), `closed` (its project closed by `close_project`, not loaded, until `open_project`), `preparing` (an action that loads it — an `open_project`, `reload_project` or `start_playback` — is queued or under way on it, named by `action`, on its `step`: `reading_the_graph_documents`, `making_the_venv` or `starting`, `null` while it waits), `stopping` (a `stop_playback`, `close_project` or `remove_project` of its project is queued or under way on it, named by `action`, on the step `stopping`, `null` while it waits), or `failed` (implicated in the runtime's last two crashes in a row, or could not re-load at its start, and skipped at the start until an `open_project` or `reload_project` of its project retries it; or recorded by an earlier runtime, never loaded, with `failed_because` saying to run it again and `remove_project` forgetting it), with the node count of its loaded graph (`null` when it is not loaded), `failed_because`, why a failed stream failed (`null` otherwise), and `capture_devices_whose_access_was_never_asked_for`, each of `camera` and `microphone` that a node of the loaded stream refused because nobody has asked the user for its access, as when the runtime re-loaded the stream on its own at its start (empty otherwise): a `close_project` then `open_project` of its project asks."
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
                        capture_devices_whose_access_was_never_asked_for: listing
                            .capture_devices_whose_access_was_never_asked_for
                            .into_iter()
                            .map(capture_device_on_the_wire)
                            .collect(),
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
        description = "Open a viewer of any output port of a running node, adding no node and changing no graph: the runtime holds the port's channel open and answers the `viewer` id, the `iceoryx2_domain_root` and the `channel` whose reserved subscriber slot the viewer takes from its own process. A viewer reads the latest message the node wrote and skips what it falls behind on, so it never slows the stream; an Image message names a pooled frame rather than carrying its pixels, and the viewer imports that frame's surface from the `surface_share_address` the way a helper process does. The viewer stays open until `close_viewer` or the end of the `/mcp/stdio` connection that opened it, so it is opened only over `/mcp/stdio`. Refused for a node that is not running."
    )]
    async fn open_viewer(
        &self,
        Parameters(OpenViewerToolArguments {
            stream,
            node_id,
            port,
            viewer_process_id,
        }): Parameters<OpenViewerToolArguments>,
    ) -> ToolCallAnswer {
        if matches!(
            self.mcp_connection,
            LocalApiMcpConnection::OneShotStreamableHttpRequest
        ) {
            return Err(
                "open_viewer failed: a viewer stays open for the connection that opened it, and \
                 a `POST /mcp` request ends with its answer; open it over `/mcp/stdio`"
                    .to_string(),
            );
        }
        let viewer = format!("{stream}/{node_id}/{port}");
        if self
            .viewers_this_connection_holds_open
            .viewers_by_id
            .lock()
            .contains_key(&viewer)
        {
            return Err(format!(
                "open_viewer failed: this connection already holds viewer `{viewer}` open"
            ));
        }
        let stream_operations = self.the_stream_a_tool_call_names("open_viewer", &stream)?;
        let viewer_held_open = stream_operations
            .open_a_viewer_of_an_output_port(node_id, port, viewer_process_id)
            .await
            .map_err(|refusal| format!("open_viewer failed: {refusal}"))?;
        let hand_off = viewer_held_open.hand_off().clone();
        let viewer = format!("{}/{}/{}", hand_off.stream, hand_off.node_id, hand_off.port);
        let viewer_already_open = {
            let mut viewers_by_id = self.viewers_this_connection_holds_open.viewers_by_id.lock();
            match viewers_by_id.entry(viewer.clone()) {
                std::collections::btree_map::Entry::Occupied(_) => Some(viewer_held_open),
                std::collections::btree_map::Entry::Vacant(vacant) => {
                    vacant.insert(viewer_held_open);
                    None
                }
            }
        };
        if let Some(viewer_opened_twice) = viewer_already_open {
            let _ = tokio::task::spawn_blocking(move || drop(viewer_opened_twice)).await;
            return Err(format!(
                "open_viewer failed: this connection already holds viewer `{viewer}` open"
            ));
        }
        serialized_tool_result(
            "open_viewer",
            &OpenViewerToolResult {
                viewer,
                stream: hand_off.stream,
                node_id: hand_off.node_id,
                node_name: hand_off.node_name,
                port: hand_off.port,
                iceoryx2_domain_root: hand_off.iceoryx2_domain_root.display().to_string(),
                channel: hand_off.channel_service_name,
                surface_share_address: hand_off.surface_share_address,
            },
        )
    }

    #[tool(
        description = "Close a viewer this connection opened, letting go of its port's channel."
    )]
    async fn close_viewer(
        &self,
        Parameters(CloseViewerToolArguments { viewer }): Parameters<CloseViewerToolArguments>,
    ) -> ToolCallAnswer {
        let closed = self
            .viewers_this_connection_holds_open
            .viewers_by_id
            .lock()
            .remove(&viewer);
        let Some(closed) = closed else {
            return Err(format!(
                "close_viewer failed: this connection holds no viewer `{viewer}` open"
            ));
        };
        tokio::task::spawn_blocking(move || drop(closed))
            .await
            .map_err(|join_failure| format!("close_viewer failed: {join_failure}"))?;
        serialized_tool_result("close_viewer", &CloseViewerToolResult { viewer })
    }

    #[tool(
        description = "Run a started stream's scaled clock at `scale` from now on: its stream time carries on from where it stands, never jumping or running backwards, and every node's `ctx.time`, write stamps and continuous pace follow the new scale. Above 1 the clock runs in steps of its `step_ns`, paced at the scale, behind the step barrier every node runs on; back at or under 1 it runs free again. Refused for a stream on the real or a stepped clock, and for a scale above 1 while the stream holds a node that paces itself (execution = manual) and is not paced by real time. Live only: the stream's next start runs the clock its graph document names."
    )]
    async fn set_stream_clock_scale(
        &self,
        Parameters(SetStreamClockScaleToolArguments { stream, scale }): Parameters<
            SetStreamClockScaleToolArguments,
        >,
    ) -> ToolCallAnswer {
        let stream_for_the_result = stream.clone();
        let rescaled = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.set_stream_clock_scale(&stream, scale)
            })
            .await?;
        serialized_tool_result(
            "set_stream_clock_scale",
            &SetStreamClockScaleToolResult {
                stream: stream_for_the_result,
                scale: rescaled.scale,
            },
        )
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
        StreamListingState::Paused => ListedStreamState::Paused,
        StreamListingState::Closed => ListedStreamState::Closed,
        StreamListingState::Failed => ListedStreamState::Failed,
        StreamListingState::Preparing => ListedStreamState::Preparing,
        StreamListingState::Stopping => ListedStreamState::Stopping,
    }
}

fn capture_device_on_the_wire(device: PrivacyGatedCaptureDevice) -> CaptureDeviceOnTheWire {
    match device {
        PrivacyGatedCaptureDevice::Camera => CaptureDeviceOnTheWire::Camera,
        PrivacyGatedCaptureDevice::Microphone => CaptureDeviceOnTheWire::Microphone,
    }
}

fn stream_action_step_on_the_wire(step: StreamActionStep) -> StreamActionStepOnTheWire {
    match step {
        StreamActionStep::MakingTheVenv => StreamActionStepOnTheWire::MakingTheVenv,
        StreamActionStep::ReadingTheGraphDocuments => {
            StreamActionStepOnTheWire::ReadingTheGraphDocuments
        }
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

/// Where a stream lives, as the local API reports it.
fn stream_location_on_the_wire(stream_location: StreamLocation) -> StreamLocationOnTheWire {
    StreamLocationOnTheWire {
        project_directory: stream_location.project_directory,
        project: stream_location.project,
        stream_asset: stream_location.stream_asset,
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
        LocalApiServedOnAFreshSocket, STUB_MACHINE_NAME, STUB_STREAM_ASSET, STUB_STREAM_ID,
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
    use serde_json::json;
    use streamlib::sdk::descriptors::ProcessorClassImportPath;

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
        crate::control_plane_stub_support::every_reserved_slot_reader_is_unreachable!("tool");
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
        "set_node_config",
        "set_pin_value",
        "rename_node",
        "set_studio_fields",
        "set_exposed_config_fields",
        "import_graph_generator",
        "refresh_fragments",
        "add_group",
        "set_group_fields",
        "remove_group",
        "add_note",
        "set_note_fields",
        "remove_note",
        "collapse_selection_into_subgraph",
        "unlock_generator_fragment",
        "transact",
        "undo",
        "redo",
        "open_project",
        "reload_project",
        "close_project",
        "start_playback",
        "create_project",
        "import_project",
        "describe_stream_action",
        "cancel_stream_action",
        "stop_playback",
        "pause_playback",
        "resume_playback",
        "remove_project",
        "list_streams",
        "open_viewer",
        "close_viewer",
        "set_stream_clock_scale",
        "describe_runtime",
        "list_assets",
        "describe_asset",
        "read_asset_preview",
        "import_asset",
        "relocate_asset",
        "delete_asset",
        "set_asset_settings",
        "replace_asset",
        "create_stream",
        "delete_stream",
        "set_stream_display_name",
        "list_project_files",
        "read_project_file",
        "write_project_file",
        "create_node_file",
        "add_dependency",
        "update_dependency",
        "list_dependencies",
        "remove_dependency",
        "use_dependency_stream",
        "list_project_node_types",
        "rename_project",
        "set_project_description",
        "set_project_cover",
        "duplicate_project",
        "delete_project",
        "list_project_views",
        "write_studio_view_output",
        "write_graph_input",
    ];

    /// The tools that act on one stream, each naming it with a required
    /// `stream`.
    const TOOLS_THAT_REQUIRE_A_STREAM: &[&str] = &[
        "add_node",
        "remove_node",
        "connect",
        "disconnect",
        "set_node_config",
        "set_pin_value",
        "rename_node",
        "set_studio_fields",
        "set_exposed_config_fields",
        "import_graph_generator",
        "add_group",
        "set_group_fields",
        "remove_group",
        "add_note",
        "set_note_fields",
        "remove_note",
        "collapse_selection_into_subgraph",
        "unlock_generator_fragment",
        "use_dependency_stream",
        "open_viewer",
        "set_stream_clock_scale",
        "write_studio_view_output",
        "write_graph_input",
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

    /// The operations the stub was handed live, in call order.
    fn recorded_stream_graph_operations(
        recorded: &crate::control_plane_stub_support::RecordedGraphMutations,
    ) -> Vec<streamlib_project_repository::StreamGraphOperation> {
        recorded.lock().clone()
    }

    const STUB_CAMERA_NODE_UUID: &str = "11111111-1111-4111-8111-111111111111";
    const STUB_EFFECT_NODE_UUID: &str = "22222222-2222-4222-8222-222222222222";

    /// A stub whose graph holds a camera linked to an effect, each by its UUID.
    fn a_stub_holding_a_camera_linked_to_an_effect() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        *runtime.exported_graph.lock() = json!({
            "machine": crate::control_plane_stub_support::STUB_MACHINE_NAME,
            "nodes": [
                { "id": STUB_CAMERA_NODE_UUID, "name": "camera", "type": "tatolab.camera:Camera",
                  "config": { "fps": 30 }, "pins": {}, "studio": { "position": [0, 0] } },
                { "id": STUB_EFFECT_NODE_UUID, "name": "fx", "type": "nodes.fx:Fx",
                  "config": {}, "pins": { "strength": 0.5 } },
            ],
            "links": [
                { "id": "link-9", "source": { "node": "camera", "port": "video" },
                  "target": { "node": "fx", "port": "video_in" }, "state": "pending" },
            ],
        });
        runtime
    }

    #[tokio::test]
    async fn tools_call_add_node_applies_the_node_live_and_answers_its_new_uuid_and_name() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let stated = first_text_block_json(
            &tool_call_result(
                runtime,
                "add_node",
                json!({
                    "stream": STUB_STREAM_ASSET,
                    "type": "processors.grayscale_effect:GrayscaleEffect",
                    "config": { "strength": 0.5 },
                    "name": "Gray",
                    "mode": "live",
                }),
            )
            .await,
        );

        let [streamlib_project_repository::StreamGraphOperation::AddNode { node, links }] =
            &recorded_stream_graph_operations(&recorded)[..]
        else {
            panic!(
                "add_node reaches exactly one add, recorded {:?}",
                recorded.lock()
            );
        };
        assert_eq!(
            node.node_type,
            "processors.grayscale_effect:GrayscaleEffect"
        );
        assert_eq!(node.config, json!({ "strength": 0.5 }));
        assert_eq!(node.name, "Gray");
        assert!(links.is_empty());
        assert_eq!(stated["mode"], "live");
        assert_eq!(
            stated["transaction"],
            Value::Null,
            "a live edit records nothing"
        );
        assert_eq!(
            stated["added_nodes"],
            json!([{ "stream": STUB_STREAM_ID, "node": node.id.to_string(), "name": "gray" }]),
            "the answer is the UUID the runtime made and the name the engine cast"
        );
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
                "stream": STUB_STREAM_ASSET,
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "name": "gray",
                "mode": "live",
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

    /// A committed edit is kept in the stream's graph document, so a stream whose directory
    /// holds no project is refused before anything is applied.
    #[tokio::test]
    async fn a_committed_edit_to_a_stream_whose_directory_holds_no_project_is_refused() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({ "stream": STUB_STREAM_ASSET, "type": "nodes.fx:Fx" }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("holds no project"), "{text}");
        assert!(recorded.lock().is_empty(), "nothing was applied");
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
        output("video", family = Image),
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
                streamlib::sdk::runtime::OptionsForLoadingOneStream::at_stream_location(
                    "main".to_owned(),
                    StreamLocation {
                        project_directory: project_directory.path().to_path_buf(),
                        project: uuid::Uuid::new_v4(),
                        stream_asset: STUB_STREAM_ASSET,
                    },
                ),
            )
            .unwrap();

        let body = tool_call_result(
            Arc::clone(&runtime) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
            "add_node",
            json!({
                "stream": STUB_STREAM_ASSET,
                "type": source_type.as_str(),
                "name": "front",
                "config": {"frame_widht": 640},
                "mode": "live",
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

        let stated = first_text_block_json(
            &tool_call_result(
                runtime,
                "add_node",
                json!({ "stream": STUB_STREAM_ASSET, "type": "tatolab.moq:MoqTrackReceive", "mode": "live" }),
            )
            .await,
        );
        assert_eq!(
            stated["added_nodes"][0]["name"],
            crate::control_plane_stub_support::STUB_ADDED_NODE_NAME,
            "an unnamed node is answered with the name the engine gave it"
        );
        let [streamlib_project_repository::StreamGraphOperation::AddNode { node, .. }] =
            &recorded_stream_graph_operations(&recorded)[..]
        else {
            panic!("expected one add, recorded {:?}", recorded.lock());
        };
        assert_eq!(node.config, json!({}));
        assert_eq!(node.name, "");
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
                "stream": STUB_STREAM_ASSET,
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "display_name": "gray",
                "mode": "live",
            }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("display_name"), "{text}");
        assert!(recorded.lock().is_empty(), "nothing was added");
    }

    /// Each end is a node's UUID and a port name, handed to the engine as given.
    #[tokio::test]
    async fn tools_call_connect_and_disconnect_name_each_end_by_its_nodes_uuid() {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();
        let link_onto = |target_port: &str| {
            json!({
                "stream": STUB_STREAM_ASSET,
                "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "port": target_port },
                "mode": "live",
            })
        };

        let connected = first_text_block_json(
            &tool_call_result(runtime.clone(), "connect", link_onto("mask")).await,
        );
        let disconnected = first_text_block_json(
            &tool_call_result(runtime, "disconnect", link_onto("video_in")).await,
        );

        let link_of = |target_port: &str| streamlib_project_repository::StreamGraphDocumentLink {
            source: streamlib_project_repository::StreamGraphDocumentLinkEnd {
                node: STUB_CAMERA_NODE_UUID.parse().unwrap(),
                port: "video".to_owned(),
            },
            target: streamlib_project_repository::StreamGraphDocumentLinkEnd {
                node: STUB_EFFECT_NODE_UUID.parse().unwrap(),
                port: target_port.to_owned(),
            },
        };
        assert_eq!(
            recorded_stream_graph_operations(&recorded),
            [
                streamlib_project_repository::StreamGraphOperation::Connect {
                    link: link_of("mask")
                },
                streamlib_project_repository::StreamGraphOperation::Disconnect {
                    link: link_of("video_in")
                },
            ]
        );
        assert_eq!(
            connected["connected_links"],
            json!([{
                "stream": STUB_STREAM_ID,
                "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "port": "mask" },
            }]),
            "the new link's state is read from the stream's own `graph`"
        );
        assert_eq!(disconnected["connected_links"], json!([]));
    }

    /// A node is named by its UUID: a name, or a UUID the graph does not hold,
    /// is refused naming it before anything reaches the runtime.
    #[tokio::test]
    async fn a_graph_tool_naming_a_node_by_its_name_or_an_absent_uuid_is_refused_naming_it() {
        let absent_node_uuid = "33333333-3333-4333-8333-333333333333";
        for (tool_name, arguments, what_the_refusal_names) in [
            (
                "remove_node",
                json!({ "stream": STUB_STREAM_ASSET, "node": "fx", "mode": "live" }),
                "`fx` is neither a node's UUID",
            ),
            (
                "connect",
                json!({
                    "stream": STUB_STREAM_ASSET,
                    "source": { "node": "camera", "port": "video" },
                    "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
                    "mode": "live",
                }),
                "`camera` is neither a node's UUID",
            ),
            (
                "set_node_config",
                json!({ "stream": STUB_STREAM_ASSET, "node": absent_node_uuid, "config": {}, "mode": "live" }),
                absent_node_uuid,
            ),
        ] {
            let runtime = a_stub_holding_a_camera_linked_to_an_effect();
            let recorded = runtime.recorded_graph_mutations.clone();
            let body = tool_call_result(runtime, tool_name, arguments.clone()).await;
            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(text.contains(&format!("{tool_name} failed")), "{text}");
            assert!(text.contains(what_the_refusal_names), "{text}");
            assert!(recorded.lock().is_empty(), "nothing reached the runtime");
        }
    }

    fn a_studio_at(
        position: [i64; 2],
    ) -> streamlib_project_repository::StreamGraphNodeStudioFields {
        streamlib_project_repository::StreamGraphNodeStudioFields {
            position: Some(position.map(Into::into)),
            ..Default::default()
        }
    }

    /// Each edit carries what its inverse needs, read from the graph as it was.
    #[tokio::test]
    async fn each_node_edit_carries_what_the_node_held_before_it() {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();
        for (tool_name, arguments) in [
            (
                "set_node_config",
                json!({ "node": STUB_CAMERA_NODE_UUID, "config": { "fps": 60 } }),
            ),
            (
                "set_pin_value",
                json!({ "node": STUB_EFFECT_NODE_UUID, "input": "strength", "value": 0.9 }),
            ),
            (
                "rename_node",
                json!({ "node": STUB_EFFECT_NODE_UUID, "name": "glow" }),
            ),
            (
                "set_studio_fields",
                json!({ "node": STUB_CAMERA_NODE_UUID, "studio": { "position": [4, 2] } }),
            ),
            (
                "set_exposed_config_fields",
                json!({ "node": STUB_CAMERA_NODE_UUID, "exposed_config_fields": ["fps"] }),
            ),
            ("remove_node", json!({ "node": STUB_EFFECT_NODE_UUID })),
        ] {
            let mut arguments = arguments;
            arguments["stream"] = json!(STUB_STREAM_ASSET);
            arguments["mode"] = json!("live");
            first_text_block_json(&tool_call_result(runtime.clone(), tool_name, arguments).await);
        }

        let camera: uuid::Uuid = STUB_CAMERA_NODE_UUID.parse().unwrap();
        let effect: uuid::Uuid = STUB_EFFECT_NODE_UUID.parse().unwrap();
        let recorded = recorded_stream_graph_operations(&recorded);
        use streamlib_project_repository::StreamGraphOperation;
        assert_eq!(
            recorded[0],
            StreamGraphOperation::SetNodeConfig {
                node: camera,
                from: json!({ "fps": 30 }),
                to: json!({ "fps": 60 }),
            }
        );
        assert_eq!(
            recorded[1],
            StreamGraphOperation::SetPinValue {
                node: effect,
                input: "strength".to_owned(),
                from: Some(json!(0.5)),
                to: Some(json!(0.9)),
            }
        );
        assert_eq!(
            recorded[2],
            StreamGraphOperation::RenameNode {
                node: effect,
                from: "fx".to_owned(),
                to: "glow".to_owned(),
            }
        );
        assert_eq!(
            recorded[3],
            StreamGraphOperation::SetStudioFields {
                node: camera,
                from: Box::new(a_studio_at([0, 0])),
                to: Box::new(a_studio_at([4, 2])),
            }
        );
        assert_eq!(
            recorded[4],
            StreamGraphOperation::SetExposedConfigFields {
                node: camera,
                from: Vec::new(),
                to: vec!["fps".to_owned()],
            }
        );
        let StreamGraphOperation::RemoveNode { node, links } = &recorded[5] else {
            panic!("the last edit removes, recorded {recorded:?}");
        };
        assert_eq!(node.id, effect);
        assert_eq!(
            links.len(),
            1,
            "a removed node carries its links, so undo brings them back: {links:?}"
        );
    }

    /// `transact` applies its operations in order, a later one naming a node an
    /// earlier `add_node` adds by its `as`; one refused undoes what came before it.
    #[tokio::test]
    async fn transact_names_an_added_node_by_its_as_and_reverses_itself_when_refused() {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();

        let stated = first_text_block_json(
            &tool_call_result(
                runtime.clone(),
                "transact",
                json!({
                    "mode": "live",
                    "operations": [
                        { "operation": "add_node", "stream": STUB_STREAM_ASSET,
                          "type": "nodes.blur:Blur", "as": "blur" },
                        { "operation": "connect", "stream": STUB_STREAM_ASSET,
                          "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                          "target": { "node": "blur", "port": "video_in" } },
                    ],
                }),
            )
            .await,
        );
        let added = stated["added_nodes"][0]["node"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(stated["added_nodes"][0]["as"], "blur");
        assert_eq!(stated["connected_links"][0]["target"]["node"], added);

        recorded.lock().clear();
        let body = tool_call_result(
            runtime,
            "transact",
            json!({
                "mode": "live",
                "operations": [
                    { "operation": "rename_node", "stream": STUB_STREAM_ASSET,
                      "node": STUB_EFFECT_NODE_UUID, "name": "glow" },
                    { "operation": "remove_node", "stream": STUB_STREAM_ASSET,
                      "node": "33333333-3333-4333-8333-333333333333" },
                ],
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        use streamlib_project_repository::StreamGraphOperation;
        let effect: uuid::Uuid = STUB_EFFECT_NODE_UUID.parse().unwrap();
        assert_eq!(
            recorded_stream_graph_operations(&recorded),
            [
                StreamGraphOperation::RenameNode {
                    node: effect,
                    from: "fx".to_owned(),
                    to: "glow".to_owned()
                },
                StreamGraphOperation::RenameNode {
                    node: effect,
                    from: "glow".to_owned(),
                    to: "fx".to_owned()
                },
            ],
            "the refused transaction's applied rename is reversed on the running stream"
        );
    }

    /// A refused graph edit answers, beside its text, a structured refusal naming the action,
    /// the operation it refused and the node or link that operation names, in the runtime's own
    /// words as the text carries them.
    #[tokio::test]
    async fn a_refused_graph_edit_answers_a_structured_refusal_naming_what_it_refused() {
        let absent_node = "33333333-3333-4333-8333-333333333333";
        let body = tool_call_result(
            a_stub_holding_a_camera_linked_to_an_effect(),
            "transact",
            json!({
                "mode": "live",
                "operations": [
                    { "operation": "rename_node", "stream": STUB_STREAM_ASSET,
                      "node": STUB_EFFECT_NODE_UUID, "name": "glow" },
                    { "operation": "remove_node", "stream": STUB_STREAM_ASSET,
                      "node": absent_node },
                ],
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        let refusal = &body["structuredContent"]["refusal"];
        assert_eq!(refusal["action"], "transact", "{refusal}");
        assert_eq!(refusal["refused_operation_index"], 1, "{refusal}");
        assert_eq!(refusal["refused_node"], absent_node, "{refusal}");
        let runtime_words = refusal["runtime_words"].as_str().unwrap();
        assert_eq!(
            body["content"][0]["text"],
            format!("transact failed: {runtime_words}"),
            "the text and the structured refusal carry the same words"
        );

        let body = tool_call_result(
            a_stub_holding_a_camera_linked_to_an_effect(),
            "connect",
            json!({
                "stream": STUB_STREAM_ASSET, "mode": "live",
                "source": { "node": absent_node, "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
            }),
        )
        .await;
        let refusal = &body["structuredContent"]["refusal"];
        assert_eq!(refusal["refused_operation_index"], 0, "{refusal}");
        assert_eq!(
            refusal["refused_link"],
            json!({
                "source": { "node": absent_node, "port": "video" },
                "target": { "node": STUB_EFFECT_NODE_UUID, "port": "video_in" },
            }),
            "{refusal}"
        );
    }

    /// A blank idempotency key is refused before anything applies.
    #[tokio::test]
    async fn a_graph_edit_under_a_blank_idempotency_key_is_refused() {
        let runtime = a_stub_holding_a_camera_linked_to_an_effect();
        let recorded = runtime.recorded_graph_mutations.clone();
        let body = tool_call_result(
            runtime,
            "rename_node",
            json!({
                "stream": STUB_STREAM_ASSET, "mode": "live", "node": STUB_EFFECT_NODE_UUID,
                "name": "glow", "idempotency_key": " ",
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        assert!(
            body["structuredContent"]["refusal"]["runtime_words"]
                .as_str()
                .unwrap()
                .contains("idempotency key"),
            "{body}"
        );
        assert!(recorded.lock().is_empty(), "nothing was applied");
    }

    #[tokio::test]
    async fn a_mutation_tool_with_malformed_arguments_is_a_tool_error_that_reaches_no_op() {
        for (tool_name, arguments, what_the_refusal_names) in [
            (
                "connect",
                json!({ "stream": STUB_STREAM_ASSET, "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" } }),
                "target",
            ),
            (
                "connect",
                json!({
                    "stream": STUB_STREAM_ASSET,
                    "source": { "node": STUB_CAMERA_NODE_UUID, "port": "video" },
                    "target": { "node": STUB_EFFECT_NODE_UUID, "prot": "video_in" },
                }),
                "prot",
            ),
            (
                "disconnect",
                json!({ "stream": STUB_STREAM_ASSET, "link_id": "link-9" }),
                "link_id",
            ),
            (
                "remove_node",
                json!({ "stream": STUB_STREAM_ASSET, "name": "fx" }),
                "name",
            ),
            (
                "transact",
                json!({ "operations": [{ "operation": "split_node" }] }),
                "split_node",
            ),
        ] {
            let runtime = a_stub_holding_a_camera_linked_to_an_effect();
            let recorded = runtime.recorded_graph_mutations.clone();
            let body = tool_call_result(runtime, tool_name, arguments.clone()).await;
            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(what_the_refusal_names),
                "{arguments} gave {text}"
            );
            assert!(
                recorded.lock().is_empty(),
                "a refused call must reach no runtime op"
            );
        }
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
                "open_project",
                json!({ "project_directory": absolute_project_directory, "keep": true }),
                "keep",
            ),
            ("open_project", json!({}), "project_directory"),
            (
                "open_project",
                json!({ "project_directory": absolute_project_directory, "name": "camera" }),
                "name",
            ),
            (
                "open_project",
                json!({ "project_directory": "projects/camera" }),
                "absolute",
            ),
            ("reload_project", json!({}), "project_directory"),
            (
                "reload_project",
                json!({ "project_directory": "projects/camera" }),
                "absolute",
            ),
            ("stop_playback", json!({}), "project_directory"),
            (
                "stop_playback",
                json!({ "project_directory": "relative/project" }),
                "absolute",
            ),
            (
                "stop_playback",
                json!({ "project_directory": absolute_project_directory, "stream": STUB_STREAM_ID }),
                "stream",
            ),
            ("start_playback", json!({}), "project_directory"),
            (
                "start_playback",
                json!({ "project_directory": "relative/project" }),
                "absolute",
            ),
            ("close_project", json!({}), "project_directory"),
            (
                "close_project",
                json!({ "project_directory": "relative/project" }),
                "absolute",
            ),
            ("pause_playback", json!({}), "project_directory"),
            (
                "pause_playback",
                json!({ "project_directory": "relative/project" }),
                "absolute",
            ),
            ("resume_playback", json!({}), "project_directory"),
            (
                "resume_playback",
                json!({ "project_directory": "relative/project" }),
                "absolute",
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
                "create_stream",
                json!({ "project_directory": absolute_project_directory, "path": "streams/a.graph.json" }),
                "display_name",
            ),
            (
                "set_stream_display_name",
                json!({ "project_directory": absolute_project_directory, "display_name": "Desk" }),
                "stream_asset",
            ),
            (
                "delete_stream",
                json!({ "project_directory": absolute_project_directory, "stream_asset": STUB_STREAM_ASSET, "stream": STUB_STREAM_ID }),
                "stream",
            ),
            (
                "list_streams",
                json!({ "stream": STUB_STREAM_ID }),
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
                "open_project",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "open_project",
            ),
            (
                "reload_project",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "reload_project",
            ),
            (
                "stop_playback",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "stop_playback",
            ),
            (
                "start_playback",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "start_playback",
            ),
            (
                "close_project",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "close_project",
            ),
            (
                "pause_playback",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "pause_playback",
            ),
            (
                "resume_playback",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "resume_playback",
            ),
            (
                "remove_project",
                json!({ "project_directory": "/home/someone/projects/camera" }),
                "remove_project",
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
                        "location": {
                            "project_directory": crate::control_plane_stub_support::STUB_PROJECT_DIRECTORY,
                            "project": crate::control_plane_stub_support::STUB_PROJECT,
                            "stream_asset": STUB_STREAM_ASSET,
                        },
                        "state": "running",
                        "node_count": null,
                        "failed_because": null,
                        "capture_devices_whose_access_was_never_asked_for": [],
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

    /// `open_project`'s and `logs`' schemas say what each takes: a required
    /// project directory and nothing else, and a count with no maximum, since
    /// a larger one is clamped.
    #[tokio::test]
    async fn the_open_project_and_logs_schemas_state_what_each_takes() {
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

        let open_project = input_schema_of("open_project");
        assert_eq!(required_of(&open_project), ["project_directory"]);
        let open_project_arguments: Vec<&String> = open_project["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(open_project_arguments, ["project_directory"]);

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
                        PortDescriptor::new(
                            "video",
                            "frames to present",
                            true,
                            streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                                streamlib::sdk::descriptors::PortFamily::Image,
                            ),
                        )
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
        (LocalApiMcpServerHandler::tool_router()
            + LocalApiMcpServerHandler::stream_graph_tool_router())
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect()
    }

    #[tokio::test]
    async fn resources_list_names_the_node_catalog_the_live_graph_and_the_actions_under_way() {
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
                "streamlib://stream-actions"
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
        let uri_templates: Vec<&str> = templates["resourceTemplates"]
            .as_array()
            .expect("a resource templates array")
            .iter()
            .map(|template| template["uriTemplate"].as_str().unwrap())
            .collect();
        assert_eq!(
            uri_templates,
            [
                "streamlib://project/{project}/transactions",
                "streamlib://stream-action/{action}"
            ],
            "{templates}"
        );
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
                    PortDescriptor::new(
                        "video_from_upstream",
                        "",
                        true,
                        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                            streamlib::sdk::descriptors::PortFamily::Image,
                        ),
                    )
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
            json!([{ "stream": STUB_STREAM_ID, "nodes": [], "installed": [] }]),
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
        assert!(
            text.contains("`target`: that UUID and the port `video`"),
            "{text}"
        );
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
            text.contains(r#"`source`: `{"node": "PatternSourceId", "port": "video"}`"#),
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
                        PortDescriptor::new(
                            "bags_from_upstream",
                            "",
                            true,
                            streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                                streamlib::sdk::descriptors::PortFamily::Image,
                            ),
                        )
                        .with_delivery_profile(delivery_profile),
                    )
                    .with_output(PortDescriptor::new(
                        "bags_to_downstream",
                        "",
                        true,
                        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                            streamlib::sdk::descriptors::PortFamily::Image,
                        ),
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
