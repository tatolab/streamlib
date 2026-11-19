// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::StreamConditionOnTheWire;

/// Where a stream lives, as the local API reports it: its project directory, resolved, its
/// project's UUID and its stream asset's UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct StreamLocationOnTheWire {
    /// The project directory's absolute path, symlinks resolved, as of the stream's last load.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: uuid::Uuid,
    /// The stream asset's UUID, from its graph document.
    pub stream_asset: uuid::Uuid,
}

/// The `pause_playback` tool's result: the project's playback paused, at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PausePlaybackToolResult {
    /// The project paused, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each loaded stream of the project this call paused.
    pub streams: Vec<String>,
}

/// The `resume_playback` tool's result: the project's playback resumed, at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ResumePlaybackToolResult {
    /// The project resumed, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each paused stream of the project this call resumed.
    pub streams: Vec<String>,
}

/// The `list_streams` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListStreamsToolResult {
    /// Every stream the runtime holds.
    pub streams: Vec<ListStreamsToolResultStream>,
}

/// One stream of a [`ListStreamsToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListStreamsToolResultStream {
    /// The stream's id; every tool naming the stream takes it.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives; `null` for a record an earlier runtime wrote.
    pub location: Option<StreamLocationOnTheWire>,
    /// Whether it is running, stopped or failed, in one word; `condition` answers its load,
    /// playback and health apart.
    pub state: ListedStreamState,
    /// Whether its graph is loaded, whether it plays and whether it is healthy, apart, with
    /// each diagnostic behind its health.
    pub condition: StreamConditionOnTheWire,
    /// How many nodes its loaded graph holds; `null` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `null` when it is not. `condition.diagnostics` types it.
    pub failed_because: Option<String>,
    /// Each capture device a node of the loaded stream refused because nobody has asked the user
    /// for its access — the runtime re-loaded it on its own at its start — once each; a
    /// `close_project` then `open_project` of its project asks. Empty when it is not loaded.
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
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ListedStreamState {
    /// Playing; or recorded playing, and re-loaded playing at the runtime's next start.
    Running,
    /// Open with its playback stopped, nothing running, until `start_playback`; its graph is
    /// loaded to read and edit while `node_count` is set.
    Stopped,
    /// Open with its playback paused: its nodes process nothing until `resume_playback`.
    Paused,
    /// Closed with its project, not loaded, until `open_project`.
    Closed,
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
            ListedStreamState::Closed => "closed",
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

/// The result of `open_project`, `reload_project`, `start_playback`, `stop_playback` and
/// `close_project`: the stream action each queued on the project's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectStreamActionQueuedToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The project whose queue holds it, symlinks resolved.
    pub project_directory: PathBuf,
}

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StreamActionKindOnTheWire {
    /// Opens a project, holding each of its streams loaded and stopped.
    Open,
    /// Re-loads graph documents of an open project, keeping its playback as it is.
    Load,
    /// Starts the playback of an open project's held streams.
    StartPlayback,
    /// Ends the playback of every stream of a project, holding each with its graph, and records
    /// each stopped.
    Stop,
    /// Ends a project's playback and unloads every stream of it, recording each closed.
    Close,
    /// Unloads every stream of a project and forgets every record of it.
    Remove,
    /// Unloads and forgets every stream of a project, then moves its folder to the Trash.
    Delete,
    /// Re-loads, at the runtime's start, a project's streams neither closed nor failed.
    ReloadAtStart,
    /// Makes a project's venv current with what it holds now, touching no stream.
    MakeVenvCurrent,
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
    /// What its latest `uv sync` of the project's `.venv` has done, from the moment uv starts,
    /// kept once the sync and the action end; `null` for an action that has not synced.
    pub venv_sync: Option<ProjectVenvSyncOnTheWire>,
    /// The streams it works on, once its project's graph documents named them.
    pub streams: Vec<DescribeStreamActionToolResultStream>,
    /// Why it failed; `null` unless it did.
    pub failed_because: Option<String>,
}

/// What one `uv sync` of a project's `.venv` has done so far, read from uv's own lines: uv
/// names a sync's downloads, builds and installs one package at a time and no byte of a
/// download, so a sync is followed by package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectVenvSyncOnTheWire {
    /// The phase uv's last line put it in.
    pub phase: ProjectVenvSyncPhaseOnTheWire,
    /// How many packages its resolution named, the project's own among them; `null` until it
    /// resolved.
    pub packages_resolved: Option<u64>,
    /// How many packages it downloaded or built before installing; `null` until uv prepared them.
    pub packages_prepared: Option<u64>,
    /// How many packages it installed; `null` until uv installed them.
    pub packages_installed: Option<u64>,
    /// How many of the downloads uv named have started; uv names only a download over its own
    /// size threshold, about a MiB, so a small one shows only in `packages_prepared`.
    pub downloads_started: usize,
    /// How many of the downloads uv named have ended.
    pub downloads_done: usize,
    /// Each package uv named by its download, build, install or removal, first named first.
    pub packages: Vec<ProjectVenvSyncPackageOnTheWire>,
}

