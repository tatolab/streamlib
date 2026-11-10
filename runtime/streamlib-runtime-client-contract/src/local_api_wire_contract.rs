// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The names a runtime's local API speaks on its socket and its loopback HTTP listener on its
//! port, and the shape of each result a client parses, written once for the runtime that serves
//! them and every client that calls them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The route MCP's Streamable HTTP transport is served on.
pub const MCP_STREAMABLE_HTTP_ROUTE_PATH: &str = "/mcp";

/// The request target a client upgrades to MCP's stdio framing on.
pub const MCP_STDIO_UPGRADE_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token [`MCP_STDIO_UPGRADE_REQUEST_TARGET`] switches to, matched
/// case-insensitively.
pub const MCP_STDIO_UPGRADE_PROTOCOL_TOKEN: &str = "mcp-stdio";

/// The `logs` tool's result: one project's log records after the cursor asked for, read from
/// the project's log on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsToolResult {
    /// The project whose log these are.
    pub project_directory: PathBuf,
    /// The records after `after` that match the call's `stream` and `action`, oldest first, each
    /// as the project's JSONL log holds it.
    pub records: Vec<serde_json::Value>,
    /// The opaque cursor the next call passes as `after` to read on from this page.
    pub next_after: String,
    /// Whether retention deleted the segment `after` named, so the page resumes at the oldest
    /// record held.
    pub records_no_longer_held: bool,
}

/// Where a stream lives, as the local API reports it: its project directory, resolved, its
/// project's UUID and its stream asset's UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLocationOnTheWire {
    /// The project directory's absolute path, symlinks resolved, as of the stream's last load.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: uuid::Uuid,
    /// The stream asset's UUID, from its graph document.
    pub stream_asset: uuid::Uuid,
}

/// The `stop_project` tool's result: the stop of the whole project queued on its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopProjectToolResult {
    /// The queued stop, as `describe_stream_action` takes it.
    pub action: String,
    /// The project whose streams it unloads and records stopped, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `pause_project` tool's result: the project paused, at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseProjectToolResult {
    /// The project paused, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each loaded stream of the project this call paused.
    pub streams: Vec<String>,
}

/// The `resume_project` tool's result: the project resumed, at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeProjectToolResult {
    /// The project resumed, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each paused stream of the project this call resumed.
    pub streams: Vec<String>,
}

/// The `remove_project` tool's result: the removal of the whole project queued on its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveProjectToolResult {
    /// The queued removal, as `describe_stream_action` takes it.
    pub action: String,
    /// The project whose streams it unloads and forgets, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `list_streams` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStreamsToolResult {
    /// Every stream the runtime holds.
    pub streams: Vec<ListStreamsToolResultStream>,
}

/// One stream of a [`ListStreamsToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStreamsToolResultStream {
    /// The stream's id; every tool naming the stream takes it.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives; `null` for a stream a library host loaded, and for a
    /// record an earlier runtime wrote.
    pub location: Option<StreamLocationOnTheWire>,
    /// Whether it is running, stopped or failed.
    pub state: ListedStreamState,
    /// How many nodes its loaded graph holds; `null` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `null` when it is not.
    pub failed_because: Option<String>,
    /// Each capture device a node of the loaded stream refused because nobody has asked the user
    /// for its access — the runtime re-loaded it on its own at its start — once each; a
    /// `stop_project` then `start_project` of its project asks. Empty when it is not loaded.
    pub capture_devices_whose_access_was_never_asked_for: Vec<CaptureDeviceOnTheWire>,
    /// The stream action preparing it, as `describe_stream_action` takes it; `null` unless it
    /// is `preparing`.
    pub action: Option<String>,
    /// The step that action is on; `null` while it waits in its project's queue, and unless the
    /// stream is `preparing`.
    pub step: Option<StreamActionStepOnTheWire>,
}

/// A capture device the platform gates behind a privacy prompt, as `list_streams` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureDeviceOnTheWire {
    /// The camera.
    Camera,
    /// The microphone.
    Microphone,
}

impl CaptureDeviceOnTheWire {
    /// The device as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            CaptureDeviceOnTheWire::Camera => "camera",
            CaptureDeviceOnTheWire::Microphone => "microphone",
        }
    }
}

/// The state of one stream `list_streams` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListedStreamState {
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the runtime's next
    /// start.
    Running,
    /// Stopped with its project, and left unloaded until `start_project`.
    Stopped,
    /// Loaded with its project paused: its processors process nothing until `resume_project`.
    Paused,
    /// Skipped at the runtime's start until a `reload_project` of its project retries it.
    Failed,
    /// A stream action of its project that loads it is queued or under way on it.
    Preparing,
    /// A stream action of its project that unloads it is queued or under way on it.
    Stopping,
}

