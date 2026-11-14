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
    PROJECT_FILE_NAME, ProjectStreamDocument, TatolabProjectFile, assets_a_node_config_references,
    every_asset_uuid_the_project_holds, every_sample_project_in,
    graph_document_with_each_stream_fragment_reference_resolved, project_stream_documents,
    stream_assets_another_stream_of_the_project_places, the_sample_project_named,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListSamplesToolResult, ListSamplesToolResultSample,
};
use uuid::Uuid;

use super::new_project_name_drawing::draw_new_project_directory_under_the_streams_root;
use super::new_project_scaffold::{NewProjectScaffold, write_the_new_project_scaffold_into};
use super::project_first_party_packages_moved_to_the_runtime_version::move_the_projects_first_party_packages_to_the_runtime_version;
use super::project_folder_moved_to_the_trash::move_the_project_folder_to_the_trash;
use super::project_node_code_a_load_describes::ProjectNodeCodeContentHash;
use super::project_stream_action_queue::{
    StreamAction, StreamActionCancelOutcome, StreamActionKind, StreamActionProgress,
    StreamActionQueueOfEachProject, StreamActionQueued, StreamActionState, StreamActionStep,
    StreamActionVenvSyncReport, StreamActionWorkingOnAStream, StreamOfAStreamAction,
    a_stream_action_not_held,
};
use super::project_venv_made_with_the_runtime_unit_uv::{
    PackagesTheRuntimeUnitCarries, ProjectVenvNotMade,
    lock_the_projects_python_requirements_with_the_runtime_unit_uv,
    make_the_projects_venv_current_with_the_runtime_unit_uv, the_projects_venv_interpreter,
};
use super::runtime::{
    a_stream_id_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
    what_interrupted_a_load,
};
use super::{
    CrashOfThePreviousRuntimeRun, HowThePreviousRuntimeRunEnded, LoadedStreamInThisRuntime,
    LoadedStreamTag, OptionsForLoadingOneStream, PendingStreamActionKind, Runner, RuntimeStatus,
    StreamEnvironment, StreamLoadObservingInterrupts, StreamLocation, StreamRecord,
    StreamRecordAsRead, StreamRecordReadFailure, StreamRecordsInTheStateDirectory,
    is_the_machines_shutdown_requested,
};
use crate::core::compiler::compiler_ops::cross_floor_check_in_the_projects_interpreter::warn_of_what_binds_the_project_to_one_floor;
use crate::core::context::{CaptureDeviceAccessPrompting, StreamClockReading};
use crate::core::error::PrivacyGatedCaptureDevice;
use crate::core::graph::names_listed_for_a_refusal;
use crate::core::graph_snapshot::{GraphSnapshot, refuse_an_integer_literal_wider_than_64_bits};
use crate::core::logging::{ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread};
use crate::core::{Error, Result};

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

/// Refuse a project two of whose stream graph documents hold a node of one UUID, naming both
/// documents, since a node's UUID keys the values it keeps in the project's store.
fn refuse_streams_whose_graph_documents_share_a_node_uuid<'read>(
    project_directory: &Path,
    each_stream_document_and_its_graph: impl IntoIterator<
        Item = (
            &'read ProjectStreamDocument,
            &'read std::result::Result<GraphSnapshot, String>,
        ),
    >,
) -> Result<()> {
    let mut document_and_node_path_of_each_node_uuid: HashMap<Uuid, (String, String)> =
        HashMap::new();
    for (stream_document, graph_or_why_none) in each_stream_document_and_its_graph {
        let Ok(graph) = graph_or_why_none else {
            continue;
        };
        let document_path = stream_document.path.to_string();
        for (node_path, node) in graph.nodes_at_every_level() {
            let Some(node_uuid) = node.id else {
                continue;
            };
            match document_and_node_path_of_each_node_uuid.get(&node_uuid) {
                Some((first_document_path, first_node_path))
                    if *first_document_path != document_path =>
                {
                    return Err(Error::Configuration(format!(
                        "the project in {} was not loaded: its graph documents \
                         `{first_document_path}` and `{document_path}` both hold the node \
                         {node_uuid} (`{first_node_path}` and `{node_path}`), and a node's UUID \
                         keys the values it keeps, so each stream's nodes need UUIDs of their \
                         own; a copied graph document keeps the UUIDs of the one it was copied \
                         from",
                        project_directory.display(),
                    )));
                }
                Some(_) => {}
                None => {
                    document_and_node_path_of_each_node_uuid
                        .insert(node_uuid, (document_path.clone(), node_path));
                }
            }
        }
    }
    Ok(())
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
        if let Some(asset) = assets_a_node_config_references(&node.config)
            .into_iter()
            .find(|asset| !asset_uuids_the_project_holds.contains(asset))
        {
            return Err(why_it_holds_no_graph(format!(
                "has node `{node_path}` whose config references the asset {asset}, which the \
                 project does not hold"
            )));
        }
    }
    Ok(graph)
}