/// The phase a project's `uv sync` is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProjectVenvSyncPhaseOnTheWire {
    /// Finding a Python and resolving the project's requirements.
    Resolving,
    /// Fetching a package or a Python.
    Downloading,
    /// Building a package from its source.
    Building,
    /// Every package is prepared and the venv is being changed.
    Installing,
    /// The venv holds every package the project requires.
    Installed,
}

/// One package a `uv sync` named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectVenvSyncPackageOnTheWire {
    /// The distribution's name, or the Python build's, as uv writes it.
    pub name: String,
    /// The version installed or removed; `null` until uv names it.
    pub version: Option<String>,
    /// A `distribution` or the `python_interpreter` the venv is made with.
    pub kind: ProjectVenvSyncPackageKindOnTheWire,
    /// Where it stands.
    pub state: ProjectVenvSyncPackageStateOnTheWire,
    /// The download's size as uv states it, to its one decimal of KiB, MiB or GiB; `null` for a
    /// package uv downloads without naming it or does not download.
    pub download_size_bytes: Option<u64>,
}

/// What a package a `uv sync` names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProjectVenvSyncPackageKindOnTheWire {
    /// A Python distribution the venv installs.
    Distribution,
    /// The Python interpreter the venv is made with, which uv fetches when the machine has none.
    PythonInterpreter,
}

/// Where one package a `uv sync` names stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProjectVenvSyncPackageStateOnTheWire {
    /// Its download has started.
    Downloading,
    /// Its download has ended.
    Downloaded,
    /// Its build from source has started.
    Building,
    /// Its build has ended.
    Built,
    /// The venv holds it.
    Installed,
    /// The venv no longer holds it.
    Uninstalled,
}

/// The resource `streamlib://stream-actions`: every stream action queued or under way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct StreamActionsUnderWayResourceContents {
    /// Each action queued or running, the earliest queued first, as `describe_stream_action`
    /// answers it.
    pub actions: Vec<DescribeStreamActionToolResult>,
}

/// The `cancel_stream_action` tool's result: the action's state once the cancel was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CancelStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// `cancelled` for an action dropped from its queue; `in_progress` for a running one, which
    /// ends `cancelled` once it stops at the step it is on.
    pub state: StreamActionStateOnTheWire,
}

/// One stream of a [`DescribeStreamActionToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DescribeStreamActionToolResultStream {
    /// The stream's id.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// `running` or `failed` once the action took it in; `null` before.
    pub state: Option<ListedStreamState>,
}

/// Which messages of an output port a viewer reads off the channel's reserved slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OutputPortViewerReads {
    /// The latest message the node wrote; the viewer's ring is one message deep, so the channel
    /// overwrites what the viewer falls behind on. Every family but Channels.
    LatestMessage,
    /// Every message, in order, as far back as the channel's depth holds them for a reader; the
    /// channel overwrites the oldest past that. A Channels port, so a viewer can play its sound.
    EveryMessage,
}

/// The `open_viewer` tool's result: what a viewer subscribes to, from its own process, to read
/// the messages an output port's node writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
    /// The iceoryx2 notify service the port's node notifies on each write, whose one listener the
    /// viewer takes, so it waits for a message rather than reading on a timer.
    pub wake_service: String,
    /// Which of the port's messages the viewer reads, by the port's family.
    pub viewer_reads: OutputPortViewerReads,
    /// Where the viewer imports an Image frame's surface: the surface-sharing socket's path on
    /// Linux, the surface-sharing Mach service's name on macOS. Absent on macOS when the open
    /// named no `viewer_process_id`, since the service admits only a named process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_share_address: Option<String>,
}

/// The `close_viewer` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CloseViewerToolResult {
    /// The viewer closed.
    pub viewer: String,
}

/// The `write_studio_view_output` tool's result: the output of the view node the value was handed
/// to, which published it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct WriteStudioViewOutputToolResult {
    /// The stream id.
    pub stream: String,
    /// The view node's UUID, as `graph` lists it under `id`.
    pub node: String,
    /// The output port the value was published on.
    pub port: String,
}

/// The `write_graph_input` tool's result: the Input of the stream's top-level `GraphInput` the
/// value was handed to, which published it to every reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct WriteGraphInputToolResult {
    /// The stream id.
    pub stream: String,
    /// The Input's name, as the `GraphInput`'s config declares it under `port`.
    pub input: String,
    /// The Input's family, as its wire name: `array`, `channels` or `records`.
    pub family: String,
}

/// The `set_stream_clock_scale` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SetStreamClockScaleToolResult {
    /// The stream's id.
    pub stream: String,
    /// Stream seconds per machine second from now on.
    pub scale: f64,
}