impl ListedStreamState {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            ListedStreamState::Running => "running",
            ListedStreamState::Stopped => "stopped",
            ListedStreamState::Paused => "paused",
            ListedStreamState::Failed => "failed",
            ListedStreamState::Preparing => "preparing",
            ListedStreamState::Stopping => "stopping",
        }
    }
}

impl std::fmt::Display for ListedStreamState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

/// The `start_project` or `reload_project` tool's result: the load it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOrReloadProjectToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The project whose queue holds it, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `create_project` tool's result: the load it queued of the project it created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The id the scaffold's `stream:main` loads as.
    pub stream: String,
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `import_project` tool's result: the project it unpacked from a `.zip` of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProjectToolResult {
    /// The new project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The new project's UUID, its own and never the zipped project's.
    pub project: String,
    /// The project's name, as the zip's `tatolab-project.json` named it.
    pub name: String,
    /// The first commit of the new project's history, which records every file it unpacked.
    pub commit: String,
}

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionKindOnTheWire {
    /// Loads every graph document of a project.
    Load,
    /// Unloads every stream of a project and records each stopped.
    Stop,
    /// Unloads every stream of a project and forgets every record of it.
    Remove,
    /// Re-loads, at the runtime's start, a project's streams neither stopped nor failed.
    ReloadAtStart,
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStateOnTheWire {
    /// Waiting behind its project's earlier actions.
    Queued,
    /// Running one of its steps.
    InProgress,
    /// Finished; each stream it took in is running or failed.
    Done,
    /// Ended without taking in the project, for `failed_because`.
    Failed,
    /// Ended early by `cancel_stream_action`: dropped from its queue, or stopped at the step it
    /// was on.
    Cancelled,
}

impl StreamActionStateOnTheWire {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            StreamActionStateOnTheWire::Queued => "queued",
            StreamActionStateOnTheWire::InProgress => "in_progress",
            StreamActionStateOnTheWire::Done => "done",
            StreamActionStateOnTheWire::Failed => "failed",
            StreamActionStateOnTheWire::Cancelled => "cancelled",
        }
    }
}

/// The step a running stream action is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStepOnTheWire {
    /// Making the project's `.venv` with the runtime's uv.
    MakingTheVenv,
    /// Reading the project's file and each stream asset's graph document.
    ReadingTheGraphDocuments,
    /// Loading and starting the streams the graph documents hold.
    Starting,
    /// Unloading the streams it works on.
    Stopping,
}

impl StreamActionStepOnTheWire {
    /// The step as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            StreamActionStepOnTheWire::MakingTheVenv => "making_the_venv",
            StreamActionStepOnTheWire::ReadingTheGraphDocuments => "reading_the_graph_documents",
            StreamActionStepOnTheWire::Starting => "starting",
            StreamActionStepOnTheWire::Stopping => "stopping",
        }
    }
}

/// The `describe_stream_action` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// The project it works on.
    pub project_directory: PathBuf,
    /// What it does.
    pub kind: StreamActionKindOnTheWire,
    /// Where it is in its life.
    pub state: StreamActionStateOnTheWire,
    /// The step it is on while `in_progress`; `null` otherwise.
    pub step: Option<StreamActionStepOnTheWire>,
    /// The streams it works on, once its project's graph documents named them.
    pub streams: Vec<DescribeStreamActionToolResultStream>,
    /// Why it failed; `null` unless it did.
    pub failed_because: Option<String>,
}

/// The `cancel_stream_action` tool's result: the action's state once the cancel was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// `cancelled` for an action dropped from its queue; `in_progress` for a running one, which
    /// ends `cancelled` once it stops at the step it is on.
    pub state: StreamActionStateOnTheWire,
}

/// One stream of a [`DescribeStreamActionToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResultStream {
    /// The stream's id.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// `running` or `failed` once the action took it in; `null` before.
    pub state: Option<ListedStreamState>,
}

/// The `open_viewer` tool's result: what a viewer subscribes to, from its own process, to read
/// the latest message an output port's node wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenViewerToolResult {
    /// The viewer's id, which `close_viewer` takes: `<stream>/<node_id>/<port>`.
    pub viewer: String,
    /// The stream's id.
    pub stream: String,
    /// The node's id, as `graph` lists it.
    pub node_id: String,
    /// The node's name: its label, not its identity.
    pub node_name: String,
    /// The output port's name.
    pub port: String,
    /// The root of the iceoryx2 domain the port's channel lives in.
    pub iceoryx2_domain_root: String,
    /// The port's channel: the iceoryx2 service whose reserved subscriber slot the viewer takes.
    pub channel: String,
    /// Where the viewer imports an Image frame's surface: the surface-sharing socket's path on
    /// Linux, the surface-sharing Mach service's name on macOS. Absent on macOS when the open
    /// named no `viewer_process_id`, since the service admits only a named process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_share_address: Option<String>,
}

