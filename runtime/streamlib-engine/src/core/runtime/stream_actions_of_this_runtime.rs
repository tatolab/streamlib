// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions a runtime takes for its owner: run a project's stream assets, each
//! loaded from its graph document and recorded under the id its project and stream asset
//! give it — failed when its document does not read or its load is refused; stop and
//! remove a project; rename a stream; list what the runtime holds; and re-load the recorded streams
//! at start.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_project_repository::{
    PROJECT_FILE_NAME, ProjectStreamDocument, TatolabProjectFile, assets_a_node_config_references,
    every_asset_uuid_the_project_holds,
    graph_document_with_each_stream_fragment_reference_resolved, project_stream_documents,
};
use uuid::Uuid;

use super::new_project_name_drawing::draw_new_project_directory_under_the_streams_root;
use super::new_project_scaffold::{NewProjectScaffold, write_the_new_project_scaffold_into};
use super::project_stream_action_queue::{
    StreamAction, StreamActionCancelOutcome, StreamActionKind, StreamActionProgress,
    StreamActionQueueOfEachProject, StreamActionQueued, StreamActionState, StreamActionStep,
    StreamActionWorkingOnAStream, StreamOfAStreamAction, a_stream_action_not_held,
};
use super::project_venv_made_with_the_runtime_unit_uv::{
    ProjectVenvNotMade, every_first_party_module_folder_in,
    lock_the_projects_python_requirements_with_the_runtime_unit_uv,
    make_the_projects_venv_current_with_the_runtime_unit_uv, the_projects_venv_interpreter,
};
use super::runtime::{
    a_stream_id_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
    what_interrupted_a_load,
};
use super::{
    CrashOfThePreviousRuntimeRun, FunctionNodeState, HowThePreviousRuntimeRunEnded,
    LoadedStreamInThisRuntime, LoadedStreamTag, OptionsForLoadingOneStream,
    PendingStreamActionKind, Runner, RuntimeStatus, StreamEnvironment,
    StreamLoadObservingInterrupts, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordReadFailure, StreamRecordsInTheStateDirectory, is_the_machines_shutdown_requested,
};
use crate::core::compiler::compiler_ops::cross_floor_check_in_the_projects_interpreter::warn_of_what_binds_the_project_to_one_floor;
use crate::core::context::{CaptureDeviceAccessPrompting, StreamClockReading};
use crate::core::error::PrivacyGatedCaptureDevice;
use crate::core::graph::names_listed_for_a_refusal;
use crate::core::graph_snapshot::{GraphSnapshot, refuse_an_integer_literal_wider_than_64_bits};
use crate::core::logging::{ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread};
use crate::core::{Error, Result};
use streamlib_consumer_rhi::runtime_unit_layout::first_party_modules_directory_beside_the_lend;

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

/// The project [`Runner::start_project`] or [`Runner::reload_project`] queues a load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStartOrReloadRequest {
    /// The project whose every stream asset is loaded, by absolute path.
    pub project_directory: PathBuf,
}

/// The project [`Runner::create_project`] creates and queues a load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectRequest {
    /// Where the project is created, by absolute path; `None` draws an
    /// `<adjective>-<noun>` folder under `$HOME/streams`.
    pub project_directory: Option<PathBuf>,
}

/// The load [`Runner::create_project`] queued of the project it created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCreationQueued {
    /// The queued action's id, which [`Runner::describe_stream_action`] follows.
    pub action_id: String,
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of the stream the scaffold's `streams/main.graph.json` loads as.
    pub stream_id: String,
}

/// What a stream action's load of a project reads and loads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectStreamsToReadAndLoad {
    /// The project, by absolute path.
    project_directory: PathBuf,
    /// The display name a stream the load records for the first time is
    /// given; `None` names it after its graph document.
    display_name_of_a_stream_first_recorded: Option<String>,
}

