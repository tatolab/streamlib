// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stream actions a runtime takes for its owner: run a project's stream assets, each
//! loaded from its graph document and recorded under the id its project and stream asset
//! give it — failed when its document does not read or its load is refused; stop and
//! remove a project; rename a stream; list what the runtime holds; and re-load the recorded streams
//! at start.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_consumer_rhi::runtime_unit_layout::sample_projects_directory_beside_the_lend;
use streamlib_project_repository::{
    PROJECT_FILE_NAME, ProjectStreamDocument, TatolabProjectFile,
    assets_a_nodes_input_values_reference, every_asset_uuid_the_project_holds,
    every_sample_project_in, graph_document_with_each_stream_fragment_reference_resolved,
    project_stream_documents, stream_assets_another_stream_of_the_project_places,
    the_sample_project_named,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticPlaceOnTheWire, ListSamplesToolResult,
    ListSamplesToolResultSample, StreamConditionOnTheWire, StreamLoadOnTheWire,
    StreamPlaybackOnTheWire,
};
use uuid::Uuid;

use crate::core::runtime::projects_and_their_venv::new_project_name_drawing::draw_new_project_directory_under_the_streams_root;
use crate::core::runtime::projects_and_their_venv::new_project_scaffold::{NewProjectScaffold, write_the_new_project_scaffold_into};
use crate::core::runtime::projects_and_their_venv::project_first_party_packages_moved_to_the_runtime_version::move_the_projects_first_party_packages_to_the_runtime_version;
use crate::core::runtime::projects_and_their_venv::project_folder_moved_to_the_trash::move_the_project_folder_to_the_trash;
use crate::core::runtime::projects_and_their_venv::project_node_code_a_load_describes::ProjectNodeCodeContentHash;
use crate::core::runtime::streams_loaded_in_this_runtime::project_stream_action_queue::{
    StreamAction, StreamActionCancelOutcome, StreamActionKind, StreamActionProgress,
    StreamActionQueueOfEachProject, StreamActionQueued, StreamActionState, StreamActionStep,
    StreamActionVenvSyncReport, StreamActionWorkingOnAStream, StreamOfAStreamAction,
    a_stream_action_not_held,
};
use crate::core::runtime::projects_and_their_venv::project_venv_made_with_the_runtime_unit_uv::{
    PackagesTheRuntimeUnitCarries, ProjectVenvNotMade,
    lock_the_projects_python_requirements_with_the_runtime_unit_uv,
    make_the_projects_venv_current_with_the_runtime_unit_uv, the_projects_venv_interpreter,
};
use crate::core::runtime::runtime::{
    a_stream_id_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
    what_interrupted_a_load,
};
use crate::core::runtime::{
    CrashOfThePreviousRuntimeRun, HowThePreviousRuntimeRunEnded, LoadedStreamInThisRuntime,
    LoadedStreamTag, OptionsForLoadingOneStream, PendingStreamActionKind, Runner, RuntimeStatus,
    StreamEnvironment, StreamFailureKind, StreamLoadObservingInterrupts, StreamLocation, StreamRecord,
    StreamRecordAsRead, StreamRecordReadFailure, StreamRecordsInTheStateDirectory,
    is_the_machines_shutdown_requested,
};
use crate::core::compiler::compiler_ops::cross_floor_check_in_the_projects_interpreter::warn_of_what_binds_the_project_to_one_floor;
use crate::core::context::{CaptureDeviceAccessPrompting, StreamClockReading};
use crate::core::error::PrivacyGatedCaptureDevice;
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, refuse_an_integer_literal_wider_than_64_bits,
};
use crate::core::graph::names_listed_for_a_refusal;
use crate::core::logging::{ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread};
use crate::core::{Error, Result};
use crate::core::runtime::streams_loaded_in_this_runtime::stream_condition::{
    StreamFailure, the_condition_of_a_stream, the_condition_while_an_action_works_on_it,
};
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;

/// How a loaded stream is held: recorded by the runtime, or living as long as
/// the process that hosts its library [`Runner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadedStreamHolding {
    /// A project's load, recorded in the runtime's state directory and
    /// re-loaded at its start until it is stopped, failed or removed.
    RecordedByTheRuntime,
    /// A stream a library host loaded — the Rust `App`, or a [`Runner`] in a
    /// test — never recorded, and gone with its host process.
    LivesWithTheHostProcess,
}