/// The graph the document of `record`'s stream asset holds as it is on disk now, or why
/// there is none: the project is gone from where it was recorded, or no document holds the
/// stream asset, or the document holds no graph.
fn the_graph_the_recorded_stream_holds(
    record: &StreamRecord,
) -> std::result::Result<GraphSnapshot, String> {
    let (project_file, stream_document) = the_graph_document_of_the_stream_at(&record.location)?;
    let project_directory = &record.location.project_directory;
    let asset_uuids_the_project_holds =
        every_asset_uuid_the_project_holds(project_directory, project_file.project)
            .map_err(|refusal| format!("its project's assets do not read: {refusal}"))?;
    the_graph_a_stream_document_holds(
        project_directory,
        &stream_document,
        &asset_uuids_the_project_holds,
    )
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
    /// Loaded with its project paused: its processors hold their state and process nothing
    /// until `resume_playback`.
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
    pub(super) stream_action_queue_of_each_project: StreamActionQueueOfEachProject,
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

impl Runner {
    /// Record this runtime's streams in `streams_directory`, created
    /// owner-only when absent. Refused a second time; a runtime never given
    /// one records no stream, and so runs none.
    pub fn record_streams_in_the_state_directory(&self, streams_directory: &Path) -> Result<()> {
        if let Some(stream_records) = self.stream_records() {
            return Err(Error::Configuration(format!(
                "this runtime already records its streams in {}; it records them in one \
                 directory for its life",
                stream_records.streams_directory().display()
            )));
        }
        let stream_records = Arc::new(StreamRecordsInTheStateDirectory::open(streams_directory)?);
        self.stream_actions
            .stream_records
            .set(stream_records)
            .map_err(|_| {
                Error::Configuration(format!(
                    "this runtime was handed a streams directory twice at once; it records its \
                     streams in {} only",
                    streams_directory.display()
                ))
            })?;
        Ok(())
    }

    /// Read the request's project — its project file and each stream asset's graph document
    /// — make its `.venv` unless its interpreter runs, then record, load and start each stream
    /// asset under the id its project and stream asset give it.
    ///
    /// A load runs no project code beyond describing its node types, and is never refused for
    /// what a document or the project's environment holds: a document that does not read, or
    /// whose load is refused, records its stream failed with the reason. A stream whose start
    /// is refused, whose project's `.venv` cannot be made, or whose node code does not describe
    /// a type its graph names is recorded failed and held stopped from its document. A record
    /// of the project whose stream asset no
    /// document holds any more is unloaded and forgotten.
    ///
    /// A load of a stream already recorded replaces it under the same id, keeping its display
    /// name: the running stream is unloaded once the new record is written, and never put back.
    /// Refused, with nothing changed, for a directory holding no project, an id another stream
    /// holds, a second copy of a project another directory holds, and on a runtime given no
    /// state directory.
    ///
    /// Run for the stream action `progress` reports when one runs it: the
    /// project's log entered with the action's id, each step reported, and a
    /// refusal written to the project's log.
    fn run_the_project_reporting_to(
        &self,
        request: ProjectStreamsToReadAndLoad,
        progress: Option<&StreamActionProgress<'_>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamRunOutcome> {
        let _in_the_projects_log = self.enter_the_log_of_the_project_a_load_takes_in(
            &request.project_directory,
            progress.map(|progress| progress.action_id.as_str()),
        );
        let run_outcome = self.run_the_project_in_its_log(request, progress, is_interrupted);
        match (&run_outcome, &_in_the_projects_log) {
            (Ok(run_outcome), Some(_)) => self.log_each_stream_a_run_failed_in_its_projects_log(
                run_outcome,
                progress.map(|progress| progress.action_id.as_str()),
            ),
            (Err(refusal), _) => {
                if let Some(progress) = progress
                    && !progress.saw_its_cancel()
                {
                    tracing::warn!("the stream action {} failed: {refusal}", progress.action_id);
                }
            }
            (Ok(_), None) => {}
        }
        if let Some(progress) = progress
            && progress.saw_its_cancel()
        {
            tracing::info!(
                "the stream action {} was cancelled at the step it was on; the streams it had not \
                 reached stay as recorded",
                progress.action_id
            );
        }
        run_outcome
    }

    /// Write why each stream `run_outcome` failed to its project's log, stamped
    /// with the stream and the action that ran it.
    fn log_each_stream_a_run_failed_in_its_projects_log(
        &self,
        run_outcome: &StreamRunOutcome,
        action_id: Option<&str>,
    ) {
        for stream in &run_outcome.streams {
            let StreamStateAfterItsRun::Failed { failed_because } = &stream.state else {
                continue;
            };
            ProjectLogRouteEntry::for_a_stream_of_the_project_in(
                &stream.stream_location.project_directory,
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
                &stream.stream_id,
                action_id,
            )
            .run_entered(|| {
                tracing::error!("the stream `{}` failed: {failed_because}", stream.stream_id)
            });
        }
    }

    /// Run `request` inside the project's log its caller entered, reporting each step to `progress`.
    fn run_the_project_in_its_log(
        &self,
        request: ProjectStreamsToReadAndLoad,
        progress: Option<&StreamActionProgress<'_>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamRunOutcome> {
        let report_the_step = |step: StreamActionStep| {
            if let Some(progress) = progress {
                progress.is_on(step);
            }
        };
        let ProjectStreamsToReadAndLoad {
            project_directory,
            only_the_stream_assets,
            playback_after_the_load,
            keeps_each_loaded_stream_whose_node_code_is_unchanged,
        } = request;
        let Some(stream_records) = self.stream_records() else {
            return Err(Error::Configuration(format!(
                "the stream in {} was not run: this runtime records no stream, because its \
                 host gave it no state directory to record them in; run it in `tatolabd`",
                project_directory.display()
            )));
        };
        report_the_step(StreamActionStep::ReadingTheGraphDocuments);
        self.move_each_tatolab_ops_node_of_the_project_to_core_nodes(&project_directory);
        let project = ProjectAsALoadReadsIt::read(&project_directory)?;
        let every_stream_asset: Vec<StreamAssetRead> = project
            .stream_documents
            .iter()
            .map(|stream_document| project.stream_asset_read_from(stream_document))
            .collect();
        refuse_streams_whose_graph_documents_share_a_node_uuid(
            &project.project_directory,
            project.stream_documents.iter().zip(
                every_stream_asset
                    .iter()
                    .map(|stream_asset| &stream_asset.graph_or_why_none),
            ),
        )?;
        let mut stream_assets: Vec<StreamAssetRead> = every_stream_asset
            .into_iter()
            .filter(|stream_asset| {
                only_the_stream_assets
                    .as_ref()
                    .is_none_or(|only| only.contains(&stream_asset.stream_location.stream_asset))
            })
            .collect();
        {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            for stream_asset in &stream_assets {
                self.refuse_a_run_of_a_stream_whose_id_another_holds(
                    stream_records,
                    &project,
                    stream_asset,
                )?;
            }
        }
        if let Some(progress) = progress {
            progress.works_on(
                stream_assets
                    .iter()
                    .map(|stream_asset| {
                        let recorded = self.the_record_of_the_stream_at(
                            stream_records,
                            &stream_asset.stream_location,
                        );
                        StreamOfAStreamAction {
                            stream_id: recorded.as_ref().map_or_else(
                                || {
                                    self.the_stream_id_a_run_records(
                                        stream_records,
                                        &project,
                                        stream_asset,
                                    )
                                },
                                |record| record.stream_id.clone(),
                            ),
                            display_name: stream_asset.display_name.clone(),
                            stream_location: Some(stream_asset.stream_location.clone()),
                            state: None,
                        }
                    })
                    .collect(),
            );
        }

        report_the_step(StreamActionStep::MakingTheVenv);
        let stream_environment = StreamEnvironment {
            project_directory: project.project_directory.clone(),
            interpreter: the_projects_venv_interpreter(&project_directory),
        };
        let mut why_the_projects_venv_was_not_made = None;
        match self.make_the_projects_venv(
            &project.project_directory,
            is_interrupted,
            progress.map(StreamActionProgress::report_of_the_next_venv_sync),
        ) {
            Ok(()) => {
                if let Some(lend_directory) = self
                    .engine_resources_shared_by_every_stream
                    .processor_interpreter_lend_directory
                    .get()
                {
                    warn_of_what_binds_the_project_to_one_floor(
                        &stream_environment,
                        lend_directory,
                        is_interrupted,
                    );
                }
            }
            Err(venv_not_made) => {
                let failed_because = match venv_not_made {
                    ProjectVenvNotMade::Refused(refusal) => return Err(refusal),
                    ProjectVenvNotMade::Failed { failed_because } => failed_because,
                };
                why_the_projects_venv_was_not_made = Some(failed_because);
            }
        }

        report_the_step(StreamActionStep::Starting);
        let node_code_of_the_project =
            ProjectNodeCodeContentHash::of_the_project_at(&project.project_directory);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_assets_the_project_holds_as_it_starts =
            ProjectAsALoadReadsIt::read(&project_directory)?.stream_assets();
        stream_assets.retain(|stream_asset| {
            stream_assets_the_project_holds_as_it_starts
                .contains(&stream_asset.stream_location.stream_asset)
        });
        let mut streams_loaded_or_failed = Vec::with_capacity(stream_assets.len());
        for stream_asset in stream_assets {
            if progress.is_some_and(StreamActionProgress::is_cancelled) {
                return Ok(StreamRunOutcome {
                    project_directory,
                    streams: streams_loaded_or_failed,
                });
            }
            let stream_id =
                self.the_stream_id_a_run_records(stream_records, &project, &stream_asset);
            if keeps_each_loaded_stream_whose_node_code_is_unchanged
                && let Some(kept) = self.keep_the_loaded_stream_reconciled_to_its_graph_document(
                    &stream_id,
                    &stream_asset,
                    node_code_of_the_project,
                )
            {
                streams_loaded_or_failed.push(kept);
                continue;
            }
            let loaded_or_failed = self.record_and_load_one_stream_a_run_read(
                stream_records,
                StreamARunRead {
                    stream_id,
                    stream_asset,
                    stream_environment: &stream_environment,
                    playback_after_the_load,
                    why_the_projects_venv_was_not_made: why_the_projects_venv_was_not_made
                        .as_deref(),
                    is_interrupted,
                },
            )?;
            self.remember_the_node_code_the_stream_was_described_from(
                &loaded_or_failed,
                node_code_of_the_project,
            );
            streams_loaded_or_failed.push(loaded_or_failed);
        }
        self.forget_every_record_of_the_project_whose_stream_asset_is_gone(
            stream_records,
            &project,
            &stream_assets_the_project_holds_as_it_starts,
        );
        Ok(StreamRunOutcome {
            project_directory,
            streams: streams_loaded_or_failed,
        })
    }

    /// Keep the loaded stream `stream_id`, its live graph reconciled to the graph document
    /// `stream_asset` was read from, when the project's node code is `node_code_of_the_project`,
    /// as it was when the stream was loaded, and its record is neither failed nor of another
    /// location; `None` when it must be loaded afresh, a document its live graph does not
    /// reconcile to among them.
    fn keep_the_loaded_stream_reconciled_to_its_graph_document(
        &self,
        stream_id: &str,
        stream_asset: &StreamAssetRead,
        node_code_of_the_project: ProjectNodeCodeContentHash,
    ) -> Option<StreamLoadedOrFailedByARun> {
        let described_from = self
            .stream_actions
            .node_code_each_loaded_stream_was_described_from
            .lock()
            .get(stream_id)
            .copied();
        if described_from != Some(node_code_of_the_project)
            || stream_asset.graph_or_why_none.is_err()
        {
            return None;
        }
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id)?;
        let record = self.current_stream_record_of_the_id(stream_id).ok()??;
        if record.is_failed() || record.location != stream_asset.stream_location {
            return None;
        }
        let (_, stream_document) =
            the_graph_document_of_the_stream_at(&stream_asset.stream_location).ok()?;
        let graph_document: serde_json::Value =
            serde_json::from_slice(stream_document.bytes.as_ref().ok()?).ok()?;
        if let Err(refusal) = loaded
            .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(graph_document)
        {
            tracing::info!(
                "the stream `{stream_id}` is loaded again from its graph document, which its \
                 live graph does not reconcile to: {refusal}"
            );
            return None;
        }
        if loaded.display_name() != stream_asset.display_name {
            loaded.rename(stream_asset.display_name.clone());
        }
        tracing::info!(
            "the stream `{stream_id}` is kept loaded and reconciled to its graph document, its \
             project's node code as it was at its load"
        );
        let stream_tag = loaded.stream_tag();
        let node_count = loaded.node_count();
        let state = if loaded.is_held_stopped() {
            StreamStateAfterItsRun::HeldStopped {
                stream_tag,
                node_count,
            }
        } else {
            StreamStateAfterItsRun::Running {
                stream_tag,
                node_count,
            }
        };
        Some(StreamLoadedOrFailedByARun::of_the_record(
            record,
            stream_asset.display_name.clone(),
            state,
            true,
        ))
    }

    /// Remember that the stream a load took in was described from `node_code_of_the_project`,
    /// or forget it for a stream the load recorded failed.
    fn remember_the_node_code_the_stream_was_described_from(
        &self,
        loaded_or_failed: &StreamLoadedOrFailedByARun,
        node_code_of_the_project: ProjectNodeCodeContentHash,
    ) {
        let mut node_code_each_loaded_stream_was_described_from = self
            .stream_actions
            .node_code_each_loaded_stream_was_described_from
            .lock();
        match loaded_or_failed.state {
            StreamStateAfterItsRun::Failed { .. } => {
                node_code_each_loaded_stream_was_described_from.remove(&loaded_or_failed.stream_id);
            }
            StreamStateAfterItsRun::Running { .. } | StreamStateAfterItsRun::HeldStopped { .. } => {
                node_code_each_loaded_stream_was_described_from
                    .insert(loaded_or_failed.stream_id.clone(), node_code_of_the_project);
            }
        }
    }

    /// The kind of the last action under way on the project in `project_directory` that
    /// opens or closes it or starts or stops its playback; `None` when none is.
    fn the_last_lifecycle_or_playback_action_queued_on_the_project(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionKind> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
            .iter()
            .rev()
            .find(|action| {
                the_same_directory(&action.project_directory, project_directory)
                    && matches!(
                        action.kind,
                        StreamActionKind::Open
                            | StreamActionKind::StartPlayback
                            | StreamActionKind::Stop
                            | StreamActionKind::Close
                            | StreamActionKind::Remove
                            | StreamActionKind::Delete
                    )
            })
            .map(|action| action.kind)
    }

    /// Whether the last action under way on the project in `project_directory` that opens or
    /// closes it or starts or stops its playback is a playback start.
    fn is_a_playback_start_the_last_action_queued_on_the_project(
        &self,
        project_directory: &Path,
    ) -> bool {
        self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory)
            == Some(StreamActionKind::StartPlayback)
    }

    /// Whether the project in `project_directory` is open once the actions queued on it have
    /// run: the last of them that opens, closes or plays it opens or plays it, or, with none
    /// queued, a stream of it is loaded or a record of it is not closed.
    fn the_project_is_open_once_its_queued_actions_run(&self, project_directory: &Path) -> bool {
        match self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory) {
            Some(StreamActionKind::Open | StreamActionKind::StartPlayback) => true,
            Some(StreamActionKind::Close | StreamActionKind::Remove | StreamActionKind::Delete) => {
                false
            }
            _ => {
                !self
                    .loaded_streams_of_the_project(project_directory)
                    .is_empty()
                    || self
                        .current_records_of_the_project(project_directory)
                        .iter()
                        .any(|record| !record.closed)
            }
        }
    }

    /// Queue a stop of the whole project in `project_directory` on its queue, and answer the
    /// action: it ends the playback of every loaded stream of the project — every instance,
    /// device claim, function-node state and the clock torn down — keeps each held with its
    /// graph to read and edit, and records each recorded one stopped. The project stays open
    /// and stopped, across the runtime's restarts too, until a `start_playback` or a
    /// `close_project`. Each record playing once the actions queued before run carries the stop
    /// as a pending mark until the action records it stopped, so a crash finishes it at the
    /// next start. Refused — with nothing queued — for a directory not named by its absolute
    /// path, a project whose removal is queued, and a project none of whose streams runs once
    /// the actions queued before run.
    pub fn stop_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        if !project_directory.is_absolute() {
            return Err(Error::Configuration(format!(
                "`project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            )));
        }
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let action_id = self.next_stream_action_id();
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        let ids_of_the_projects_records =
            self.ids_of_every_record_of_the_project(&project_directory);
        let the_project_runs_once_its_queued_actions_run =
            self.is_a_playback_start_the_last_action_queued_on_the_project(&project_directory);
        let take_the_marks_off = |streams: &[StreamOfAStreamAction]| {
            for stream_marked_already in streams {
                self.finish_the_pending_stream_action_on_the_record(
                    &stream_marked_already.stream_id,
                    &action_id,
                );
            }
        };
        if let Some(stream_records) = self.stream_records() {
            for stream_id in &ids_of_the_projects_records {
                let marked = stream_records.update(stream_id, |record| match record {
                    Some(record) => {
                        refuse_a_stream_whose_project_removal_is_queued(stream_id, record)?;
                        if !the_project_runs_once_its_queued_actions_run
                            && (record.is_failed()
                                || record.is_stopped_once_its_pending_stream_actions_run())
                        {
                            return Ok(None);
                        }
                        record.queue_the_pending_stream_action(
                            &action_id,
                            PendingStreamActionKind::Stop,
                        );
                        Ok::<_, Error>(Some(record.clone()))
                    }
                    None => Ok(None),
                });
                match marked {
                    Ok(Ok(Some(record))) => streams.push(StreamOfAStreamAction {
                        display_name: the_display_name_of_the_stream_at(
                            &record.location,
                            &record.stream_id,
                        ),
                        stream_id: record.stream_id,
                        stream_location: Some(record.location),
                        state: None,
                    }),
                    Ok(Ok(None)) => {}
                    Ok(Err(refusal)) | Err(refusal) => {
                        take_the_marks_off(&streams);
                        return Err(refusal);
                    }
                }
            }
        }
        for loaded in self.every_loaded_stream() {
            if the_same_directory(loaded.project_directory(), &project_directory)
                && !ids_of_the_projects_records
                    .iter()
                    .any(|recorded| recorded == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: Some(loaded.stream_location().clone()),
                    state: None,
                });
            }
        }
        if streams.is_empty() {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} plays, so there is none to stop; `start_playback` \
                 plays it. \
                 Loaded: {}. Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        let stream_ids: Vec<String> = streams
            .iter()
            .map(|stream| stream.stream_id.clone())
            .collect();
        self.forget_that_the_project_is_paused(&project_directory);
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory,
            kind: StreamActionKind::Stop,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        Ok(self
            .stream_actions
            .stream_action_queue_of_each_project
            .queue_the_stream_action(
                action,
                Box::new(move |runner: &Runner| {
                    runner.stop_the_project_for_its_stream_action(&action_id, &stream_ids);
                }),
                &self.tokio_handle(),
            ))
    }

    /// Queue the removal of the whole project in `project_directory` on its
    /// queue, and answer the action: it unloads every loaded stream of the
    /// project and forgets every record of it, deleting no file. Each record
    /// carries the removal as a pending mark until it is forgotten. Refused
    /// for a directory not named by its absolute path, and for a project the
    /// runtime holds no stream of.
    pub fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let action_id = self.next_stream_action_id();
        let streams = self
            .every_stream_of_the_project_marked_for_its_removal(&action_id, &project_directory)?;
        if streams.is_empty() {
            return Err(Error::NotFound(format!(
                "this runtime holds no stream of the project in {}, so there is none to remove. \
                 Loaded: {}. Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        Ok(self.queue_the_removal_of_the_project(
            action_id,
            project_directory,
            StreamActionKind::Remove,
            streams,
        ))
    }

    /// Queue the deletion of the whole project in `project_directory` on its
    /// queue, and answer the action: it unloads every loaded stream of the
    /// project and forgets every record of it, as a removal does, then moves the
    /// project's folder to this machine's Trash. Refused for a directory not
    /// named by its absolute path, and for one that holds no project.
    pub fn delete_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !project_directory.join(PROJECT_FILE_NAME).is_file() {
            return Err(Error::NotFound(format!(
                "{} holds no project: it has no {PROJECT_FILE_NAME}, so there is none to delete",
                project_directory.display()
            )));
        }
        let action_id = self.next_stream_action_id();
        let streams = self
            .every_stream_of_the_project_marked_for_its_removal(&action_id, &project_directory)?;
        Ok(self.queue_the_removal_of_the_project(
            action_id,
            project_directory,
            StreamActionKind::Delete,
            streams,
        ))
    }

    /// Every stream of the project in `project_directory`, recorded or loaded,
    /// each record marked with the pending removal `action_id`; a record that
    /// cannot be marked refuses the whole, every mark already made taken off.
    fn every_stream_of_the_project_marked_for_its_removal(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) -> Result<Vec<StreamOfAStreamAction>> {
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
            let marked = match self.stream_records() {
                Some(stream_records) => {
                    match stream_records.change_the_record(&stream_id, |record| {
                        record.queue_the_pending_stream_action(
                            action_id,
                            PendingStreamActionKind::Remove,
                        );
                    }) {
                        Ok(marked) => marked,
                        Err(not_marked) => {
                            for stream_marked_already in &streams {
                                self.finish_the_pending_stream_action_on_the_record(
                                    &stream_marked_already.stream_id,
                                    action_id,
                                );
                            }
                            return Err(not_marked);
                        }
                    }
                }
                None => None,
            };
            streams.push(StreamOfAStreamAction {
                display_name: marked.as_ref().map_or_else(
                    || stream_id.clone(),
                    |record| the_display_name_of_the_stream_at(&record.location, &record.stream_id),
                ),
                stream_location: marked.map(|record| record.location),
                stream_id,
                state: None,
            });
        }
        for loaded in self.every_loaded_stream() {
            if the_same_directory(loaded.project_directory(), project_directory)
                && !streams
                    .iter()
                    .any(|stream| stream.stream_id == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: Some(loaded.stream_location().clone()),
                    state: None,
                });
            }
        }
        Ok(streams)
    }

    /// Queue the removal `action_id` of the project in `project_directory`, a
    /// `Remove` or a `Delete`, over `streams`.
    fn queue_the_removal_of_the_project(
        &self,
        action_id: String,
        project_directory: PathBuf,
        kind: StreamActionKind,
        streams: Vec<StreamOfAStreamAction>,
    ) -> StreamActionQueued {
        self.forget_that_the_project_is_paused(&project_directory);
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        self.stream_actions
            .stream_action_queue_of_each_project
            .queue_the_stream_action(
                action,
                Box::new(move |runner: &Runner| match kind {
                    StreamActionKind::Delete => runner
                        .delete_the_project_for_its_stream_action(&action_id, &project_directory),
                    _ => runner
                        .remove_the_project_for_its_stream_action(&action_id, &project_directory),
                }),
                &self.tokio_handle(),
            )
    }

    /// Pause every loaded stream of the project in `project_directory` now, never waiting
    /// behind an action on its queue, and answer the streams paused: each processor holds its
    /// state and processes nothing, and a graph change made meanwhile compiles on
    /// [`Self::resume_playback`], and each stream's clock holds where it stands. The project
    /// stays paused until that resume, a `stop_playback`, a `close_project` or a
    /// `remove_project`: a stream a `reload_project` starts meanwhile, `tatolab dev`'s re-load
    /// on a save among them, starts paused. The pause is recorded, so a restart of the runtime
    /// re-loads the project paused. Pausing a paused project pauses nothing more.
    /// Refused for a directory not named by its absolute path, and for a project none of whose
    /// streams plays and whose last action queued is not a playback start.
    pub fn pause_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        let loaded_streams_of_the_project = self.loaded_streams_of_the_project(&project_directory);
        if loaded_streams_of_the_project
            .iter()
            .all(|loaded| loaded.is_held_stopped())
            && !self.is_a_playback_start_the_last_action_queued_on_the_project(&project_directory)
        {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} plays, so there is none to pause; `start_playback` \
                 plays it. \
                 Loaded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal()
            )));
        }
        if !directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, &project_directory))
        {
            directories_of_the_paused_projects.push(project_directory.clone());
        }
        self.record_each_stream_of_the_project_paused(&project_directory, true);
        let mut stream_ids = Vec::new();
        for stream in loaded_streams_of_the_project {
            if stream.status() != RuntimeStatus::Started {
                continue;
            }
            stream.pause()?;
            stream_ids.push(stream.stream_id().to_string());
        }
        Ok(ProjectPausedOrResumed {
            project_directory,
            stream_ids,
        })
    }

    /// Resume every paused stream of the project in `project_directory` now, compiling each
    /// one's graph changes made while it was paused, and answer the streams resumed; a compile
    /// that fails is logged on the stream, as a live edit's is. Refused for a directory not
    /// named by its absolute path, and for a project that is not paused.
    pub fn resume_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        let paused_streams_of_the_project: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .loaded_streams_of_the_project(&project_directory)
            .into_iter()
            .filter(|stream| is_paused(stream.status()))
            .collect();
        let the_project_was_marked_paused = directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, &project_directory));
        if !the_project_was_marked_paused && paused_streams_of_the_project.is_empty() {
            return Err(Error::Runtime(format!(
                "the project in {} is not paused, so there is none to resume",
                project_directory.display()
            )));
        }
        directories_of_the_paused_projects
            .retain(|paused| !the_same_directory(paused, &project_directory));
        self.record_each_stream_of_the_project_paused(&project_directory, false);
        let mut stream_ids = Vec::new();
        for stream in paused_streams_of_the_project {
            if let Err(not_compiled) = stream.resume() {
                let _entered_this_streams_log_route = stream.log_route().enter_on_this_thread();
                tracing::error!(
                    "the stream `{}` resumed, and the graph changes made while it was paused \
                     did not compile: {not_compiled}",
                    stream.stream_id()
                );
            }
            stream_ids.push(stream.stream_id().to_string());
        }
        Ok(ProjectPausedOrResumed {
            project_directory,
            stream_ids,
        })
    }

    /// Start `stream`, paused while its project is paused, holding the paused projects so a
    /// `pause_playback` cannot land between the check and the start.
    fn start_the_stream_paused_while_its_project_is(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        let directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        if directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, stream.project_directory()))
        {
            stream.start_paused()
        } else {
            stream.start()
        }
    }

    /// Put the project in `project_directory` among the paused projects, so each stream of it
    /// a load starts starts paused.
    fn hold_the_project_paused(&self, project_directory: &Path) {
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        if !directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, project_directory))
        {
            directories_of_the_paused_projects.push(project_directory.to_path_buf());
        }
    }

    /// Take the project in `project_directory` off the paused projects, as its stop or
    /// removal is queued.
    fn forget_that_the_project_is_paused(&self, project_directory: &Path) {
        self.stream_actions
            .directories_of_the_paused_projects
            .lock()
            .retain(|paused| !the_same_directory(paused, project_directory));
        self.record_each_stream_of_the_project_paused(project_directory, false);
    }

    /// Record each stream of the project in `project_directory` paused, or not; a record
    /// that cannot be written is logged on its stream, and the live pause stands.
    fn record_each_stream_of_the_project_paused(&self, project_directory: &Path, paused: bool) {
        for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
            if let Err(not_recorded) = self.change_the_record_of_the_stream(&stream_id, |record| {
                record.paused = paused && !record.stopped && !record.closed;
            }) {
                tracing::error!(
                    "the stream `{stream_id}` was not recorded {}: {not_recorded}",
                    if paused { "paused" } else { "unpaused" }
                );
            }
        }
    }

    /// Every loaded stream of the project in `project_directory`.
    fn loaded_streams_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.every_loaded_stream()
            .into_iter()
            .filter(|loaded| the_same_directory(loaded.project_directory(), project_directory))
            .collect()
    }

    /// The next stream action id of this runtime.
    fn next_stream_action_id(&self) -> String {
        self.stream_actions
            .stream_action_queue_of_each_project
            .next_action_id(
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
            )
    }

    /// The ids of every record of the project in `project_directory`, those an
    /// earlier runtime wrote that name it included.
    fn ids_of_every_record_of_the_project(&self, project_directory: &Path) -> Vec<String> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        stream_records
            .read_every()
            .into_iter()
            .filter_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if the_same_directory(
                        &record.location.project_directory,
                        project_directory,
                    ) =>
                {
                    Some(record.stream_id)
                }
                Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))
                    if earlier
                        .project_directory
                        .as_deref()
                        .is_some_and(|recorded| {
                            the_same_directory(recorded, project_directory)
                        }) =>
                {
                    Some(earlier.stream_id)
                }
                _ => None,
            })
            .collect()
    }

    /// Run the stop the stream action `action_id` queued of the streams `stream_ids` of one
    /// project, and finish the action done with each stream stopped, or failed naming each
    /// stream unloaded and not recorded stopped. A stream that is no longer loaded and is
    /// recorded failed or stopped when the stop reaches it is left as it is.
    fn stop_the_project_for_its_stream_action(&self, action_id: &str, stream_ids: &[String]) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let mut states = Vec::with_capacity(stream_ids.len());
        let mut not_stopped = Vec::new();
        for stream_id in stream_ids {
            let left_as_it_is = self
                .loaded_stream_of_the_id_when_loaded(stream_id)
                .is_none()
                && self
                    .current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.is_failed() || record.stopped);
            if left_as_it_is {
                self.finish_the_pending_stream_action_on_the_record(stream_id, action_id);
                continue;
            }
            match self.stop_the_stream_finishing_its_pending_stop(stream_id, Some(action_id)) {
                Ok(StreamStopOutcome {
                    stop_not_recorded_because: None,
                    ..
                }) => states.push((stream_id.clone(), StreamListingState::Stopped)),
                Ok(StreamStopOutcome {
                    stop_not_recorded_because: Some(not_recorded_because),
                    ..
                }) => not_stopped.push(format!(
                    "the stream `{stream_id}` is unloaded and was not recorded stopped, so the \
                     runtime's restart loads it again: {not_recorded_because}"
                )),
                Err(refusal) => {
                    self.finish_the_pending_stream_action_on_the_record(stream_id, action_id);
                    not_stopped.push(refusal.to_string());
                }
            }
        }
        progress.finish(if not_stopped.is_empty() {
            Ok(states)
        } else {
            Err(not_stopped.join("; "))
        });
    }

    /// Run the removal the stream action `action_id` queued of the project in
    /// `project_directory`: unload every loaded stream of the project and
    /// forget every record of it, and finish the action.
    fn remove_the_project_for_its_stream_action(&self, action_id: &str, project_directory: &Path) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let not_forgotten = self.unload_and_forget_every_stream_of_the_project(project_directory);
        progress.finish(if not_forgotten.is_empty() {
            Ok(Vec::new())
        } else {
            Err(the_records_not_forgotten(&not_forgotten))
        });
    }

    /// Run the deletion the stream action `action_id` queued of the project in
    /// `project_directory`: unload and forget every stream of it as a removal
    /// does, then move its folder to the Trash, and finish the action. A record
    /// left unforgotten leaves the folder where it is.
    fn delete_the_project_for_its_stream_action(&self, action_id: &str, project_directory: &Path) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let not_forgotten = self.unload_and_forget_every_stream_of_the_project(project_directory);
        if !not_forgotten.is_empty() {
            progress.finish(Err(format!(
                "{}, so the project's folder is left where it is",
                the_records_not_forgotten(&not_forgotten)
            )));
            return;
        }
        progress.finish(
            move_the_project_folder_to_the_trash(project_directory)
                .map(|()| Vec::new())
                .map_err(|not_moved| {
                    format!(
                        "the project's streams are unloaded and forgotten, and its folder {} \
                         was not moved to the Trash: {not_moved}",
                        project_directory.display()
                    )
                }),
        );
    }

    /// Unload every loaded stream of the project in `project_directory` and
    /// forget every record of it, answering why each record that was not
    /// forgotten was not.
    fn unload_and_forget_every_stream_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<String> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded_of_the_project: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .every_loaded_stream()
            .into_iter()
            .filter(|loaded| the_same_directory(loaded.project_directory(), project_directory))
            .collect();
        let mut not_forgotten = Vec::new();
        let mut ids_of_the_records_forgotten = Vec::new();
        if let Some(stream_records) = self.stream_records() {
            for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
                match stream_records.remove(&stream_id) {
                    Ok(_) => ids_of_the_records_forgotten.push(stream_id),
                    Err(not_removed) => not_forgotten.push(not_removed.to_string()),
                }
            }
        }
        unload_every_stream_an_action_took_back(
            &loaded_of_the_project,
            "its owner removed its project",
        );
        for stream_id in ids_of_the_records_forgotten {
            self.forget_the_function_node_states_of_the_stream(&stream_id);
        }
        not_forgotten
    }

    /// How the running stream action `action_id` reports its progress, it
    /// marked in progress.
    fn progress_of_the_stream_action(&self, action_id: &str) -> StreamActionProgress<'_> {
        let progress = StreamActionProgress::of_the_stream_action(
            &self.stream_actions.stream_action_queue_of_each_project,
            action_id,
        );
        progress.has_begun();
        progress
    }

    /// Stop the stream whose id is `stream_id`, finishing the pending stop `action_id` on its
    /// record: a loaded recorded one is recorded stopped and its playback ended, its graph held
    /// to read and edit; a stream its host loaded is unloaded. A loaded recorded stream's
    /// playback ends even when its record cannot be read or written, and the outcome says why
    /// it was not recorded stopped. Refused — with nothing
    /// changed — naming the streams the runtime holds when it holds none of
    /// that id, and for a stream already stopped or failed.
    fn stop_the_stream_finishing_its_pending_stop(
        &self,
        stream_id: &str,
        action_id: Option<&str>,
    ) -> Result<StreamStopOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        if let Some(hosted) = loaded
            .as_ref()
            .filter(|loaded| loaded.holding() == LoadedStreamHolding::LivesWithTheHostProcess)
        {
            unload_a_stream_an_action_took_back(hosted, "its owner stopped it");
            return Ok(StreamStopOutcome {
                stream_id: stream_id.to_string(),
                stop_not_recorded_because: None,
            });
        }
        let record = match self.current_stream_record_of_the_id(stream_id) {
            Ok(record) => record,
            Err(unreadable) => {
                let Some(loaded) = loaded else {
                    return Err(unreadable);
                };
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                return Ok(a_recorded_stream_unloaded_and_not_recorded_stopped(
                    stream_id.to_string(),
                    &unreadable,
                ));
            }
        };
        match (loaded, record) {
            (None, None) => Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
            (None, Some(record)) if record.is_failed() => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is failed, so it is not loaded to stop: {}. \
                 `reload_project` \
                 retries it, `rm` forgets its project",
                record.failed_because.as_deref().unwrap_or_default()
            ))),
            (None, Some(record)) if record.stopped => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already stopped; `start_playback` plays its project"
            ))),
            (None, Some(_)) => {
                self.record_the_stream_stopped_finishing_its_pending_stop(stream_id, action_id)?;
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    stop_not_recorded_because: None,
                })
            }
            (Some(loaded), Some(_)) => {
                let recorded_stopped =
                    self.record_the_stream_stopped_finishing_its_pending_stop(stream_id, action_id);
                self.stop_the_playback_of_a_held_stream(&loaded)?;
                Ok(match recorded_stopped {
                    Ok(()) => StreamStopOutcome {
                        stream_id: stream_id.to_string(),
                        stop_not_recorded_because: None,
                    },
                    Err(not_written) => a_recorded_stream_unloaded_and_not_recorded_stopped(
                        stream_id.to_string(),
                        &not_written,
                    ),
                })
            }
            (Some(loaded), None) => {
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    stop_not_recorded_because: None,
                })
            }
        }
    }

    /// End the playback of `loaded`, which stays held: every instance torn down, its graph
    /// kept to read and edit, and its function-node states forgotten so the next start begins
    /// fresh. A stream whose graph cannot be put back is unloaded, and refused naming why.
    fn stop_the_playback_of_a_held_stream(
        &self,
        loaded: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        let playback_stopped = loaded
            .log_route()
            .run_entered(|| loaded.stop_playback_keeping_the_graph());
        self.forget_the_function_node_states_of_the_stream(loaded.stream_id());
        let Err(not_kept) = playback_stopped else {
            return Ok(());
        };
        let why_it_is_unloaded = format!(
            "the stream `{}` stopped and its graph was not kept to edit, so it is unloaded: \
             {not_kept}",
            loaded.stream_id()
        );
        loaded
            .log_route()
            .run_entered(|| tracing::error!("{why_it_is_unloaded}"));
        unload_a_stream_an_action_took_back(loaded, "its graph was not kept at its stop");
        Err(Error::Runtime(why_it_is_unloaded))
    }

    /// Record the stream `stream_id` stopped, finishing the pending stop
    /// `action_id` on its record.
    fn record_the_stream_stopped_finishing_its_pending_stop(
        &self,
        stream_id: &str,
        action_id: Option<&str>,
    ) -> Result<()> {
        self.change_the_record_of_the_stream(stream_id, |record| {
            record.stopped = true;
            if let Some(action_id) = action_id {
                record.finish_the_pending_stream_action(action_id);
            }
        })
    }

    /// Take the pending mark of the action `action_id` off the record of
    /// `stream_id`, when it has one.
    fn finish_the_pending_stream_action_on_the_record(&self, stream_id: &str, action_id: &str) {
        let Some(stream_records) = self.stream_records() else {
            return;
        };
        if let Err(not_finished) = stream_records.change_the_record(stream_id, |record| {
            record.finish_the_pending_stream_action(action_id);
        }) {
            tracing::error!(
                "the stream action {action_id} stays pending on the record of the stream \
                 `{stream_id}`: {not_finished}"
            );
        }
    }

    /// Show the loaded stream of `project`'s stream asset `stream_asset` under
    /// `display_name`, which its graph document now writes; a stream not loaded reads it from
    /// its document at its next load or listing. Answers the stream's id, `None` when none is
    /// loaded.
    pub fn show_the_stream_of_a_stream_asset_under(
        &self,
        project: Uuid,
        stream_asset: Uuid,
        display_name: &str,
    ) -> Option<String> {
        let loaded = self.every_loaded_stream().into_iter().find(|loaded| {
            let location = loaded.stream_location();
            location.project == project && location.stream_asset == stream_asset
        })?;
        loaded.rename(display_name.to_string());
        Some(loaded.stream_id().to_string())
    }

    /// Queue the load of `stream_asset`, a stream asset just created in the project in
    /// `project_directory`, alone on the project's stream action queue, playing while the
    /// project plays and held stopped while its playback is stopped; `None`, with nothing
    /// queued, when the project is not open.
    pub fn load_the_stream_of_a_created_stream_asset(
        &self,
        project_directory: &Path,
        stream_asset: Uuid,
    ) -> Option<StreamActionQueued> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return None;
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Some(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            Some(BTreeSet::from([stream_asset])),
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// Bring the streams the open project in `project_directory` runs as its own in line with
    /// what its streams place, as its graph documents are on disk now: each stream another
    /// stream of the project places is unloaded and forgotten, and the load of each stream asset
    /// placed nowhere that the runtime records no stream of is queued. `None` when no load is
    /// queued.
    pub fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionQueued> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return None;
        }
        let stream_records = self.stream_records()?;
        let project = match ProjectAsALoadReadsIt::read(&project_directory) {
            Ok(project) => project,
            Err(not_read) => {
                tracing::warn!(
                    project_directory = %project_directory.display(),
                    %not_read,
                    "the streams the project runs as its own were not brought in line with what \
                     its streams place"
                );
                return None;
            }
        };
        let stream_assets_the_project_runs_as_its_own = project.stream_assets();
        let stream_assets_the_runtime_records = {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            self.forget_every_record_of_the_project_whose_stream_asset_is_gone(
                stream_records,
                &project,
                &stream_assets_the_project_runs_as_its_own,
            );
            self.current_records_of_the_project(&project_directory)
                .into_iter()
                .map(|record| record.location.stream_asset)
                .collect::<BTreeSet<Uuid>>()
        };
        let stream_assets_to_load: BTreeSet<Uuid> = stream_assets_the_project_runs_as_its_own
            .into_iter()
            .filter(|stream_asset| !stream_assets_the_runtime_records.contains(stream_asset))
            .collect();
        if stream_assets_to_load.is_empty() {
            return None;
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Some(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            Some(stream_assets_to_load),
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// Why the record of `stream`, a stream the runtime records, holds it failed; `None` when
    /// it records it not failed, or records none.
    fn why_the_record_of_a_loaded_stream_holds_it_failed(
        &self,
        stream: &LoadedStreamInThisRuntime,
    ) -> Option<String> {
        if stream.holding() != LoadedStreamHolding::RecordedByTheRuntime {
            return None;
        }
        match self.stream_record_of_the_id(stream.stream_id()) {
            Ok(Some(StreamRecordAsRead::Current(record))) => record.failed_because,
            _ => None,
        }
    }

    /// Every stream the runtime holds, by id: each loaded stream, then each
    /// record not loaded — one an earlier runtime wrote listed failed. A record
    /// that cannot be read is logged and left out.
    pub fn list_streams(&self) -> Vec<StreamListing> {
        let mut listings: Vec<StreamListing> = self
            .every_loaded_stream()
            .iter()
            .map(|stream| {
                let why_it_cannot_play = stream
                    .why_its_graph_as_held_keeps_this_stream_from_playing()
                    .or_else(|| self.why_the_record_of_a_loaded_stream_holds_it_failed(stream));
                StreamListing {
                    stream_id: stream.stream_id().to_string(),
                    display_name: stream.display_name(),
                    stream_location: Some(stream.stream_location().clone()),
                    state: if why_it_cannot_play.is_some() {
                        StreamListingState::Failed
                    } else if is_paused(stream.status()) {
                        StreamListingState::Paused
                    } else if stream.is_held_stopped() {
                        StreamListingState::Stopped
                    } else {
                        StreamListingState::Running
                    },
                    node_count: Some(stream.node_count()),
                    failed_because: why_it_cannot_play,
                    capture_devices_whose_access_was_never_asked_for: stream
                        .capture_devices_whose_access_was_never_asked_for(),
                    stream_action_under_way: None,
                }
            })
            .collect();
        if let Some(stream_records) = self.stream_records() {
            for read in stream_records.read_every() {
                let listing = match read {
                    Ok(StreamRecordAsRead::Current(record)) => StreamListing {
                        state: record.listing_state(),
                        display_name: the_display_name_of_the_stream_at(
                            &record.location,
                            &record.stream_id,
                        ),
                        stream_id: record.stream_id,
                        stream_location: Some(record.location),
                        node_count: None,
                        failed_because: record.failed_because,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: None,
                    },
                    Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => StreamListing {
                        state: StreamListingState::Failed,
                        failed_because: Some(earlier.failed_because()),
                        display_name: earlier.stream_id.clone(),
                        stream_id: earlier.stream_id,
                        stream_location: None,
                        node_count: None,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: None,
                    },
                    Err(unreadable) => {
                        tracing::warn!("{unreadable}; it is left out of the stream listing");
                        continue;
                    }
                };
                if listings
                    .iter()
                    .any(|listed| listed.stream_id == listing.stream_id)
                {
                    continue;
                }
                listings.push(listing);
            }
        }
        self.show_the_streams_actions_under_way_prepare(&mut listings);
        listings.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        listings
    }

    /// Run the scaled clock of the loaded stream whose id is `stream_id` at
    /// `scale` from now on; stream time carries on from where it stands. Live
    /// only: the stream's graph document keeps the clock it names.
    pub fn set_stream_clock_scale(
        &self,
        stream_id: &str,
        scale: f64,
    ) -> Result<StreamClockReading> {
        let loaded = self.loaded_stream_of_the_id(stream_id)?;
        loaded
            .log_route()
            .run_entered(|| loaded.set_stream_clock_scale(scale))
    }

    /// Finish, in order, every pending stream action a crash left on a
    /// record — a stop recorded stopped, a removal forgotten — before the runtime
    /// counts its previous run's end and serves any call.
    pub fn finish_the_pending_stream_actions_a_crash_left(&self) {
        if let Some(stream_records) = self.stream_records() {
            finish_the_pending_stream_actions_a_crash_left_on_the_records(stream_records);
        }
    }

    /// Queue one re-load per project holding a record not closed, as a runtime does at its
    /// start, and answer the actions. Each loads only those records of its project, a failed
    /// one held stopped from its document; a record failed for crashing the runtime, one an
    /// earlier runtime wrote and one that cannot be read are logged and left in place.
    pub fn queue_the_reload_of_every_project_at_the_start(&self) -> Vec<StreamActionQueued> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        let mut streams_of_each_project: Vec<(PathBuf, Vec<StreamOfAStreamAction>)> = Vec::new();
        for read in stream_records.read_every() {
            let record = match read {
                Ok(StreamRecordAsRead::Current(record)) => record,
                Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => {
                    tracing::warn!(
                        "the stream `{}` is failed, so it is not re-loaded: {}",
                        earlier.stream_id,
                        earlier.failed_because()
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    continue;
                }
            };
            if record.closed {
                tracing::info!(
                    "the stream `{}` is closed with its project, so it is not re-loaded",
                    record.stream_id
                );
                continue;
            }
            let project_directory = std::fs::canonicalize(&record.location.project_directory)
                .unwrap_or_else(|_| record.location.project_directory.clone());
            let stream = StreamOfAStreamAction {
                display_name: the_display_name_of_the_stream_at(
                    &record.location,
                    &record.stream_id,
                ),
                stream_id: record.stream_id,
                stream_location: Some(record.location),
                state: None,
            };
            match streams_of_each_project
                .iter_mut()
                .find(|(project, _)| *project == project_directory)
            {
                Some((_, streams)) => streams.push(stream),
                None => streams_of_each_project.push((project_directory, vec![stream])),
            }
        }
        streams_of_each_project
            .into_iter()
            .map(|(project_directory, streams)| {
                let action_id = self.next_stream_action_id();
                let stream_ids: Vec<String> = streams
                    .iter()
                    .map(|stream| stream.stream_id.clone())
                    .collect();
                let action = StreamAction {
                    action_id: action_id.clone(),
                    project_directory,
                    kind: StreamActionKind::ReloadAtStart,
                    state: StreamActionState::Queued,
                    step: None,
                    venv_sync: None,
                    streams,
                    failed_because: None,
                };
                self.stream_actions
                    .stream_action_queue_of_each_project
                    .queue_the_stream_action(
                        action,
                        Box::new(move |runner: &Runner| {
                            runner
                                .reload_the_records_for_its_stream_action(&action_id, &stream_ids);
                        }),
                        &self.tokio_handle(),
                    )
            })
            .collect()
    }

    /// Run the re-load at the start the stream action `action_id` queued of
    /// the records `stream_ids`, and finish the action done with each
    /// stream's state; a stream left unloaded but not recorded failed — an
    /// engine refusal or an interrupt, re-loaded at the next start — keeps
    /// no state.
    fn reload_the_records_for_its_stream_action(&self, action_id: &str, stream_ids: &[String]) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Starting);
        let Some(stream_records) = self.stream_records() else {
            progress.finish(Ok(Vec::new()));
            return;
        };
        let listed = stream_ids
            .iter()
            .filter_map(|stream_id| stream_records.read(stream_id).transpose())
            .collect();
        let reloads = self.reload_each_recorded_stream_listed(stream_records, listed, &|| {
            progress.is_interrupted()
        });
        let states = reloads
            .iter()
            .filter_map(|reload| match reload {
                StreamReloadAtTheStart::Reloaded { stream_id } => {
                    Some((stream_id.clone(), StreamListingState::Running))
                }
                StreamReloadAtTheStart::HeldStopped { stream_id } => Some((
                    stream_id.clone(),
                    if self
                        .current_stream_record_of_the_id(stream_id)
                        .ok()
                        .flatten()
                        .is_some_and(|record| record.is_failed())
                    {
                        StreamListingState::Failed
                    } else {
                        StreamListingState::Stopped
                    },
                )),
                StreamReloadAtTheStart::NotReloaded { stream_id, .. } => self
                    .current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.is_failed())
                    .then(|| (stream_id.clone(), StreamListingState::Failed)),
                StreamReloadAtTheStart::SkippedAsFailed { stream_id, .. } => {
                    Some((stream_id.clone(), StreamListingState::Failed))
                }
                StreamReloadAtTheStart::RecordUnreadable(_) => None,
            })
            .collect();
        progress.finish(Ok(states));
    }

    /// Re-load each record of `listed` from its stream asset's graph document as it is on
    /// disk. The local API serves while this runs, so each record is read again under the
    /// one-action lock and loaded from that read: a stream stopped or removed since the
    /// listing stays unloaded, and one loaded since is left as it is. A document that gives
    /// no graph records its stream failed. Once `is_interrupted`, the stream being re-loaded
    /// and every one after it stay unloaded and are re-loaded at the runtime's next start. A
    /// venv is never made under the lock: a record that was failed or stopped at the first
    /// read and is neither at the second is read again once its venv is made.
    fn reload_each_recorded_stream_listed(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        listed: Vec<std::result::Result<StreamRecordAsRead, StreamRecordReadFailure>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Vec<StreamReloadAtTheStart> {
        let mut reloads = Vec::new();
        let mut listed = std::collections::VecDeque::from(listed);
        while let Some(listed_record) = listed.pop_front() {
            if is_interrupted() {
                break;
            }
            let stream_id = match listed_record {
                Ok(listed_record) => listed_record.stream_id().to_string(),
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            let venv_made_outside_the_lock = match stream_records.read(&stream_id) {
                Ok(Some(StreamRecordAsRead::Current(record)))
                    if !record.closed && record.is_failed_for_crashing_the_runtime() =>
                {
                    Some(Ok(()))
                }
                Ok(Some(StreamRecordAsRead::Current(record))) if !record.closed => {
                    Some(self.make_the_venv_of_a_recorded_stream(&record, is_interrupted))
                }
                _ => None,
            };
            let one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let record = match stream_records.read(&stream_id) {
                Ok(Some(StreamRecordAsRead::Current(record))) => record,
                Ok(Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))) => {
                    let failed_because = earlier.failed_because();
                    tracing::warn!(
                        "the stream `{stream_id}` is failed, so it is not re-loaded: \
                         {failed_because}"
                    );
                    reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                        stream_id,
                        failed_because,
                    });
                    continue;
                }
                Ok(None) => {
                    tracing::info!(
                        "the stream `{stream_id}` was removed before its re-load, so it is not \
                         re-loaded"
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            if record.crashed_the_runtime_even_held_from_its_document()
                && let Some(failed_because) = record.failed_because.clone()
            {
                tracing::error!(
                    "the stream `{stream_id}` crashed the runtime even held from its document, \
                     so it is not loaded at the start: {failed_because}; `tatolab reload` \
                     retries it"
                );
                reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                    stream_id,
                    failed_because,
                });
                continue;
            }
            if record.closed {
                tracing::info!(
                    "the stream `{stream_id}` is closed with its project, so it is not re-loaded"
                );
                continue;
            }
            if self
                .loaded_stream_of_the_id_when_loaded(&stream_id)
                .is_some()
            {
                tracing::info!(
                    "the stream `{stream_id}` was loaded before its re-load reached it, so it \
                     is left as it is"
                );
                continue;
            }
            let Some(venv_made_outside_the_lock) = venv_made_outside_the_lock else {
                drop(one_stream_action_at_a_time);
                listed.push_front(Ok(StreamRecordAsRead::Current(record)));
                continue;
            };
            let playback_after_the_load = if record.stopped || record.is_failed() {
                ProjectPlaybackAfterItsLoad::HeldStopped
            } else {
                ProjectPlaybackAfterItsLoad::Playing
            };
            if record.comes_back_paused_at_a_restart() {
                self.hold_the_project_paused(&record.location.project_directory);
            }
            let why_its_venv_was_not_made = match &venv_made_outside_the_lock {
                Err(venv_not_made) if !is_interrupted() => Some(format!(
                    "its `.venv` was not made as it re-loaded at the runtime's start: \
                     {venv_not_made}"
                )),
                _ => None,
            };
            let playback_after_the_load = if why_its_venv_was_not_made.is_some() {
                ProjectPlaybackAfterItsLoad::HeldStopped
            } else {
                playback_after_the_load
            };
            let venv_made_outside_the_lock = match venv_made_outside_the_lock {
                Err(_) if why_its_venv_was_not_made.is_some() => Ok(()),
                made_or_interrupted => made_or_interrupted,
            };
            let reloaded = venv_made_outside_the_lock.and_then(|()| {
                let graph = the_graph_the_recorded_stream_holds(&record).map_err(Error::Runtime)?;
                let display_name =
                    the_display_name_of_the_stream_at(&record.location, &record.stream_id);
                self.load_a_recorded_stream_from_its_record(
                    &record,
                    &display_name,
                    &graph,
                    CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
                    playback_after_the_load,
                    is_interrupted,
                )
            });
            match reloaded {
                Ok((_, Some(start_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)))) => {
                    tracing::error!(
                        "the stream `{stream_id}` is held stopped, its start refused, and is \
                         started again at the runtime's next start: {start_refusal}"
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: start_refusal,
                    });
                }
                Ok((_, Some(start_refusal))) => {
                    let failed_because =
                        format!("its start at the runtime's start was refused: {start_refusal}");
                    tracing::error!(
                        "the stream `{stream_id}` is held stopped, and is failed: {failed_because}"
                    );
                    if let Err(not_written) =
                        stream_records.change_the_record(&stream_id, |recorded| {
                            recorded.mark_open_with_its_playback_stopped();
                            recorded.failed_because = Some(failed_because);
                        })
                    {
                        tracing::error!(
                            "the stream `{stream_id}` was not recorded failed: {not_written}"
                        );
                    }
                    reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                }
                Ok((stream, None)) => {
                    let why_it_cannot_play = why_its_venv_was_not_made
                        .clone()
                        .or_else(|| stream.why_its_graph_as_held_keeps_this_stream_from_playing());
                    if let Some(why_it_cannot_play) = why_it_cannot_play
                        && !record.is_failed()
                    {
                        tracing::error!(
                            "the stream `{stream_id}` re-loaded held stopped from its document, \
                             and is failed: {why_it_cannot_play}"
                        );
                        if let Err(not_written) =
                            stream_records.change_the_record(&stream_id, |recorded| {
                                recorded.failed_because = Some(why_it_cannot_play);
                            })
                        {
                            tracing::error!(
                                "the stream `{stream_id}` was not recorded failed: {not_written}"
                            );
                        }
                        reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                        continue;
                    }
                    if let Some(failed_because) = &record.failed_because {
                        tracing::warn!(
                            "the stream `{stream_id}` is failed, so it is re-loaded held stopped \
                             from its document: {failed_because}; a play or `tatolab reload` \
                             retries it"
                        );
                        reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                        continue;
                    }
                    tracing::info!(
                        "the stream `{stream_id}` re-loaded from {} with {} nodes",
                        record.location,
                        stream.node_count()
                    );
                    reloads.push(match playback_after_the_load {
                        ProjectPlaybackAfterItsLoad::HeldStopped => {
                            StreamReloadAtTheStart::HeldStopped { stream_id }
                        }
                        ProjectPlaybackAfterItsLoad::Playing => {
                            StreamReloadAtTheStart::Reloaded { stream_id }
                        }
                    });
                }
                Err(reload_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)) => {
                    tracing::error!(
                        "the stream `{stream_id}` was not re-loaded, and is re-loaded at the \
                         runtime's next start: {reload_refusal}"
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) if is_interrupted() => {
                    tracing::info!(
                        "the stream `{stream_id}` was not re-loaded, because {}: \
                         {reload_refusal}",
                        what_interrupted_a_load()
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) => {
                    let failed_because =
                        format!("it did not re-load at the runtime's start: {reload_refusal}");
                    match stream_records.change_the_record(&stream_id, |recorded| {
                        if !recorded.is_failed() {
                            recorded.failed_because = Some(failed_because);
                        }
                    }) {
                        Ok(_) => tracing::error!(
                            "the stream `{stream_id}` did not re-load, and is failed: \
                             {reload_refusal}; `tatolab reload` retries it"
                        ),
                        Err(not_written) => tracing::error!(
                            "the stream `{stream_id}` did not re-load: {reload_refusal}; and it \
                             was not recorded failed: {not_written}"
                        ),
                    }
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
            }
        }
        reloads
    }

    /// Add one to the crash count of the stream `stream_id`, already failed for crashing the
    /// runtime, which a clean end never resets: past
    /// [`RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM`] it crashed the runtime even held from
    /// its document, and is not loaded at a start at all.
    fn count_a_crash_against_a_stream_already_failed_for_crashing(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_id: &str,
    ) {
        if let Err(not_written) = stream_records.change_the_record(stream_id, |recorded| {
            recorded.runtime_crashes_in_a_row_implicating_it += 1;
        }) {
            tracing::error!(
                "the stream `{stream_id}`'s count of the runtime's crashes was not recorded: \
                 {not_written}"
            );
        }
    }

    /// Count how the runtime's previous run ended against every current
    /// record not failed for crashing the runtime, before any re-load: a clean end resets each crash
    /// count; a crash adds one to each stream it was pinned on — failing it at
    /// [`RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM`] — and resets every
    /// other's. Returns the ids of the streams it failed.
    pub fn count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &self,
        how_the_previous_run_ended: &HowThePreviousRuntimeRunEnded,
    ) -> Vec<String> {
        let no_crash = CrashOfThePreviousRuntimeRun::default();
        let crash = match how_the_previous_run_ended {
            HowThePreviousRuntimeRunEnded::Cleanly => &no_crash,
            HowThePreviousRuntimeRunEnded::Crashed(crash) => {
                log_the_previous_runtime_runs_crash(crash);
                crash
            }
        };
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let mut streams_failed = Vec::new();
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(mut record)) = read else {
                continue;
            };
            if record.is_failed_for_crashing_the_runtime() {
                if crash
                    .causes_by_implicated_stream
                    .contains_key(&record.stream_id)
                {
                    self.count_a_crash_against_a_stream_already_failed_for_crashing(
                        stream_records,
                        &record.stream_id,
                    );
                }
                continue;
            }
            let crashes_in_a_row_before = record.runtime_crashes_in_a_row_implicating_it;
            match crash.causes_by_implicated_stream.get(&record.stream_id) {
                Some(cause) => {
                    record.runtime_crashes_in_a_row_implicating_it += 1;
                    let crashes_in_a_row = record.runtime_crashes_in_a_row_implicating_it;
                    if crashes_in_a_row >= RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM {
                        record.failed_because = Some(format!(
                            "it was implicated in the runtime's last {crashes_in_a_row} crashes \
                             in a row, the last: {cause}"
                        ));
                    }
                }
                None if crashes_in_a_row_before == 0 => continue,
                None => record.runtime_crashes_in_a_row_implicating_it = 0,
            }
            let counted = (
                record.runtime_crashes_in_a_row_implicating_it,
                record.failed_because.clone(),
            );
            if let Err(not_written) =
                stream_records.change_the_record(&record.stream_id, |recorded| {
                    (
                        recorded.runtime_crashes_in_a_row_implicating_it,
                        recorded.failed_because,
                    ) = counted;
                })
            {
                tracing::error!(
                    "the stream `{}`'s count of the runtime's crashes was not recorded: \
                     {not_written}",
                    record.stream_id
                );
                continue;
            }
            if record.is_failed_for_crashing_the_runtime()
                && let Some(failed_because) = &record.failed_because
            {
                tracing::error!(
                    "the stream `{}` is failed: {failed_because}; it is held at a start with \
                     none of its node code read until `tatolab reload` tries it",
                    record.stream_id
                );
                streams_failed.push(record.stream_id);
            }
        }
        streams_failed
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn stream_records(&self) -> Option<&StreamRecordsInTheStateDirectory> {
        self.stream_actions.stream_records.get().map(Arc::as_ref)
    }

    fn stream_record_of_the_id(&self, stream_id: &str) -> Result<Option<StreamRecordAsRead>> {
        match self.stream_records() {
            Some(stream_records) => Ok(stream_records.read(stream_id)?),
            None => Ok(None),
        }
    }

    /// The record of `stream_id` this runtime writes, refused naming why a
    /// record an earlier runtime wrote is failed.
    fn current_stream_record_of_the_id(&self, stream_id: &str) -> Result<Option<StreamRecord>> {
        match self.stream_record_of_the_id(stream_id)? {
            Some(StreamRecordAsRead::Current(record)) => Ok(Some(record)),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => {
                Err(Error::Runtime(format!(
                    "the stream `{stream_id}` is failed: {}",
                    earlier.failed_because()
                )))
            }
            None => Ok(None),
        }
    }

    /// Change the record of the stream `stream_id` with `edit` when there is
    /// one, refused when the runtime records no stream.
    fn change_the_record_of_the_stream(
        &self,
        stream_id: &str,
        edit: impl FnOnce(&mut StreamRecord),
    ) -> Result<()> {
        match self.stream_records() {
            Some(stream_records) => stream_records.change_the_record(stream_id, edit).map(drop),
            None => Err(Error::Configuration(format!(
                "the stream `{stream_id}` was not recorded: this runtime records no stream"
            ))),
        }
    }

    fn a_stream_this_runtime_does_not_hold(&self, stream_id: &str) -> Error {
        Error::NotFound(format!(
            "no stream with the id `{stream_id}` is loaded or recorded in this runtime. Loaded: \
             {}. Recorded: {}",
            self.loaded_stream_ids_listed_for_a_refusal(),
            self.recorded_stream_ids_listed_for_a_refusal()
        ))
    }

    fn recorded_stream_ids_listed_for_a_refusal(&self) -> String {
        let recorded_ids: Vec<String> = self
            .stream_records()
            .map(|stream_records| {
                stream_records
                    .read_every()
                    .into_iter()
                    .filter_map(|read| read.ok())
                    .map(|read| match read {
                        StreamRecordAsRead::Current(record) => match record.listing_state() {
                            StreamListingState::Running => record.stream_id,
                            state => format!("{} ({state})", record.stream_id),
                        },
                        StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier) => format!(
                            "{} ({}, recorded by an earlier runtime)",
                            earlier.stream_id,
                            StreamListingState::Failed
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();
        names_listed_for_a_refusal(recorded_ids, "none")
    }

    /// The record of the stream at `stream_location`, its project and stream asset, when one
    /// is recorded, wherever the project sat when it was.
    fn the_record_of_the_stream_at(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_location: &StreamLocation,
    ) -> Option<StreamRecord> {
        stream_records
            .read_every()
            .into_iter()
            .find_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.is_the_same_stream_as(stream_location) =>
                {
                    Some(record)
                }
                _ => None,
            })
    }

    /// The id a run records `stream_asset` of `project` under: its record's, else the one its
    /// project's name, its name and its two UUIDs give it.
    fn the_stream_id_a_run_records(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_asset: &StreamAssetRead,
    ) -> String {
        self.the_record_of_the_stream_at(stream_records, &stream_asset.stream_location)
            .map_or_else(
                || {
                    stream_asset
                        .stream_location
                        .stream_id_named(&project.project_file.name, &stream_asset.stream_name)
                },
                |record| record.stream_id,
            )
    }

    /// Refuse a run of `stream_asset` of `project` whose id another stream holds, loaded or
    /// recorded, or that another copy of its project holds.
    fn refuse_a_run_of_a_stream_whose_id_another_holds(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_asset: &StreamAssetRead,
    ) -> Result<()> {
        let stream_id = self.the_stream_id_a_run_records(stream_records, project, stream_asset);
        self.the_recorded_stream_a_run_of_its_location_replaces(
            &stream_id,
            &stream_asset.stream_location,
        )
        .map(|_| ())
    }

    /// Record the stream asset a run read — failed with the reason when its document gave no
    /// graph — keeping the display name of the record it replaces, and load its graph, started
    /// or held stopped as the run's playback says. A replace is recorded first, then the running stream unloaded; a new stream is
    /// recorded once loaded, before its start, so a load a machine shutdown abandons leaves
    /// nothing recorded. A load or start that is refused records the stream failed with the
    /// reason. A record that cannot be written leaves the running stream as it was.
    fn record_and_load_one_stream_a_run_read(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        read: StreamARunRead<'_>,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let StreamARunRead {
            stream_id,
            stream_asset:
                StreamAssetRead {
                    stream_location,
                    stream_name: _,
                    display_name,
                    graph_or_why_none,
                },
            stream_environment,
            playback_after_the_load,
            why_the_projects_venv_was_not_made,
            is_interrupted,
        } = read;
        let playback_after_the_load = if why_the_projects_venv_was_not_made.is_some() {
            ProjectPlaybackAfterItsLoad::HeldStopped
        } else {
            playback_after_the_load
        };
        let (previous_record, previously_loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?;
        let mut stream_record =
            StreamRecord::of_a_running_stream(stream_id, stream_location, stream_environment);
        if playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped {
            stream_record.mark_open_with_its_playback_stopped();
        }
        let (graph, failed_because_its_document_gave_no_graph) = match graph_or_why_none {
            Ok(graph) => (Some(graph), None),
            Err(failed_because) => {
                stream_record.failed_because = Some(failed_because.clone());
                (None, Some(failed_because))
            }
        };
        let replaced_a_recorded_stream = previous_record.is_some();
        let a_new_stream_recorded_once_loaded =
            !replaced_a_recorded_stream && failed_because_its_document_gave_no_graph.is_none();
        if !a_new_stream_recorded_once_loaded {
            stream_record = stream_records.write_the_record_a_load_made(stream_record)?;
            if let Some(previously_loaded) = previously_loaded {
                unload_a_stream_an_action_took_back(&previously_loaded, "its record is replaced");
            }
        }
        let state = match (graph, failed_because_its_document_gave_no_graph) {
            (Some(graph), _) => self.load_a_recorded_stream_recording_it_failed_when_refused(
                stream_records,
                &mut stream_record,
                &display_name,
                &graph,
                a_new_stream_recorded_once_loaded,
                playback_after_the_load,
                is_interrupted,
            )?,
            (None, failed_because) => StreamStateAfterItsRun::Failed {
                failed_because: failed_because.unwrap_or_default(),
            },
        };
        let state = match (why_the_projects_venv_was_not_made, state) {
            (Some(why_the_venv_was_not_made), StreamStateAfterItsRun::HeldStopped { .. })
            | (Some(why_the_venv_was_not_made), StreamStateAfterItsRun::Failed { .. }) => {
                record_a_stream_held_from_its_document_failed(
                    stream_records,
                    &mut stream_record,
                    why_the_venv_was_not_made.to_string(),
                )?
            }
            (_, state) => state,
        };
        Ok(StreamLoadedOrFailedByARun::of_the_record(
            stream_record,
            display_name,
            state,
            replaced_a_recorded_stream,
        ))
    }

    /// Load `graph` as the stream `stream_record` records — writing the record once it is
    /// loaded when `record_it_once_loaded`, and abandoning it once
    /// `is_interrupted` reports true — then start it or hold it stopped as
    /// `playback_after_the_load` says. A refused load or start records the stream failed with
    /// the reason; a record that cannot be written unloads the stream and is refused.
    #[allow(clippy::too_many_arguments)]
    fn load_a_recorded_stream_recording_it_failed_when_refused(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_record: &mut StreamRecord,
        display_name: &str,
        graph: &GraphSnapshot,
        record_it_once_loaded: bool,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamStateAfterItsRun> {
        let loaded = self.load_a_recorded_stream(
            OneStreamAStreamActionLoads::recorded_by(stream_record, display_name, graph)
                .abandoned_once(is_interrupted),
        );
        let stream = match loaded {
            Ok(stream) => stream,
            Err(load_refusal) => {
                return record_a_refused_load_failed(stream_records, stream_record, load_refusal);
            }
        };
        if record_it_once_loaded
            && let Err(not_written) = stream_records
                .write_the_record_a_load_made(stream_record.clone())
                .map(|written| *stream_record = written)
        {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(not_written);
        }
        if let Some(why_it_cannot_play) =
            stream.why_its_graph_as_held_keeps_this_stream_from_playing()
        {
            stream.hold_stopped_without_a_start();
            return record_a_stream_held_from_its_document_failed(
                stream_records,
                stream_record,
                why_it_cannot_play,
            );
        }
        if playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped {
            return Ok(self.hold_a_loaded_stream_stopped(&stream));
        }
        if let Err(start_refusal) = self.start_the_stream_paused_while_its_project_is(&stream) {
            hold_a_stream_whose_start_was_refused_stopped(&stream);
            if !stream.has_ended() {
                stream_record.mark_open_with_its_playback_stopped();
            }
            return record_a_refused_load_failed(stream_records, stream_record, start_refusal);
        }
        Ok(StreamStateAfterItsRun::Running {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        })
    }

    /// Hold `stream`, loaded and never started, stopped: its graph there to read and edit and
    /// nothing running.
    fn hold_a_loaded_stream_stopped(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> StreamStateAfterItsRun {
        stream.hold_stopped_without_a_start();
        StreamStateAfterItsRun::HeldStopped {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        }
    }

    /// The record of `stream_location` a run of it replaces, and its stream
    /// when it is loaded. Refused naming both for an id another location
    /// holds, and for a stream loaded under the id that no record holds.
    fn the_recorded_stream_a_run_of_its_location_replaces(
        &self,
        stream_id: &str,
        stream_location: &StreamLocation,
    ) -> Result<(Option<StreamRecord>, Option<Arc<LoadedStreamInThisRuntime>>)> {
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        let record = self.stream_record_of_the_id(stream_id)?;
        refuse_an_id_another_location_holds(
            stream_id,
            stream_location,
            loaded.as_deref(),
            record.as_ref(),
        )?;
        let record = match record {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(_)) | None => None,
        };
        match (record, loaded) {
            (_, Some(loaded))
                if loaded.holding() == LoadedStreamHolding::LivesWithTheHostProcess =>
            {
                Err(a_stream_id_already_loaded_refusal(
                    &loaded,
                    STOP_IT_FIRST_TO_LOAD_THIS_ONE,
                ))
            }
            (None, Some(loaded)) => Err(a_stream_id_already_loaded_refusal(
                &loaded,
                STOP_IT_FIRST_TO_LOAD_THIS_ONE,
            )),
            (record, loaded) => Ok((record, loaded)),
        }
    }

    /// Unload and forget every record of `project` whose stream asset is not among
    /// `stream_assets_the_project_holds`: its graph document is gone, or another stream of the
    /// project places it, so it runs only there. A record of the project that another copy of it
    /// holds is left as it is; one that cannot be removed is logged.
    fn forget_every_record_of_the_project_whose_stream_asset_is_gone(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_assets_the_project_holds: &[Uuid],
    ) {
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(record)) = read else {
                continue;
            };
            if record.location.project != project.project_file.project
                || stream_assets_the_project_holds.contains(&record.location.stream_asset)
                || another_copy_of_the_project_holds(&record.location, &project.project_directory)
            {
                continue;
            }
            let why_it_is_forgotten = if project
                .stream_assets_another_stream_places
                .contains(&record.location.stream_asset)
            {
                "another stream of its project places it, so it runs only inside that stream"
                    .to_owned()
            } else {
                format!(
                    "no graph document of {} holds its stream asset {} any more",
                    project.project_directory.display(),
                    record.location.stream_asset
                )
            };
            if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(&record.stream_id) {
                unload_a_stream_an_action_took_back(&loaded, &why_it_is_forgotten);
            }
            match stream_records.remove(&record.stream_id) {
                Ok(_) => {
                    self.forget_the_function_node_states_of_the_stream(&record.stream_id);
                    tracing::info!(
                        "the stream `{}` is forgotten: {why_it_is_forgotten}",
                        record.stream_id,
                    )
                }
                Err(not_removed) => tracing::error!(
                    "the stream `{}` has no graph document any more, and its record was not \
                     removed: {not_removed}",
                    record.stream_id
                ),
            }
        }
    }

    /// Enter, on this thread, the log of the project in `project_directory`
    /// for a load of it, so its venv making and document reading land there under no
    /// stream; `None` for a directory that is not a project — one that does not
    /// resolve or holds no `tatolab-project.json` — whose load logs to the runtime's own log.
    fn enter_the_log_of_the_project_a_load_takes_in(
        &self,
        project_directory: &Path,
        action_id: Option<&str>,
    ) -> Option<TheProjectsLogEnteredForOneLoad> {
        let project_directory = std::fs::canonicalize(project_directory).ok()?;
        if why_a_directory_is_no_project(&project_directory).is_some() {
            return None;
        }
        let entry = ProjectLogRouteEntry::for_the_project_in(
            &project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
            action_id,
        );
        Some(TheProjectsLogEnteredForOneLoad {
            _entered_on_this_thread: entry.enter_on_this_thread(),
            entry,
        })
    }

    /// Lock the Python requirements the `pyproject.toml` in `project_directory` declares with
    /// the uv this runtime was handed, refused in uv's own words when they cannot be resolved.
    /// A runtime never handed a uv locks nothing, and the project's next load resolves them.
    pub fn lock_the_python_requirements_of_the_project_in(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String> {
        match self
            .engine_resources_shared_by_every_stream
            .runtime_unit_uv_executable
            .get()
        {
            Some(runtime_unit_uv) => {
                lock_the_projects_python_requirements_with_the_runtime_unit_uv(
                    project_directory,
                    runtime_unit_uv,
                    &self.packages_this_runtime_unit_carries(),
                )
            }
            None => Ok(()),
        }
    }

    /// Where the Core Nodes project the runtime unit carries is, when this runtime was handed a
    /// lend and its unit carries one.
    pub fn the_core_nodes_project_directory_this_runtime_carries(&self) -> Option<PathBuf> {
        self.packages_this_runtime_unit_carries()
            .core_nodes_project_directory
    }

    /// What the runtime unit carries beside this runtime's lend, nothing when it was handed no
    /// lend.
    fn packages_this_runtime_unit_carries(&self) -> PackagesTheRuntimeUnitCarries {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .map(PackagesTheRuntimeUnitCarries::beside_the_lend)
            .unwrap_or_default()
    }

    /// Move each node of a `tatolab-ops` type in the project in `project_directory` to the Core
    /// Nodes type that replaced it, through the project history the host handed this runtime.
    /// A move not recorded is logged in its own words, and the load goes on with the documents
    /// as they are.
    fn move_each_tatolab_ops_node_of_the_project_to_core_nodes(&self, project_directory: &Path) {
        let Some(history) = self
            .engine_resources_shared_by_every_stream
            .project_history_recording_first_party_package_moves
            .lock()
            .clone()
        else {
            return;
        };
        if let Err(not_moved) = history.move_each_tatolab_ops_node_to_core_nodes(project_directory)
        {
            tracing::warn!(
                "the tatolab-ops nodes of {} were not moved to Core Nodes: {not_moved}",
                project_directory.display()
            );
        }
    }

    /// Make the `.venv` of the project in `project_directory` current with the
    /// uv this runtime was handed and the first-party modules beside its lend,
    /// its sync reported to `report_of_the_venv_sync` when one is given.
    fn make_the_projects_venv(
        &self,
        project_directory: &Path,
        is_interrupted: &dyn Fn() -> bool,
        report_of_the_venv_sync: Option<StreamActionVenvSyncReport>,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        let runtime_unit_uv = self
            .engine_resources_shared_by_every_stream
            .runtime_unit_uv_executable
            .get();
        if let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) {
            let history = self
                .engine_resources_shared_by_every_stream
                .project_history_recording_first_party_package_moves
                .lock()
                .clone();
            move_the_projects_first_party_packages_to_the_runtime_version(
                project_directory,
                runtime_unit_uv,
                env!("CARGO_PKG_VERSION"),
                history.as_deref(),
                is_interrupted,
            )?;
        }
        make_the_projects_venv_current_with_the_runtime_unit_uv(
            project_directory,
            runtime_unit_uv,
            &self.packages_this_runtime_unit_carries(),
            is_interrupted,
            report_of_the_venv_sync,
        )
    }

    /// Make the `.venv` that `record`'s interpreter is in, unless it runs,
    /// refused for why when it cannot be made or `is_interrupted` cut it short.
    /// An interpreter in no project's `.venv` is left as it is.
    fn make_the_venv_of_a_recorded_stream(
        &self,
        record: &StreamRecord,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(project_directory) = the_project_directory_whose_venv_holds(&record.interpreter)
        else {
            return Ok(());
        };
        Ok(self.make_the_projects_venv(&project_directory, is_interrupted, None)?)
    }

    /// Load a recorded stream from `record` and the graph `graph` its document holds, started
    /// or held stopped as `playback_after_the_load` says, its capture devices asking the user
    /// as `capture_device_access_prompting` says, the load abandoned once `is_interrupted`
    /// reports true. A start that is refused holds the stream stopped, answered beside it.
    fn load_a_recorded_stream_from_its_record(
        &self,
        record: &StreamRecord,
        display_name: &str,
        graph: &GraphSnapshot,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<(Arc<LoadedStreamInThisRuntime>, Option<Error>)> {
        let stream_to_load = OneStreamAStreamActionLoads::recorded_by(record, display_name, graph)
            .with_capture_device_access_prompting(capture_device_access_prompting)
            .reading_none_of_its_node_code(
                record
                    .failed_because
                    .clone()
                    .filter(|_| record.is_failed_for_crashing_the_runtime()),
            )
            .abandoned_once(is_interrupted);
        let stream = self.load_a_recorded_stream(stream_to_load)?;
        let start_refusal = self.start_a_loaded_recorded_stream_unless_it_is_held_stopped(
            &stream,
            playback_after_the_load,
        );
        stream.log_route().write_every_record_it_emitted_and_sync();
        match start_refusal {
            Some(start_refusal) if stream.has_ended() => Err(start_refusal),
            start_refusal => Ok((stream, start_refusal)),
        }
    }

    /// Start `stream` as `playback_after_the_load` says, or hold it stopped when that says so
    /// or its document names a node type not described; a refused start holds it stopped and
    /// is answered.
    fn start_a_loaded_recorded_stream_unless_it_is_held_stopped(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
    ) -> Option<Error> {
        let held_from_its_document = stream
            .why_its_graph_as_held_keeps_this_stream_from_playing()
            .is_some();
        if held_from_its_document
            || playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped
        {
            stream.hold_stopped_without_a_start();
            return None;
        }
        let start_refusal = self
            .start_the_stream_paused_while_its_project_is(stream)
            .err()?;
        hold_a_stream_whose_start_was_refused_stopped(stream);
        Some(start_refusal)
    }

    /// Load `stream_to_load` without starting it.
    fn load_a_recorded_stream(
        &self,
        stream_to_load: OneStreamAStreamActionLoads<'_>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let OneStreamAStreamActionLoads {
            stream_id,
            display_name,
            stream_location,
            graph,
            stream_environment,
            capture_device_access_prompting,
            why_none_of_its_node_code_is_read,
            is_interrupted,
        } = stream_to_load;
        let load_options = OptionsForLoadingOneStream::at_stream_location(
            stream_id.to_string(),
            stream_location.clone(),
        )
        .in_stream_environment(stream_environment)
        .with_display_name(display_name)
        .with_capture_device_access_prompting(capture_device_access_prompting);
        let load_options = match why_none_of_its_node_code_is_read {
            Some(why_none_of_it_is_read) => {
                load_options.reading_none_of_its_node_code(why_none_of_it_is_read)
            }
            None => load_options,
        };
        let stream = match self.load_stream_held_as_unless_interrupted(
            graph,
            load_options,
            LoadedStreamHolding::RecordedByTheRuntime,
            is_interrupted,
        )? {
            StreamLoadObservingInterrupts::Loaded(stream) => stream,
            StreamLoadObservingInterrupts::AbandonedForAnInterrupt => {
                let why_it_was_abandoned = if is_the_machines_shutdown_requested() {
                    "the machine is shutting every stream down"
                } else {
                    "its stream action was cancelled"
                };
                return Err(Error::Runtime(format!(
                    "the stream `{stream_id}` was not loaded: {why_it_was_abandoned}"
                )));
            }
        };
        Ok(stream)
    }
}