/// The `close_viewer` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseViewerToolResult {
    /// The viewer closed.
    pub viewer: String,
}

/// The `write_studio_view_output` tool's result: the output of the view node the value was handed
/// to, which published it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteStudioViewOutputToolResult {
    /// The stream id.
    pub stream: String,
    /// The view node's UUID, as `graph` lists it under `id`.
    pub node: String,
    /// The output port the value was published on.
    pub port: String,
}

/// The `set_stream_clock_scale` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetStreamClockScaleToolResult {
    /// The stream's id.
    pub stream: String,
    /// Stream seconds per machine second from now on.
    pub scale: f64,
}

/// The `describe_runtime` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeRuntimeToolResult {
    /// The runtime's crate version.
    pub version: String,
    /// `<crate version>+<git sha>.<per-build nonce>`, the build the helper check compares.
    pub build_id: String,
    /// Monotonic milliseconds since this runtime's engine started; a restart resets it.
    pub up_for_ms: u64,
    /// The folder the runtime's own log is written to, absolute for `tatolabd`; `None` when it
    /// keeps none.
    pub runtime_log_directory: Option<PathBuf>,
}

/// The port the loopback HTTP listener binds on `127.0.0.1` when nothing names another; on macOS
/// its bind is the machine runtime lock.
pub const LOOPBACK_HTTP_LISTENER_DEFAULT_PORT: u16 = 4446;

/// The loopback HTTP listener's route answering the pid of the process serving it, as a
/// [`MachineRuntimeLockHolderPidAnswer`].
pub const MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH: &str = "/machine-runtime-lock-holder";

/// `GET` [`MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH`]'s answer: the serving process's pid, which a
/// prober names through the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRuntimeLockHolderPidAnswer {
    /// The pid of the process serving the loopback HTTP listener.
    pub pid: i32,
}

/// One asset of a project as the asset tools report it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectAssetOnTheWire {
    /// The asset's UUID, which the runtime made at import and which a rename or move keeps.
    pub asset: String,
    /// `stream`, `media`, `scene` or `data`.
    pub kind: String,
    /// Its path in the project, `/`-separated, relative to the project directory.
    pub path: String,
    /// The SHA-256 of its bytes as its sidecar records them, in lowercase hex.
    pub content_sha256: String,
    /// The size of its bytes.
    pub content_byte_len: u64,
    /// The settings its import took.
    pub import_settings: serde_json::Map<String, serde_json::Value>,
}

/// The `list_assets` tool's result: the project's assets, ordered by path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListAssetsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The assets, narrowed to one kind when the call named one.
    pub assets: Vec<ProjectAssetOnTheWire>,
    /// The project's change feed: the resource a client reads for its newest transactions and
    /// names in `subscriptions/listen` to be told of each new one.
    pub transactions_resource_uri: String,
}

/// One transaction a project's change feed holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransactionOnTheWire {
    /// The format the transaction was recorded in, `1` for one recorded before formats were.
    pub format_version: u32,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
    /// The commit history stood at when it applied, absent for a project's first.
    pub parent_commit: Option<String>,
    /// `person`, `agent` or `runtime`.
    pub author_kind: String,
    /// The author's name as history shows it.
    pub author_name: String,
    /// What it changed, in order, each tagged by its `operation`.
    pub operations: Vec<serde_json::Value>,
    /// What undoes it, in order.
    pub inverse_operations: Vec<serde_json::Value>,
    /// The transaction this one undoes, when it is an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    /// The transaction this one redoes, when it is a redo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redoes: Option<String>,
    /// The key the caller sent it under, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The actions its caller sent, exactly as sent, each tagged by its `operation`; absent for
    /// an undo, a redo, or a change the runtime made itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<serde_json::Value>,
    /// Where an edit or a redo stands with its author's undo and redo now: `applied`,
    /// `undone_redoable` or `undone_not_redoable`, the same for an edit and every redo of it;
    /// absent for an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_state: Option<String>,
    /// When the runtime applied it, RFC 3339 in UTC to the second, for display only.
    pub applied_at: String,
}