impl std::fmt::Display for LoadedStreamHolding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RecordedByTheRuntime => "recorded by the runtime",
            Self::LivesWithTheHostProcess => "living with its host process",
        })
    }
}

/// The project [`Runner::open_project`] or [`Runner::reload_project`] queues a load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOpenOrReloadRequest {
    /// The project whose every stream asset is loaded, by absolute path.
    pub project_directory: PathBuf,
}

/// The project [`Runner::create_project`] creates and queues a load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectRequest {
    /// Where the project is created, by absolute path; `None` draws an
    /// `<adjective>-<noun>` folder under `$HOME/streams`.
    pub project_directory: Option<PathBuf>,
    /// The sample project the runtime unit carries to copy, by name; `None` renders the starter.
    pub sample: Option<String>,
}

/// The load [`Runner::create_project`] queued of the project it created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCreationQueued {
    /// The queued action's id, which [`Runner::describe_stream_action`] follows.
    pub action_id: String,
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of the stream the new project's `streams/main.graph.json` loads as.
    pub stream_id: String,
}

/// What a stream action's load of a project reads and loads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectStreamsToReadAndLoad {
    /// The project, by absolute path.
    project_directory: PathBuf,
    /// The stream assets the load reads and loads; `None` reads every one the project holds.
    only_the_stream_assets: Option<BTreeSet<Uuid>>,
    /// Whether each stream it loads starts playing or is held stopped.
    playback_after_the_load: ProjectPlaybackAfterItsLoad,
    /// Whether a stream it finds loaded is kept and its live graph reconciled to its graph
    /// document while the project's node code is as it was at the stream's load, rather than
    /// replaced.
    keeps_each_loaded_stream_whose_node_code_is_unchanged: bool,
}

/// Whether the streams a load of a project loads start playing or are held stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectPlaybackAfterItsLoad {
    /// Each stream is held loaded and stopped, its graph there to read and edit, nothing
    /// running and nothing claimed.
    HeldStopped,
    /// Each stream starts, paused while its project is.
    Playing,
}

/// A project as a load reads it: where it sits, its project file, and each stream asset's
/// graph document, read as it is on disk.
struct ProjectAsALoadReadsIt {
    /// The project's directory, symlinks resolved.
    project_directory: PathBuf,
    /// What its `tatolab-project.json` holds.
    project_file: TatolabProjectFile,
    /// The graph document of each stream asset that runs as its own stream, sorted by path:
    /// every one no other stream of the project places.
    stream_documents: Vec<ProjectStreamDocument>,
    /// Each stream asset another stream of the project places, which runs only inside it.
    stream_assets_another_stream_places: BTreeSet<Uuid>,
    /// Every asset the project holds, by UUID, which a node's config may reference.
    asset_uuids_the_project_holds: BTreeSet<Uuid>,
}

impl ProjectAsALoadReadsIt {
    /// Read the project at `project_directory`, refused for a directory holding no project
    /// file or one that does not read.
    fn read(project_directory: &Path) -> Result<Self> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let refused = |refusal: streamlib_project_repository::ProjectRepositoryRefusal| {
            Error::Configuration(format!(
                "the project in {} was not loaded: {refusal}",
                project_directory.display()
            ))
        };
        let project_file =
            TatolabProjectFile::read_from_project_directory(&project_directory).map_err(refused)?;
        let mut stream_documents = project_stream_documents(&project_directory).map_err(refused)?;
        let stream_assets_another_stream_places =
            stream_assets_another_stream_of_the_project_places(&stream_documents);
        stream_documents.retain(|stream_document| {
            !stream_assets_another_stream_places
                .contains(&stream_document.stream_asset(project_file.project))
        });
        let asset_uuids_the_project_holds =
            every_asset_uuid_the_project_holds(&project_directory, project_file.project)
                .map_err(refused)?;
        Ok(Self {
            project_directory,
            project_file,
            stream_documents,
            stream_assets_another_stream_places,
            asset_uuids_the_project_holds,
        })
    }

    /// Where the stream `stream_document` holds lives.
    fn location_of(&self, stream_document: &ProjectStreamDocument) -> StreamLocation {
        StreamLocation {
            project_directory: self.project_directory.clone(),
            project: self.project_file.project,
            stream_asset: stream_document.stream_asset(self.project_file.project),
        }
    }

    /// The stream asset whose document is `stream_document`, read: its location, name and
    /// graph, or why the document gives none.
    /// The UUID of each stream asset whose graph document the project holds.
    fn stream_assets(&self) -> Vec<Uuid> {
        self.stream_documents
            .iter()
            .map(|stream_document| stream_document.stream_asset(self.project_file.project))
            .collect()
    }

    fn stream_asset_read_from(&self, stream_document: &ProjectStreamDocument) -> StreamAssetRead {
        StreamAssetRead {
            stream_location: self.location_of(stream_document),
            stream_name: stream_document.stream_name().to_string(),
            display_name: stream_document.display_name(),
            graph_or_why_none: the_graph_a_stream_document_holds(
                &self.project_directory,
                stream_document,
                &self.asset_uuids_the_project_holds,
            ),
        }
    }
}