impl Runner {
    /// Queue the opening of the project in `request`'s directory on its stream action queue,
    /// answering the action's id at once: it reads every stream asset's graph document, makes
    /// the project's `.venv`, and holds each stream loaded and stopped, its graph there to read
    /// and edit, nothing running and nothing claimed; a stream whose document gives no graph or
    /// whose load is refused is recorded failed. The project stays open across the runtime's
    /// restarts until a `close_project`. Refused, with nothing queued, for a directory not named
    /// by its absolute path and for a project already open once its queued actions run, naming
    /// [`Self::reload_project`].
    pub fn open_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is already open, so there is none to open; `reload_project` \
                 re-reads what changed on disk",
                project_directory.display()
            )));
        }
        Ok(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            None,
            ProjectPlaybackAfterItsLoad::HeldStopped,
            StreamActionKind::Open,
        ))
    }

    /// Queue the start of the playback of the open project in `project_directory` on its stream
    /// action queue, answering the action's id at once: each stream it holds stopped starts
    /// fresh from the graph it holds, on a new clock. The action reads no document, makes no
    /// venv and loads nothing. Refused, with nothing queued, for a directory not named by its
    /// absolute path, a project that is not open, naming [`Self::open_project`], and a project
    /// that plays once its queued actions run — a paused one naming [`Self::resume_playback`].
    pub fn start_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to play; `open_project` \
                 opens it",
                project_directory.display()
            )));
        }
        if self.the_project_runs_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(
                if self.is_the_project_paused(&project_directory) {
                    format!(
                        "the project in {} is paused, so there is none to play; \
                         `resume_playback` resumes it",
                        project_directory.display()
                    )
                } else {
                    format!(
                        "the project in {} already plays, so there is none to play; \
                         `stop_playback` stops it",
                        project_directory.display()
                    )
                },
            ));
        }
        Ok(self.queue_the_playback_of_the_held_project(project_directory))
    }

    /// Queue the making current of the `.venv` of the project in `project_directory` on its
    /// stream action queue, answering the action's id at once: the venv is made, or synced
    /// again when what it was synced with changed, as a load makes it, and no stream is read,
    /// loaded, started or stopped. Refused, with nothing queued, for a directory not named by
    /// its absolute path.
    pub fn make_the_projects_venv_current(
        &self,
        project_directory: &Path,
    ) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::MakeTheVenvCurrent,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: Vec::new(),
            failed_because: None,
        };
        Ok(queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.make_the_projects_venv_current_for_its_stream_action(
                    &action_id,
                    &project_directory,
                );
            }),
            &self.tokio_handle(),
        ))
    }

    /// Run the making current of the venv the stream action `action_id` queued of the project
    /// in `project_directory`, and finish the action, failed for why the venv was not made.
    fn make_the_projects_venv_current_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::MakingTheVenv);
        let made = self.make_the_projects_venv(
            project_directory,
            &|| progress.is_interrupted(),
            Some(progress.report_of_the_next_venv_sync()),
        );
        progress.finish(match made {
            Ok(()) => Ok(Vec::new()),
            Err(ProjectVenvNotMade::Refused(refusal)) => Err(refusal.to_string()),
            Err(ProjectVenvNotMade::Failed { failed_because }) => Err(failed_because),
        });
    }

    /// Queue the start of the playback of every held stopped stream of the project in
    /// `project_directory`, answering the action's id at once. The action reads no document
    /// and makes no venv: each stream starts fresh from the graph it holds, paused while its
    /// project is.
    fn queue_the_playback_of_the_held_project(
        &self,
        project_directory: PathBuf,
    ) -> StreamActionQueued {
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let streams = self
            .loaded_streams_of_the_project(&project_directory)
            .iter()
            .map(|held| stream_of_a_stream_action_not_yet_taken_in(held))
            .collect();
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::StartPlayback,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.start_the_playback_of_the_held_project(&action_id, &project_directory);
            }),
            &self.tokio_handle(),
        )
    }

    /// Run the playback start the stream action `action_id` queued of the project in
    /// `project_directory`: name the held stopped streams as the ones it takes in — an open
    /// queued ahead of it loads them only after this action is queued — record each running and
    /// start it, and finish the action. A stream whose start is refused is held stopped and recorded failed with the
    /// reason; a project holding no stream stopped fails the action.
    fn start_the_playback_of_the_held_project(&self, action_id: &str, project_directory: &Path) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let mut states = Vec::new();
        let held_streams: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .loaded_streams_of_the_project(project_directory)
            .into_iter()
            .filter(|loaded| loaded.is_held_stopped())
            .collect();
        if held_streams.is_empty() {
            progress.finish(Err(format!(
                "no stream of the project in {} is held stopped to play: each failed or none \
                 is loaded; `reload_project` retries a failed one",
                project_directory.display()
            )));
            return;
        }
        progress.works_on(
            held_streams
                .iter()
                .map(|held| stream_of_a_stream_action_not_yet_taken_in(held))
                .collect(),
        );
        for held in held_streams {
            if progress.is_cancelled() {
                break;
            }
            let stream_id = held.stream_id().to_string();
            if let Some(why_it_cannot_play) =
                held.why_its_graph_as_held_keeps_this_stream_from_playing()
            {
                held.log_route().run_entered(|| {
                    tracing::error!(
                        "the stream `{stream_id}` was not started, and is held stopped from its \
                         document: {why_it_cannot_play}"
                    )
                });
                states.push((stream_id, StreamListingState::Failed));
                continue;
            }
            if let Err(not_recorded) = self.change_the_record_of_the_stream(&stream_id, |record| {
                record.mark_running_resetting_its_crash_count();
            }) {
                held.log_route().run_entered(|| {
                    tracing::error!(
                        "the stream `{stream_id}` was not started: its record was not written \
                         running: {not_recorded}"
                    )
                });
                states.push((stream_id, StreamListingState::Stopped));
                continue;
            }
            let started = held
                .log_route()
                .run_entered(|| self.start_the_stream_paused_while_its_project_is(&held));
            match started {
                Ok(()) => states.push((
                    stream_id,
                    if is_paused(held.status()) {
                        StreamListingState::Paused
                    } else {
                        StreamListingState::Running
                    },
                )),
                Err(start_refusal) => {
                    held.log_route().run_entered(|| {
                        tracing::error!("the stream `{stream_id}` failed: {start_refusal}")
                    });
                    hold_a_stream_whose_start_was_refused_stopped(&held);
                    let still_held = !held.has_ended();
                    if let Err(not_recorded) =
                        self.change_the_record_of_the_stream(&stream_id, |record| {
                            if still_held {
                                record.mark_open_with_its_playback_stopped();
                            }
                            record.failed_because = Some(start_refusal.to_string());
                        })
                    {
                        tracing::error!("{not_recorded}");
                    }
                    states.push((stream_id, StreamListingState::Failed));
                }
            }
        }
        progress.finish(Ok(states));
    }

    /// Queue the reload of the open project in `request`'s directory on its stream action
    /// queue, answering the action's id at once: [`Self::queue_the_load_of_the_stream_assets_of_the_project`]'s load, which
    /// re-reads every stream asset's graph document and replaces each stream under its id,
    /// keeping the project's playback as it is — playing, paused or stopped — and retries its
    /// failed streams. Refused, with nothing queued, for a directory not named by its absolute
    /// path and for a project that is not open, naming [`Self::open_project`].
    pub fn reload_project(
        &self,
        request: ProjectOpenOrReloadRequest,
    ) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to reload; `open_project` \
                 opens it",
                project_directory.display()
            )));
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Ok(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            None,
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// The playback a load of the open project in `project_directory` gives each stream it
    /// loads: playing while the project plays once its queued actions run, or a stream of it
    /// failed while it played, and held stopped otherwise.
    fn the_playback_a_load_of_the_open_project_keeps(
        &self,
        project_directory: &Path,
    ) -> ProjectPlaybackAfterItsLoad {
        let a_stream_failed_while_it_played = || {
            self.current_records_of_the_project(project_directory)
                .iter()
                .any(|record| record.is_failed() && !record.stopped)
        };
        if self.the_project_runs_once_its_queued_actions_run(project_directory)
            || a_stream_failed_while_it_played()
        {
            ProjectPlaybackAfterItsLoad::Playing
        } else {
            ProjectPlaybackAfterItsLoad::HeldStopped
        }
    }

    /// Queue the closing of the open project in `project_directory` on its queue, and answer
    /// the action: it ends the playback of every stream of the project, unloads each, forgets
    /// its function-node states, and records each closed, so the runtime's restart leaves the
    /// project unloaded until an `open_project`. It deletes nothing: the project's files, its
    /// `.venv` and its synced dependencies stay. Each record carries the close as a pending
    /// mark until the action records it closed, so a crash finishes it at the next start.
    /// Refused — with nothing queued — for a directory not named by its absolute path, a
    /// project whose removal is queued, and a project that is not open once its queued actions
    /// run.
    pub fn close_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to close. Loaded: {}. \
                 Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        let action_id = self.next_stream_action_id();
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        let ids_of_the_projects_records =
            self.ids_of_every_record_of_the_project(&project_directory);
        if let Some(stream_records) = self.stream_records() {
            for stream_id in &ids_of_the_projects_records {
                let marked = stream_records.update(stream_id, |record| match record {
                    Some(record) => {
                        refuse_a_stream_whose_project_removal_is_queued(stream_id, record)?;
                        record.queue_the_pending_stream_action(
                            &action_id,
                            PendingStreamActionKind::Close,
                        );
                        Ok::<_, Error>(Some(record.clone()))
                    }
                    None => Ok(None),
                });
                match marked {
                    Ok(Ok(Some(record))) => streams.push(StreamOfAStreamAction {
                        display_name: the_display_name_of_the_stream_at(
                            &record.location,
                            &record.stream_id,
                        ),
                        stream_id: record.stream_id,
                        stream_location: Some(record.location),
                        state: None,
                    }),
                    Ok(Ok(None)) => {}
                    Ok(Err(refusal)) | Err(refusal) => {
                        for stream_marked_already in &streams {
                            self.finish_the_pending_stream_action_on_the_record(
                                &stream_marked_already.stream_id,
                                &action_id,
                            );
                        }
                        return Err(refusal);
                    }
                }
            }
        }
        for loaded in self.loaded_streams_of_the_project(&project_directory) {
            if !ids_of_the_projects_records
                .iter()
                .any(|recorded| recorded == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: Some(loaded.stream_location().clone()),
                    state: None,
                });
            }
        }
        let stream_ids: Vec<String> = streams
            .iter()
            .map(|stream| stream.stream_id.clone())
            .collect();
        self.forget_that_the_project_is_paused(&project_directory);
        let project_directory_closing = project_directory.clone();
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory,
            kind: StreamActionKind::Close,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        Ok(self
            .stream_actions
            .stream_action_queue_of_each_project
            .queue_the_stream_action(
                action,
                Box::new(move |runner: &Runner| {
                    runner.close_the_project_for_its_stream_action(
                        &action_id,
                        &project_directory_closing,
                        &stream_ids,
                    );
                }),
                &self.tokio_handle(),
            ))
    }

    /// Run the close the stream action `action_id` queued of the streams `stream_ids` of the
    /// project in `project_directory`: unload each loaded one, its playback ended, forget its
    /// function-node states, record each closed, close the project's store, and finish the
    /// action done, or failed naming each stream whose record was not written closed.
    fn close_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
        stream_ids: &[String],
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let mut states = Vec::with_capacity(stream_ids.len());
        let mut not_closed = Vec::new();
        for stream_id in stream_ids {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(stream_id) {
                unload_a_stream_an_action_took_back(&loaded, "its project closed");
            }
            self.forget_the_function_node_states_of_the_stream(stream_id);
            let recorded = match self.stream_records() {
                Some(stream_records) => stream_records
                    .change_the_record(stream_id, |record| {
                        record.mark_closed();
                        record.finish_the_pending_stream_action(action_id);
                    })
                    .map(|_| ()),
                None => Ok(()),
            };
            match recorded {
                Ok(()) => states.push((stream_id.clone(), StreamListingState::Closed)),
                Err(not_written) => not_closed.push(format!(
                    "the stream `{stream_id}` is unloaded and was not recorded closed, so the \
                     runtime's restart loads it again: {not_written}"
                )),
            }
        }
        self.engine_resources_shared_by_every_stream
            .project_stores_open_in_this_runtime
            .close_the_store_of_the_project_at(
                project_directory,
                every_node_uuid_the_graph_documents_of_the_project_hold(project_directory).as_ref(),
            );
        progress.finish(if not_closed.is_empty() {
            Ok(states)
        } else {
            Err(not_closed.join("; "))
        });
    }

    /// Queue a load of every stream asset of the project in `project_directory`, absolute and
    /// resolved, on its stream action queue, answering the action's id at once;
    /// [`Runner::describe_stream_action`] follows it. The load reads each stream asset's graph
    /// document, makes the project's `.venv`, unbounded, and loads or fails each stream under
    /// the id its project and stream asset give it: a directory that does not exist or holds no
    /// `tatolab-project.json`, and an id another stream holds, fail the action.
    fn queue_the_load_of_the_stream_assets_of_the_project(
        &self,
        project_directory: PathBuf,
        only_the_stream_assets: Option<BTreeSet<Uuid>>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        kind: StreamActionKind,
    ) -> StreamActionQueued {
        let keeps_each_loaded_stream_whose_node_code_is_unchanged = kind == StreamActionKind::Load;
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: Vec::new(),
            failed_because: None,
        };
        queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.load_the_project_for_its_stream_action(
                    &action_id,
                    project_directory,
                    only_the_stream_assets,
                    playback_after_the_load,
                    keeps_each_loaded_stream_whose_node_code_is_unchanged,
                );
            }),
            &self.tokio_handle(),
        )
    }

    /// Whether the project in `project_directory` plays once the actions queued on it have
    /// run: the last of them that opens, closes or plays it is a playback start, or, with none
    /// queued, a loaded stream of it is not held stopped or a record of it not loaded is
    /// running.
    fn the_project_runs_once_its_queued_actions_run(&self, project_directory: &Path) -> bool {
        match self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory) {
            Some(kind) => kind == StreamActionKind::StartPlayback,
            None => {
                let loaded_streams_of_the_project =
                    self.loaded_streams_of_the_project(project_directory);
                loaded_streams_of_the_project
                    .iter()
                    .any(|loaded| !loaded.is_held_stopped())
                    || self
                        .current_records_of_the_project(project_directory)
                        .iter()
                        .filter(|record| {
                            !loaded_streams_of_the_project
                                .iter()
                                .any(|loaded| loaded.stream_id() == record.stream_id)
                        })
                        .any(|record| record.listing_state() == StreamListingState::Running)
            }
        }
    }

    /// Every record of the project in `project_directory` that reads.
    fn current_records_of_the_project(&self, project_directory: &Path) -> Vec<StreamRecord> {
        self.ids_of_every_record_of_the_project(project_directory)
            .iter()
            .filter_map(|stream_id| {
                self.current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// Whether the project in `project_directory` is paused, or a loaded stream of it is.
    fn is_the_project_paused(&self, project_directory: &Path) -> bool {
        self.stream_actions
            .directories_of_the_paused_projects
            .lock()
            .iter()
            .any(|paused| the_same_directory(paused, project_directory))
            || self
                .loaded_streams_of_the_project(project_directory)
                .iter()
                .any(|stream| is_paused(stream.status()))
    }

    /// Create a project from the starter `tatolab new` writes, or from the sample project
    /// `request` names, and queue its open, answering the action's id and the id its stream
    /// asset `main` loads as at once. The directory is `request`'s, created when missing, or one
    /// drawn under `$HOME/streams`. The action writes the starter's files or copies the
    /// sample's under a new project UUID, has the host commit them as the first commit of the
    /// project's history, then opens it as [`Runner::open_project`] does, the stream asset
    /// `main` shown under the folder's name and held stopped; a directory already holding a file
    /// the create writes fails the action with nothing written. Refused for a sample the runtime
    /// unit does not carry, naming the ones it does, a relative directory, one that cannot be
    /// created, and a draw that finds no free name.
    pub fn create_project(&self, request: CreateProjectRequest) -> Result<ProjectCreationQueued> {
        let sample = request
            .sample
            .as_deref()
            .map(|sample_name| {
                the_sample_project_named(
                    self.sample_projects_directory_of_this_runtime_unit()
                        .as_deref(),
                    sample_name,
                )
                .map_err(|refusal| Error::Configuration(refusal.to_string()))
            })
            .transpose()?;
        let project_directory = match request.project_directory {
            Some(project_directory) => {
                if !project_directory.is_absolute() {
                    return Err(Error::Configuration(format!(
                        "`project_directory` must be an absolute path; got `{}`",
                        project_directory.display()
                    )));
                }
                std::fs::create_dir_all(&project_directory).map_err(|not_created| {
                    Error::Configuration(format!(
                        "{} could not be created, so no project was created there: {not_created}",
                        project_directory.display()
                    ))
                })?;
                project_directory
            }
            None => draw_new_project_directory_under_the_streams_root()?,
        };
        let project_directory =
            std::fs::canonicalize(&project_directory).unwrap_or(project_directory);
        let scaffold = match sample {
            Some(sample) => {
                NewProjectScaffold::of_a_new_project_from_a_sample_in(&project_directory, sample)
            }
            None => NewProjectScaffold::of_a_new_project_in(&project_directory),
        };
        let stream_id = scaffold.stream_id_in(&project_directory);
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::Open,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: vec![StreamOfAStreamAction {
                stream_id: stream_id.clone(),
                display_name: scaffold.project_name.clone(),
                stream_location: Some(scaffold.stream_location_in(&project_directory)),
                state: None,
            }],
            failed_because: None,
        };
        let project_directory_the_action_creates = project_directory.clone();
        let queued = queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.create_the_project_for_its_stream_action(
                    &action_id,
                    project_directory_the_action_creates,
                    &scaffold,
                );
            }),
            &self.tokio_handle(),
        );
        Ok(ProjectCreationQueued {
            action_id: queued.action_id,
            project_directory: queued.project_directory,
            stream_id,
        })
    }

    /// Every sample project the runtime unit carries beside this runtime's lend, in name order;
    /// none when it was handed no lend.
    pub fn list_samples(&self) -> ListSamplesToolResult {
        ListSamplesToolResult {
            samples: self
                .sample_projects_directory_of_this_runtime_unit()
                .map(|samples_directory| every_sample_project_in(&samples_directory))
                .unwrap_or_default()
                .into_iter()
                .map(|sample| ListSamplesToolResultSample {
                    name: sample.listed.name,
                    title: sample.listed.title,
                    summary: sample.listed.summary,
                })
                .collect(),
        }
    }

    fn sample_projects_directory_of_this_runtime_unit(&self) -> Option<PathBuf> {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .and_then(sample_projects_directory_beside_the_lend)
    }

    /// Forget every function-node state of `stream_id`, in memory and in its project's
    /// store, as its playback ends, its project closes or its record is forgotten.
    fn forget_the_function_node_states_of_the_stream(&self, stream_id: &str) {
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .forget_every_state_of_the_stream(stream_id);
    }

    /// Unload and forget the stream of `project`'s stream asset `stream_asset`, whose graph
    /// document was deleted, answering its id; `None` when the runtime records none.
    pub fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>> {
        let Some(stream_records) = self.stream_records() else {
            return Ok(None);
        };
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let Some(record) = stream_records
            .read_every()
            .into_iter()
            .find_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.project == project
                        && record.location.stream_asset == stream_asset =>
                {
                    Some(record)
                }
                _ => None,
            })
        else {
            return Ok(None);
        };
        if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(&record.stream_id) {
            unload_a_stream_an_action_took_back(&loaded, "its stream asset was deleted");
        }
        stream_records.remove(&record.stream_id)?;
        self.forget_the_function_node_states_of_the_stream(&record.stream_id);
        Ok(Some(record.stream_id))
    }

    /// The stream action `action_id`, refused when the engine never held it or
    /// let it go among its project's older finished actions; its records stay
    /// in the project's log.
    pub fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .held_stream_action(action_id)
            .ok_or_else(|| a_stream_action_not_held(action_id))
    }

    /// Every queued or running stream action, the earliest queued first.
    pub fn stream_actions_under_way(&self) -> Vec<StreamAction> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
    }

    /// Receive the id of each stream action as it is queued or changes — its state, its step,
    /// its streams or its venv sync — from now on.
    pub fn listen_for_each_stream_action_change(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .listen_for_each_stream_action_change()
    }

    /// Cancel the stream action `action_id`. One still queued is dropped,
    /// its pending marks taken off its streams' records, and finished
    /// cancelled. A running load stops at the step it is on and
    /// finishes cancelled once it sees the cancel: a stream it was loading is
    /// recorded failed, and the streams it had not reached stay as recorded.
    /// Refused for a running stop, close or removal, whose unload cannot be cut
    /// short, and for an action already finished or not held.
    pub fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        let cancelled = self
            .stream_actions
            .stream_action_queue_of_each_project
            .cancel_the_stream_action(action_id, refuse_the_cancel_of_a_running_unload)?;
        let action = match cancelled {
            StreamActionCancelOutcome::DroppedFromItsQueue(action) => {
                for stream in &action.streams {
                    self.finish_the_pending_stream_action_on_the_record(
                        &stream.stream_id,
                        action_id,
                    );
                }
                action
            }
            StreamActionCancelOutcome::AskedOfTheRunningAction(action) => action,
        };
        ProjectLogRouteEntry::for_the_project_in(
            &action.project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
            Some(action_id),
        )
        .run_entered(|| {
            tracing::info!(
                "the stream action {action_id} ({}) was asked to cancel while {}",
                action.kind.as_str(),
                action.state.as_str()
            )
        });
        Ok(action)
    }

    /// Run the load the stream action `action_id` queued of the project in
    /// `project_directory`, reporting its steps, and finish the action done,
    /// failed, or cancelled once it saw its cancel.
    fn load_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: PathBuf,
        only_the_stream_assets: Option<BTreeSet<Uuid>>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        keeps_each_loaded_stream_whose_node_code_is_unchanged: bool,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        self.load_the_project_reporting_to(
            action_id,
            &progress,
            ProjectStreamsToReadAndLoad {
                project_directory,
                only_the_stream_assets,
                playback_after_the_load,
                keeps_each_loaded_stream_whose_node_code_is_unchanged,
            },
        );
    }

    /// Run the load of the project in `project_directory` for the stream
    /// action `action_id`, reporting to `progress`, and finish the action.
    fn load_the_project_reporting_to(
        &self,
        action_id: &str,
        progress: &StreamActionProgress<'_>,
        request: ProjectStreamsToReadAndLoad,
    ) {
        let run_outcome = match why_a_directory_is_no_project(&request.project_directory) {
            Some(why_it_is_no_project) => {
                tracing::warn!("the stream action {action_id} failed: {why_it_is_no_project}");
                Err(why_it_is_no_project)
            }
            None => self
                .run_the_project_reporting_to(request, Some(progress), &|| {
                    progress.is_interrupted()
                })
                .map_err(|refusal| refusal.to_string()),
        };
        self.finish_the_load_action(action_id, progress, run_outcome);
    }

    /// Write `scaffold` into `project_directory` for the stream action
    /// `action_id` [`Runner::create_project`] queued, have the host commit it as the first
    /// commit of the project's history, then run its load; a scaffold that cannot be written
    /// or committed fails the action, written to the project's log, and a cancel seen before
    /// the write ends it cancelled with nothing written.
    fn create_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: PathBuf,
        scaffold: &NewProjectScaffold,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        if progress.is_interrupted() {
            self.finish_the_load_action(
                action_id,
                &progress,
                Err("it was interrupted before the project's files were written".to_string()),
            );
            return;
        }
        let written = write_the_new_project_scaffold_into(&project_directory, scaffold)
            .and_then(|()| self.commit_the_first_revision_of_a_created_project(&project_directory));
        if let Err(not_written) = written {
            ProjectLogRouteEntry::for_the_project_in(
                &project_directory,
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
                Some(action_id),
            )
            .run_entered(|| tracing::warn!("the stream action {action_id} failed: {not_written}"));
            self.finish_the_load_action(action_id, &progress, Err(not_written.to_string()));
            return;
        }
        self.load_the_project_reporting_to(
            action_id,
            &progress,
            ProjectStreamsToReadAndLoad {
                project_directory,
                only_the_stream_assets: None,
                playback_after_the_load: ProjectPlaybackAfterItsLoad::HeldStopped,
                keeps_each_loaded_stream_whose_node_code_is_unchanged: false,
            },
        );
    }

    /// Finish the load action `action_id` done with the streams `run_outcome`
    /// gave, failed for why, or cancelled once `progress` saw its cancel.
    fn finish_the_load_action(
        &self,
        action_id: &str,
        progress: &StreamActionProgress<'_>,
        run_outcome: std::result::Result<StreamRunOutcome, String>,
    ) {
        let saw_its_cancel = progress.saw_its_cancel();
        self.stream_actions
            .stream_action_queue_of_each_project
            .change_the_held_stream_action(action_id, |action| {
                action.step = None;
                match run_outcome {
                    Ok(run_outcome) => {
                        action.state = if saw_its_cancel {
                            StreamActionState::Cancelled
                        } else {
                            StreamActionState::Done
                        };
                        action.streams = run_outcome
                            .streams
                            .into_iter()
                            .map(|stream| StreamOfAStreamAction {
                                state: Some(match stream.state {
                                    StreamStateAfterItsRun::Running { .. } => {
                                        StreamListingState::Running
                                    }
                                    StreamStateAfterItsRun::HeldStopped { .. } => {
                                        StreamListingState::Stopped
                                    }
                                    StreamStateAfterItsRun::Failed { .. } => {
                                        StreamListingState::Failed
                                    }
                                }),
                                stream_id: stream.stream_id,
                                display_name: stream.display_name,
                                stream_location: Some(stream.stream_location),
                            })
                            .collect();
                    }
                    Err(_) if saw_its_cancel => action.state = StreamActionState::Cancelled,
                    Err(failed_because) => {
                        action.state = StreamActionState::Failed;
                        action.failed_because = Some(failed_because);
                    }
                }
            });
    }

    /// Show each stream an action under way works on `preparing` — or
    /// `stopping`, for an action that unloads it — with the action and its
    /// step and no `failed_because`, the earliest action on a stream winning:
    /// the streams the action names, else the project's listed ones.
    fn show_the_streams_actions_under_way_prepare(&self, listings: &mut Vec<StreamListing>) {
        let mut streams_an_earlier_action_works_on: Vec<String> = Vec::new();
        for action in self
            .stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
        {
            if !action.kind.works_on_its_projects_streams() {
                continue;
            }
            let working_on_it = StreamActionWorkingOnAStream {
                action_id: action.action_id.clone(),
                step: action.step,
            };
            let state_it_shows = if action.kind.unloads_its_streams() {
                StreamListingState::Stopping
            } else {
                StreamListingState::Preparing
            };
            let streams_it_works_on: Vec<(String, String, Option<StreamLocation>)> = if action
                .streams
                .is_empty()
            {
                let listed_of_the_project: Vec<(String, String, Option<StreamLocation>)> = listings
                    .iter()
                    .filter_map(|listing| {
                        let location = listing.stream_location.as_ref()?;
                        the_same_directory(&location.project_directory, &action.project_directory)
                            .then(|| {
                                (
                                    listing.stream_id.clone(),
                                    listing.display_name.clone(),
                                    Some(location.clone()),
                                )
                            })
                    })
                    .collect();
                if listed_of_the_project
                    .iter()
                    .any(|(stream_id, _, _)| streams_an_earlier_action_works_on.contains(stream_id))
                {
                    continue;
                }
                listed_of_the_project
            } else {
                action
                    .streams
                    .iter()
                    .filter(|stream| stream.state.is_none())
                    .map(|stream| {
                        (
                            stream.stream_id.clone(),
                            stream.display_name.clone(),
                            stream.stream_location.clone(),
                        )
                    })
                    .collect()
            };
            for (stream_id, display_name, stream_location) in streams_it_works_on {
                if streams_an_earlier_action_works_on.contains(&stream_id) {
                    continue;
                }
                streams_an_earlier_action_works_on.push(stream_id.clone());
                match listings
                    .iter_mut()
                    .find(|listing| listing.stream_id == stream_id)
                {
                    Some(listing) => {
                        listing.state = state_it_shows;
                        listing.failed_because = None;
                        listing.stream_action_under_way = Some(working_on_it.clone());
                    }
                    None => listings.push(StreamListing {
                        stream_id,
                        display_name,
                        stream_location,
                        state: state_it_shows,
                        node_count: None,
                        failed_because: None,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: Some(working_on_it.clone()),
                    }),
                }
            }
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

/// How a run refused for an id a stream another caller loaded holds is told to
/// go on.
const STOP_IT_FIRST_TO_LOAD_THIS_ONE: &str = "stop it first to load this one";

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

/// Refuse a load of `stream_location` under `stream_id` when the id is held by
/// another stream — loaded, or recorded — or by the same stream of another copy of its
/// project, naming both. A record an earlier runtime wrote from the same project
/// directory holds nothing: the load writes its own record over it.
fn refuse_an_id_another_location_holds(
    stream_id: &str,
    stream_location: &StreamLocation,
    loaded: Option<&LoadedStreamInThisRuntime>,
    record: Option<&StreamRecordAsRead>,
) -> Result<()> {
    let refuse = |held_by: String| {
        Err(Error::GraphError(format!(
            "the stream id `{stream_id}` of {stream_location} is held by {held_by}; two \
             streams cannot share one id, so `rm` the stream that holds it, or give a second \
             copy of a project its own project UUID"
        )))
    };
    let held_by_another = |held: &StreamLocation| {
        !held.is_the_same_stream_as(stream_location)
            || another_copy_of_the_project_holds(held, &stream_location.project_directory)
    };
    if let Some(loaded) = loaded {
        let loaded_location = loaded.stream_location();
        if held_by_another(loaded_location) {
            return refuse(format!("the loaded stream from {loaded_location}"));
        }
    }
    match record {
        Some(StreamRecordAsRead::Current(record)) if held_by_another(&record.location) => {
            refuse(format!(
                "a{} recorded stream from {}",
                the_state_a_refusal_names_a_record_by(record),
                record.location
            ))
        }
        Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))
            if earlier
                .project_directory
                .as_deref()
                .is_some_and(|recorded_directory| {
                    the_same_directory(recorded_directory, &stream_location.project_directory)
                }) =>
        {
            Ok(())
        }
        Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => refuse(format!(
            "a record an earlier runtime wrote{}",
            earlier
                .project_directory
                .as_ref()
                .map(|project_directory| format!(" from {}", project_directory.display()))
                .unwrap_or_default()
        )),
        _ => Ok(()),
    }
}