/// An action on a project the runtime refused, as its change feed holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefusedProjectActionOnTheWire {
    /// `person`, `agent` or `runtime`.
    pub author_kind: String,
    /// The author's name as an applied transaction would carry it.
    pub author_name: String,
    /// `commit` or `live`, as the action asked.
    pub mode: String,
    /// The operations the action asked for, in order, as the request named them.
    pub operations: Vec<serde_json::Value>,
    /// The key the caller sent it under, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// What the runtime refused, in its own words.
    pub refusal: ProjectActionRefusalOnTheWire,
    /// When the runtime refused it, RFC 3339 in UTC to the second, for display only.
    pub refused_at: String,
}

/// One entry of a project's change feed, tagged by its `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProjectChangeFeedEntryOnTheWire {
    /// A transaction the project's history records.
    Applied(ProjectTransactionOnTheWire),
    /// An action the runtime refused since it started, which no history records.
    Refused(RefusedProjectActionOnTheWire),
}

/// What a refused action answers under `refusal` in its tool result's `structuredContent`,
/// beside the same words as the result's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectActionRefusalOnTheWire {
    /// The action refused, by the name of the tool it was sent to.
    pub action: String,
    /// The position, from 0, of the operation refused among those the action sent, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_operation_index: Option<usize>,
    /// The node the refused operation names, as the caller named it: a UUID or an `as` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_node: Option<String>,
    /// The link the refused operation names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_link: Option<RefusedGraphLinkOnTheWire>,
    /// The node's input or config field the refused operation names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_field: Option<String>,
    /// Why, in the runtime's own words, never rewritten.
    pub runtime_words: String,
}

/// The link a refused operation names, each end as the caller named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedGraphLinkOnTheWire {
    /// The output port the link carries from.
    pub source: GraphLinkEndOnTheWire,
    /// The input port the link carries into.
    pub target: GraphLinkEndOnTheWire,
}

/// One end of a link a graph tool names: a node by its UUID and one of its ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphLinkEndOnTheWire {
    /// The node's UUID.
    pub node: String,
    /// The port's name.
    pub port: String,
}

/// A node a graph transaction added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphTransactionAddedNode {
    /// The stream it was added to, by stream id.
    pub stream: String,
    /// The UUID the runtime made for it.
    pub node: String,
    /// The name it took.
    pub name: String,
    /// The `as` its `add_node` gave it, which later operations of the transaction named it by.
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A labelled group a graph transaction made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphTransactionAddedGroup {
    /// The stream it was made in, by stream id.
    pub stream: String,
    /// The UUID the runtime made for it.
    pub group: String,
    /// Its title.
    pub title: String,
}

/// A link a graph transaction made, with the state the stream's graph reads it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphTransactionConnectedLink {
    /// The stream it was made in, by stream id.
    pub stream: String,
    /// The output port it carries from.
    pub source: GraphLinkEndOnTheWire,
    /// The input port it carries into.
    pub target: GraphLinkEndOnTheWire,
    /// `pending`, `wired` or `error`; absent when the graph could not be read after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Why a link reading `error` will carry nothing, in the refusing end's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
}

/// The result of a graph tool: the transaction it applied, committed or live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphTransactionToolResult {
    /// `commit`, written to the graph documents and recorded in history, or `live`, applied
    /// to the running streams alone.
    pub mode: String,
    /// The transaction's UUID; absent on a live one, which history never records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// The commit on the project history's `main` that records it; absent on a live one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Each operation as applied, in order, tagged by its `operation`, its `stream` by
    /// stream id.
    pub operations: Vec<serde_json::Value>,
    /// Each node it added.
    pub added_nodes: Vec<GraphTransactionAddedNode>,
    /// Each link it made.
    pub connected_links: Vec<GraphTransactionConnectedLink>,
    /// Each labelled group it made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_groups: Vec<GraphTransactionAddedGroup>,
}

/// What a project's transactions resource holds: its newest transactions and the actions refused
/// since the runtime started, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransactionsResourceContents {
    /// The project's UUID.
    pub project: String,
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// Its newest applied transactions and refused actions, newest first.
    pub transactions: Vec<ProjectChangeFeedEntryOnTheWire>,
}

/// The `describe_asset` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DescribeAssetToolResult {
    /// The asset.
    pub asset: ProjectAssetOnTheWire,
    /// Whether a file is at the asset's path.
    pub file_at_its_path: bool,
}