/// One stream asset a load read: where it lives, its name, and the graph its document holds
/// or why it holds none.
struct StreamAssetRead {
    stream_location: StreamLocation,
    stream_name: String,
    display_name: String,
    graph_or_why_none: std::result::Result<GraphSnapshot, String>,
}

/// The graph `stream_document` holds, or why it holds none: it does not read, it names no
/// stream asset, or it does not load as a graph.
fn the_graph_a_stream_document_holds(
    project_directory: &Path,
    stream_document: &ProjectStreamDocument,
    asset_uuids_the_project_holds: &BTreeSet<Uuid>,
) -> std::result::Result<GraphSnapshot, String> {
    let why_it_holds_no_graph =
        |why: String| format!("its graph document `{}` {why}", stream_document.path);
    let bytes = stream_document
        .bytes
        .as_ref()
        .map_err(|not_read| why_it_holds_no_graph(format!("does not read: {not_read}")))?;
    stream_document
        .asset_written_in_it()
        .map_err(|why| why_it_holds_no_graph(format!("names no stream asset: {why}")))?;
    let document_text = std::str::from_utf8(bytes)
        .map_err(|not_utf8| why_it_holds_no_graph(format!("is not UTF-8: {not_utf8}")))?;
    refuse_an_integer_literal_wider_than_64_bits(document_text)
        .map_err(|too_wide| why_it_holds_no_graph(format!("does not load: {too_wide}")))?;
    let document: serde_json::Value = serde_json::from_str(document_text)
        .map_err(|not_parsed| why_it_holds_no_graph(format!("does not parse: {not_parsed}")))?;
    let document =
        graph_document_with_each_stream_fragment_reference_resolved(project_directory, &document)
            .map_err(|not_resolved| {
            why_it_holds_no_graph(format!(
                "holds a stream fragment that does not load: {not_resolved}"
            ))
        })?;
    let graph = GraphSnapshot::from_graph_document(document)
        .map_err(|not_a_graph| why_it_holds_no_graph(format!("does not load: {not_a_graph}")))?;
    let nodes_at_every_level = graph.nodes_at_every_level();
    let nodes_holding_no_uuid: Vec<String> = nodes_at_every_level
        .iter()
        .filter(|(_, node)| node.id.is_none())
        .map(|(node_path, _)| format!("`{node_path}`"))
        .collect();
    if !nodes_holding_no_uuid.is_empty() {
        return Err(why_it_holds_no_graph(format!(
            "holds no UUID as the `id` of {}; the runtime writes every node's UUID, so a node \
             without one was written outside the runtime",
            nodes_holding_no_uuid.join(", ")
        )));
    }
    for (node_path, node) in &nodes_at_every_level {
        if let Some(asset) = assets_a_nodes_input_values_reference(&node.input_values)
            .into_iter()
            .find(|asset| !asset_uuids_the_project_holds.contains(asset))
        {
            return Err(why_it_holds_no_graph(format!(
                "has node `{node_path}` whose input values reference the asset {asset}, which \
                 the project does not hold"
            )));
        }
    }
    Ok(graph)
}