/// One stream asset a run read, the id it records it under, and what interrupts its load.
struct StreamARunRead<'run> {
    stream_id: String,
    stream_asset: StreamAssetRead,
    stream_environment: &'run StreamEnvironment,
    playback_after_the_load: ProjectPlaybackAfterItsLoad,
    /// Why the project's `.venv` was not made, which holds the stream stopped and failed.
    why_the_projects_venv_was_not_made: Option<&'run str>,
    is_interrupted: &'run dyn Fn() -> bool,
}

/// The project directory whose `.venv/bin/python` is `interpreter`; `None` when
/// it is not one.
fn the_project_directory_whose_venv_holds(interpreter: &Path) -> Option<PathBuf> {
    let venv_bin = interpreter.parent()?;
    let venv = venv_bin.parent()?;
    if interpreter.file_name()? != "python"
        || venv_bin.file_name()? != "bin"
        || venv.file_name()? != ".venv"
    {
        return None;
    }
    venv.parent().map(Path::to_path_buf)
}

/// A project's log entered on the thread of one load of the project; on drop,
/// every record the load wrote is written to the log and synced, so a reader
/// right after the load finds them.
struct TheProjectsLogEnteredForOneLoad {
    entry: Arc<ProjectLogRouteEntry>,
    _entered_on_this_thread: ProjectLogRouteEntryEnteredOnThisThread,
}