/// The result of an asset tool that changed one asset and committed the change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetTransactionToolResult {
    /// The asset as the transaction left it; a deleted asset as it was.
    pub asset: ProjectAssetOnTheWire,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
    /// The stream action loading the stream a created stream asset holds, while its project
    /// runs; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// A project this one imports, as a dependency tool answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDependencyOnTheWire {
    /// The imported project's UUID.
    pub project: String,
    /// The folder under `imports/` that holds its copy.
    pub folder: String,
    /// Where it was copied from, as `add_dependency` or `update_dependency` was given it.
    pub source: String,
    /// The SHA-256 naming the version its copy carried.
    pub version: String,
}

/// The result of `add_dependency` and `remove_dependency`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyTransactionToolResult {
    /// The import as the transaction left it; a removed one as it was.
    pub dependency: ProjectDependencyOnTheWire,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// The result of `update_dependency`: the copy it committed, or, when the import's folder was
/// edited since its copy and the overwrite was not confirmed, each edited file and no change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateDependencyToolResult {
    /// The import as the update left it.
    pub dependency: ProjectDependencyOnTheWire,
    /// Each file of the import's folder edited since its copy, by its path inside the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_files: Vec<String>,
    /// Whether the update waits on `overwrite_edited_files` to overwrite `edited_files`.
    pub overwrite_confirmation_needed: bool,
    /// The transaction's UUID, when the update committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// The commit on the project history's `main` that records it, when the update committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// One import as `list_dependencies` answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListedProjectDependencyOnTheWire {
    /// The import as `tatolab-project.json` records it.
    pub dependency: ProjectDependencyOnTheWire,
    /// Each file of its folder edited since its copy, by its path inside the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_files: Vec<String>,
    /// With `check_for_newer`, the version its source holds now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_on_the_source: Option<String>,
    /// With `check_for_newer`, whether `version_on_the_source` is not the copy's `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_version_exists: Option<bool>,
    /// Why the source could not be read, or the folder's files listed, in the runtime's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_refused_because: Option<String>,
}

/// The result of `list_dependencies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListDependenciesToolResult {
    /// Each import of the project, in the order `tatolab-project.json` lists them.
    pub dependencies: Vec<ListedProjectDependencyOnTheWire>,
}

/// The `list_project_views` tool's result: every Tato Studio view a project and its imports
/// bring, read from each `views/<folder>/tatolab_view.json` without running anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListProjectViewsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The project's own views by folder, then each import's in the order its project file
    /// records them.
    pub views: Vec<ProjectViewListed>,
}

/// One folder of a `views/`: the view type it declares, and whether it opens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectViewListed {
    /// The view type's id, `<project name>:<view folder>`, the node type a node of the view is.
    pub view_type: String,
    /// The UUID of the project whose `views/` holds it.
    pub project: String,
    /// That project's name.
    pub project_name: String,
    /// That project's folder under `imports/`; `None` for the listed project's own view.
    pub import_folder: Option<String>,
    /// The view's folder relative to the listed project, as `views/deck` or
    /// `imports/levels/views/meter`: the path a project file read names its files under.
    pub view_folder_in_the_project: String,
    /// Whether its manifest reads against the view contract this runtime reads.
    pub opens: bool,
    /// Why it does not open, in the runtime's own words; `None` when it opens.
    pub not_opened_because: Option<String>,
    /// The name the view-type menu lists; `None` when it does not open.
    pub title: Option<String>,
    /// What the view is for.
    pub description: Option<String>,
    /// Its icon, a path inside its folder.
    pub icon: Option<String>,
    /// Its preview picture, a path inside its folder.
    pub preview: Option<String>,
    /// `area` or `node`; `None` when it does not open.
    pub placement: Option<String>,
    /// The inputs a `node` view declares.
    pub inputs: Vec<ProjectViewDeclaredPort>,
    /// The outputs a `node` view declares.
    pub outputs: Vec<ProjectViewDeclaredPort>,
    /// Its `tatolab_view.json` as written, when it is JSON.
    pub manifest: Option<serde_json::Value>,
}

/// An input or output a view declares, as a node declares a port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectViewDeclaredPort {
    /// The port's name.
    pub name: String,
    /// The port family's wire name.
    pub family: String,
    /// What the port carries.
    pub description: Option<String>,
    /// The lowest and highest value an Array port carries.
    pub range: Option<[f64; 2]>,
}

/// The `list_project_node_types` tool's result: what a project and each source it draws node
/// code from declares, read from source without running it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListProjectNodeTypesToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The project first, then each import, then each installed distribution.
    pub sources: Vec<NodeTypeSourceReadWithoutRunningIt>,
}