/// Every UUID the graph documents of the project in `project_directory` name, each placed
/// stream's held nodes among them under the UUIDs their placements give them, a superset of the
/// node UUIDs they hold; `None` when a document does not read or resolve, since a node it holds
/// is unknown.
fn every_node_uuid_the_graph_documents_of_the_project_hold(
    project_directory: &Path,
) -> Option<std::collections::HashSet<Uuid>> {
    fn every_uuid_named_in(json: &serde_json::Value, uuids: &mut std::collections::HashSet<Uuid>) {
        match json {
            serde_json::Value::String(text) => {
                if let Ok(uuid) = text.parse::<Uuid>() {
                    uuids.insert(uuid);
                }
            }
            serde_json::Value::Array(items) => items
                .iter()
                .for_each(|item| every_uuid_named_in(item, uuids)),
            serde_json::Value::Object(fields) => fields
                .values()
                .for_each(|field| every_uuid_named_in(field, uuids)),
            _ => {}
        }
    }
    let mut uuids = std::collections::HashSet::new();
    for stream_document in project_stream_documents(project_directory).ok()? {
        let bytes = stream_document.bytes.ok()?;
        let document: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let with_each_placed_stream_resolved =
            streamlib_project_repository::graph_document_with_each_stream_fragment_reference_resolved(
                project_directory,
                &document,
            )
            .ok()?;
        every_uuid_named_in(&with_each_placed_stream_resolved, &mut uuids);
    }
    Some(uuids)
}

/// The project file of the stream at `location` and the graph document holding its stream
/// asset, or why its project or document is not where it was recorded.
fn the_graph_document_of_the_stream_at(
    location: &StreamLocation,
) -> std::result::Result<(TatolabProjectFile, ProjectStreamDocument), String> {
    let project_directory = &location.project_directory;
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)
        .map_err(|refusal| format!("its project is not where it was recorded: {refusal}"))?;
    if project_file.project != location.project {
        return Err(format!(
            "{} holds the project {} now, not the project {} it was recorded from",
            project_directory.display(),
            project_file.project,
            location.project
        ));
    }
    let stream_document = project_stream_documents(project_directory)
        .map_err(|refusal| format!("its project's graph documents do not read: {refusal}"))?
        .into_iter()
        .find(|stream_document| {
            stream_document.stream_asset(project_file.project) == location.stream_asset
        })
        .ok_or_else(|| {
            format!(
                "no graph document in {} holds its stream asset {} any more",
                project_directory.display(),
                location.stream_asset
            )
        })?;
    Ok((project_file, stream_document))
}

/// The name the stream at `location` is shown under, read from its graph document; its
/// `stream_id` while the document does not read.
fn the_display_name_of_the_stream_at(location: &StreamLocation, stream_id: &str) -> String {
    the_graph_document_of_the_stream_at(location).map_or_else(
        |_| stream_id.to_owned(),
        |(_, stream_document)| stream_document.display_name(),
    )
}

/// What a load of a project did: each stream it loaded and started, or
/// recorded failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamRunOutcome {
    /// The project the run was given.
    pub project_directory: PathBuf,
    /// Each stream the run loaded or recorded failed.
    pub streams: Vec<StreamLoadedOrFailedByARun>,
}

/// One stream a load of a project loaded, started or held stopped, or recorded failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamLoadedOrFailedByARun {
    /// The stream's id, computed from its location.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives.
    pub stream_location: StreamLocation,
    /// Whether it runs, or why it is failed.
    pub state: StreamStateAfterItsRun,
    /// Whether the run replaced the recorded stream of the same location.
    pub replaced_a_recorded_stream: bool,
}

impl StreamLoadedOrFailedByARun {
    /// The stream `record` records, in `state`.
    fn of_the_record(
        record: StreamRecord,
        display_name: String,
        state: StreamStateAfterItsRun,
        replaced_a_recorded_stream: bool,
    ) -> Self {
        Self {
            stream_id: record.stream_id,
            display_name,
            stream_location: record.location,
            state,
            replaced_a_recorded_stream,
        }
    }
}