impl Drop for TheProjectsLogEnteredForOneLoad {
    fn drop(&mut self) {
        self.entry.write_every_record_it_emitted_and_sync();
    }
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

/// Record `stream_record` failed for `refusal`, its load or start refused, and
/// answer it failed — unless a machine shutdown abandoned it, when the refusal
/// is returned and the record left as it was.
fn record_a_refused_load_failed(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_record: &mut StreamRecord,
    refusal: Error,
) -> Result<StreamStateAfterItsRun> {
    if is_the_machines_shutdown_requested() {
        return Err(refusal);
    }
    record_a_stream_held_from_its_document_failed(
        stream_records,
        stream_record,
        refusal.to_string(),
    )
}

/// Record the stream `stream_record` records failed with `failed_because`; a record not
/// written is logged.
fn record_a_stream_held_from_its_document_failed(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_record: &mut StreamRecord,
    failed_because: String,
) -> Result<StreamStateAfterItsRun> {
    stream_record.failed_because = Some(failed_because.clone());
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

/// Why a removal finished failed: the records in `not_forgotten`, each with why.
fn the_records_not_forgotten(not_forgotten: &[String]) -> String {
    format!(
        "the project's streams are unloaded, and these records were not forgotten, so \
         they stay listed: {}",
        not_forgotten.join("; ")
    )
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

/// Finish, in order, every pending stream action a crash left on a record
/// of `stream_records`: a removal forgets the record, and a stop records it
/// stopped.
fn finish_the_pending_stream_actions_a_crash_left_on_the_records(
    stream_records: &StreamRecordsInTheStateDirectory,
) {
    for read in stream_records.read_every() {
        let Ok(StreamRecordAsRead::Current(record)) = read else {
            continue;
        };
        if record.pending_stream_actions.is_empty() {
            continue;
        }
        let stream_id = record.stream_id;
        let finished = stream_records.update(&stream_id, |recorded| {
            let Some(record) = recorded.as_mut() else {
                return;
            };
            let pending_stream_actions = std::mem::take(&mut record.pending_stream_actions);
            for pending in &pending_stream_actions {
                tracing::info!(
                    "the stream action {} left the stream `{stream_id}` pending its {:?}; \
                     it is finished now",
                    pending.action_id,
                    pending.kind
                );
            }
            if pending_stream_actions
                .iter()
                .any(|pending| pending.kind == PendingStreamActionKind::Remove)
            {
                *recorded = None;
                return;
            }
            for pending in pending_stream_actions {
                match pending.kind {
                    PendingStreamActionKind::Stop => record.stopped = true,
                    PendingStreamActionKind::Close => record.mark_closed(),
                    PendingStreamActionKind::Remove => {}
                }
            }
        });
        if let Err(not_finished) = finished {
            tracing::error!(
                "the pending stream actions of the stream `{stream_id}` were not finished: \
                 {not_finished}"
            );
        }
    }
}

/// Refuse a display name that is empty or only whitespace: a stream is always
/// shown under something.
/// Log what the runtime's previous run crashed on, a crash no stream owns as
/// the runtime bug it is.
fn log_the_previous_runtime_runs_crash(crash: &CrashOfThePreviousRuntimeRun) {
    for (stream_id, cause) in &crash.causes_by_implicated_stream {
        tracing::error!(
            "the runtime's previous run crashed, implicating the stream `{stream_id}`: {cause}"
        );
    }
    for cause in &crash.causes_on_threads_no_stream_owns {
        tracing::error!(
            "the runtime's previous run crashed on a thread no stream owns: {cause}; this is a \
             runtime bug to file"
        );
    }
    if crash.left_no_trace() {
        tracing::warn!(
            "the runtime's previous run ended without a clean stop and recorded no crash — a \
             SIGKILL, or an out-of-memory kill; repeated, it is a runtime bug to file"
        );
    }
}

/// The state a refusal names a record by, a leading space before it:
/// ` stopped`, ` closed`, ` failed`, or nothing for one the runtime re-loads playing.
fn the_state_a_refusal_names_a_record_by(record: &StreamRecord) -> &'static str {
    match record.listing_state() {
        StreamListingState::Failed => " failed",
        StreamListingState::Stopped => " stopped",
        StreamListingState::Closed => " closed",
        StreamListingState::Running
        | StreamListingState::Paused
        | StreamListingState::Preparing
        | StreamListingState::Stopping => "",
    }
}

/// Ask every one of `streams` for its shutdown at once, so their teardowns run
/// side by side within one teardown's budget, then unload each as
/// [`unload_a_stream_an_action_took_back`] does.
fn unload_every_stream_an_action_took_back(
    streams: &[Arc<LoadedStreamInThisRuntime>],
    reason: &str,
) {
    for stream in streams {
        stream.ask_for_this_streams_shutdown(reason);
    }
    for stream in streams {
        unload_a_stream_an_action_took_back(stream, reason);
    }
}

/// Ask `stream` for its shutdown, wait until it has ended, and log a failing
/// end in its own log — the action that unloaded it goes on regardless.
/// Hold `stream`, whose start was refused, stopped with its graph kept to read and edit;
/// unloaded instead while the machine shuts down, or when its graph is not put back.
/// `stream` as an action names it before taking it in.
fn stream_of_a_stream_action_not_yet_taken_in(
    stream: &LoadedStreamInThisRuntime,
) -> StreamOfAStreamAction {
    StreamOfAStreamAction {
        stream_id: stream.stream_id().to_string(),
        display_name: stream.display_name(),
        stream_location: Some(stream.stream_location().clone()),
        state: None,
    }
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

/// The stop of the recorded stream `stream_id`, unloaded already, whose
/// record could not be marked stopped because of `not_recorded`.
fn a_recorded_stream_unloaded_and_not_recorded_stopped(
    stream_id: String,
    not_recorded: &dyn std::fmt::Display,
) -> StreamStopOutcome {
    tracing::warn!(
        "the stream `{stream_id}` was unloaded, and is not recorded stopped, so a restart of \
         this runtime loads it again: {not_recorded}"
    );
    StreamStopOutcome {
        stream_id,
        stop_not_recorded_because: Some(not_recorded.to_string()),
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

#[cfg(test)]
mod tests;