/// Where a listed node type's code comes from, under `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum NodeTypeSourceOrigin {
    /// The project's own node code, outside `imports/`.
    Project,
    /// An import's node code as its folder under `imports/` holds it now.
    Dependency {
        /// The imported project's UUID.
        project: String,
        /// Its folder under `imports/`.
        folder: String,
        /// The version its copy carried.
        version: String,
    },
    /// An installed distribution that declares `tatolab.nodes` entry points.
    InstalledDistribution {
        /// The distribution's name.
        distribution: String,
        /// The distribution's version.
        distribution_version: String,
    },
}

/// One source's node types, graph generators, and the modules of it that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTypeSourceReadWithoutRunningIt {
    /// Where the code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceOrigin,
    /// Each node type declared, by type.
    pub node_types: Vec<NodeTypeReadFromSource>,
    /// Each `@graph_generator` declared, by import path.
    pub graph_generators: Vec<GraphGeneratorReadFromSource>,
    /// Each module that does not parse, with why and where; the rest of the source still lists.
    pub unreadable_modules: Vec<NodeModuleNotReadable>,
}

/// One node type as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTypeReadFromSource {
    /// The type `add_node` takes: the import path `<module>:<qualname>`.
    #[serde(rename = "type")]
    pub node_type: String,
    /// `process`, `function` or `shader`; `None` for an entry point whose declaration source does not show.
    pub kind: Option<String>,
    /// The declared description, or the docstring.
    pub description: String,
    /// The palette category, the declaring module when none is declared.
    pub category: String,
    /// The input ports, `None` where only the node's native half states them.
    pub inputs: Option<Vec<NodeTypePortReadFromSource>>,
    /// The output ports, `None` where only the node's native half states them.
    pub outputs: Option<Vec<NodeTypePortReadFromSource>>,
    /// The file declaring it, relative to its source's root.
    pub path: Option<String>,
    /// The line of its `class`, `def` or assignment.
    pub line: Option<u32>,
}

/// One port as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTypePortReadFromSource {
    /// The port's name, cast as every exposed name is.
    pub name: String,
    /// The port family's wire name, `None` where source alone does not say.
    pub family: Option<String>,
    /// The port's declared description.
    pub description: String,
    /// A function or shader port's annotation as written; `None` for a process node's port.
    pub annotation: Option<String>,
    /// A function input's default pin value, where it is a literal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "a_present_value_even_null"
    )]
    pub pin_default: Option<serde_json::Value>,
    /// A function input's default as written, where it is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_default_as_written: Option<String>,
}

/// One `@graph_generator` as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphGeneratorReadFromSource {
    /// The import path `import_graph_generator` takes: `<module>:<function>`.
    pub generator: String,
    /// Its docstring.
    pub description: String,
    /// Each parameter after the builder: a setting the fragment promotes.
    pub promoted_config: Vec<GraphGeneratorPromotedSettingReadFromSource>,
    /// The file declaring it, relative to its source's root.
    pub path: String,
    /// The line of its `def`.
    pub line: u32,
}

/// One setting a graph generator promotes, as its parameter states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphGeneratorPromotedSettingReadFromSource {
    /// The parameter's name.
    pub name: String,
    /// Its annotation as written.
    pub annotation: Option<String>,
    /// Its default, where it is a literal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "a_present_value_even_null"
    )]
    pub default: Option<serde_json::Value>,
    /// Its default as written, where it is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_as_written: Option<String>,
}

/// A module of a source that could not be read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeModuleNotReadable {
    /// The module's import name.
    pub module: String,
    /// Its file, relative to its source's root.
    pub path: String,
    /// The line the parser stopped at.
    pub line: Option<u32>,
    /// The column the parser stopped at.
    pub column: Option<u32>,
    /// Why it was not read, in the parser's words.
    pub why: String,
}