/// Whether a stream a load of a project took in runs, or why it is failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamStateAfterItsRun {
    /// Loaded and started.
    Running {
        /// The tag of this load of it, which its `logs` pages name.
        stream_tag: LoadedStreamTag,
        /// How many nodes the loaded graph holds.
        node_count: usize,
    },
    /// Loaded and held stopped, its graph there to read and edit.
    HeldStopped {
        /// The tag of this load of it, which its `logs` pages name.
        stream_tag: LoadedStreamTag,
        /// How many nodes the loaded graph holds.
        node_count: usize,
    },
    /// Recorded failed: its code gave no graph, or its load or start was refused, and it is
    /// not loaded; or its node code did not describe a type its graph names, and it is held
    /// stopped from its document, that type's nodes held as placeholders.
    Failed {
        /// Why — a traceback, or the refusal.
        failed_because: String,
    },
}

/// A stream [`Runner::stop_playback`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamStopOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Why a recorded stream unloaded here was not recorded stopped, so a
    /// restart of the runtime loads it again; `None` when it was recorded,
    /// and for a stream its host process loaded.
    pub stop_not_recorded_because: Option<String>,
}

/// The project [`Runner::pause_playback`] paused or [`Runner::resume_playback`] resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPausedOrResumed {
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each loaded stream of the project paused or resumed now.
    pub stream_ids: Vec<String>,
}

/// The state one stream of [`Runner::list_streams`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamListingState {
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the
    /// runtime's next start.
    Running,
    /// Its project is open and its playback stopped: its graph held to read and edit when it
    /// is loaded, nothing running, until `start_playback`.
    Stopped,
    /// Its project is closed: not loaded, and left unloaded across restarts until
    /// `open_project`.
    Closed,
    Paused,
    /// Its code, its venv, its load or its start failed: held stopped from its document when
    /// that reads, its graph there to read and edit, until a play or a `reload_project` retries
    /// it; or it kept crashing the runtime, and is held at a start with none of its node code
    /// read; or it was recorded by an earlier runtime, and is never loaded.
    Failed,
    /// A stream action queued or running loads it.
    Preparing,
    /// A stream action queued or running unloads it.
    Stopping,
}

impl StreamListingState {
    /// The state as a listing spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Closed => "closed",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Preparing => "preparing",
            Self::Stopping => "stopping",
        }
    }
}

impl std::fmt::Display for StreamListingState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One stream the runtime holds, as [`Runner::list_streams`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamListing {
    /// The stream's id.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// The project and stream asset the stream is; `None` for a record an earlier runtime
    /// wrote.
    pub stream_location: Option<StreamLocation>,
    /// Whether it is running, stopped or failed.
    pub state: StreamListingState,
    /// How many nodes its loaded graph holds; `None` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `None` when it is not.
    pub failed_because: Option<String>,
    /// Its load, playback and health apart, with each diagnostic behind its health.
    pub condition: StreamConditionOnTheWire,
    /// Each capture device a node of the loaded stream refused because nobody
    /// had asked the user for its access; a stop and run of its project asks. Empty
    /// when it is not loaded.
    pub capture_devices_whose_access_was_never_asked_for: Vec<PrivacyGatedCaptureDevice>,
    /// The stream action working on it while one is queued or running.
    pub stream_action_under_way: Option<StreamActionWorkingOnAStream>,
}

/// How many of the runtime's crashes in a row a recorded stream is implicated
/// in before it is failed.
pub const RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM: u32 = 2;

/// What the re-load of a project's records at the start
/// did with one record; a stream it left unloaded because it is closed,
/// removed or already loaded is not reported.
#[derive(Debug)]
pub enum StreamReloadAtTheStart {
    /// The recorded stream loaded and started.
    Reloaded {
        /// The stream's id.
        stream_id: String,
    },
    /// The recorded stream, its project open and its playback stopped, loaded and held
    /// stopped; recorded failed when its node code did not describe a type its graph names.
    HeldStopped {
        /// The stream's id.
        stream_id: String,
    },
    /// The recorded stream is failed, so it was skipped.
    SkippedAsFailed {
        /// The stream's id.
        stream_id: String,
        /// Why it is failed.
        failed_because: String,
    },
    /// The recorded stream did not re-load, and is recorded failed for it —
    /// unless the refusal was the engine's own, or a machine shutdown
    /// abandoned the re-load, when it is re-loaded at the next start.
    NotReloaded {
        /// The stream's id.
        stream_id: String,
        /// Why it did not re-load.
        refusal: Error,
    },
    /// A record that could not be read, left in place.
    RecordUnreadable(StreamRecordReadFailure),
}

/// Where a runtime records its streams, and the lock that lets one stream
/// action run at a time.
#[derive(Default)]
pub(crate) struct StreamActionsOfTheEngine {
    stream_records: OnceLock<Arc<StreamRecordsInTheStateDirectory>>,
    /// Held across an action's check, load and record, and across the re-load
    /// at the runtime's start of one record, its venv made included — never
    /// across a load's venv or compile.
    one_stream_action_at_a_time: Mutex<()>,
    /// Each project's serial queue of stream actions, and the actions held.
    pub(in crate::core::runtime) stream_action_queue_of_each_project:
        StreamActionQueueOfEachProject,
    /// The directory of each project `pause_playback` paused, held across the start of each
    /// stream a stream action starts, so a stream of a paused project starts paused. Each
    /// record of a paused project is recorded paused, and the runtime's start pauses its
    /// project again.
    directories_of_the_paused_projects: Mutex<Vec<PathBuf>>,
    /// The node code each stream a project's load loaded was described from, by stream id.
    node_code_each_loaded_stream_was_described_from:
        Mutex<HashMap<String, ProjectNodeCodeContentHash>>,
}

/// One stream a stream action loads, recorded by the runtime under its id: its display name
/// and location, the graph its document holds, where its interpreters start, and whether its
/// capture devices may ask the user.
#[derive(Clone)]
struct OneStreamAStreamActionLoads<'load> {
    stream_id: &'load str,
    display_name: &'load str,
    stream_location: &'load StreamLocation,
    graph: &'load GraphSnapshot,
    stream_environment: StreamEnvironment,
    capture_device_access_prompting: CaptureDeviceAccessPrompting,
    /// Why none of the stream's node code is read, every node held as written.
    why_none_of_its_node_code_is_read: Option<String>,
    /// Abandons the load once it reports true.
    is_interrupted: &'load dyn Fn() -> bool,
}