/// A project as a load reads it: where it sits, its project file, and each stream asset's
/// graph document, read as it is on disk.
struct ProjectAsALoadReadsIt {
    /// The project's directory, symlinks resolved.
    project_directory: PathBuf,
    /// What its `tatolab-project.json` holds.
    project_file: TatolabProjectFile,
    /// Each stream asset's graph document, sorted by path.
    stream_documents: Vec<ProjectStreamDocument>,
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
        let stream_documents = project_stream_documents(&project_directory).map_err(refused)?;
        let asset_uuids_the_project_holds =
            every_asset_uuid_the_project_holds(&project_directory, project_file.project)
                .map_err(refused)?;
        Ok(Self {
            project_directory,
            project_file,
            stream_documents,
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
    fn stream_asset_read_from(&self, stream_document: &ProjectStreamDocument) -> StreamAssetRead {
        StreamAssetRead {
            stream_location: self.location_of(stream_document),
            stream_name: stream_document.stream_name().to_string(),
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
    let project_directory = &record.location.project_directory;
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)
        .map_err(|refusal| format!("its project is not where it was recorded: {refusal}"))?;
    if project_file.project != record.location.project {
        return Err(format!(
            "{} holds the project {} now, not the project {} it was recorded from",
            project_directory.display(),
            project_file.project,
            record.location.project
        ));
    }
    let stream_documents = project_stream_documents(project_directory)
        .map_err(|refusal| format!("its project's graph documents do not read: {refusal}"))?;
    let stream_document = stream_documents
        .iter()
        .find(|stream_document| {
            stream_document.stream_asset(project_file.project) == record.location.stream_asset
        })
        .ok_or_else(|| {
            format!(
                "no graph document in {} holds its stream asset {} any more",
                project_directory.display(),
                record.location.stream_asset
            )
        })?;
    let asset_uuids_the_project_holds =
        every_asset_uuid_the_project_holds(project_directory, project_file.project)
            .map_err(|refusal| format!("its project's assets do not read: {refusal}"))?;
    the_graph_a_stream_document_holds(
        project_directory,
        stream_document,
        &asset_uuids_the_project_holds,
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

/// One stream a load of a project loaded and started, or recorded failed.
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
        state: StreamStateAfterItsRun,
        replaced_a_recorded_stream: bool,
    ) -> Self {
        Self {
            stream_id: record.stream_id,
            display_name: record.display_name,
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
    /// Recorded failed and not loaded: its code gave no graph, or its load or
    /// start was refused.
    Failed {
        /// Why — a traceback, or the refusal.
        failed_because: String,
    },
}

/// A stream [`Runner::stop_project`] unloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamStopOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Why a recorded stream unloaded here was not recorded stopped, so a
    /// restart of the runtime loads it again; `None` when it was recorded,
    /// and for a stream its host process loaded.
    pub stop_not_recorded_because: Option<String>,
}

/// The project [`Runner::pause_project`] paused or [`Runner::resume_project`] resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPausedOrResumed {
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The id of each loaded stream of the project paused or resumed now.
    pub stream_ids: Vec<String>,
}

/// A stream [`Runner::rename_stream`] shows under a new display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRenameOutcome {
    /// The stream's id, which a rename leaves as it was.
    pub stream_id: String,
    /// The label the stream is shown under now.
    pub display_name: String,
}

/// The state one stream of [`Runner::list_streams`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamListingState {
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the
    /// runtime's next start.
    Running,
    /// Stopped with its project, and left unloaded until `start_project`.
    Stopped,
    /// Loaded with its project paused: its processors hold their state and process nothing
    /// until `resume_project`.
    Paused,
    /// It kept crashing the runtime or could not re-load at its start, and is
    /// skipped at the start until a `start_project` or `reload_project` retries it; or it was
    /// recorded by an earlier runtime, and is never loaded.
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
    /// Where the stream's code lives; `None` for a stream a library host
    /// loaded, and for a record an earlier runtime wrote.
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
/// did with one record; a stream it left unloaded because it is stopped,
/// removed or already loaded is not reported.
#[derive(Debug)]
pub enum StreamReloadAtTheStart {
    /// The recorded stream loaded and started.
    Reloaded {
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
    /// The directory of each project `pause_project` paused, held across the start of each
    /// stream a stream action starts, so a stream of a paused project starts paused. Live
    /// state only: a restart of the runtime starts every project running.
    directories_of_the_paused_projects: Mutex<Vec<PathBuf>>,
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
    /// The function-node states the record kept at the stream's last end.
    function_node_states_recorded: BTreeMap<String, FunctionNodeState>,
    /// Abandons the load once it reports true.
    is_interrupted: &'load dyn Fn() -> bool,
}

impl<'load> OneStreamAStreamActionLoads<'load> {
    /// The stream `record` records, with the graph `graph` its document holds, its load
    /// abandoned by a machine shutdown request.
    fn recorded_by(record: &'load StreamRecord, graph: &'load GraphSnapshot) -> Self {
        Self {
            stream_id: &record.stream_id,
            display_name: &record.display_name,
            stream_location: &record.location,
            graph,
            stream_environment: record.stream_environment(),
            capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
            function_node_states_recorded: record.function_node_states(),
            is_interrupted: &is_the_machines_shutdown_requested,
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
            .set(Arc::clone(&stream_records))
            .map_err(|_| {
                Error::Configuration(format!(
                    "this runtime was handed a streams directory twice at once; it records its \
                     streams in {} only",
                    streams_directory.display()
                ))
            })?;
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .record_the_states_of_each_stream_at_its_end_through(Arc::new(
                move |stream_id, states| {
                    if let Err(not_recorded) = stream_records
                        .change_the_record(stream_id, |record| {
                            record.keep_the_function_node_states(&states)
                        })
                    {
                        tracing::warn!(
                            "the stream `{stream_id}` ended, and its function nodes' state was \
                             not recorded, so a restart of this runtime starts them from their \
                             defaults: {not_recorded}"
                        );
                    }
                },
            ));
        Ok(())
    }

    /// Read the request's project — its project file and each stream asset's graph document
    /// — make its `.venv` unless its interpreter runs, then record, load and start each stream
    /// asset under the id its project and stream asset give it.
    ///
    /// A load runs no project code and is never refused for what a document or the project's
    /// environment holds: a document that does not read, or whose load or start is refused,
    /// records its stream failed with the reason, and a `.venv` that cannot be made records
    /// every stream of the project failed. A record of the project whose stream asset no
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
            display_name_of_a_stream_first_recorded,
        } = request;
        let Some(stream_records) = self.stream_records() else {
            return Err(Error::Configuration(format!(
                "the stream in {} was not run: this runtime records no stream, because its \
                 host gave it no state directory to record them in; run it in `tatolabd`",
                project_directory.display()
            )));
        };
        report_the_step(StreamActionStep::ReadingTheGraphDocuments);
        let project = ProjectAsALoadReadsIt::read(&project_directory)?;
        let mut stream_assets: Vec<StreamAssetRead> = project
            .stream_documents
            .iter()
            .map(|stream_document| project.stream_asset_read_from(stream_document))
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
                            display_name: recorded.map_or_else(
                                || stream_asset.stream_name.clone(),
                                |record| record.display_name,
                            ),
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
        match self.make_the_projects_venv(&project.project_directory, is_interrupted) {
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
                for stream_asset in &mut stream_assets {
                    stream_asset.graph_or_why_none = Err(failed_because.clone());
                }
            }
        }

        report_the_step(StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_assets_the_project_holds: Vec<Uuid> = stream_assets
            .iter()
            .map(|stream_asset| stream_asset.stream_location.stream_asset)
            .collect();
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
            streams_loaded_or_failed.push(self.record_and_load_one_stream_a_run_read(
                stream_records,
                StreamARunRead {
                    stream_id,
                    stream_asset,
                    stream_environment: &stream_environment,
                    display_name_of_a_stream_first_recorded:
                        display_name_of_a_stream_first_recorded.as_deref(),
                    is_interrupted,
                },
            )?);
        }
        self.forget_every_record_of_the_project_whose_stream_asset_is_gone(
            stream_records,
            &project,
            &stream_assets_the_project_holds,
        );
        Ok(StreamRunOutcome {
            project_directory,
            streams: streams_loaded_or_failed,
        })
    }

    /// Whether the last load or stop under way on the project in
    /// `project_directory` is the load, so every stream of it runs once those actions have run.
    fn is_a_run_the_last_action_queued_on_the_project(&self, project_directory: &Path) -> bool {
        self.stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
            .iter()
            .rev()
            .find(|action| {
                the_same_directory(&action.project_directory, project_directory)
                    && matches!(action.kind, StreamActionKind::Load | StreamActionKind::Stop)
            })
            .is_some_and(|action| action.kind == StreamActionKind::Load)
    }

    /// Queue a stop of the whole project in `project_directory` on its queue, and answer the
    /// action: it unloads every loaded stream of the project and records each recorded one
    /// stopped, so the runtime leaves the project stopped across its restarts until a
    /// `start_project`. Each record running once the actions queued before run carries the stop
    /// as a pending mark until the action records it stopped, so a crash finishes it at the
    /// next start. Refused — with nothing queued — for a directory not named by its absolute
    /// path, a project whose removal is queued, and a project none of whose streams runs once
    /// the actions queued before run.
    pub fn stop_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
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
            self.is_a_run_the_last_action_queued_on_the_project(&project_directory);
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
                        stream_id: record.stream_id,
                        display_name: record.display_name,
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
                    stream_location: loaded.stream_location().cloned(),
                    state: None,
                });
            }
        }
        if streams.is_empty() {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} runs, so there is none to stop; `run` runs it. \
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
        for stream_id in self.ids_of_every_record_of_the_project(&project_directory) {
            let marked = match self.stream_records() {
                Some(stream_records) => {
                    match stream_records.change_the_record(&stream_id, |record| {
                        record.queue_the_pending_stream_action(
                            &action_id,
                            PendingStreamActionKind::Remove,
                        );
                    }) {
                        Ok(marked) => marked,
                        Err(not_marked) => {
                            for stream_marked_already in &streams {
                                self.finish_the_pending_stream_action_on_the_record(
                                    &stream_marked_already.stream_id,
                                    &action_id,
                                );
                            }
                            return Err(not_marked);
                        }
                    }
                }
                None => None,
            };
            streams.push(StreamOfAStreamAction {
                display_name: marked
                    .as_ref()
                    .map_or_else(|| stream_id.clone(), |record| record.display_name.clone()),
                stream_location: marked.map(|record| record.location),
                stream_id,
                state: None,
            });
        }
        for loaded in self.every_loaded_stream() {
            if the_same_directory(loaded.project_directory(), &project_directory)
                && !streams
                    .iter()
                    .any(|stream| stream.stream_id == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: loaded.stream_location().cloned(),
                    state: None,
                });
            }
        }
        if streams.is_empty() {
            return Err(Error::NotFound(format!(
                "this runtime holds no stream of the project in {}, so there is none to remove. \
                 Loaded: {}. Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        self.forget_that_the_project_is_paused(&project_directory);
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::Remove,
            state: StreamActionState::Queued,
            step: None,
            streams,
            failed_because: None,
        };
        Ok(self
            .stream_actions
            .stream_action_queue_of_each_project
            .queue_the_stream_action(
                action,
                Box::new(move |runner: &Runner| {
                    runner.remove_the_project_for_its_stream_action(&action_id, &project_directory);
                }),
                &self.tokio_handle(),
            ))
    }

    /// Pause every loaded stream of the project in `project_directory` now, never waiting
    /// behind an action on its queue, and answer the streams paused: each processor holds its
    /// state and processes nothing, and a graph change made meanwhile compiles on
    /// [`Self::resume_project`]. The project stays paused until that resume, a `stop_project`
    /// or a `remove_project`: a stream a `reload_project` starts meanwhile, `tatolab dev`'s re-load
    /// on a save among them, starts paused. Live state, never recorded, so a restart of the
    /// runtime re-loads the project running. Pausing a paused project pauses nothing more.
    /// Refused for a directory not named by its absolute path, and for a project none of whose
    /// streams is loaded and whose last action queued is not a run.
    pub fn pause_project(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        let loaded_streams_of_the_project = self.loaded_streams_of_the_project(&project_directory);
        if loaded_streams_of_the_project.is_empty()
            && !self.is_a_run_the_last_action_queued_on_the_project(&project_directory)
        {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} runs, so there is none to pause; `run` runs it. \
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
    pub fn resume_project(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
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
    /// `pause_project` cannot land between the check and the start.
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

    /// Take the project in `project_directory` off the paused projects, as its stop or
    /// removal is queued.
    fn forget_that_the_project_is_paused(&self, project_directory: &Path) {
        self.stream_actions
            .directories_of_the_paused_projects
            .lock()
            .retain(|paused| !the_same_directory(paused, project_directory));
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
        progress.finish(if not_forgotten.is_empty() {
            Ok(Vec::new())
        } else {
            Err(format!(
                "the project's streams are unloaded, and these records were not forgotten, so \
                 they stay listed: {}",
                not_forgotten.join("; ")
            ))
        });
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

    /// Unload the stream whose id is `stream_id`, finishing the pending stop
    /// `action_id` on its record; a recorded one is recorded stopped first, so a restart leaves it unloaded. A loaded recorded
    /// stream is unloaded even when its record cannot be read or written, and
    /// the outcome says why it was not recorded stopped. Refused — with nothing
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
                "the stream `{stream_id}` is failed, so it is not loaded to stop: {}. `run` \
                 retries it, `rm` forgets its project",
                record.failed_because.as_deref().unwrap_or_default()
            ))),
            (None, Some(record)) if record.stopped => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already stopped; `run` loads its project again"
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
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
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

    /// Show the stream whose id is `stream_id` under `display_name`, leaving
    /// its id, address and broadcast as they are. A recorded stream's record
    /// is rewritten first, so a rename whose record cannot be written is
    /// refused with the stream shown as it was; a loaded stream is shown under
    /// the new name once it is recorded. Refused for a display name with
    /// nothing to show, a record an earlier runtime wrote, and an id the
    /// runtime holds no stream of.
    pub fn rename_stream(
        &self,
        stream_id: &str,
        display_name: &str,
    ) -> Result<StreamRenameOutcome> {
        refuse_a_display_name_with_nothing_to_show(display_name)?;
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        self.current_stream_record_of_the_id(stream_id)
            .map_err(|unreadable| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` was not renamed: {unreadable}"
                ))
            })?;
        let renamed_on_its_record = match self.stream_records() {
            Some(stream_records) => stream_records
                .change_the_record(stream_id, |record| {
                    record.display_name = display_name.to_string();
                })?
                .is_some(),
            None => false,
        };
        if !renamed_on_its_record && loaded.is_none() {
            return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
        }
        if let Some(loaded) = loaded {
            loaded.rename(display_name.to_string());
        }
        Ok(StreamRenameOutcome {
            stream_id: stream_id.to_string(),
            display_name: display_name.to_string(),
        })
    }

    /// Every stream the runtime holds, by id: each loaded stream, then each
    /// record not loaded — one an earlier runtime wrote listed failed. A record
    /// that cannot be read is logged and left out.
    pub fn list_streams(&self) -> Vec<StreamListing> {
        let mut listings: Vec<StreamListing> = self
            .every_loaded_stream()
            .iter()
            .map(|stream| StreamListing {
                stream_id: stream.stream_id().to_string(),
                display_name: stream.display_name(),
                stream_location: stream.stream_location().cloned(),
                state: if is_paused(stream.status()) {
                    StreamListingState::Paused
                } else {
                    StreamListingState::Running
                },
                node_count: Some(stream.node_count()),
                failed_because: None,
                capture_devices_whose_access_was_never_asked_for: stream
                    .capture_devices_whose_access_was_never_asked_for(),
                stream_action_under_way: None,
            })
            .collect();
        if let Some(stream_records) = self.stream_records() {
            for read in stream_records.read_every() {
                let listing = match read {
                    Ok(StreamRecordAsRead::Current(record)) => StreamListing {
                        state: record.listing_state(),
                        stream_id: record.stream_id,
                        display_name: record.display_name,
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

    /// Queue one re-load per project holding a record neither stopped nor
    /// failed, as a runtime does at its start, and answer the actions. Each
    /// loads only those records of its project; a record failed, one an
    /// earlier runtime wrote, and one that cannot be read are logged and left
    /// in place.
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
            if let Some(failed_because) = &record.failed_because {
                tracing::warn!(
                    "the stream `{}` is failed, so it is not re-loaded: {failed_because}; \
                     `tatolab reload` retries it",
                    record.stream_id
                );
                continue;
            }
            if record.stopped {
                tracing::info!(
                    "the stream `{}` is stopped, so it is not re-loaded",
                    record.stream_id
                );
                continue;
            }
            let project_directory = std::fs::canonicalize(&record.location.project_directory)
                .unwrap_or_else(|_| record.location.project_directory.clone());
            let stream = StreamOfAStreamAction {
                stream_id: record.stream_id,
                display_name: record.display_name,
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
                    if !record.is_failed() && !record.stopped =>
                {
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
            if let Some(failed_because) = record.failed_because {
                tracing::warn!(
                    "the stream `{stream_id}` is failed, so it is not re-loaded: \
                     {failed_because}; `tatolab reload` retries it"
                );
                reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                    stream_id,
                    failed_because,
                });
                continue;
            }
            if record.stopped {
                tracing::info!("the stream `{stream_id}` is stopped, so it is not re-loaded");
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
            let reloaded = venv_made_outside_the_lock.and_then(|()| {
                let graph = the_graph_the_recorded_stream_holds(&record).map_err(Error::Runtime)?;
                self.load_a_recorded_stream_from_its_record(
                    &record,
                    &graph,
                    CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
                    is_interrupted,
                )
            });
            match reloaded {
                Ok(stream) => {
                    tracing::info!(
                        "the stream `{stream_id}` re-loaded from {} with {} nodes",
                        record.location,
                        stream.node_count()
                    );
                    reloads.push(StreamReloadAtTheStart::Reloaded { stream_id });
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
                        recorded.failed_because = Some(failed_because);
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

    /// Count how the runtime's previous run ended against every current
    /// record not failed, before any re-load: a clean end resets each crash
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
            if record.is_failed() {
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
            if let Some(failed_because) = &record.failed_because {
                tracing::error!(
                    "the stream `{}` is failed: {failed_because}; it is not re-loaded until \
                     `tatolab reload` retries it",
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
    /// graph — keeping the display name of the record it replaces, and load and start its
    /// graph. A replace is recorded first, then the running stream unloaded; a new stream is
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
                    stream_name,
                    graph_or_why_none,
                },
            stream_environment,
            display_name_of_a_stream_first_recorded,
            is_interrupted,
        } = read;
        let (previous_record, previously_loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?;
        let display_name = previous_record
            .as_ref()
            .map(|previous| previous.display_name.clone())
            .or_else(|| display_name_of_a_stream_first_recorded.map(str::to_string))
            .unwrap_or(stream_name);
        let mut stream_record = StreamRecord::of_a_running_stream(
            stream_id,
            stream_location,
            display_name,
            stream_environment,
        );
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
            (Some(graph), _) => self
                .load_and_start_a_recorded_stream_recording_it_failed_when_refused(
                    stream_records,
                    &mut stream_record,
                    &graph,
                    a_new_stream_recorded_once_loaded,
                    is_interrupted,
                )?,
            (None, failed_because) => StreamStateAfterItsRun::Failed {
                failed_because: failed_because.unwrap_or_default(),
            },
        };
        Ok(StreamLoadedOrFailedByARun::of_the_record(
            stream_record,
            state,
            replaced_a_recorded_stream,
        ))
    }

    /// Load `graph` as the stream `stream_record` records — writing the record once it is
    /// loaded when `record_it_once_loaded`, and abandoning it once
    /// `is_interrupted` reports true — then start it. A refused load or start
    /// records the stream failed with the reason; a record that cannot be
    /// written unloads the stream and is refused.
    fn load_and_start_a_recorded_stream_recording_it_failed_when_refused(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_record: &mut StreamRecord,
        graph: &GraphSnapshot,
        record_it_once_loaded: bool,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamStateAfterItsRun> {
        let loaded = self.load_a_recorded_stream(
            OneStreamAStreamActionLoads::recorded_by(stream_record, graph)
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
        if let Err(start_refusal) = self.start_the_stream_paused_while_its_project_is(&stream) {
            unload_a_stream_an_action_took_back(&stream, "its start was refused");
            return record_a_refused_load_failed(stream_records, stream_record, start_refusal);
        }
        Ok(StreamStateAfterItsRun::Running {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        })
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
    /// `stream_assets_the_project_holds`: its graph document is gone. A record of the project
    /// that another copy of it holds is left as it is; one that cannot be removed is logged.
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
            if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(&record.stream_id) {
                unload_a_stream_an_action_took_back(&loaded, "its graph document is gone");
            }
            match stream_records.remove(&record.stream_id) {
                Ok(_) => {
                    self.forget_the_function_node_states_of_the_stream(&record.stream_id);
                    tracing::info!(
                        "the stream `{}` is forgotten: no graph document of {} holds its stream \
                         asset {} any more",
                        record.stream_id,
                        project.project_directory.display(),
                        record.location.stream_asset
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
                    &self.first_party_module_folders_this_runtime_carries(),
                )
            }
            None => Ok(()),
        }
    }

    /// Each first-party module folder beside this runtime's lend, none when it was handed no lend.
    fn first_party_module_folders_this_runtime_carries(&self) -> Vec<PathBuf> {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .and_then(first_party_modules_directory_beside_the_lend)
            .map(|directory| every_first_party_module_folder_in(&directory))
            .unwrap_or_default()
    }

    /// Make the `.venv` of the project in `project_directory` current with the
    /// uv this runtime was handed and the first-party modules beside its lend.
    fn make_the_projects_venv(
        &self,
        project_directory: &Path,
        is_interrupted: &dyn Fn() -> bool,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        make_the_projects_venv_current_with_the_runtime_unit_uv(
            project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_unit_uv_executable
                .get(),
            &self.first_party_module_folders_this_runtime_carries(),
            is_interrupted,
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
        Ok(self.make_the_projects_venv(&project_directory, is_interrupted)?)
    }

    /// Load and start a recorded stream from `record` and the graph `graph` its document
    /// holds, its capture devices asking the user as
    /// `capture_device_access_prompting` says, the load abandoned once
    /// `is_interrupted` reports true.
    fn load_a_recorded_stream_from_its_record(
        &self,
        record: &StreamRecord,
        graph: &GraphSnapshot,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_and_start_a_stream(
            OneStreamAStreamActionLoads::recorded_by(record, graph)
                .with_capture_device_access_prompting(capture_device_access_prompting)
                .abandoned_once(is_interrupted),
        )
    }

    /// [`Self::load_a_recorded_stream`], then start it; a
    /// refused start unloads it.
    fn load_and_start_a_stream(
        &self,
        stream_to_load: OneStreamAStreamActionLoads<'_>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let stream = self.load_a_recorded_stream(stream_to_load)?;
        if let Err(start_refusal) = self.start_the_stream_paused_while_its_project_is(&stream) {
            unload_a_stream_an_action_took_back(&stream, "its start was refused");
            return Err(start_refusal);
        }
        Ok(stream)
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
            function_node_states_recorded,
            is_interrupted,
        } = stream_to_load;
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .carry_the_recorded_states_unless_some_are_carried(
                stream_id,
                function_node_states_recorded,
            );
        let stream = match self.load_stream_held_as_unless_interrupted(
            graph,
            OptionsForLoadingOneStream::in_stream_environment(stream_environment)
                .with_display_name(display_name)
                .at_stream_location(stream_id.to_string(), stream_location.clone())
                .with_capture_device_access_prompting(capture_device_access_prompting),
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
    /// Queue the start of the project in `request`'s directory on its stream action queue,
    /// answering the action's id at once: [`Self::queue_the_load_of_the_project`]'s load, which
    /// runs a stopped project again and retries its failed streams. Refused, with nothing
    /// queued, for a directory not named by its absolute path and for a project that runs once
    /// its queued actions run — a paused one naming [`Self::resume_project`], a running one
    /// naming [`Self::reload_project`].
    pub fn start_project(
        &self,
        request: ProjectStartOrReloadRequest,
    ) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if self.the_project_runs_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(
                if self.is_the_project_paused(&project_directory) {
                    format!(
                        "the project in {} is paused, so there is none to start; `resume_project` \
                     resumes it",
                        project_directory.display()
                    )
                } else {
                    format!(
                        "the project in {} already runs, so there is none to start; \
                     `reload_project` re-reads it from disk",
                        project_directory.display()
                    )
                },
            ));
        }
        Ok(self.queue_the_load_of_the_project(project_directory))
    }

    /// Queue the reload of the project in `request`'s directory on its stream action queue,
    /// answering the action's id at once: [`Self::queue_the_load_of_the_project`]'s load, which
    /// re-reads every stream asset's graph document and replaces each stream under its id,
    /// keeping the project running or paused as it is, and retries its failed streams.
    /// Refused, with nothing queued, for a directory not named by its absolute path and for a
    /// project that is stopped or was never started, naming [`Self::start_project`].
    pub fn reload_project(
        &self,
        request: ProjectStartOrReloadRequest,
    ) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if !self.the_project_runs_once_its_queued_actions_run(&project_directory)
            && !self.a_record_of_the_project_is_failed_and_not_stopped(&project_directory)
        {
            let why_none_runs = if self
                .ids_of_every_record_of_the_project(&project_directory)
                .is_empty()
            {
                "was never started"
            } else {
                "is stopped"
            };
            return Err(Error::Runtime(format!(
                "the project in {} {why_none_runs}, so there is none to reload; \
                 `start_project` starts it",
                project_directory.display()
            )));
        }
        Ok(self.queue_the_load_of_the_project(project_directory))
    }

    /// Queue a load of every stream asset of the project in `project_directory`, absolute and
    /// resolved, on its stream action queue, answering the action's id at once;
    /// [`Runner::describe_stream_action`] follows it. The load reads each stream asset's graph
    /// document, makes the project's `.venv`, unbounded, and loads or fails each stream under
    /// the id its project and stream asset give it: a directory that does not exist or holds no
    /// `tatolab-project.json`, and an id another stream holds, fail the action.
    fn queue_the_load_of_the_project(&self, project_directory: PathBuf) -> StreamActionQueued {
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::Load,
            state: StreamActionState::Queued,
            step: None,
            streams: Vec::new(),
            failed_because: None,
        };
        queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.load_the_project_for_its_stream_action(&action_id, project_directory, None);
            }),
            &self.tokio_handle(),
        )
    }

    /// Whether the project in `project_directory` runs once the loads and stops queued on it
    /// have run: the last of them is a load, or, with none queued, a stream of it is loaded or
    /// recorded running.
    fn the_project_runs_once_its_queued_actions_run(&self, project_directory: &Path) -> bool {
        let the_last_load_or_stop_queued = self
            .stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
            .iter()
            .rev()
            .find(|action| {
                the_same_directory(&action.project_directory, project_directory)
                    && matches!(action.kind, StreamActionKind::Load | StreamActionKind::Stop)
            })
            .map(|action| action.kind);
        match the_last_load_or_stop_queued {
            Some(kind) => kind == StreamActionKind::Load,
            None => {
                !self
                    .loaded_streams_of_the_project(project_directory)
                    .is_empty()
                    || self
                        .current_records_of_the_project(project_directory)
                        .iter()
                        .any(|record| record.listing_state() == StreamListingState::Running)
            }
        }
    }

    /// Whether a record of the project in `project_directory` is failed and not stopped, so a
    /// reload retries it.
    fn a_record_of_the_project_is_failed_and_not_stopped(&self, project_directory: &Path) -> bool {
        self.current_records_of_the_project(project_directory)
            .iter()
            .any(|record| {
                record.is_failed() && !record.is_stopped_once_its_pending_stream_actions_run()
            })
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

    /// Create a project from the scaffold `tatolab new` writes and queue its
    /// load, answering the action's id and the id its stream asset `main` loads as
    /// at once. The directory is `request`'s, created when missing, or one
    /// drawn under `$HOME/streams`. The action writes the scaffold — the project file, one
    /// stream asset's graph document and its node modules — has the host commit it as the first
    /// commit of the project's history, then runs [`Runner::start_project`]'s load, the stream
    /// shown under the folder's name; a directory already holding a file the
    /// scaffold writes fails the action with nothing written. Refused for a
    /// relative directory, one that cannot be created, and a draw that finds
    /// no free name.
    pub fn create_project(&self, request: CreateProjectRequest) -> Result<ProjectCreationQueued> {
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
        let scaffold = NewProjectScaffold::of_a_new_project_in(&project_directory);
        let stream_id = scaffold.stream_id_in(&project_directory);
        let display_name_of_the_created_stream = scaffold.project_name.clone();
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::Load,
            state: StreamActionState::Queued,
            step: None,
            streams: vec![StreamOfAStreamAction {
                stream_id: stream_id.clone(),
                display_name: display_name_of_the_created_stream.clone(),
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
                    display_name_of_the_created_stream,
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

    /// Forget the function node states this runtime carries for `stream_id`,
    /// whose record is gone, so a stream recorded again under that id starts
    /// its function nodes from their defaults.
    fn forget_the_function_node_states_of_the_stream(&self, stream_id: &str) {
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .forget_the_stream(stream_id);
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

    /// Cancel the stream action `action_id`. One still queued is dropped,
    /// its pending marks taken off its streams' records, and finished
    /// cancelled. A running load stops at the step it is on and
    /// finishes cancelled once it sees the cancel: a stream it was loading is
    /// recorded failed, and the streams it had not reached stay as recorded.
    /// Refused for a running stop or removal, whose unload cannot be cut
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
        display_name_of_a_stream_first_recorded: Option<String>,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        self.load_the_project_reporting_to(
            action_id,
            &progress,
            project_directory,
            display_name_of_a_stream_first_recorded,
        );
    }

    /// Run the load of the project in `project_directory` for the stream
    /// action `action_id`, reporting to `progress`, and finish the action.
    fn load_the_project_reporting_to(
        &self,
        action_id: &str,
        progress: &StreamActionProgress<'_>,
        project_directory: PathBuf,
        display_name_of_a_stream_first_recorded: Option<String>,
    ) {
        let run_outcome = match why_a_directory_is_no_project(&project_directory) {
            Some(why_it_is_no_project) => {
                tracing::warn!("the stream action {action_id} failed: {why_it_is_no_project}");
                Err(why_it_is_no_project)
            }
            None => self
                .run_the_project_reporting_to(
                    ProjectStreamsToReadAndLoad {
                        project_directory,
                        display_name_of_a_stream_first_recorded,
                    },
                    Some(progress),
                    &|| progress.is_interrupted(),
                )
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
        display_name_of_the_created_stream: String,
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
            project_directory,
            Some(display_name_of_the_created_stream),
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
/// project, naming both.
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
        match loaded.stream_location() {
            Some(loaded_location) if !held_by_another(loaded_location) => {}
            Some(loaded_location) => {
                return refuse(format!("the loaded stream from {loaded_location}"));
            }
            None => {
                return refuse(format!(
                    "a stream its host loaded from {}",
                    loaded.project_directory().display()
                ));
            }
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
    display_name_of_a_stream_first_recorded: Option<&'run str>,
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
    let failed_because = refusal.to_string();
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
fn refuse_a_display_name_with_nothing_to_show(display_name: &str) -> Result<()> {
    if display_name.trim().is_empty() {
        return Err(Error::Configuration(format!(
            "{display_name:?} cannot be a stream's display name: it shows nothing; a display \
             name is any text with something to show"
        )));
    }
    Ok(())
}

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
/// ` stopped`, ` failed`, or nothing for one the runtime re-loads.
fn the_state_a_refusal_names_a_record_by(record: &StreamRecord) -> &'static str {
    match record.listing_state() {
        StreamListingState::Failed => " failed",
        StreamListingState::Stopped => " stopped",
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
/// its streams: a stop or removal that has begun cannot be cut short.
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
mod tests {
    use super::*;

    /// What [`Runner::remove_one_stream`] did.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct StreamRemoveOutcome {
        /// The stream's id.
        pub stream_id: String,
        /// Whether the stream was loaded and is now unloaded.
        pub unloaded: bool,
        /// Whether the stream was recorded and its record is now gone.
        pub forgotten: bool,
    }

    impl Runner {
        /// Compile and load `request` on this thread, as a stream action's load
        /// does, with no action queued and a machine shutdown its interrupt.
        fn load_the_project_off_its_queue(
            &self,
            request: ProjectStreamsToReadAndLoad,
        ) -> Result<StreamRunOutcome> {
            self.run_the_project_reporting_to(request, None, &is_the_machines_shutdown_requested)
        }

        /// Forget the record of the stream whose id is `stream_id` — one an earlier
        /// runtime wrote included — then unload it if it is loaded. Refused when the
        /// runtime holds no stream of that id.
        fn remove_one_stream(&self, stream_id: &str) -> Result<StreamRemoveOutcome> {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
            let forgotten = match self.stream_records() {
                Some(stream_records) => stream_records.remove(stream_id)?,
                None => false,
            };
            if loaded.is_none() && !forgotten {
                return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
            }
            if let Some(loaded) = &loaded {
                unload_a_stream_an_action_took_back(loaded, "its owner removed it");
            }
            Ok(StreamRemoveOutcome {
                stream_id: stream_id.to_string(),
                unloaded: loaded.is_some(),
                forgotten,
            })
        }

        /// Re-load every recorded stream neither stopped nor failed on this
        /// thread, as each project's re-load at the start does.
        fn reload_every_recorded_stream_neither_stopped_nor_failed_at_once(
            &self,
        ) -> Vec<StreamReloadAtTheStart> {
            let Some(stream_records) = self.stream_records() else {
                return Vec::new();
            };
            self.reload_each_recorded_stream_listed(
                stream_records,
                stream_records.read_every(),
                &is_the_machines_shutdown_requested,
            )
        }
    }
    use crate::core::test_support::{
        MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR, MockOutputOnlyProcessor,
        MockSourceRefusingAnAccessNobodyAskedFor, a_temporary_directory_at_owner_only_mode,
        ensure_test_mocks_registered, write_an_executable_script_from_a_child_process,
    };
    use serial_test::serial;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use crate::core::runtime::STREAM_RECORD_SCHEMA_VERSION;
    use streamlib_runtime_client_contract::directory_at_an_explicit_mode;

    const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

    /// A runner handed a lend directory, recording its streams in
    /// `streams_directory` when one is given.
    fn a_runner_recording_its_streams_in(streams_directory: Option<&Path>) -> Arc<Runner> {
        ensure_test_mocks_registered();
        let runner = Runner::new().expect("the runner builds");
        runner
            .set_processor_interpreter_lend_directory(PathBuf::from(LEND_DIRECTORY_FOR_TEST))
            .expect("the lend directory is handed over");
        if let Some(streams_directory) = streams_directory {
            runner
                .record_streams_in_the_state_directory(streams_directory)
                .expect("the runner records its streams there");
        }
        runner
    }

    /// The graph a stream named `stream_name` holds: one node, `source`, with the output
    /// ports `out1` and `out2`.
    fn the_graph_of_a_stream_named(stream_name: &str) -> serde_json::Value {
        serde_json::json!({
            "stream": stream_name,
            "nodes": [{
                "name": "source",
                "type": MockOutputOnlyProcessor::Processor::processor_class_import_path().as_str(),
                "config": {},
            }],
            "links": [],
        })
    }

    /// The graph of a stream named `stream_name` whose one node is of a type no module
    /// provides, which its load refuses.
    fn the_graph_of_a_stream_whose_load_is_refused(stream_name: &str) -> serde_json::Value {
        serde_json::json!({
            "stream": stream_name,
            "nodes": [{
                "name": "source",
                "type": "tatolab.absent:AbsentNodeType",
                "config": {},
            }],
            "links": [],
        })
    }

    /// How a stub `.venv/bin/python` answers the runtime's probe of whether the interpreter
    /// runs, `-I -c ""`: it exits 0.
    const STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE: &str = "#!/bin/sh\nexit 0\n";

    /// The name every test project's `tatolab-project.json` gives it.
    const TEST_PROJECT_NAME: &str = "rig";

    /// A temporary directory spelled as the runtime keys a project, symlinks resolved:
    /// macOS's temporary directory sits under `/var`, a link to `/private/var`.
    fn resolved_temporary_directory(temporary_directory: &tempfile::TempDir) -> PathBuf {
        std::fs::canonicalize(temporary_directory.path()).expect("a resolvable temporary directory")
    }

    /// A project of its own UUID whose streams are the graph documents under `streams/`, and
    /// whose `.venv/bin/python` is a shell script that answers the runtime's probe.
    struct ProjectWithGraphDocuments {
        project_directory: PathBuf,
        project: Uuid,
        _directory_removed_on_drop: Arc<tempfile::TempDir>,
    }

    impl ProjectWithGraphDocuments {
        /// A project with a venv holding one graph document, `graph`'s, named after its
        /// `stream`.
        fn holding(graph: serde_json::Value) -> Self {
            let project = Self::with_no_venv().given_a_venv();
            project.write_the_graph_document(graph);
            project
        }

        /// A project with no venv and no graph document.
        fn with_no_venv() -> Self {
            let temporary_directory =
                a_temporary_directory_at_owner_only_mode().expect("a project directory");
            Self::of_the_project_at(
                resolved_temporary_directory(&temporary_directory),
                Uuid::new_v4(),
                Arc::new(temporary_directory),
            )
        }

        fn of_the_project_at(
            project_directory: PathBuf,
            project: Uuid,
            directory_removed_on_drop: Arc<tempfile::TempDir>,
        ) -> Self {
            directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                &project_directory,
                0o700,
            )
            .expect("the project directory");
            std::fs::write(
                project_directory.join("pyproject.toml"),
                "[project]\nname = \"project\"\n",
            )
            .expect("the project's pyproject.toml");
            let project_file = TatolabProjectFile {
                project,
                name: TEST_PROJECT_NAME.to_string(),
                created_with_runtime: None,
                dependencies: Vec::new(),
            };
            std::fs::write(
                project_directory.join(PROJECT_FILE_NAME),
                project_file
                    .canonical_bytes()
                    .expect("the project file serializes"),
            )
            .expect("the project's tatolab-project.json");
            Self {
                project_directory,
                project,
                _directory_removed_on_drop: directory_removed_on_drop,
            }
        }

        /// A second checkout of this project — its project UUID — at another directory,
        /// with a venv and no graph document.
        fn another_copy(&self) -> Self {
            let temporary_directory =
                a_temporary_directory_at_owner_only_mode().expect("a project directory");
            Self::of_the_project_at(
                resolved_temporary_directory(&temporary_directory),
                self.project,
                Arc::new(temporary_directory),
            )
            .given_a_venv()
        }

        /// Two projects with no venv whose stream `stream_name` computes to one stream id:
        /// project UUIDs drawn until their hashes' six hex digits collide.
        fn two_with_no_venv_whose_ids_collide(stream_name: &str) -> (Self, Self) {
            let parent =
                Arc::new(a_temporary_directory_at_owner_only_mode().expect("a parent directory"));
            let resolved_parent = resolved_temporary_directory(&parent);
            let mut project_by_stream_id = std::collections::HashMap::new();
            let (first, second) = std::iter::repeat_with(Uuid::new_v4)
                .find_map(|project| {
                    let stream_id = StreamLocation {
                        project_directory: PathBuf::new(),
                        project,
                        stream_asset: Self::the_stream_asset_in(project, stream_name),
                    }
                    .stream_id_named(TEST_PROJECT_NAME, stream_name);
                    project_by_stream_id
                        .insert(stream_id, project)
                        .map(|colliding| (colliding, project))
                })
                .expect("two projects whose ids collide");
            let project_at = |project: Uuid| {
                Self::of_the_project_at(
                    resolved_parent.join(project.to_string()),
                    project,
                    Arc::clone(&parent),
                )
            };
            (project_at(first), project_at(second))
        }

        /// This project, given a venv whose interpreter answers the runtime's probe.
        fn given_a_venv(self) -> Self {
            directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                &self.path().join(".venv").join("bin"),
                0o755,
            )
            .expect("the venv's bin directory");
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
            );
            crate::core::runtime::project_venv_made_with_the_runtime_unit_uv::record_the_venv_synced_with_what_the_project_holds_now(
                self.path(),
                &[],
            )
            .expect("the venv's sync stamp");
            self
        }

        fn path(&self) -> &Path {
            &self.project_directory
        }

        fn interpreter(&self) -> PathBuf {
            self.path().join(".venv").join("bin").join("python")
        }

        fn stream_environment(&self) -> StreamEnvironment {
            StreamEnvironment {
                project_directory: self.path().to_path_buf(),
                interpreter: self.interpreter(),
            }
        }

        /// The stream asset UUID the stream `stream_name` of the project `project` is given.
        fn the_stream_asset_in(project: Uuid, stream_name: &str) -> Uuid {
            Uuid::new_v5(&project, stream_name.as_bytes())
        }

        /// The stream asset UUID of this project's stream `stream_name`.
        fn stream_asset_of(&self, stream_name: &str) -> Uuid {
            Self::the_stream_asset_in(self.project, stream_name)
        }

        /// Where this project's graph document of the stream `stream_name` sits.
        fn graph_document_path_of(&self, stream_name: &str) -> PathBuf {
            self.path()
                .join("streams")
                .join(format!("{stream_name}.graph.json"))
        }

        /// Where this project's stream `stream_name` lives.
        fn location_of(&self, stream_name: &str) -> StreamLocation {
            StreamLocation {
                project_directory: self.path().to_path_buf(),
                project: self.project,
                stream_asset: self.stream_asset_of(stream_name),
            }
        }

        /// The stream id a first run records this project's stream `stream_name` under.
        fn stream_id_of(&self, stream_name: &str) -> String {
            self.location_of(stream_name)
                .stream_id_named(TEST_PROJECT_NAME, stream_name)
        }

        /// Write `graph` as the graph document of the stream its `stream` names, holding that
        /// stream's asset UUID and, as the runtime writes one, a UUID for each node holding
        /// none, the same one for a node of one name in every write.
        fn write_the_graph_document(&self, graph: serde_json::Value) {
            let stream_name = graph
                .get("stream")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("main")
                .to_string();
            let mut document = graph;
            let stream_asset = self.stream_asset_of(&stream_name);
            if let Some(nodes) = document["nodes"].as_array_mut() {
                for node in nodes {
                    if node.get("id").is_none() {
                        let node_name = node["name"].as_str().unwrap_or_default().to_string();
                        node["id"] = serde_json::json!(
                            Uuid::new_v5(&stream_asset, node_name.as_bytes()).to_string()
                        );
                    }
                }
            }
            document["asset"] = serde_json::json!(stream_asset);
            self.write_the_graph_document_bytes(&stream_name, &document.to_string());
        }

        /// Write the graph document of the stream `stream_name` holding its asset UUID and a
        /// `nodes` that is no list, which does not load as a graph.
        fn write_an_unloadable_graph_document(&self, stream_name: &str) {
            self.write_the_graph_document_bytes(
                stream_name,
                &serde_json::json!({
                    "asset": self.stream_asset_of(stream_name),
                    "nodes": "not a list",
                    "links": [],
                })
                .to_string(),
            );
        }

        fn write_the_graph_document_bytes(&self, stream_name: &str, document: &str) {
            let path = self.graph_document_path_of(stream_name);
            std::fs::create_dir_all(path.parent().expect("a streams directory"))
                .expect("the streams directory");
            std::fs::write(path, document).expect("the graph document");
        }

        /// Remove the graph document of the stream `stream_name`.
        fn remove_the_graph_document(&self, stream_name: &str) {
            std::fs::remove_file(self.graph_document_path_of(stream_name))
                .expect("the graph document is removed");
        }

        /// Make the graph documents under `streams/` exactly those of `streams_loading`, each
        /// its stream's graph, and of `streams_unloadable`, each one that does not load.
        fn hold_the_streams(&self, streams_loading: &[&str], streams_unloadable: &[&str]) {
            let _ = std::fs::remove_dir_all(self.path().join("streams"));
            for stream_name in streams_loading {
                self.write_the_graph_document(the_graph_of_a_stream_named(stream_name));
            }
            for stream_name in streams_unloadable {
                self.write_an_unloadable_graph_document(stream_name);
            }
        }

        fn run_request(&self) -> ProjectStreamsToReadAndLoad {
            ProjectStreamsToReadAndLoad {
                project_directory: self.path().to_path_buf(),
                display_name_of_a_stream_first_recorded: None,
            }
        }
    }

    fn records_in(streams_directory: &Path) -> StreamRecordsInTheStateDirectory {
        StreamRecordsInTheStateDirectory::open(streams_directory).expect("the records open")
    }

    /// The record of `stream_id` in `streams_directory`, `None` when there is
    /// none; one an earlier runtime wrote fails the test.
    fn the_current_record_in(streams_directory: &Path, stream_id: &str) -> Option<StreamRecord> {
        match records_in(streams_directory)
            .read(stream_id)
            .expect("the record reads")
        {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(earlier) => {
                panic!("`{stream_id}` is a record an earlier runtime wrote: {earlier:?}")
            }
            None => None,
        }
    }

    fn the_record_of(streams_directory: &Path, stream_id: &str) -> StreamRecord {
        the_current_record_in(streams_directory, stream_id).expect("the record is there")
    }

    /// A record of the stream `stream_name` of `project`, shown under its name.
    fn a_record_of(project: &ProjectWithGraphDocuments, stream_name: &str) -> StreamRecord {
        StreamRecord::of_a_running_stream(
            project.stream_id_of(stream_name),
            project.location_of(stream_name),
            stream_name.to_string(),
            &project.stream_environment(),
        )
    }

    /// Write `record` and load it recorded by the runtime, not started — what
    /// a recorded stream is between its load and its start.
    fn a_recorded_stream_loaded_without_its_start(
        runner: &Runner,
        streams_directory: &Path,
        record: &StreamRecord,
    ) -> Arc<LoadedStreamInThisRuntime> {
        records_in(streams_directory)
            .write(record)
            .expect("the record is written");
        a_stream_loaded_from_an_unwritten_record(runner, record)
    }

    /// Load `record` recorded by the runtime, not started, without writing it —
    /// a loaded stream whose record is gone — from a graph of one node.
    fn a_stream_loaded_from_an_unwritten_record(
        runner: &Runner,
        record: &StreamRecord,
    ) -> Arc<LoadedStreamInThisRuntime> {
        let graph =
            GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(&record.display_name))
                .expect("the graph reads");
        runner
            .load_a_recorded_stream(OneStreamAStreamActionLoads::recorded_by(record, &graph))
            .expect("the recorded stream loads")
    }

    /// The stream `stream_name` of `project` loaded by a library host under the id a run
    /// records it under, not started.
    fn a_stream_its_host_loaded(
        runner: &Runner,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        let graph = GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(stream_name))
            .expect("the graph reads");
        runner
            .load_stream_from_graph_snapshot(
                &graph,
                OptionsForLoadingOneStream::in_project_directory(project.path())
                    .with_stream_id(project.stream_id_of(stream_name)),
            )
            .expect("the host loads its stream")
    }

    fn refusal_of<T: std::fmt::Debug>(outcome: Result<T>) -> String {
        match outcome {
            Ok(unexpected) => panic!("the action was refused, and it returned {unexpected:?}"),
            Err(refusal) => refusal.to_string(),
        }
    }

    /// The one stream `run` took in, which runs, with its load's tag and its
    /// node count.
    fn the_one_running_stream_of(
        run: &StreamRunOutcome,
    ) -> (&StreamLoadedOrFailedByARun, LoadedStreamTag, usize) {
        match run.streams.as_slice() {
            [
                stream @ StreamLoadedOrFailedByARun {
                    state:
                        StreamStateAfterItsRun::Running {
                            stream_tag,
                            node_count,
                        },
                    ..
                },
            ] => (stream, *stream_tag, *node_count),
            streams => panic!("the run took in one running stream, and took in {streams:?}"),
        }
    }

    /// The one stream `run` took in, which is failed, and why.
    fn the_one_failed_stream_of(run: &StreamRunOutcome) -> (&StreamLoadedOrFailedByARun, &str) {
        match run.streams.as_slice() {
            [
                stream @ StreamLoadedOrFailedByARun {
                    state: StreamStateAfterItsRun::Failed { failed_because },
                    ..
                },
            ] => (stream, failed_because),
            streams => panic!("the run took in one failed stream, and took in {streams:?}"),
        }
    }

    #[test]
    #[serial]
    fn a_run_on_a_runtime_given_no_state_directory_is_refused_by_name() {
        let runner = a_runner_recording_its_streams_in(None);
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(refusal.contains("records no stream"), "{refusal}");
        assert!(refusal.contains("state directory"), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_second_streams_directory_is_refused_naming_the_first() {
        let state_directory = tempfile::tempdir().unwrap();
        let first = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&first));

        let refusal = refusal_of(
            runner.record_streams_in_the_state_directory(&state_directory.path().join("other")),
        );

        assert!(refusal.contains(&first.display().to_string()), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_run_whose_id_a_loaded_stream_of_another_location_holds_is_refused_naming_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let (project, other_project) =
            ProjectWithGraphDocuments::two_with_no_venv_whose_ids_collide("camera");
        let project = project.given_a_venv();
        project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let other_location = other_project.location_of("camera");
        assert_eq!(other_project.stream_id_of("camera"), camera);
        let holder = a_stream_loaded_from_an_unwritten_record(
            &runner,
            &a_record_of(&other_project, "camera"),
        );

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(refusal.contains(&format!("`{camera}`")), "{refusal}");
        assert!(
            refusal.contains(&project.location_of("camera").to_string()),
            "{refusal}"
        );
        assert!(refusal.contains(&other_location.to_string()), "{refusal}");
        assert!(refusal.contains("cannot share one id"), "{refusal}");
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&camera)
                .unwrap()
                .stream_tag(),
            holder.stream_tag()
        );
        assert!(!holder.has_ended());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_whose_id_a_stream_its_host_loaded_holds_is_refused_naming_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let host_project = ProjectWithGraphDocuments::with_no_venv();
        let graph =
            GraphSnapshot::from_graph_document(the_graph_of_a_stream_named("camera")).unwrap();
        let host_loaded = runner
            .load_stream_from_graph_snapshot(
                &graph,
                OptionsForLoadingOneStream::in_project_directory(host_project.path())
                    .with_stream_id(camera.clone()),
            )
            .expect("the host loads its stream under the id it names");

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(refusal.contains("its host loaded"), "{refusal}");
        assert!(
            refusal.contains(&host_project.path().display().to_string()),
            "{refusal}"
        );
        assert!(
            refusal.contains(&project.location_of("camera").to_string()),
            "{refusal}"
        );
        assert!(!host_loaded.has_ended());
    }

    #[test]
    #[serial]
    fn a_run_whose_id_a_record_of_another_location_holds_is_refused_naming_both_whatever_its_state()
    {
        let (project, other_project) =
            ProjectWithGraphDocuments::two_with_no_venv_whose_ids_collide("camera");
        let project = project.given_a_venv();
        project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let other_location = other_project.location_of("camera");
        assert_eq!(other_project.stream_id_of("camera"), camera);

        let holder_states: [(&str, fn(&mut StreamRecord)); 3] = [
            ("stopped", |record| record.stopped = true),
            ("failed", |record| {
                record.failed_because = Some("it crashed the runtime".to_string())
            }),
            ("running", |_| {}),
        ];
        for (state, mark) in holder_states {
            let state_directory = tempfile::tempdir().unwrap();
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            let mut holder = a_record_of(&other_project, "camera");
            mark(&mut holder);
            records_in(state_directory.path()).write(&holder).unwrap();

            let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

            assert!(
                refusal.contains(&other_location.to_string()),
                "{state}: {refusal}"
            );
            assert!(
                refusal.contains(&project.location_of("camera").to_string()),
                "{state}: {refusal}"
            );
            assert!(
                refusal.contains("cannot share one id"),
                "{state}: {refusal}"
            );
            if state != "running" {
                assert!(refusal.contains(state), "{state}: {refusal}");
            }
            assert!(runner.ids_of_the_loaded_streams().is_empty());
            assert_eq!(
                the_current_record_in(state_directory.path(), &camera),
                Some(holder)
            );
            if state == "failed" {
                let refusal = refusal_of(runner.remove_one_stream("nothing-here"));
                assert!(refusal.contains(&format!("{camera} (failed)")), "{refusal}");
            }
        }
    }

    #[test]
    #[serial]
    fn a_run_whose_project_file_does_not_read_is_refused_leaving_the_running_stream_and_its_record()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let record = a_record_of(&project, "camera");
        let running =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);
        std::fs::write(project.path().join(PROJECT_FILE_NAME), "{").unwrap();

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(refusal.contains("was not loaded"), "{refusal}");
        assert!(refusal.contains(PROJECT_FILE_NAME), "{refusal}");
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&camera)
                .unwrap()
                .stream_tag(),
            running.stream_tag(),
            "the running stream is the one loaded before the replace"
        );
        assert!(!running.has_ended());
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(record)
        );
    }

    #[test]
    #[serial]
    fn a_run_of_a_directory_holding_no_project_file_is_refused_naming_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        std::fs::remove_file(project.path().join(PROJECT_FILE_NAME)).unwrap();

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(refusal.contains(PROJECT_FILE_NAME), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_replace_whose_load_is_refused_unloads_the_running_stream_and_records_it_failed_never_restoring_it()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(
            the_graph_of_a_stream_whose_load_is_refused("camera"),
        );
        let camera = project.stream_id_of("camera");
        let mut previous_record = a_record_of(&project, "camera");
        previous_record.display_name = "Front camera".to_string();
        let previous = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &previous_record,
        );

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a refused load is a failed stream, not a refused run");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains("AbsentNodeType"),
            "{failed_because}"
        );
        assert_eq!(stream.stream_id, camera);
        assert_eq!(stream.display_name, "Front camera");
        assert!(stream.replaced_a_recorded_stream);
        assert!(
            previous.has_ended(),
            "the replace unloaded the running stream"
        );
        assert!(
            runner.ids_of_the_loaded_streams().is_empty(),
            "the previous version is not put back"
        );
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .failed_because
                .as_deref(),
            Some(failed_because)
        );
    }

    #[test]
    #[serial]
    fn a_graph_document_that_does_not_load_is_recorded_failed_naming_the_document() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        project.write_an_unloadable_graph_document("camera");
        let camera = project.stream_id_of("camera");

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a document that does not load is a failed stream, not a refused run");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains("`streams/camera.graph.json` does not load"),
            "{failed_because}"
        );
        assert_eq!(stream.stream_id, camera);
        assert_eq!(
            stream.display_name, "camera",
            "shown under its graph document's name"
        );
        assert_eq!(stream.stream_location, project.location_of("camera"));
        assert!(!stream.replaced_a_recorded_stream);
        assert_eq!(run.project_directory, project.path());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        let recorded = the_record_of(state_directory.path(), &camera);
        assert_eq!(recorded.location, project.location_of("camera"));
        assert_eq!(recorded.display_name, "camera");
        assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
        let listed = runner.list_streams();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, StreamListingState::Failed);
        assert_eq!(listed[0].failed_because.as_deref(), Some(failed_because));
    }

    #[test]
    #[serial]
    fn a_graph_document_holding_an_integer_wider_than_64_bits_is_recorded_failed_naming_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let mut graph = the_graph_of_a_stream_named("camera");
        graph["nodes"][0]["config"] = serde_json::json!({ "value": "the wide integer" });
        project.write_the_graph_document(graph);
        let document_path = project.graph_document_path_of("camera");
        let document_text = std::fs::read_to_string(&document_path).unwrap();
        std::fs::write(
            &document_path,
            document_text.replace("\"the wide integer\"", "18446744073709551616"),
        )
        .unwrap();

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("an integer wider than 64 bits fails its stream, not the run");

        let (_, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains("`streams/camera.graph.json` does not load")
                && failed_because.contains("18446744073709551616"),
            "{failed_because}"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_graph_document_holding_a_node_with_no_uuid_is_recorded_failed_naming_the_node() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let mut graph = the_graph_of_a_stream_named("camera");
        graph["nodes"][0]["id"] = serde_json::json!("not-a-uuid");
        project.write_the_graph_document(graph);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a node with no UUID fails its stream, not the run");

        let (_, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because
                .contains("`streams/camera.graph.json` holds no UUID as the `id` of `source`"),
            "{failed_because}"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    fn the_graph_of_a_stream_reading_the_image_asset(
        stream_name: &str,
        image: Uuid,
    ) -> serde_json::Value {
        serde_json::json!({
            "stream": stream_name,
            "nodes": [{
                "name": "overlay",
                "type": crate::core::test_support::MockSourceReadingAnImageAsset::Processor::processor_class_import_path().as_str(),
                "config": { "image": { "asset": image } },
            }],
            "links": [],
        })
    }

    #[test]
    #[serial]
    fn a_node_referencing_an_asset_the_project_does_not_hold_fails_its_stream_naming_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let image = Uuid::new_v4();
        project.write_the_graph_document(the_graph_of_a_stream_reading_the_image_asset(
            "camera", image,
        ));

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a reference to no asset fails its stream, not the run");

        let (_, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains(&format!(
                "has node `overlay` whose config references the asset {image}, which the \
                 project does not hold"
            )),
            "{failed_because}"
        );
    }

    #[test]
    #[serial]
    fn a_node_referencing_an_imported_image_asset_loads() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let image = streamlib_project_repository::ProjectAsset {
            asset: Uuid::new_v4(),
            kind: streamlib_project_repository::ProjectAssetKind::Media,
            path: streamlib_project_repository::ProjectRelativeAssetPath::parse("media/logo.png")
                .unwrap(),
            content_sha256: "0".repeat(64),
            content_byte_len: 0,
            import_settings: serde_json::Map::new(),
        };
        let sidecar = project.path().join(image.path.sidecar_path());
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, image.sidecar_bytes().unwrap()).unwrap();
        project.write_the_graph_document(the_graph_of_a_stream_reading_the_image_asset(
            "camera",
            image.asset,
        ));

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        assert!(
            matches!(
                run.streams.as_slice(),
                [StreamLoadedOrFailedByARun {
                    state: StreamStateAfterItsRun::Running { .. },
                    ..
                }]
            ),
            "{run:?}"
        );
    }

    #[test]
    #[serial]
    fn a_graph_document_naming_no_stream_asset_is_recorded_failed_under_one_derived_from_its_path()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        project.write_the_graph_document_bytes("camera", "{\"nodes\": [], \"links\": []}");

        let first = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        let second = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&first);
        assert!(
            failed_because.contains("names no stream asset"),
            "{failed_because}"
        );
        assert_eq!(
            the_one_failed_stream_of(&second).0.stream_id,
            stream.stream_id
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
    }

    #[test]
    #[serial]
    fn a_running_stream_whose_graph_document_no_longer_loads_is_unloaded_and_recorded_failed() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        project.write_an_unloadable_graph_document("camera");

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(failed_because.contains("does not load"), "{failed_because}");
        assert!(stream.replaced_a_recorded_stream);
        assert!(running.has_ended(), "the old version is not running");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(
            the_record_of(state_directory.path(), &camera)
                .failed_because
                .is_some()
        );
    }

    #[test]
    #[serial]
    fn a_project_whose_graph_documents_are_gone_unloads_and_forgets_its_streams() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            std::slice::from_ref(&camera)
        );
        project.remove_the_graph_document("camera");

        let emptied = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        assert!(emptied.streams.is_empty());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(the_current_record_in(state_directory.path(), &camera), None);
    }

    #[test]
    #[serial]
    fn a_run_of_every_stream_one_of_whose_ids_the_host_holds_is_refused_loading_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let hosted = a_stream_its_host_loaded(&runner, &project, "preview");
        project.hold_the_streams(&["camera", "preview"], &[]);

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(
            refusal.contains(&project.stream_id_of("preview")),
            "{refusal}"
        );
        assert_eq!(runner.ids_of_the_loaded_streams(), [hosted.stream_id()]);
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_takes_in_every_graph_document_and_forgets_a_stream_whose_document_is_gone() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        project.hold_the_streams(&["camera", "preview"], &["broken"]);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("every graph document of the project is taken in");

        let states: Vec<(&str, bool)> = run
            .streams
            .iter()
            .map(|stream| {
                (
                    stream.stream_id.as_str(),
                    matches!(stream.state, StreamStateAfterItsRun::Running { .. }),
                )
            })
            .collect();
        assert_eq!(
            states,
            [
                (project.stream_id_of("broken").as_str(), false),
                (project.stream_id_of("camera").as_str(), true),
                (project.stream_id_of("preview").as_str(), true),
            ],
            "each document in path order"
        );
        let mut loaded = runner.ids_of_the_loaded_streams();
        loaded.sort();
        let mut expected_loaded = vec![
            project.stream_id_of("camera"),
            project.stream_id_of("preview"),
        ];
        expected_loaded.sort();
        assert_eq!(loaded, expected_loaded);
        assert_eq!(records_in(state_directory.path()).read_every().len(), 3);

        project.hold_the_streams(&["camera"], &["broken"]);
        let rerun = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        assert!(
            rerun
                .streams
                .iter()
                .all(|stream| stream.stream_id != project.stream_id_of("preview")),
            "{rerun:?}"
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &project.stream_id_of("preview")),
            None
        );
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [project.stream_id_of("camera")]
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 2);
    }

    #[test]
    #[serial]
    fn a_project_moved_to_another_directory_keeps_each_streams_id_and_display_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        runner
            .rename_stream(&camera, "Front camera")
            .expect("the stream is renamed");
        runner
            .stop_the_stream_finishing_its_pending_stop(&camera, None)
            .unwrap();
        let moved_to = project.path().with_file_name(format!(
            "{}-moved",
            project.path().file_name().unwrap().to_string_lossy()
        ));
        std::fs::rename(project.path(), &moved_to).unwrap();
        let moved = ProjectWithGraphDocuments {
            project_directory: moved_to.clone(),
            project: project.project,
            _directory_removed_on_drop: Arc::clone(&project._directory_removed_on_drop),
        };

        let run = runner
            .load_the_project_off_its_queue(moved.run_request())
            .unwrap();

        let (stream, _, _) = the_one_running_stream_of(&run);
        assert_eq!(stream.stream_id, camera);
        assert_eq!(stream.display_name, "Front camera");
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .location
                .project_directory,
            moved_to
        );
        let _ = std::fs::rename(&moved_to, project.path());
    }

    #[test]
    #[serial]
    fn a_second_copy_of_a_project_is_refused_while_the_first_still_holds_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        let copy = project.another_copy();
        copy.write_the_graph_document(the_graph_of_a_stream_named("camera"));

        let refusal = refusal_of(runner.load_the_project_off_its_queue(copy.run_request()));

        assert!(refusal.contains("cannot share one id"), "{refusal}");
        assert!(refusal.contains("its own project UUID"), "{refusal}");
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            std::slice::from_ref(&camera)
        );
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .location
                .project_directory,
            project.path()
        );
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_reads_each_records_graph_document_and_fails_one_that_does_not_load()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        project.hold_the_streams(&["camera"], &["broken"]);
        for stream_name in ["camera", "broken"] {
            records_in(state_directory.path())
                .write(&a_record_of(&project, stream_name))
                .unwrap();
        }
        let broken = project.stream_id_of("broken");

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert!(
            reloads.iter().any(|reload| matches!(
                reload,
                StreamReloadAtTheStart::Reloaded { stream_id }
                    if *stream_id == project.stream_id_of("camera")
            )),
            "{reloads:?}"
        );
        assert!(
            reloads.iter().any(|reload| matches!(
                reload,
                StreamReloadAtTheStart::NotReloaded { stream_id, refusal }
                    if *stream_id == broken && refusal.to_string().contains("does not load")
            )),
            "{reloads:?}"
        );
        assert!(
            the_record_of(state_directory.path(), &broken)
                .failed_because
                .is_some_and(|failed_because| failed_because.contains("does not load"))
        );
    }

    #[test]
    #[serial]
    fn a_stopped_recorded_stream_is_recorded_stopped_and_a_fresh_runtime_leaves_it_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            let running = a_recorded_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_record_of(&project, "camera"),
            );

            let stopped = runner
                .stop_the_stream_finishing_its_pending_stop(&camera, None)
                .expect("the recorded stream stops");

            assert_eq!(
                stopped,
                StreamStopOutcome {
                    stream_id: camera.clone(),
                    stop_not_recorded_because: None,
                }
            );
            assert!(running.has_ended());
            assert!(runner.ids_of_the_loaded_streams().is_empty());
            let refusal =
                refusal_of(runner.stop_the_stream_finishing_its_pending_stop(&camera, None));
            assert!(refusal.contains("already stopped"), "{refusal}");
        }

        let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        assert!(
            fresh_runner
                .reload_every_recorded_stream_neither_stopped_nor_failed_at_once()
                .is_empty()
        );
        assert!(fresh_runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            fresh_runner.list_streams(),
            [StreamListing {
                stream_id: camera.clone(),
                display_name: "camera".to_string(),
                stream_location: Some(project.location_of("camera")),
                state: StreamListingState::Stopped,
                node_count: None,
                failed_because: None,
                capture_devices_whose_access_was_never_asked_for: Vec::new(),
                stream_action_under_way: None,
            }]
        );
        assert!(the_record_of(state_directory.path(), &camera).stopped);
    }

    /// Makes `streams_directory` refuse every new file until it drops, so a
    /// record write fails.
    struct StreamsDirectoryRefusingWrites<'a> {
        streams_directory: &'a Path,
    }

    impl<'a> StreamsDirectoryRefusingWrites<'a> {
        fn from_now_on(streams_directory: &'a Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(streams_directory, std::fs::Permissions::from_mode(0o500))
                .expect("the streams directory is made read-only");
            Self { streams_directory }
        }
    }

    impl Drop for StreamsDirectoryRefusingWrites<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                self.streams_directory,
                std::fs::Permissions::from_mode(0o700),
            );
        }
    }

    /// Overwrite the record of `stream_id` in `streams_directory` with a file
    /// of each unreadable kind in turn — not JSON, then a record of a later
    /// schema version — handing each one's bytes to `observe`.
    fn for_each_unreadable_record_of(
        streams_directory: &Path,
        stream_id: &str,
        mut observe: impl FnMut(&Path, &[u8]),
    ) {
        let record_path = records_in(streams_directory)
            .record_path_of(stream_id)
            .unwrap();
        let mut later_schema =
            serde_json::to_value(the_record_of(streams_directory, stream_id)).unwrap();
        later_schema["schema_version"] = serde_json::json!(STREAM_RECORD_SCHEMA_VERSION + 1);
        for unreadable_bytes in [
            b"{ not json".to_vec(),
            serde_json::to_vec(&later_schema).unwrap(),
        ] {
            std::fs::write(&record_path, &unreadable_bytes).unwrap();
            observe(&record_path, &unreadable_bytes);
        }
    }

    #[test]
    #[serial]
    fn stopping_a_loaded_recorded_stream_whose_record_is_unreadable_unloads_it_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_record_of(&project, "camera");
        records_in(state_directory.path()).write(&record).unwrap();

        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let running = a_stream_loaded_from_an_unwritten_record(&runner, &record);

                let stopped = runner
                    .stop_the_stream_finishing_its_pending_stop(&camera, None)
                    .expect("the stop succeeds, since the stream is unloaded");

                assert_eq!(stopped.stream_id, camera);
                let not_recorded = stopped
                    .stop_not_recorded_because
                    .expect("the outcome says the stop was not recorded");
                assert!(
                    not_recorded.contains(&record_path.display().to_string()),
                    "{not_recorded}"
                );
                assert!(running.has_ended());
                assert!(runner.ids_of_the_loaded_streams().is_empty());
                assert_eq!(
                    std::fs::read(record_path).unwrap(),
                    unreadable_bytes,
                    "the unreadable record is left as it was"
                );

                let refusal =
                    refusal_of(runner.stop_the_stream_finishing_its_pending_stop(&camera, None));
                assert!(
                    refusal.contains(&record_path.display().to_string()),
                    "with nothing loaded, the refusal names the record: {refusal}"
                );
            },
        );
    }

    #[test]
    #[serial]
    fn stopping_a_loaded_recorded_stream_whose_record_cannot_be_written_still_unloads_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_record_of(&project, "camera"),
        );

        let stopped = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            runner.stop_the_stream_finishing_its_pending_stop(&camera, None)
        }
        .expect("the stop succeeds, since the stream is unloaded");
        assert!(
            stopped.stop_not_recorded_because.is_some(),
            "the outcome says the stop was not recorded: {stopped:?}"
        );
        assert!(running.has_ended());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(!the_record_of(&streams_directory, &camera).stopped);
    }

    #[test]
    #[serial]
    fn stopping_a_stream_its_host_loaded_unloads_it_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "camera");

        let stopped = runner
            .stop_the_stream_finishing_its_pending_stop(&project.stream_id_of("camera"), None)
            .expect("the stream its host loaded stops");

        assert_eq!(stopped.stop_not_recorded_because, None);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn an_action_naming_no_stream_the_runtime_holds_is_refused_naming_the_loaded_and_recorded_ones()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "hosted-one");
        let mut stopped = a_record_of(&project, "recorded-one");
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let hosted_one = project.stream_id_of("hosted-one");
        let recorded_one = project.stream_id_of("recorded-one");

        let refusals = [
            refusal_of(runner.stop_the_stream_finishing_its_pending_stop("absent", None)),
            refusal_of(runner.remove_one_stream("absent")),
        ];

        for refusal in refusals {
            assert!(refusal.contains("`absent`"), "{refusal}");
            assert!(refusal.contains(&hosted_one), "{refusal}");
            assert!(
                refusal.contains(&format!("{recorded_one} (stopped)")),
                "{refusal}"
            );
        }
    }

    #[test]
    #[serial]
    fn a_stream_action_finds_a_stream_by_its_exact_id_alone() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let camera = project.stream_id_of("camera");
        let mut stopped = a_record_of(&project, "microphone");
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let microphone = project.stream_id_of("microphone");

        for spelled_otherwise in [
            camera.to_uppercase(),
            format!(" {camera}"),
            "camera".to_string(),
        ] {
            let refusal = refusal_of(
                runner.stop_the_stream_finishing_its_pending_stop(&spelled_otherwise, None),
            );

            assert!(refusal.contains("no stream with the id"), "{refusal}");
        }
        assert!(!hosted.has_ended());
        assert_eq!(runner.ids_of_the_loaded_streams(), [camera]);
        assert_eq!(
            the_current_record_in(state_directory.path(), &microphone),
            Some(stopped)
        );
    }

    #[test]
    #[serial]
    fn removing_a_recorded_stream_unloads_it_and_forgets_its_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let mut stopped = a_record_of(&project, "microphone");
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        assert_eq!(
            runner.remove_one_stream(&camera).unwrap(),
            StreamRemoveOutcome {
                stream_id: camera.clone(),
                unloaded: true,
                forgotten: true,
            }
        );
        assert_eq!(
            runner.remove_one_stream(&microphone).unwrap(),
            StreamRemoveOutcome {
                stream_id: microphone.clone(),
                unloaded: false,
                forgotten: true,
            }
        );

        assert!(running.has_ended());
        assert!(runner.list_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn renaming_a_recorded_stream_rewrites_its_record_then_its_table_entry_and_a_restart_keeps_the_name()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            let running = a_recorded_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_record_of(&project, "camera"),
            );
            let mut stopped = a_record_of(&project, "microphone");
            stopped.stopped = true;
            records_in(state_directory.path()).write(&stopped).unwrap();

            let renamed = runner
                .rename_stream(&camera, "Hello camera")
                .expect("the loaded recorded stream is renamed");
            runner
                .rename_stream(&microphone, "  Desk mic ✨  ")
                .expect("the stopped stream is renamed");

            assert_eq!(
                renamed,
                StreamRenameOutcome {
                    stream_id: camera.clone(),
                    display_name: "Hello camera".to_string(),
                }
            );
            assert_eq!(running.display_name(), "Hello camera");
            assert_eq!(running.stream_id(), camera, "a rename never changes the id");
            assert_eq!(
                the_record_of(state_directory.path(), &camera).display_name,
                "Hello camera"
            );
            assert_eq!(
                the_record_of(state_directory.path(), &microphone),
                StreamRecord {
                    display_name: "  Desk mic ✨  ".to_string(),
                    ..stopped
                },
                "the display name is recorded as given, never cast, and nothing else changes"
            );
        }

        let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let shown_as: Vec<(String, String)> = fresh_runner
            .list_streams()
            .into_iter()
            .map(|listing| (listing.stream_id, listing.display_name))
            .collect();
        let mut expected = vec![
            (camera, "Hello camera".to_string()),
            (microphone, "  Desk mic ✨  ".to_string()),
        ];
        expected.sort_by(|first, second| first.0.cmp(&second.0));
        assert_eq!(shown_as, expected);
    }

    #[test]
    #[serial]
    fn a_rename_whose_record_cannot_be_written_is_refused_and_the_stream_is_shown_as_it_was() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_record_of(&project, "camera"),
        );

        let refused = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            runner.rename_stream(&camera, "Hello camera")
        };

        refusal_of(refused);
        assert_eq!(running.display_name(), "camera");
        assert_eq!(
            the_record_of(&streams_directory, &camera).display_name,
            "camera"
        );
    }

    #[test]
    #[serial]
    fn renaming_a_loaded_recorded_stream_whose_record_is_unreadable_is_refused_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );

        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let refusal = refusal_of(runner.rename_stream(&camera, "Hello camera"));

                assert!(
                    refusal.contains(&record_path.display().to_string()),
                    "{refusal}"
                );
                assert_eq!(running.display_name(), "camera");
                assert_eq!(std::fs::read(record_path).unwrap(), unreadable_bytes);
            },
        );
    }

    #[test]
    #[serial]
    fn renaming_a_stream_its_host_loaded_shows_it_under_the_name_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");

        runner
            .rename_stream(&project.stream_id_of("camera"), "Hello camera")
            .expect("the stream its host loaded is renamed");

        assert_eq!(hosted.display_name(), "Hello camera");
        assert_eq!(runner.list_streams()[0].display_name, "Hello camera");
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_rename_is_refused_for_a_name_showing_nothing_an_id_held_by_no_stream_and_an_earlier_record()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let camera = project.stream_id_of("camera");
        std::fs::write(
            state_directory.path().join("main.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "stream_name": "main",
                "project_directory": "/home/someone/hello",
                "interpreter": "/home/someone/hello/.venv/bin/python",
                "stream_function": null,
                "graph": {"stream": "main", "nodes": [], "links": []},
                "stopped": false,
                "failed_because": null,
                "runtime_crashes_in_a_row_implicating_it": 0,
                "exposure_rulings": [],
            }))
            .unwrap(),
        )
        .unwrap();

        for showing_nothing in ["", "   ", "\t\n"] {
            let refusal = refusal_of(runner.rename_stream(&camera, showing_nothing));

            assert!(refusal.contains("shows nothing"), "{refusal}");
        }
        let refusal = refusal_of(runner.rename_stream("absent", "Hello"));
        assert!(refusal.contains("`absent`"), "{refusal}");
        assert!(refusal.contains(&camera), "{refusal}");
        let refusal = refusal_of(runner.rename_stream("main", "Hello"));
        assert!(refusal.contains("was not renamed"), "{refusal}");
        assert!(refusal.contains("earlier runtime"), "{refusal}");
        assert_eq!(hosted.display_name(), camera);
    }

    #[test]
    #[serial]
    fn the_listing_names_each_stream_by_id_display_name_and_location_and_one_not_loaded_counts_no_node()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "a-hosted");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "b-recorded"),
        );
        let mut stopped = a_record_of(&project, "c-stopped");
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        records_in(state_directory.path())
            .write(&a_record_of(&project, "d-not-re-loaded"))
            .unwrap();

        let listing_of = |function_name: &str, state, node_count| StreamListing {
            stream_id: project.stream_id_of(function_name),
            display_name: function_name.to_string(),
            stream_location: Some(project.location_of(function_name)),
            state,
            node_count,
            failed_because: None,
            capture_devices_whose_access_was_never_asked_for: Vec::new(),
            stream_action_under_way: None,
        };
        let mut expected = [
            StreamListing {
                display_name: project.stream_id_of("a-hosted"),
                stream_location: None,
                ..listing_of("a-hosted", StreamListingState::Running, Some(1))
            },
            listing_of("b-recorded", StreamListingState::Running, Some(1)),
            listing_of("c-stopped", StreamListingState::Stopped, None),
            listing_of("d-not-re-loaded", StreamListingState::Running, None),
        ];
        expected.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        assert_eq!(runner.list_streams(), expected);
    }

    #[test]
    #[serial]
    fn a_record_an_earlier_runtime_wrote_is_listed_failed_never_loaded_and_removed_by_its_file_name()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let earlier_record_path = state_directory.path().join("main.json");
        let earlier_record_bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": "/home/someone/hello",
            "interpreter": "/home/someone/hello/.venv/bin/python",
            "stream_function": null,
            "graph": {"stream": "main", "nodes": [], "links": []},
            "stopped": false,
            "failed_because": null,
            "runtime_crashes_in_a_row_implicating_it": 0,
            "exposure_rulings": [],
        }))
        .unwrap();
        std::fs::write(&earlier_record_path, &earlier_record_bytes).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &a_crash_pinned_on(&["main"]),
        );
        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();
        let listing = runner.list_streams();

        assert_eq!(
            std::fs::read(&earlier_record_path).unwrap(),
            earlier_record_bytes,
            "neither the crash count nor the re-load rewrites it"
        );
        let [
            StreamReloadAtTheStart::SkippedAsFailed {
                stream_id,
                failed_because,
            },
        ] = reloads.as_slice()
        else {
            panic!("the record is skipped as failed: {reloads:?}");
        };
        assert_eq!(stream_id, "main");
        assert!(
            failed_because.contains("an earlier runtime recorded it"),
            "{failed_because}"
        );
        let [listed] = listing.as_slice() else {
            panic!("the one record is listed: {listing:?}");
        };
        assert_eq!(listed.stream_id, "main");
        assert_eq!(listed.display_name, "main");
        assert_eq!(listed.stream_location, None);
        assert_eq!(listed.state, StreamListingState::Failed);
        assert_eq!(listed.node_count, None);
        let listed_failed_because = listed.failed_because.as_deref().unwrap_or_default();
        assert!(
            listed_failed_because.contains("an earlier runtime recorded it")
                && listed_failed_because.contains("/home/someone/hello"),
            "{listed_failed_because}"
        );
        let refusal = refusal_of(runner.stop_the_stream_finishing_its_pending_stop("main", None));
        assert!(
            refusal.contains("an earlier runtime recorded it"),
            "{refusal}"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());

        assert_eq!(
            runner.remove_one_stream("main").unwrap(),
            StreamRemoveOutcome {
                stream_id: "main".to_string(),
                unloaded: false,
                forgotten: true,
            }
        );
        assert!(!earlier_record_path.exists());
        assert!(runner.list_streams().is_empty());
    }

    #[test]
    #[serial]
    fn reloading_reports_each_recorded_stream_whose_venv_cannot_be_made_by_id_and_keeps_going() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv();
        let records = records_in(state_directory.path());
        for function_name in ["a-camera", "c-microphone"] {
            records
                .write(&a_record_of(&project, function_name))
                .unwrap();
        }
        let malformed = state_directory.path().join("b-malformed.json");
        std::fs::write(&malformed, b"{").unwrap();
        let mut stopped = a_record_of(&project, "d-stopped");
        stopped.stopped = true;
        records.write(&stopped).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert_eq!(reloads.len(), 3, "{reloads:?}");
        let mut ids_not_reloaded = Vec::new();
        let mut unreadable_paths = Vec::new();
        for reload in &reloads {
            match reload {
                StreamReloadAtTheStart::NotReloaded { stream_id, refusal } => {
                    let refusal = refusal.to_string();
                    assert!(
                        refusal.contains(&project.interpreter().display().to_string()),
                        "{refusal}"
                    );
                    assert!(refusal.contains("carries no uv"), "{refusal}");
                    ids_not_reloaded.push(stream_id.clone());
                }
                StreamReloadAtTheStart::RecordUnreadable(unreadable) => {
                    unreadable_paths.push(unreadable.path.clone());
                }
                other => {
                    panic!("only the two recorded streams and the malformed record: {other:?}")
                }
            }
        }
        ids_not_reloaded.sort();
        let mut expected_ids = vec![
            project.stream_id_of("a-camera"),
            project.stream_id_of("c-microphone"),
        ];
        expected_ids.sort();
        assert_eq!(ids_not_reloaded, expected_ids);
        assert_eq!(unreadable_paths, [malformed]);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(records.read_every().len(), 4, "every record stays");
    }

    #[test]
    #[serial]
    fn a_recorded_stream_stopped_or_removed_between_the_listing_and_its_reload_stays_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let records = records_in(state_directory.path());
        for function_name in ["a-stopped", "b-removed"] {
            records
                .write(&a_record_of(&project, function_name))
                .unwrap();
        }
        let listed = records.read_every();
        let mut stopped_since =
            the_record_of(state_directory.path(), &project.stream_id_of("a-stopped"));
        stopped_since.stopped = true;
        records.write(&stopped_since).unwrap();
        assert!(records.remove(&project.stream_id_of("b-removed")).unwrap());

        let reloads = runner.reload_each_recorded_stream_listed(
            &records,
            listed,
            &is_the_machines_shutdown_requested,
        );

        assert!(reloads.is_empty(), "neither is reported: {reloads:?}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            records.read_every(),
            vec![Ok(StreamRecordAsRead::Current(stopped_since))]
        );
    }

    #[test]
    #[serial]
    fn a_record_unreadable_by_its_reload_is_reported_unreadable_by_its_path() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let records = records_in(state_directory.path());
        records.write(&a_record_of(&project, "camera")).unwrap();
        let listed = records.read_every();
        let record_path_malformed_since_the_listing = records
            .record_path_of(&project.stream_id_of("camera"))
            .unwrap();
        std::fs::write(&record_path_malformed_since_the_listing, b"{").unwrap();

        let reloads = runner.reload_each_recorded_stream_listed(
            &records,
            listed,
            &is_the_machines_shutdown_requested,
        );

        let [StreamReloadAtTheStart::RecordUnreadable(unreadable)] = reloads.as_slice() else {
            panic!("the record is reported unreadable: {reloads:?}");
        };
        assert_eq!(unreadable.path, record_path_malformed_since_the_listing);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(
            record_path_malformed_since_the_listing.is_file(),
            "the record is left in place"
        );
    }

    #[test]
    #[serial]
    fn a_recorded_stream_loaded_between_the_listing_and_its_reload_is_left_as_it_is() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let record = a_record_of(&project, "camera");
        records_in(state_directory.path()).write(&record).unwrap();
        let listed = records_in(state_directory.path()).read_every();
        let loaded_since =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let reloads = runner.reload_each_recorded_stream_listed(
            &records_in(state_directory.path()),
            listed,
            &is_the_machines_shutdown_requested,
        );

        assert!(reloads.is_empty(), "nothing is reported: {reloads:?}");
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&record.stream_id)
                .unwrap()
                .stream_tag(),
            loaded_since.stream_tag()
        );
        assert!(!loaded_since.has_ended());
    }
    #[test]
    #[serial]
    fn a_run_leaves_a_stream_its_host_loaded_and_forgets_a_record_whose_document_is_gone() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(
            the_graph_of_a_stream_whose_load_is_refused("studio"),
        );
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let record = a_record_of(&project, "recorded-camera");
        let recorded =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains("AbsentNodeType"),
            "{failed_because}"
        );
        assert_eq!(stream.stream_id, project.stream_id_of("studio"));
        assert!(!hosted.has_ended());
        assert!(recorded.has_ended());
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [project.stream_id_of("camera")]
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &record.stream_id),
            None
        );
    }

    // The tests below start streams, which needs the engine's GPU context.

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn two_projects_of_one_stream_name_run_side_by_side_under_two_ids_and_neither_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let main_worktree = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
        let second_worktree =
            ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

        let first_run = runner
            .load_the_project_off_its_queue(main_worktree.run_request())
            .expect("the first worktree's stream runs");
        let second_run = runner
            .load_the_project_off_its_queue(second_worktree.run_request())
            .expect("the second worktree's stream runs beside it");
        let (first, _, _) = the_one_running_stream_of(&first_run);
        let (second, _, _) = the_one_running_stream_of(&second_run);

        assert_eq!(first.stream_id, main_worktree.stream_id_of("main"));
        assert_eq!(second.stream_id, second_worktree.stream_id_of("main"));
        assert_ne!(first.stream_id, second.stream_id);
        assert_eq!(
            (first.display_name.as_str(), second.display_name.as_str()),
            ("main", "main")
        );
        let mut loaded_ids = runner.ids_of_the_loaded_streams();
        loaded_ids.sort();
        let mut expected_ids = vec![first.stream_id.clone(), second.stream_id.clone()];
        expected_ids.sort();
        assert_eq!(loaded_ids, expected_ids);
        assert!(the_current_record_in(state_directory.path(), &first.stream_id).is_some());
        assert!(the_current_record_in(state_directory.path(), &second.stream_id).is_some());

        let rerun = runner
            .load_the_project_off_its_queue(main_worktree.run_request())
            .expect("the first worktree runs again");
        let (rerun, _, _) = the_one_running_stream_of(&rerun);
        assert_eq!(rerun.stream_id, first.stream_id, "a re-run keeps its id");
        assert!(rerun.replaced_a_recorded_stream);
        assert_eq!(records_in(state_directory.path()).read_every().len(), 2);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_starts_the_stream_under_its_locations_id_and_records_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let camera_project =
            ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let preview_project =
            ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));
        let camera = camera_project.stream_id_of("camera");
        let preview = preview_project.stream_id_of("preview");

        let camera_outcome = runner
            .load_the_project_off_its_queue(camera_project.run_request())
            .expect("the camera stream runs");
        let preview_outcome = runner
            .load_the_project_off_its_queue(preview_project.run_request())
            .expect("the preview stream runs");
        let (camera_run, _, camera_node_count) = the_one_running_stream_of(&camera_outcome);
        let (preview_run, _, _) = the_one_running_stream_of(&preview_outcome);

        assert_eq!(camera_run.stream_id, camera);
        assert_eq!(
            camera_run.display_name, "camera",
            "its graph document's name by default"
        );
        assert_eq!(
            camera_run.stream_location,
            camera_project.location_of("camera")
        );
        assert_eq!(camera_node_count, 1);
        assert!(!camera_run.replaced_a_recorded_stream);
        assert_eq!(camera_outcome.project_directory, camera_project.path());
        assert_eq!(preview_run.stream_id, preview);
        assert_eq!(preview_run.display_name, "preview");
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(a_record_of(&camera_project, "camera"))
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &preview),
            Some(a_record_of(&preview_project, "preview"))
        );
        let mut expected_listing = [
            StreamListing {
                stream_id: camera.clone(),
                display_name: "camera".to_string(),
                stream_location: Some(camera_project.location_of("camera")),
                state: StreamListingState::Running,
                node_count: Some(1),
                failed_because: None,
                capture_devices_whose_access_was_never_asked_for: Vec::new(),
                stream_action_under_way: None,
            },
            StreamListing {
                stream_id: preview.clone(),
                display_name: "preview".to_string(),
                stream_location: Some(preview_project.location_of("preview")),
                state: StreamListingState::Running,
                node_count: Some(1),
                failed_because: None,
                capture_devices_whose_access_was_never_asked_for: Vec::new(),
                stream_action_under_way: None,
            },
        ];
        expected_listing.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        assert_eq!(runner.list_streams(), expected_listing);
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(
            running.status(),
            crate::core::runtime::RuntimeStatus::Started
        );
        assert_eq!(running.holding(), LoadedStreamHolding::RecordedByTheRuntime);
        assert_eq!(
            running.to_json().unwrap()["stream"],
            serde_json::json!(camera),
            "the loaded graph's stream is the id"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_of_the_records_location_replaces_it_under_its_id_keeping_its_renamed_display_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let first_run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        let (first, first_stream_tag, _) = the_one_running_stream_of(&first_run);
        runner.rename_stream(&camera, "Front camera").unwrap();
        let changed_source = the_graph_of_a_stream_named("camera");
        project.write_the_graph_document(changed_source);

        let replaced_run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("the recorded stream is replaced");
        let (replaced, replaced_stream_tag, _) = the_one_running_stream_of(&replaced_run);

        assert_eq!(
            first.display_name, "camera",
            "a new stream is shown under its graph document's name"
        );
        assert!(replaced.replaced_a_recorded_stream);
        assert_eq!(replaced.stream_id, camera);
        assert_eq!(
            replaced.display_name, "Front camera",
            "a load keeps the renamed display name"
        );
        assert_ne!(replaced_stream_tag, first_stream_tag);
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(running.stream_tag(), replaced_stream_tag);
        assert_eq!(running.display_name(), "Front camera");
        assert_eq!(
            the_record_of(state_directory.path(), &camera).display_name,
            "Front camera"
        );

        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_stopped_project_runs_again_and_re_loads_at_a_restart() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            runner
                .load_the_project_off_its_queue(project.run_request())
                .unwrap();
            runner
                .stop_the_stream_finishing_its_pending_stop(&camera, None)
                .unwrap();

            let run = runner
                .load_the_project_off_its_queue(project.run_request())
                .expect("the stopped project runs again");

            let (stream, _, node_count) = the_one_running_stream_of(&run);
            assert_eq!(stream.stream_id, camera);
            assert_eq!(node_count, 1);
            assert!(runner.loaded_stream_of_the_id(&camera).is_ok());
            assert!(!the_record_of(state_directory.path(), &camera).stopped);
        }

        let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let reloads = restarted.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
            ),
            "{reloads:?}"
        );
        assert!(restarted.loaded_stream_of_the_id(&camera).is_ok());
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_records_its_stream_before_the_start_and_a_refused_start_records_it_failed() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let records_seen_by_the_start = Arc::new(Mutex::new(None));
        let records_the_start_saw = Arc::clone(&records_seen_by_the_start);
        let streams_directory = state_directory.path().to_path_buf();
        runner
            .install_setup_hook(move |_| {
                *records_the_start_saw.lock() =
                    Some(records_in(&streams_directory).read_every().len());
                Err(Error::Configuration("the setup hook refuses".into()))
            })
            .unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a refused start is a failed stream, not a refused run");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains("the setup hook refuses"),
            "{failed_because}"
        );
        assert_eq!(
            *records_seen_by_the_start.lock(),
            Some(1),
            "the stream is recorded before its start, so a crash in the start is counted"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        let recorded = the_record_of(state_directory.path(), &stream.stream_id);
        assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_whose_record_is_not_written_unloads_its_stream_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(runner.load_the_project_off_its_queue(project.run_request()))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(&streams_directory).read_every().is_empty());
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_replace_whose_record_is_not_written_leaves_the_running_stream_as_it_was() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        let first_run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        let (_, first_stream_tag, _) = the_one_running_stream_of(&first_run);
        let mut previous_record = the_record_of(&streams_directory, &camera);
        previous_record.runtime_crashes_in_a_row_implicating_it = 1;
        records_in(&streams_directory)
            .write(&previous_record)
            .unwrap();
        let mut a_replacement_graph = the_graph_of_a_stream_named("camera");
        a_replacement_graph["nodes"][0]["name"] = "replacement-source".into();
        project.write_the_graph_document(a_replacement_graph);

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(runner.load_the_project_off_its_queue(project.run_request()))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        let running = runner
            .loaded_stream_of_the_id(&camera)
            .expect("the running stream stays loaded");
        assert_eq!(
            running.stream_tag(),
            first_stream_tag,
            "the stream running is the previous record's, not the one whose record was refused"
        );
        assert_eq!(
            the_current_record_in(&streams_directory, &camera),
            Some(previous_record)
        );
    }

    /// A previous run's crash pinned on each of `streams`, `SIGSEGV` on each.
    fn a_crash_pinned_on(streams: &[&str]) -> HowThePreviousRuntimeRunEnded {
        HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
            causes_by_implicated_stream: streams
                .iter()
                .map(|stream| (stream.to_string(), "SIGSEGV".to_string()))
                .collect(),
            causes_on_threads_no_stream_owns: Vec::new(),
        })
    }

    #[test]
    #[serial]
    fn a_recorded_stream_implicated_in_two_crashes_in_a_row_is_failed_naming_the_last_cause() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        for function_name in ["crasher", "steady"] {
            records_in(state_directory.path())
                .write(&a_record_of(&project, function_name))
                .unwrap();
        }
        let crasher = project.stream_id_of("crasher");
        let steady = project.stream_id_of("steady");
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let failed_by_the_first = runner
            .count_the_previous_runtime_runs_end_against_the_recorded_streams(&a_crash_pinned_on(
                &[&crasher],
            ));

        assert!(failed_by_the_first.is_empty(), "{failed_by_the_first:?}");
        let crasher_record = the_record_of(state_directory.path(), &crasher);
        assert_eq!(crasher_record.runtime_crashes_in_a_row_implicating_it, 1);
        assert!(!crasher_record.is_failed());

        let failed_by_the_second = runner
            .count_the_previous_runtime_runs_end_against_the_recorded_streams(&a_crash_pinned_on(
                &[&crasher],
            ));

        assert_eq!(failed_by_the_second, std::slice::from_ref(&crasher));
        let crasher_record = the_record_of(state_directory.path(), &crasher);
        assert_eq!(crasher_record.runtime_crashes_in_a_row_implicating_it, 2);
        let failed_because = crasher_record
            .failed_because
            .expect("the crasher is failed");
        assert!(
            failed_because.contains("last 2 crashes in a row"),
            "{failed_because}"
        );
        assert!(failed_because.contains("SIGSEGV"), "{failed_because}");
        let steady_record = the_record_of(state_directory.path(), &steady);
        assert_eq!(steady_record.runtime_crashes_in_a_row_implicating_it, 0);
        assert!(!steady_record.is_failed());
        assert_eq!(
            runner
                .list_streams()
                .into_iter()
                .map(|listing| (
                    listing.stream_id,
                    listing.state,
                    listing.failed_because.is_some()
                ))
                .collect::<Vec<_>>(),
            [
                (crasher, StreamListingState::Failed, true),
                (steady, StreamListingState::Running, false),
            ]
        );
    }

    #[test]
    #[serial]
    fn a_clean_end_or_a_crash_pinned_elsewhere_between_two_crashes_resets_the_count() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        records_in(state_directory.path())
            .write(&a_record_of(&project, "crasher"))
            .unwrap();
        let crasher = project.stream_id_of("crasher");
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let a_crash_pinned_on_no_stream =
            HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
                causes_by_implicated_stream: BTreeMap::new(),
                causes_on_threads_no_stream_owns: vec!["SIGSEGV".to_string()],
            });

        for previous_run_between_the_crashes in [
            HowThePreviousRuntimeRunEnded::Cleanly,
            a_crash_pinned_on(&["another-stream"]),
            a_crash_pinned_on_no_stream,
        ] {
            runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
                &a_crash_pinned_on(&[&crasher]),
            );
            runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
                &previous_run_between_the_crashes,
            );
            assert_eq!(
                the_record_of(state_directory.path(), &crasher)
                    .runtime_crashes_in_a_row_implicating_it,
                0,
                "{previous_run_between_the_crashes:?} did not reset the count"
            );
            let failed = runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
                &a_crash_pinned_on(&[&crasher]),
            );

            assert!(
                failed.is_empty(),
                "{previous_run_between_the_crashes:?}: {failed:?}"
            );
            assert!(!the_record_of(state_directory.path(), &crasher).is_failed());
            runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
                &HowThePreviousRuntimeRunEnded::Cleanly,
            );
        }
    }

    #[test]
    #[serial]
    fn a_failed_stream_is_left_failed_by_a_crash_count_and_skipped_at_the_start() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let crasher = project.stream_id_of("crasher");
        let mut failed = a_record_of(&project, "crasher");
        failed.failed_because = Some("it crashed the runtime".to_string());
        failed.runtime_crashes_in_a_row_implicating_it = 2;
        records_in(state_directory.path()).write(&failed).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &HowThePreviousRuntimeRunEnded::Cleanly,
        );
        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert_eq!(the_record_of(state_directory.path(), &crasher), failed);
        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::SkippedAsFailed { stream_id, failed_because }]
                    if *stream_id == crasher && failed_because == "it crashed the runtime"
            ),
            "{reloads:?}"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        let refusal = refusal_of(runner.stop_the_stream_finishing_its_pending_stop(&crasher, None));
        assert!(refusal.contains("is failed"), "{refusal}");
        assert!(refusal.contains("it crashed the runtime"), "{refusal}");
        assert_eq!(the_record_of(state_directory.path(), &crasher), failed);
    }

    #[test]
    #[serial]
    fn a_recorded_stream_whose_venv_is_gone_is_failed_at_the_start_naming_its_interpreter() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera"))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::NotReloaded { stream_id, .. }] if *stream_id == camera
            ),
            "{reloads:?}"
        );
        let failed_because = the_record_of(state_directory.path(), &camera)
            .failed_because
            .expect("the stream is failed");
        assert!(
            failed_because.contains(&project.interpreter().display().to_string()),
            "{failed_because}"
        );
        assert!(failed_because.contains("re-load"), "{failed_because}");
        let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        assert!(matches!(
            fresh_runner
                .reload_every_recorded_stream_neither_stopped_nor_failed_at_once()
                .as_slice(),
            [StreamReloadAtTheStart::SkippedAsFailed { .. }]
        ));
    }

    /// A stub of the runtime unit's uv, outside every project, that counts its
    /// runs.
    struct StubRuntimeUnitUv {
        uv: PathBuf,
        uv_runs: PathBuf,
        directory: tempfile::TempDir,
    }

    impl StubRuntimeUnitUv {
        fn running(body: &str) -> Self {
            let directory = a_temporary_directory_at_owner_only_mode().expect("the uv's directory");
            let uv = directory.path().join("uv");
            let uv_runs = directory.path().join("uv-runs");
            write_an_executable_script_from_a_child_process(
                &uv,
                &format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n",
                    uv_runs.display()
                ),
            );
            Self {
                uv,
                uv_runs,
                directory,
            }
        }

        /// A uv whose sync puts back `project`'s `.venv`, which is moved aside
        /// here: the project is left with no venv for the runtime to make.
        fn making_the_venv_moved_aside_from(project: &ProjectWithGraphDocuments) -> Self {
            let stub_uv = Self::running("");
            let venv_uv_makes = stub_uv.directory.path().join("venv-uv-makes");
            std::fs::rename(project.path().join(".venv"), &venv_uv_makes)
                .expect("the venv is moved aside");
            write_an_executable_script_from_a_child_process(
                &stub_uv.uv,
                &format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\ncp -R '{}' .venv\n",
                    stub_uv.uv_runs.display(),
                    venv_uv_makes.display()
                ),
            );
            stub_uv
        }

        /// A uv whose sync half-makes a `.venv`, then fails writing
        /// `standard_error`.
        fn failing_writing(standard_error: &str) -> Self {
            Self::running(&format!(
                "mkdir -p .venv/bin\necho '{standard_error}' >&2\nexit 1"
            ))
        }

        /// The runtime handed this uv.
        fn handed_to(&self, runner: &Runner) {
            runner
                .set_runtime_unit_uv_executable(self.uv.clone())
                .expect("the runtime is handed its uv");
        }

        /// Each run, by its arguments.
        fn runs(&self) -> Vec<String> {
            std::fs::read_to_string(&self.uv_runs)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    const UV_RESOLUTION_FAILURE: &str =
        "error: No solution found when resolving dependencies: tatolab-stream==99 is unavailable";

    #[test]
    #[serial]
    fn a_run_in_a_project_with_no_venv_makes_it_with_the_runtime_unit_uv_then_compiles() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        the_one_running_stream_of(&run);
        assert_eq!(stub_uv.runs(), ["sync"]);
        assert!(project.interpreter().is_file());
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_interpreter_runs_never_runs_uv() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        the_one_running_stream_of(&run);
        assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_interpreter_does_not_run_has_its_venv_made_again() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);
        directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &project.path().join(".venv").join("bin"),
            0o755,
        )
        .unwrap();
        std::os::unix::fs::symlink("/nonexistent/python3.12", project.interpreter()).unwrap();

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        the_one_running_stream_of(&run);
        assert_eq!(stub_uv.runs(), ["sync"]);
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_uv_cannot_make_records_each_stream_failed_in_uvs_words() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains(UV_RESOLUTION_FAILURE),
            "{failed_because}"
        );
        assert_eq!(stream.stream_location, project.location_of("camera"));
        assert!(
            !project.path().join(".venv").exists(),
            "the half-made venv was left"
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            the_record_of(state_directory.path(), &stream.stream_id).failed_because,
            Some(failed_because.to_string())
        );
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_uv_cannot_make_writes_uvs_lines_to_the_projects_log_under_no_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let page =
            streamlib_runtime_client_contract::project_log_records::project_log_records_after(
                &std::fs::canonicalize(project.path()).unwrap(),
                None,
                None,
                None,
                4096,
            )
            .expect("the project's log reads");
        let uv_line = page
            .records
            .iter()
            .find(|record| {
                record["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(UV_RESOLUTION_FAILURE))
            })
            .unwrap_or_else(|| panic!("the project's log lacks uv's line: {:#?}", page.records));
        assert!(
            uv_line.get("stream").is_none_or(serde_json::Value::is_null),
            "{uv_line}"
        );
        assert_eq!(
            uv_line["runtime_id"].as_str(),
            Some(runner.runtime_id().to_string().as_str())
        );
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_uv_cannot_make_unloads_and_fails_every_record_of_the_project() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert_eq!(stream.stream_id, camera);
        assert!(
            failed_because.contains(UV_RESOLUTION_FAILURE),
            "{failed_because}"
        );
        assert!(running.has_ended());
        assert!(the_record_of(state_directory.path(), &camera).is_failed());
    }

    #[test]
    #[serial]
    fn a_run_on_a_runtime_given_no_uv_records_each_stream_failed_saying_the_unit_carries_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        project.write_the_graph_document(the_graph_of_a_stream_named("camera"));

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains(
                "the runtime unit carries no uv; rebuild it with `cargo xtask build-runtime`"
            ),
            "{failed_because}"
        );
        assert_eq!(stream.stream_location, project.location_of("camera"));
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_in_a_missing_project_directory_is_refused_with_nothing_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let refusal = refusal_of(runner.load_the_project_off_its_queue(
            ProjectStreamsToReadAndLoad {
                project_directory: project.path().join("missing"),
                ..project.run_request()
            },
        ));

        assert!(refusal.contains("holds no project"), "{refusal}");
        assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_makes_a_gone_venv_once_for_every_record_of_its_project() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        project.hold_the_streams(&["camera", "microphone"], &[]);
        let records = records_in(state_directory.path());
        for stream_name in ["camera", "microphone"] {
            records.write(&a_record_of(&project, stream_name)).unwrap();
        }
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        stub_uv.handed_to(&runner);

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert!(
            reloads
                .iter()
                .all(|reload| matches!(reload, StreamReloadAtTheStart::Reloaded { .. })),
            "{reloads:?}"
        );
        assert_eq!(reloads.len(), 2, "{reloads:?}");
        assert_eq!(stub_uv.runs(), ["sync"]);
    }

    /// A record of the stream `stream_name` of `project`, whose graph document it writes,
    /// whose one node refuses an access nobody asked for unless its stream may ask the user.
    fn a_record_of_a_stream_whose_node_asks_for_a_capture_device(
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> StreamRecord {
        project.write_the_graph_document(serde_json::json!({
            "stream": stream_name,
            "nodes": [{
                "name": "camera",
                "type": MockSourceRefusingAnAccessNobodyAskedFor::Processor::processor_class_import_path().as_str(),
                "config": {},
            }],
            "links": [],
        }));
        a_record_of(project, stream_name)
    }

    /// Mental revert: load the re-load's records as a start does, asking the
    /// user, and the node sets up with its prompt up at login.
    #[test]
    #[serial]
    fn the_reload_at_the_start_never_lets_a_node_ask_the_user_and_its_node_refuses_by_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of_a_stream_whose_node_asks_for_a_capture_device(
                &project, "camera",
            ))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        let reloaded = runner
            .loaded_stream_of_the_id_when_loaded(&camera)
            .expect("the rest of the stream re-loads");
        let refusal = reloaded
            .wait_until_every_processor_is_running(STREAM_ACTION_TEST_DEADLINE)
            .expect_err("the node may not ask, so its setup refuses")
            .to_string();
        assert!(
            refusal.contains(MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR),
            "{refusal}"
        );
        let listing = runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == camera)
            .expect("the re-loaded stream is listed");
        assert_eq!(
            listing.capture_devices_whose_access_was_never_asked_for,
            vec![PrivacyGatedCaptureDevice::Camera],
            "the listing names the device the stream waits on"
        );
    }

    #[test]
    #[serial]
    fn a_run_after_the_reload_lets_the_node_ask_the_user() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of_a_stream_whose_node_asks_for_a_capture_device(
                &project, "camera",
            ))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        runner
            .stop_the_stream_finishing_its_pending_stop(&camera, None)
            .expect("the re-loaded stream stops");
        runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("the stopped project runs");

        runner
            .loaded_stream_of_the_id_when_loaded(&camera)
            .expect("the started stream is loaded")
            .wait_until_every_processor_is_running(STREAM_ACTION_TEST_DEADLINE)
            .expect("a start someone asked for may ask the user, so the node sets up");
        let listing = runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == camera)
            .expect("the started stream is listed");
        assert!(
            listing
                .capture_devices_whose_access_was_never_asked_for
                .is_empty(),
            "a started stream waits on no access: {listing:?}"
        );
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_records_failed_in_uvs_words_a_stream_whose_venv_uv_cannot_make() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera"))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::NotReloaded { stream_id, .. }] if *stream_id == camera
            ),
            "{reloads:?}"
        );
        let failed_because = the_record_of(state_directory.path(), &camera)
            .failed_because
            .expect("the stream is failed");
        assert!(
            failed_because.contains(UV_RESOLUTION_FAILURE),
            "{failed_because}"
        );
        assert!(failed_because.contains("re-load"), "{failed_because}");
    }

    #[test]
    fn a_runtime_handed_its_uv_twice_refuses_the_second() {
        let runner = a_runner_recording_its_streams_in(None);
        runner
            .set_runtime_unit_uv_executable(PathBuf::from("/opt/tatolab/bin/uv"))
            .unwrap();

        let refusal =
            refusal_of(runner.set_runtime_unit_uv_executable(PathBuf::from("/elsewhere/uv")));

        assert!(refusal.contains("/opt/tatolab/bin/uv"), "{refusal}");
    }

    /// A uv whose sync waits for its gate file before it puts back `project`'s
    /// `.venv`, moved aside here, and the gate that releases it.
    fn a_uv_making_the_venv_once_released(
        project: &ProjectWithGraphDocuments,
    ) -> (StubRuntimeUnitUv, PathBuf) {
        let stub_uv = StubRuntimeUnitUv::running("");
        let venv_uv_makes = stub_uv.directory.path().join("venv-uv-makes");
        let gate = stub_uv.directory.path().join("released");
        std::fs::rename(project.path().join(".venv"), &venv_uv_makes)
            .expect("the venv is moved aside");
        write_an_executable_script_from_a_child_process(
            &stub_uv.uv,
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nwhile [ ! -e '{}' ]; do sleep 0.02; done\ncp -R \
                 '{}' .venv\n",
                stub_uv.uv_runs.display(),
                gate.display(),
                venv_uv_makes.display()
            ),
        );
        (stub_uv, gate)
    }

    /// How long a test waits for a stream action to reach what it asserts.
    const STREAM_ACTION_TEST_DEADLINE: Duration = Duration::from_secs(30);

    /// The stream action `action_id` once `reached` holds of it.
    fn the_stream_action_once(
        runner: &Runner,
        action_id: &str,
        reached: impl Fn(&StreamAction) -> bool,
    ) -> StreamAction {
        let deadline = Instant::now() + STREAM_ACTION_TEST_DEADLINE;
        loop {
            let action = runner
                .describe_stream_action(action_id)
                .expect("the action is held");
            if reached(&action) {
                return action;
            }
            assert!(
                Instant::now() < deadline,
                "the action never got there: {action:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The stream action `action_id` once it has finished.
    fn the_stream_action_once_finished(runner: &Runner, action_id: &str) -> StreamAction {
        the_stream_action_once(runner, action_id, |action| action.state.is_finished())
    }

    fn project_start_or_reload_request_of(project_directory: &Path) -> ProjectStartOrReloadRequest {
        ProjectStartOrReloadRequest {
            project_directory: project_directory.to_path_buf(),
        }
    }

    /// A project directory holding only a venv, so a create writes every scaffold file.
    fn a_directory_holding_only_a_venv() -> ProjectWithGraphDocuments {
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
        std::fs::remove_file(project.path().join("pyproject.toml")).unwrap();
        std::fs::remove_file(project.path().join(PROJECT_FILE_NAME)).unwrap();
        project
    }

    #[test]
    #[serial]
    fn a_create_project_writes_the_scaffold_commits_it_and_loads_its_stream_under_the_folders_name()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let first_commits = Arc::new(Mutex::new(Vec::new()));
        let first_commits_made = Arc::clone(&first_commits);
        runner
            .set_created_project_first_commit(Arc::new(move |project_directory: &Path| {
                first_commits_made
                    .lock()
                    .push(project_directory.to_path_buf());
                Ok(())
            }))
            .unwrap();
        let project = a_directory_holding_only_a_venv();
        let project_directory = std::fs::canonicalize(project.path()).unwrap();
        let folder_name = project_directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let queued = runner
            .create_project(CreateProjectRequest {
                project_directory: Some(project.path().to_path_buf()),
            })
            .expect("a create of a project is queued");

        assert_eq!(queued.project_directory, project_directory);
        let folder_cast = crate::core::graph::cast_exposed_name_to_url_safe(&folder_name).unwrap();
        assert!(
            queued
                .stream_id
                .starts_with(&format!("{folder_cast}-main-")),
            "{}",
            queued.stream_id
        );
        let done = the_stream_action_once_finished(&runner, &queued.action_id);
        assert_eq!(done.state, StreamActionState::Done, "{done:?}");
        assert_eq!(done.kind, StreamActionKind::Load);
        assert_eq!(
            done.streams
                .iter()
                .map(|stream| (stream.stream_id.clone(), stream.display_name.clone()))
                .collect::<Vec<_>>(),
            [(queued.stream_id.clone(), folder_name.clone())]
        );
        for scaffolded_file in [
            PROJECT_FILE_NAME,
            "streams/main.graph.json",
            "nodes/__init__.py",
            "nodes/halftone_lens_effect.py",
            "nodes/brightness_meter.py",
            "pyproject.toml",
            ".python-version",
            ".gitignore",
        ] {
            assert!(
                project_directory.join(scaffolded_file).is_file(),
                "{scaffolded_file}"
            );
        }
        assert!(!project_directory.join("stream.py").exists());
        let project_file = TatolabProjectFile::read_from_project_directory(&project_directory)
            .expect("the project file reads");
        assert_eq!(project_file.name, folder_name);
        let record = the_record_of(state_directory.path(), &queued.stream_id);
        assert_eq!(record.display_name, folder_name);
        assert_eq!(record.location.project, project_file.project);
        assert_eq!(*first_commits.lock(), [project_directory]);
    }

    #[test]
    #[serial]
    fn a_create_over_a_scaffold_file_fails_its_action_with_nothing_written() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = a_directory_holding_only_a_venv();
        std::fs::create_dir_all(project.path().join("streams")).unwrap();
        std::fs::write(
            project.path().join("streams/main.graph.json"),
            "# the user's own\n",
        )
        .unwrap();

        let queued = runner
            .create_project(CreateProjectRequest {
                project_directory: Some(project.path().to_path_buf()),
            })
            .expect("a create over a scaffold file is queued, and its action fails");

        let failed = the_stream_action_once_finished(&runner, &queued.action_id);
        assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
        assert!(
            failed
                .failed_because
                .as_deref()
                .is_some_and(|why| why.contains("already has streams/main.graph.json")),
            "{failed:?}"
        );
        assert!(!project.path().join("pyproject.toml").exists());
        assert!(!project.path().join(PROJECT_FILE_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(project.path().join("streams/main.graph.json")).unwrap(),
            "# the user's own\n"
        );
        assert!(runner.list_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_create_in_a_relative_directory_is_refused_with_nothing_created() {
        let runner = a_runner_recording_its_streams_in(None);
        let refusal = runner
            .create_project(CreateProjectRequest {
                project_directory: Some(PathBuf::from("a-relative-project")),
            })
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("must be an absolute path"), "{refusal}");
        assert!(!Path::new("a-relative-project").exists());
    }

    #[test]
    #[serial]
    fn a_start_project_answers_its_action_at_once_and_the_action_walks_to_done_with_its_streams() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        project.hold_the_streams(&["camera", "preview"], &[]);
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let project_directory = std::fs::canonicalize(project.path()).unwrap();

        let queued = runner
            .start_project(project_start_or_reload_request_of(project.path()))
            .expect("a run of a project is queued");

        assert_eq!(queued.project_directory, project_directory);
        let making_the_venv = the_stream_action_once(&runner, &queued.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        assert_eq!(making_the_venv.state, StreamActionState::InProgress);
        assert_eq!(making_the_venv.kind, StreamActionKind::Load);
        let mut streams_preparing: Vec<StreamListing> = runner
            .list_streams()
            .into_iter()
            .filter(|listing| listing.state == StreamListingState::Preparing)
            .collect();
        streams_preparing.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        let mut expected_preparing = vec![
            project.stream_id_of("camera"),
            project.stream_id_of("preview"),
        ];
        expected_preparing.sort();
        assert_eq!(
            streams_preparing
                .iter()
                .map(|listing| listing.stream_id.clone())
                .collect::<Vec<_>>(),
            expected_preparing,
            "each graph document's stream is listed preparing while the venv is made"
        );
        for listing in &streams_preparing {
            assert_eq!(
                listing.stream_action_under_way,
                Some(StreamActionWorkingOnAStream {
                    action_id: queued.action_id.clone(),
                    step: Some(StreamActionStep::MakingTheVenv),
                })
            );
        }

        std::fs::write(&gate, "").unwrap();
        let done = the_stream_action_once_finished(&runner, &queued.action_id);

        assert_eq!(done.state, StreamActionState::Done, "{done:?}");
        assert_eq!(done.step, None);
        assert_eq!(
            done.streams
                .iter()
                .map(|stream| (stream.stream_id.clone(), stream.state))
                .collect::<Vec<_>>(),
            [
                (
                    project.stream_id_of("camera"),
                    Some(StreamListingState::Running)
                ),
                (
                    project.stream_id_of("preview"),
                    Some(StreamListingState::Running)
                ),
            ]
        );
        assert!(
            runner
                .list_streams()
                .iter()
                .all(|listing| listing.state == StreamListingState::Running
                    && listing.stream_action_under_way.is_none()),
            "{:?}",
            runner.list_streams()
        );
        assert_eq!(stub_uv.runs().len(), 1, "{:?}", stub_uv.runs());
    }

    #[test]
    #[serial]
    fn an_action_whose_body_panics_is_recorded_failed_and_its_projects_next_action_runs() {
        let runner = a_runner_recording_its_streams_in(None);
        let project = tempfile::tempdir().unwrap();
        let queue_of_each_project = &runner.stream_actions.stream_action_queue_of_each_project;
        let an_action_of_the_project = |action_id: &str| StreamAction {
            action_id: action_id.to_string(),
            project_directory: project.path().to_path_buf(),
            kind: StreamActionKind::Load,
            state: StreamActionState::Queued,
            step: None,
            streams: Vec::new(),
            failed_because: None,
        };
        let panicking = queue_of_each_project.queue_the_stream_action(
            an_action_of_the_project("test-1"),
            Box::new(|_: &Runner| panic!("a body panicking on purpose")),
            &runner.tokio_handle(),
        );
        let after_it = queue_of_each_project.queue_the_stream_action(
            an_action_of_the_project("test-2"),
            Box::new(|runner: &Runner| {
                runner
                    .stream_actions
                    .stream_action_queue_of_each_project
                    .change_the_held_stream_action("test-2", |action| {
                        action.state = StreamActionState::Done
                    });
            }),
            &runner.tokio_handle(),
        );

        let after_it_finished = the_stream_action_once_finished(&runner, &after_it.action_id);
        let panicked = runner.describe_stream_action(&panicking.action_id).unwrap();

        assert_eq!(after_it_finished.state, StreamActionState::Done);
        assert_eq!(panicked.state, StreamActionState::Failed, "{panicked:?}");
        assert_eq!(
            panicked.failed_because.as_deref(),
            Some("the action panicked")
        );
    }

    #[test]
    #[serial]
    fn one_projects_actions_run_in_order_while_another_projects_run_beside_them() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let waiting_on_its_venv =
            ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&waiting_on_its_venv);
        stub_uv.handed_to(&runner);
        let beside_it = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));

        let first = runner
            .start_project(project_start_or_reload_request_of(
                waiting_on_its_venv.path(),
            ))
            .unwrap();
        let second = runner
            .reload_project(project_start_or_reload_request_of(
                waiting_on_its_venv.path(),
            ))
            .unwrap();
        the_stream_action_once(&runner, &first.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        let other_project = runner
            .start_project(project_start_or_reload_request_of(beside_it.path()))
            .unwrap();
        let other_project_done = the_stream_action_once_finished(&runner, &other_project.action_id);

        assert_eq!(
            other_project_done.state,
            StreamActionState::Done,
            "{other_project_done:?}"
        );
        assert_eq!(
            runner
                .describe_stream_action(&first.action_id)
                .unwrap()
                .state,
            StreamActionState::InProgress,
            "another project's queue never waits on this one's venv"
        );
        assert_eq!(
            runner
                .describe_stream_action(&second.action_id)
                .unwrap()
                .state,
            StreamActionState::Queued,
            "a project's second action waits for its first"
        );
        assert_ne!(first.action_id, second.action_id);
        let preparing_while_the_first_makes_the_venv: Vec<_> = runner
            .list_streams()
            .into_iter()
            .filter(|listing| listing.state == StreamListingState::Preparing)
            .map(|listing| listing.stream_action_under_way)
            .collect();
        assert_eq!(
            preparing_while_the_first_makes_the_venv,
            [Some(StreamActionWorkingOnAStream {
                action_id: first.action_id.clone(),
                step: Some(StreamActionStep::MakingTheVenv),
            })],
            "the project shows the action running on it, never the one queued behind it"
        );

        std::fs::write(&gate, "").unwrap();
        let second_done = the_stream_action_once_finished(&runner, &second.action_id);

        assert_eq!(
            second_done.state,
            StreamActionState::Done,
            "{second_done:?}"
        );
        assert_eq!(
            runner
                .describe_stream_action(&first.action_id)
                .unwrap()
                .state,
            StreamActionState::Done
        );
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [
                waiting_on_its_venv.stream_id_of("camera"),
                beside_it.stream_id_of("preview")
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
    }

    #[test]
    #[serial]
    fn a_start_project_of_a_folder_that_is_no_project_fails_its_action_saying_why() {
        let runner = a_runner_recording_its_streams_in(None);
        let no_project = a_temporary_directory_at_owner_only_mode().unwrap();

        for (project_directory, why) in [
            (no_project.path().join("missing"), "does not exist"),
            (
                no_project.path().to_path_buf(),
                "is not a project: it holds no tatolab-project.json",
            ),
        ] {
            let queued = runner
                .start_project(project_start_or_reload_request_of(&project_directory))
                .expect("a load refuses nothing at the call");
            let failed = the_stream_action_once_finished(&runner, &queued.action_id);

            assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
            let failed_because = failed.failed_because.unwrap_or_default();
            assert!(failed_because.contains(why), "{failed_because}");
        }
        assert!(
            refusal_of(
                runner.start_project(project_start_or_reload_request_of(Path::new(
                    "relative/desk"
                )))
            )
            .contains("`project_directory` must be an absolute path"),
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_queued_run_of_a_project_holding_no_graph_document_is_done_with_no_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();

        let queued = runner
            .start_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        let done = the_stream_action_once_finished(&runner, &queued.action_id);

        assert_eq!(done.state, StreamActionState::Done, "{done:?}");
        assert!(done.streams.is_empty(), "{done:?}");
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn an_action_never_held_is_refused_naming_it() {
        let runner = a_runner_recording_its_streams_in(None);

        assert!(
            refusal_of(runner.describe_stream_action("Rgone-7"))
                .contains("the stream action `Rgone-7` is not held")
        );
    }

    /// The pending actions on the record of `stream_id`, as `(action, kind)`.
    fn the_pending_stream_actions_on(
        streams_directory: &Path,
        stream_id: &str,
    ) -> Vec<(String, PendingStreamActionKind)> {
        the_record_of(streams_directory, stream_id)
            .pending_stream_actions
            .into_iter()
            .map(|pending| (pending.action_id, pending.kind))
            .collect()
    }

    #[test]
    #[serial]
    fn a_stop_then_a_run_of_the_project_each_finish_in_order_behind_the_run_ahead_of_them() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let run_ahead_of_them = runner
            .reload_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &run_ahead_of_them.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });

        let stop = runner.stop_project(project.path()).unwrap();
        let run_after_the_stop = runner
            .start_project(project_start_or_reload_request_of(project.path()))
            .unwrap();

        assert_eq!(
            the_pending_stream_actions_on(state_directory.path(), &camera),
            [(stop.action_id.clone(), PendingStreamActionKind::Stop)]
        );
        assert_eq!(
            runner
                .describe_stream_action(&stop.action_id)
                .unwrap()
                .state,
            StreamActionState::Queued
        );

        std::fs::write(&gate, "").unwrap();
        let stopped = the_stream_action_once_finished(&runner, &stop.action_id);
        let run_again = the_stream_action_once_finished(&runner, &run_after_the_stop.action_id);

        assert_eq!(stopped.kind, StreamActionKind::Stop);
        assert_eq!(stopped.state, StreamActionState::Done, "{stopped:?}");
        assert_eq!(stopped.streams[0].state, Some(StreamListingState::Stopped));
        assert_eq!(run_again.state, StreamActionState::Done, "{run_again:?}");
        assert_eq!(
            run_again.streams[0].state,
            Some(StreamListingState::Running)
        );
        let record = the_record_of(state_directory.path(), &camera);
        assert!(!record.stopped);
        assert!(record.pending_stream_actions.is_empty(), "{record:?}");
        assert_eq!(runner.ids_of_the_loaded_streams(), [camera]);
    }

    #[test]
    #[serial]
    fn a_stop_queued_behind_a_run_queued_behind_a_stop_stops_the_project_last() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let run_ahead_of_them = runner
            .reload_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &run_ahead_of_them.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });

        let first_stop = runner.stop_project(project.path()).unwrap();
        let run_between_the_stops = runner
            .start_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        let last_stop = runner.stop_project(project.path()).unwrap();

        assert_eq!(
            the_pending_stream_actions_on(state_directory.path(), &camera),
            [
                (first_stop.action_id.clone(), PendingStreamActionKind::Stop),
                (last_stop.action_id.clone(), PendingStreamActionKind::Stop),
            ]
        );

        std::fs::write(&gate, "").unwrap();
        the_stream_action_once_finished(&runner, &first_stop.action_id);
        let run_between =
            the_stream_action_once_finished(&runner, &run_between_the_stops.action_id);
        let stopped_last = the_stream_action_once_finished(&runner, &last_stop.action_id);

        assert_eq!(
            run_between.state,
            StreamActionState::Done,
            "{run_between:?}"
        );
        assert_eq!(
            stopped_last.state,
            StreamActionState::Done,
            "{stopped_last:?}"
        );
        assert_eq!(
            stopped_last.streams[0].state,
            Some(StreamListingState::Stopped)
        );
        let record = the_record_of(state_directory.path(), &camera);
        assert!(record.stopped);
        assert!(record.pending_stream_actions.is_empty(), "{record:?}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_queued_stop_cancelled_takes_its_mark_off_and_a_load_cancelled_in_its_venv_leaves_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let (stub_uv, _gate_never_released) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let load = runner
            .reload_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &load.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        let stop = runner.stop_project(project.path()).unwrap();

        let stop_cancelled = runner.cancel_stream_action(&stop.action_id).unwrap();
        let load_asked_to_cancel = runner.cancel_stream_action(&load.action_id).unwrap();
        let load_cancelled = the_stream_action_once_finished(&runner, &load.action_id);

        assert_eq!(stop_cancelled.state, StreamActionState::Cancelled);
        assert!(the_pending_stream_actions_on(state_directory.path(), &camera).is_empty());
        assert_eq!(load_asked_to_cancel.state, StreamActionState::InProgress);
        assert_eq!(
            load_cancelled.state,
            StreamActionState::Cancelled,
            "{load_cancelled:?}"
        );
        assert_eq!(load_cancelled.failed_because, None);
        assert!(!project.path().join(".venv").exists());
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            std::slice::from_ref(&camera)
        );
        assert!(!the_record_of(state_directory.path(), &camera).stopped);
        assert!(
            refusal_of(runner.cancel_stream_action(&load.action_id))
                .contains("is already cancelled")
        );
        assert!(refusal_of(runner.cancel_stream_action("R-never")).contains("is not held"));
        let follow_on = runner.stop_project(project.path()).unwrap();
        assert_eq!(
            the_stream_action_once_finished(&runner, &follow_on.action_id).state,
            StreamActionState::Done
        );
    }

    #[test]
    fn a_running_stop_or_removal_refuses_its_cancel_and_a_running_load_takes_it() {
        let running_of = |kind: StreamActionKind| StreamAction {
            action_id: "R-1".to_string(),
            project_directory: PathBuf::from("/srv/desk"),
            kind,
            state: StreamActionState::InProgress,
            step: None,
            streams: Vec::new(),
            failed_because: None,
        };

        for unload in [StreamActionKind::Stop, StreamActionKind::Remove] {
            let refusal = refusal_of(refuse_the_cancel_of_a_running_unload(&running_of(unload)));
            assert!(refusal.contains("cannot be cut short"), "{refusal}");
        }
        for load in [StreamActionKind::Load, StreamActionKind::ReloadAtStart] {
            assert!(refuse_the_cancel_of_a_running_unload(&running_of(load)).is_ok());
        }
    }

    #[test]
    #[serial]
    fn a_stop_of_a_stopped_project_is_refused_with_nothing_queued() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );

        let stop = runner.stop_project(project.path()).unwrap();
        let refusal = refusal_of(runner.stop_project(project.path()));
        assert!(refusal.contains("none to stop"), "{refusal}");
        let elsewhere = ProjectWithGraphDocuments::with_no_venv();
        assert!(refusal_of(runner.stop_project(elsewhere.path())).contains("none to stop"));
        assert!(refusal_of(runner.stop_project(Path::new("relative"))).contains("absolute"));

        the_stream_action_once_finished(&runner, &stop.action_id);
        assert!(the_pending_stream_actions_on(state_directory.path(), &camera).is_empty());
        assert!(the_record_of(state_directory.path(), &camera).stopped);
    }

    #[test]
    #[serial]
    fn a_stop_of_a_project_stops_each_running_stream_and_leaves_a_failed_one_as_it_is() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        project.write_the_graph_document(the_graph_of_a_stream_named("microphone"));
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "microphone"),
        );
        let failed = StreamRecord {
            failed_because: Some("it crashed the runtime".to_string()),
            ..a_record_of(&project, "preview")
        };
        records_in(state_directory.path()).write(&failed).unwrap();

        let stop = runner.stop_project(project.path()).unwrap();
        let stopped = the_stream_action_once_finished(&runner, &stop.action_id);

        assert_eq!(stopped.state, StreamActionState::Done, "{stopped:?}");
        let mut stream_states: Vec<_> = stopped
            .streams
            .iter()
            .map(|stream| (stream.stream_id.clone(), stream.state))
            .collect();
        stream_states.sort_by(|first, second| first.0.cmp(&second.0));
        let mut expected = vec![
            (camera.clone(), Some(StreamListingState::Stopped)),
            (microphone.clone(), Some(StreamListingState::Stopped)),
        ];
        expected.sort_by(|first, second| first.0.cmp(&second.0));
        assert_eq!(stream_states, expected);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(the_record_of(state_directory.path(), &camera).stopped);
        assert!(the_record_of(state_directory.path(), &microphone).stopped);
        let preview = the_record_of(state_directory.path(), &failed.stream_id);
        assert!(preview.is_failed() && !preview.stopped, "{preview:?}");
        assert!(preview.pending_stream_actions.is_empty(), "{preview:?}");
    }

    #[test]
    #[serial]
    fn a_stream_that_fails_before_the_stop_reaches_it_is_left_failed_with_its_mark_taken_off() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera"))
            .unwrap();
        project.write_an_unloadable_graph_document("camera");
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let run_ahead_of_it = runner
            .reload_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &run_ahead_of_it.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        let stop = runner.stop_project(project.path()).unwrap();

        std::fs::write(&gate, "").unwrap();
        the_stream_action_once_finished(&runner, &run_ahead_of_it.action_id);
        let stop_once_finished = the_stream_action_once_finished(&runner, &stop.action_id);

        assert_eq!(
            stop_once_finished.state,
            StreamActionState::Done,
            "{stop_once_finished:?}"
        );
        let record = the_record_of(state_directory.path(), &camera);
        assert!(record.pending_stream_actions.is_empty(), "{record:?}");
        assert!(record.is_failed() && !record.stopped, "{record:?}");
    }

    #[test]
    #[serial]
    fn a_stop_queued_behind_a_removal_of_its_project_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let mut removed_with_its_project = a_record_of(&project, "camera");
        removed_with_its_project
            .queue_the_pending_stream_action("R-1", PendingStreamActionKind::Remove);
        records_in(state_directory.path())
            .write(&removed_with_its_project)
            .unwrap();

        let refusal = refusal_of(runner.stop_project(project.path()));

        assert!(
            refusal.contains("being removed with its project"),
            "{refusal}"
        );
        assert_eq!(
            the_pending_stream_actions_on(
                state_directory.path(),
                &removed_with_its_project.stream_id
            )
            .len(),
            1
        );
    }

    #[test]
    #[serial]
    fn a_fresh_runtime_finishes_the_marks_a_crash_left_in_order_before_it_queues_its_reloads() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let records = records_in(state_directory.path());
        let running = a_record_of(&project, "camera");
        let mut stopped = a_record_of(&project, "mic");
        stopped.queue_the_pending_stream_action("R-3", PendingStreamActionKind::Stop);
        let mut removed = a_record_of(&project, "preview");
        removed.queue_the_pending_stream_action("R-4", PendingStreamActionKind::Stop);
        removed.queue_the_pending_stream_action("R-5", PendingStreamActionKind::Remove);
        for record in [&running, &stopped, &removed] {
            records.write(record).unwrap();
        }
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        runner.finish_the_pending_stream_actions_a_crash_left();
        let reloads = runner.queue_the_reload_of_every_project_at_the_start();

        assert_eq!(reloads.len(), 1, "one re-load per project: {reloads:?}");
        assert!(the_current_record_in(state_directory.path(), &removed.stream_id).is_none());
        let mic = the_record_of(state_directory.path(), &stopped.stream_id);
        assert!(
            mic.stopped && mic.pending_stream_actions.is_empty(),
            "{mic:?}"
        );
        let reload = the_stream_action_once_finished(&runner, &reloads[0].action_id);
        assert_eq!(reload.kind, StreamActionKind::ReloadAtStart);
        assert_eq!(reload.state, StreamActionState::Done, "{reload:?}");
        assert_eq!(
            reload
                .streams
                .iter()
                .map(|stream| (stream.stream_id.clone(), stream.state))
                .collect::<Vec<_>>(),
            [(running.stream_id.clone(), Some(StreamListingState::Running))]
        );
        assert!(
            the_record_of(state_directory.path(), &running.stream_id)
                .pending_stream_actions
                .is_empty()
        );
        assert_eq!(runner.ids_of_the_loaded_streams(), [running.stream_id]);
    }

    #[test]
    #[serial]
    fn a_rename_answers_during_a_load_and_the_load_keeps_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&StreamRecord {
                stopped: true,
                ..a_record_of(&project, "camera")
            })
            .unwrap();
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let load = runner
            .start_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &load.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });

        runner.rename_stream(&camera, "Desk camera").unwrap();
        std::fs::write(&gate, "").unwrap();
        let loaded = the_stream_action_once_finished(&runner, &load.action_id);

        assert_eq!(loaded.state, StreamActionState::Done, "{loaded:?}");
        let record = the_record_of(state_directory.path(), &camera);
        assert_eq!(record.display_name, "Desk camera");
        assert_eq!(record.failed_because, None);
    }

    #[test]
    #[serial]
    fn a_remove_project_unloads_and_forgets_every_stream_of_the_project_shown_stopping_until_then()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let other_project =
            ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));
        let camera = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        project.hold_the_streams(&["camera", "mic"], &[]);
        let mic = StreamRecord {
            stopped: true,
            ..a_record_of(&project, "mic")
        };
        records_in(state_directory.path()).write(&mic).unwrap();
        let preview = a_record_of(&other_project, "preview");
        records_in(state_directory.path()).write(&preview).unwrap();
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let run_ahead_of_it = runner
            .reload_project(project_start_or_reload_request_of(project.path()))
            .unwrap();
        the_stream_action_once(&runner, &run_ahead_of_it.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });

        let removal = runner.remove_project(project.path()).unwrap();

        assert_eq!(
            the_pending_stream_actions_on(state_directory.path(), &mic.stream_id),
            [(removal.action_id.clone(), PendingStreamActionKind::Remove)]
        );
        let mic_listed = runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == mic.stream_id)
            .unwrap();
        assert_eq!(mic_listed.state, StreamListingState::Preparing);
        std::fs::write(&gate, "").unwrap();
        the_stream_action_once_finished(&runner, &run_ahead_of_it.action_id);
        let removed = the_stream_action_once_finished(&runner, &removal.action_id);

        assert_eq!(removed.kind, StreamActionKind::Remove);
        assert_eq!(removed.state, StreamActionState::Done, "{removed:?}");
        assert!(camera.has_ended());
        assert!(the_current_record_in(state_directory.path(), camera.stream_id()).is_none());
        assert!(the_current_record_in(state_directory.path(), &mic.stream_id).is_none());
        assert!(the_current_record_in(state_directory.path(), &preview.stream_id).is_some());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(
            refusal_of(runner.remove_project(project.path()))
                .contains("holds no stream of the project")
        );
        assert!(refusal_of(runner.remove_project(Path::new("relative"))).contains("absolute"));
    }

    #[test]
    #[serial]
    fn a_stop_queued_behind_a_load_shows_its_stream_stopping_once_the_load_has_run() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let stop = runner.stop_project(project.path()).unwrap();
        let stopped = the_stream_action_once_finished(&runner, &stop.action_id);

        assert_eq!(stopped.state, StreamActionState::Done);
        assert_eq!(runner.list_streams()[0].state, StreamListingState::Stopped);
        assert_eq!(runner.list_streams()[0].stream_id, camera);
    }

    fn the_listed_state_of(runner: &Runner, stream_id: &str) -> StreamListingState {
        runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == stream_id)
            .unwrap_or_else(|| panic!("the stream `{stream_id}` is listed"))
            .state
    }

    #[test]
    #[serial]
    fn a_pause_or_resume_of_a_relative_directory_or_a_project_running_nothing_is_refused() {
        let runner = a_runner_recording_its_streams_in(None);
        let idle = ProjectWithGraphDocuments::with_no_venv();

        assert!(refusal_of(runner.pause_project(Path::new("relative"))).contains("absolute"));
        assert!(refusal_of(runner.resume_project(Path::new("relative"))).contains("absolute"));
        let pause_refusal = refusal_of(runner.pause_project(idle.path()));
        assert!(pause_refusal.contains("none to pause"), "{pause_refusal}");
        let resume_refusal = refusal_of(runner.resume_project(idle.path()));
        assert!(resume_refusal.contains("is not paused"), "{resume_refusal}");
    }

    #[test]
    #[serial]
    fn a_start_or_reload_of_a_project_already_in_that_state_is_refused_naming_the_verb_that_fits() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let never_started = ProjectWithGraphDocuments::with_no_venv();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );

        for relative_refusal in [
            refusal_of(
                runner.start_project(project_start_or_reload_request_of(Path::new("relative"))),
            ),
            refusal_of(
                runner.reload_project(project_start_or_reload_request_of(Path::new("relative"))),
            ),
        ] {
            assert!(relative_refusal.contains("absolute"), "{relative_refusal}");
        }
        let refusal = refusal_of(
            runner.reload_project(project_start_or_reload_request_of(never_started.path())),
        );
        assert!(
            refusal.contains("was never started") && refusal.contains("`start_project`"),
            "{refusal}"
        );
        let refusal =
            refusal_of(runner.start_project(project_start_or_reload_request_of(project.path())));
        assert!(
            refusal.contains("already runs") && refusal.contains("`reload_project`"),
            "{refusal}"
        );

        runner.pause_project(project.path()).unwrap();
        let refusal =
            refusal_of(runner.start_project(project_start_or_reload_request_of(project.path())));
        assert!(
            refusal.contains("is paused") && refusal.contains("`resume_project`"),
            "{refusal}"
        );

        let stop = runner.stop_project(project.path()).unwrap();
        let refusal =
            refusal_of(runner.reload_project(project_start_or_reload_request_of(project.path())));
        assert!(
            refusal.contains("is stopped") && refusal.contains("`start_project`"),
            "{refusal}"
        );
        the_stream_action_once_finished(&runner, &stop.action_id);
        let refusal =
            refusal_of(runner.reload_project(project_start_or_reload_request_of(project.path())));
        assert!(refusal.contains("is stopped"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_paused_project_stays_paused_until_its_resume_or_its_stop_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        let camera = project.stream_id_of("camera");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );

        let paused = runner.pause_project(project.path()).unwrap();
        assert_eq!(
            paused.stream_ids,
            Vec::<String>::new(),
            "an unstarted stream is not paused"
        );
        let paused_again = runner.pause_project(project.path()).unwrap();
        assert_eq!(paused_again.project_directory, paused.project_directory);
        assert!(!the_record_of(state_directory.path(), &camera).stopped);
        assert!(runner.resume_project(project.path()).is_ok());
        let refusal = refusal_of(runner.resume_project(project.path()));
        assert!(refusal.contains("is not paused"), "{refusal}");

        runner.pause_project(project.path()).unwrap();
        let stop = runner.stop_project(project.path()).unwrap();
        let refusal = refusal_of(runner.resume_project(project.path()));
        assert!(refusal.contains("is not paused"), "{refusal}");
        the_stream_action_once_finished(&runner, &stop.action_id);
        assert!(the_record_of(state_directory.path(), &camera).stopped);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_paused_project_lists_paused_re_loads_paused_and_resumes_running_and_a_restart_runs_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
        project.write_the_graph_document(the_graph_of_a_stream_named("microphone"));
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        let mut both_streams = vec![camera.clone(), microphone.clone()];
        both_streams.sort();
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            runner
                .load_the_project_off_its_queue(project.run_request())
                .unwrap();

            let paused = runner.pause_project(project.path()).unwrap();
            let mut paused_ids = paused.stream_ids.clone();
            paused_ids.sort();
            assert_eq!(paused_ids, both_streams);
            for stream_id in &both_streams {
                assert_eq!(
                    the_listed_state_of(&runner, stream_id),
                    StreamListingState::Paused
                );
                assert_eq!(
                    runner.loaded_stream_of_the_id(stream_id).unwrap().status(),
                    RuntimeStatus::Paused
                );
            }

            runner
                .load_the_project_off_its_queue(project.run_request())
                .expect("a run of the paused project re-loads it");
            for stream_id in &both_streams {
                assert_eq!(
                    runner.loaded_stream_of_the_id(stream_id).unwrap().status(),
                    RuntimeStatus::Paused,
                    "a stream a run re-loads while its project is paused starts paused"
                );
            }

            let resumed = runner.resume_project(project.path()).unwrap();
            let mut resumed_ids = resumed.stream_ids.clone();
            resumed_ids.sort();
            assert_eq!(resumed_ids, both_streams);
            for stream_id in &both_streams {
                assert_eq!(
                    the_listed_state_of(&runner, stream_id),
                    StreamListingState::Running
                );
            }

            runner.pause_project(project.path()).unwrap();
        }

        let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
        restarted.reload_every_recorded_stream_neither_stopped_nor_failed_at_once();
        for stream_id in &both_streams {
            assert_eq!(
                restarted
                    .loaded_stream_of_the_id(stream_id)
                    .unwrap()
                    .status(),
                RuntimeStatus::Started,
                "pause is not recorded, so a restart runs the project"
            );
        }
    }
}