/// A key that is present deserializes to `Some`, `null` included, so a default of `None` is kept.
fn a_present_value_even_null<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_description_names_its_log_folder_or_null_when_it_keeps_none() {
        let described = |runtime_log_directory: Option<&str>| DescribeRuntimeToolResult {
            version: "0.55.1".to_owned(),
            build_id: "0.55.1+0123abc.9f8e7d".to_owned(),
            up_for_ms: 266_400_000,
            runtime_log_directory: runtime_log_directory.map(PathBuf::from),
        };
        for (description, expected_wire_text) in [
            (
                described(Some("/home/u/.local/state/tatolab/logs")),
                r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":"/home/u/.local/state/tatolab/logs"}"#,
            ),
            (
                described(None),
                r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":null}"#,
            ),
        ] {
            let wire_text = serde_json::to_string(&description).unwrap();
            assert_eq!(wire_text, expected_wire_text);
            assert_eq!(
                serde_json::from_str::<DescribeRuntimeToolResult>(&wire_text).unwrap(),
                description
            );
        }
    }

    #[test]
    fn the_machine_runtime_lock_holders_answer_is_its_pid_alone() {
        let answer = MachineRuntimeLockHolderPidAnswer { pid: 4242 };
        let wire_text = serde_json::to_string(&answer).unwrap();
        assert_eq!(wire_text, r#"{"pid":4242}"#);
        assert_eq!(
            serde_json::from_str::<MachineRuntimeLockHolderPidAnswer>(&wire_text).unwrap(),
            answer
        );
    }

    /// Each stream-action result in the order and spelling the runtime writes its keys.
    #[test]
    fn every_stream_action_result_serializes_its_keys_in_wire_order() {
        fn wire_text(result: &impl Serialize) -> String {
            serde_json::to_string(result).unwrap()
        }

        assert_eq!(
            wire_text(&StopProjectToolResult {
                action: "R-8".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
            }),
            r#"{"action":"R-8","project_directory":"/srv/project"}"#
        );
        assert_eq!(
            wire_text(&RemoveProjectToolResult {
                action: "R-9".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
            }),
            r#"{"action":"R-9","project_directory":"/srv/project"}"#
        );
        assert_eq!(
            wire_text(&ListStreamsToolResult {
                streams: vec![
                    ListStreamsToolResultStream {
                        stream: "cam-main-0a1b2c".to_owned(),
                        display_name: "Desk camera".to_owned(),
                        location: Some(StreamLocationOnTheWire {
                            project_directory: PathBuf::from("/srv/cam"),
                            project: uuid::Uuid::from_u128(1),
                            stream_asset: uuid::Uuid::from_u128(2),
                        }),
                        state: ListedStreamState::Running,
                        node_count: Some(4),
                        failed_because: None,
                        capture_devices_whose_access_was_never_asked_for: vec![
                            CaptureDeviceOnTheWire::Camera,
                            CaptureDeviceOnTheWire::Microphone,
                        ],
                        action: None,
                        step: None,
                    },
                    ListStreamsToolResultStream {
                        stream: "main".to_owned(),
                        display_name: "main".to_owned(),
                        location: None,
                        state: ListedStreamState::Failed,
                        node_count: None,
                        failed_because: Some("an earlier runtime recorded it".to_owned()),
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        action: None,
                        step: None,
                    },
                    ListStreamsToolResultStream {
                        stream: "desk-0a1b2c".to_owned(),
                        display_name: "desk".to_owned(),
                        location: Some(StreamLocationOnTheWire {
                            project_directory: PathBuf::from("/srv/desk"),
                            project: uuid::Uuid::from_u128(3),
                            stream_asset: uuid::Uuid::from_u128(4),
                        }),
                        state: ListedStreamState::Preparing,
                        node_count: None,
                        failed_because: None,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        action: Some("Rabc-3".to_owned()),
                        step: Some(StreamActionStepOnTheWire::MakingTheVenv),
                    },
                ],
            }),
            r#"{"streams":[{"stream":"cam-main-0a1b2c","display_name":"Desk camera","location":{"project_directory":"/srv/cam","project":"00000000-0000-0000-0000-000000000001","stream_asset":"00000000-0000-0000-0000-000000000002"},"state":"running","node_count":4,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":["camera","microphone"],"action":null,"step":null},{"stream":"main","display_name":"main","location":null,"state":"failed","node_count":null,"failed_because":"an earlier runtime recorded it","capture_devices_whose_access_was_never_asked_for":[],"action":null,"step":null},{"stream":"desk-0a1b2c","display_name":"desk","location":{"project_directory":"/srv/desk","project":"00000000-0000-0000-0000-000000000003","stream_asset":"00000000-0000-0000-0000-000000000004"},"state":"preparing","node_count":null,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":[],"action":"Rabc-3","step":"making_the_venv"}]}"#
        );
        assert_eq!(
            wire_text(&StartOrReloadProjectToolResult {
                action: "Rabc-3".to_owned(),
                project_directory: PathBuf::from("/srv/desk"),
            }),
            r#"{"action":"Rabc-3","project_directory":"/srv/desk"}"#
        );
        assert_eq!(
            wire_text(&CreateProjectToolResult {
                action: "Rabc-4".to_owned(),
                stream: "brisk-otter-main-0a1b2c".to_owned(),
                project_directory: PathBuf::from("/home/me/streams/brisk-otter"),
            }),
            r#"{"action":"Rabc-4","stream":"brisk-otter-main-0a1b2c","project_directory":"/home/me/streams/brisk-otter"}"#
        );
        assert_eq!(
            wire_text(&DescribeStreamActionToolResult {
                action: "Rabc-3".to_owned(),
                project_directory: PathBuf::from("/srv/desk"),
                kind: StreamActionKindOnTheWire::Load,
                state: StreamActionStateOnTheWire::InProgress,
                step: Some(StreamActionStepOnTheWire::Starting),
                streams: vec![DescribeStreamActionToolResultStream {
                    stream: "desk-main-0a1b2c".to_owned(),
                    display_name: "main".to_owned(),
                    state: None,
                }],
                failed_because: None,
            }),
            r#"{"action":"Rabc-3","project_directory":"/srv/desk","kind":"load","state":"in_progress","step":"starting","streams":[{"stream":"desk-main-0a1b2c","display_name":"main","state":null}],"failed_because":null}"#
        );
        assert_eq!(
            wire_text(&CancelStreamActionToolResult {
                action: "Rabc-3".to_owned(),
                state: StreamActionStateOnTheWire::Cancelled,
            }),
            r#"{"action":"Rabc-3","state":"cancelled"}"#
        );
        assert_eq!(
            wire_text(&PauseProjectToolResult {
                project_directory: PathBuf::from("/srv/project"),
                streams: vec!["project-main-1a2b3c".to_owned()],
            }),
            r#"{"project_directory":"/srv/project","streams":["project-main-1a2b3c"]}"#
        );
        assert_eq!(
            wire_text(&ResumeProjectToolResult {
                project_directory: PathBuf::from("/srv/project"),
                streams: vec!["project-main-1a2b3c".to_owned()],
            }),
            r#"{"project_directory":"/srv/project","streams":["project-main-1a2b3c"]}"#
        );
        assert_eq!(
            wire_text(&LogsToolResult {
                project_directory: PathBuf::from("/srv/project"),
                records: vec![serde_json::json!({"message": "hello"})],
                next_after: "2:17".to_owned(),
                records_no_longer_held: false,
            }),
            r#"{"project_directory":"/srv/project","records":[{"message":"hello"}],"next_after":"2:17","records_no_longer_held":false}"#
        );
    }

    /// The `Display` spelling a client prints is the spelling the wire carries.
    #[test]
    fn each_state_displays_as_the_wire_spells_it() {
        for state in [
            ListedStreamState::Running,
            ListedStreamState::Stopped,
            ListedStreamState::Paused,
            ListedStreamState::Failed,
            ListedStreamState::Preparing,
            ListedStreamState::Stopping,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
            );
        }
    }

    /// Asset results carry the UUID, kind, path and content hash under the names clients read.
    #[test]
    fn an_asset_transaction_result_spells_its_fields_as_the_wire_carries_them() {
        let result = AssetTransactionToolResult {
            asset: ProjectAssetOnTheWire {
                asset: "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11".to_owned(),
                kind: "media".to_owned(),
                path: "media/intro.mp4".to_owned(),
                content_sha256: "ab".repeat(32),
                content_byte_len: 12,
                import_settings: serde_json::Map::new(),
            },
            transaction: "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21".to_owned(),
            commit: "c".repeat(40),
            action: None,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({
                "asset": {
                    "asset": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
                    "kind": "media",
                    "path": "media/intro.mp4",
                    "content_sha256": "ab".repeat(32),
                    "content_byte_len": 12,
                    "import_settings": {},
                },
                "transaction": "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21",
                "commit": "c".repeat(40),
            })
        );
    }

    /// A source's origin rides flat beside its listing, under `source`.
    #[test]
    fn a_node_type_source_spells_its_origin_flat_and_keeps_a_null_default() {
        let wire = serde_json::json!({
            "source": "dependency",
            "project": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
            "folder": "gripper",
            "version": "ab".repeat(32),
            "node_types": [{
                "type": "nodes.gripper:Gripper",
                "kind": "function",
                "description": "",
                "category": "nodes.gripper",
                "inputs": [{"name": "grip", "family": "channels", "description": "", "annotation": "float | None", "pin_default": null}],
                "outputs": null,
                "path": "nodes/gripper.py",
                "line": 4,
            }],
            "graph_generators": [],
            "unreadable_modules": [],
        });

        let source: NodeTypeSourceReadWithoutRunningIt =
            serde_json::from_value(wire.clone()).unwrap();

        assert_eq!(
            source.origin,
            NodeTypeSourceOrigin::Dependency {
                project: "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11".to_owned(),
                folder: "gripper".to_owned(),
                version: "ab".repeat(32),
            }
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), wire);
    }
}