impl<'load> OneStreamAStreamActionLoads<'load> {
    /// The stream `record` records, shown as `display_name`, with the graph `graph` its
    /// document holds, its load abandoned by a machine shutdown request.
    fn recorded_by(
        record: &'load StreamRecord,
        display_name: &'load str,
        graph: &'load GraphSnapshot,
    ) -> Self {
        Self {
            stream_id: &record.stream_id,
            display_name,
            stream_location: &record.location,
            graph,
            stream_environment: record.stream_environment(),
            capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
            why_none_of_its_node_code_is_read: None,
            is_interrupted: &is_the_machines_shutdown_requested,
        }
    }

    /// The same load, reading none of the stream's node code for `why_none_of_it_is_read`.
    fn reading_none_of_its_node_code(self, why_none_of_it_is_read: Option<String>) -> Self {
        Self {
            why_none_of_its_node_code_is_read: why_none_of_it_is_read,
            ..self
        }
    }

    /// The same load, its capture devices asking the user as
    /// `capture_device_access_prompting` says.
    fn with_capture_device_access_prompting(
        self,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
    ) -> Self {
        Self {
            capture_device_access_prompting,
            ..self
        }
    }

    /// The same load, abandoned once `is_interrupted` reports true.
    fn abandoned_once(self, is_interrupted: &'load dyn Fn() -> bool) -> Self {
        Self {
            is_interrupted,
            ..self
        }
    }
}

/// Why `project_directory` is no project a load takes in — it does not
/// exist, or holds no `tatolab-project.json` — or `None` when it is one.
fn why_a_directory_is_no_project(project_directory: &Path) -> Option<String> {
    if !project_directory.is_dir() {
        return Some(format!(
            "the project directory {} does not exist",
            project_directory.display()
        ));
    }
    if !project_directory.join(PROJECT_FILE_NAME).is_file() {
        return Some(format!(
            "{} is not a project: it holds no {}",
            project_directory.display(),
            PROJECT_FILE_NAME
        ));
    }
    None
}

/// Whether the project at `recorded_location`'s directory is another copy of the project at
/// `project_directory`: a directory besides it whose project file names the same project.
fn another_copy_of_the_project_holds(
    recorded_location: &StreamLocation,
    project_directory: &Path,
) -> bool {
    !the_same_directory(&recorded_location.project_directory, project_directory)
        && TatolabProjectFile::read_from_project_directory(&recorded_location.project_directory)
            .is_ok_and(|project_file| project_file.project == recorded_location.project)
}

/// `project_directory` with its symlinks resolved, refused when it is not absolute.
fn the_absolute_project_directory_resolved(project_directory: &Path) -> Result<PathBuf> {
    if !project_directory.is_absolute() {
        return Err(Error::Configuration(format!(
            "`project_directory` must be an absolute path; got `{}`",
            project_directory.display()
        )));
    }
    Ok(
        std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf()),
    )
}

/// Whether a stream in `status` is paused or pausing.
fn is_paused(status: RuntimeStatus) -> bool {
    matches!(status, RuntimeStatus::Paused | RuntimeStatus::Pausing)
}

/// Whether `first` and `second` name one directory, symlinks resolved; spelled
/// alike when either cannot be resolved.
fn the_same_directory(first: &Path, second: &Path) -> bool {
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => first == second,
    }
}

/// Record the stream `stream_record` records failed as `kind`, with `failed_because`; a record
/// not written is logged.
fn record_a_stream_held_from_its_document_failed(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_record: &mut StreamRecord,
    kind: StreamFailureKind,
    failed_because: String,
) -> Result<StreamStateAfterItsRun> {
    stream_record.fail_as(kind, failed_because.clone());
    if let Err(not_written) = stream_records
        .write_the_record_a_load_made(stream_record.clone())
        .map(|written| *stream_record = written)
    {
        tracing::error!(
            "the stream `{}` did not load, and was not recorded failed: {not_written}",
            stream_record.stream_id
        );
    }
    Ok(StreamStateAfterItsRun::Failed { failed_because })
}

/// Refuse a stop of `stream_id`, whose record is `record`, once a removal of
/// its project is queued.
fn refuse_a_stream_whose_project_removal_is_queued(
    stream_id: &str,
    record: &StreamRecord,
) -> Result<()> {
    if record
        .pending_stream_actions
        .iter()
        .any(|pending| pending.kind == PendingStreamActionKind::Remove)
    {
        return Err(Error::Runtime(format!(
            "the stream `{stream_id}` is being removed with its project, so it is not \
             stopped"
        )));
    }
    Ok(())
}

fn hold_a_stream_whose_start_was_refused_stopped(stream: &Arc<LoadedStreamInThisRuntime>) {
    if is_the_machines_shutdown_requested() {
        unload_a_stream_an_action_took_back(stream, "its start was refused");
        return;
    }
    if let Err(not_held) = stream.stop_playback_keeping_the_graph() {
        stream.log_route().run_entered(|| {
            tracing::warn!(
                "the stream `{}` was not held stopped after its refused start: {not_held}",
                stream.stream_id()
            )
        });
        unload_a_stream_an_action_took_back(stream, "its start was refused");
    }
}

fn unload_a_stream_an_action_took_back(stream: &Arc<LoadedStreamInThisRuntime>, reason: &str) {
    request_a_streams_shutdown_and_wait_until_it_has_ended(stream, reason);
    if let Err(failing_end) = stream.how_this_stream_ended_as_a_waiter_reports_it() {
        stream.log_route().run_entered(|| {
            tracing::warn!(
                "the stream `{}` was unloaded because {reason}, and its end reported: \
                 {failing_end}",
                stream.stream_id()
            )
        });
    }
}

/// Refuse the cancel of the running stream action `running` when it unloads
/// its streams: a stop, close or removal that has begun cannot be cut short.
fn refuse_the_cancel_of_a_running_unload(running: &StreamAction) -> Result<()> {
    if running.kind.unloads_its_streams() {
        return Err(Error::Configuration(format!(
            "the stream action `{}` is a running {}, which has begun unloading its streams and \
             cannot be cut short; it ends done",
            running.action_id,
            running.kind.as_str()
        )));
    }
    Ok(())
}

mod playback_actions;
mod project_lifecycle_actions;
mod project_removal_actions;
mod project_runs;
mod project_venv_actions;
mod recorded_stream_reload;
mod stream_action_queue;
mod stream_records;
mod streams_to_read_or_edit;

pub use streams_to_read_or_edit::StreamsToReadOrEdit;

#[cfg(test)]
mod tests;
