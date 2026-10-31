// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions a runtime takes for its owner: run or create a project,
//! loading every `@stream` of its `stream.py` under the stream id each location
//! computes to and recording it — failed when the project's code gives no
//! graph or its load is refused; stop and start a stream; remove a project;
//! rename a stream and set a port's exposure; list what the runtime holds; and
//! re-load the recorded streams at start. Each load and unload is a stream
//! action on its project's queue.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use super::new_project_name_drawing::draw_new_project_directory_under_the_streams_root;
use super::new_project_scaffold::{
    NEW_PROJECT_SCAFFOLD_STREAM_FUNCTION, write_the_new_project_scaffold_into,
};
use super::project_stream_action_queue::{
    StreamActionBody, StreamActionOfAProject, StreamActionQueued, StreamActionQueuesOfTheEngine,
    StreamActionStep,
};
use super::project_venv_made_with_the_runtime_unit_uv::{
    ProjectVenvNotMade, make_the_projects_venv_unless_its_interpreter_runs,
};
use super::runtime::{
    a_stream_id_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
};
use super::{
    CrashOfThePreviousRuntimeRun, HowThePreviousRuntimeRunEnded, LoadedStreamInThisRuntime,
    LoadedStreamTag, OptionsForLoadingOneStream, OwnerExposureRuling,
    OwnerExposureRulingsSplitAroundTheLoad, PendingStreamActionKind,
    PendingStreamActionOnTheRecord, Runner, StreamEnvironment,
    StreamLoadObservingMachineShutdownRequests, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordReadFailure, StreamRecordsInTheStateDirectory, is_the_machines_shutdown_requested,
    is_this_threads_stream_work_interrupted, the_owners_exposure_rulings_split_around_the_load,
};
use crate::core::compiler::compiler_ops::stream_function_compile_in_the_projects_interpreter::{
    OneStreamOfTheProjectCompiled, StreamFunctionCompileInTheProjectsInterpreterReport,
    WhatTheProjectsCodeGaveTheCompile, compile_the_stream_function_in_the_projects_interpreter,
    refuse_a_project_directory_not_named_by_its_absolute_path, the_projects_venv_interpreter,
};
use crate::core::graph::{
    OutputPortExposureLevel, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
    node_names_listed_for_a_refusal,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::{Error, Result};

/// How a loaded stream is held: recorded by the runtime, or living as long as
/// the process that hosts its library [`Runner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadedStreamHolding {
    /// A stream a project's load took in, recorded in the runtime's state
    /// directory and re-loaded at its start until it is stopped, failed or
    /// its project removed.
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

/// What a load of a project did: each stream it loaded and started, or
/// recorded failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLoadOutcome {
    /// The project the load was given.
    pub project_directory: PathBuf,
    /// Each stream the load loaded or recorded failed.
    pub streams: Vec<StreamLoadedOrFailedByARun>,
    /// Why the project gave no stream — its `.venv` was not made, its
    /// `stream.py` did not import or defines no `@stream`; `None` when it gave
    /// streams.
    pub gave_no_stream_because: Option<String>,
}

/// One stream a load of its project loaded and started, or recorded failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLoadedOrFailedByARun {
    /// The stream's id, computed from its location.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives.
    pub stream_location: StreamLocation,
    /// Whether it runs, or why it is failed.
    pub state: StreamStateAfterItsRun,
    /// Whether the load replaced the recorded stream of the same location.
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

/// Whether a stream a load took in runs, or why it is failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStateAfterItsRun {
    /// Loaded and started.
    Running {
        /// The tag of this load of it.
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

/// A stream a stop unloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStopOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Why a recorded stream unloaded here was not recorded stopped, so a
    /// restart of the runtime loads it again; `None` when it was recorded,
    /// and for a stream its host process loaded.
    pub stop_not_recorded_because: Option<String>,
}

/// A recorded stream a start loaded and started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStartOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
}

/// What a remove of a project did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRemoveOutcome {
    /// The project removed.
    pub project_directory: PathBuf,
    /// The id of each stream of it that was loaded and is now unloaded.
    pub streams_unloaded: Vec<String>,
    /// The id of each stream of it that was recorded and is now forgotten.
    pub records_forgotten: Vec<String>,
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
    /// A load or a start of it is under way.
    Preparing,
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the
    /// runtime's next start.
    Running,
    /// A stop of it, or a remove of its project, is under way.
    Stopping,
    /// Stopped by its owner, and left unloaded until `start_stream`.
    Stopped,
    /// It kept crashing the runtime or could not load, and is skipped at the
    /// start until `start_stream` retries it; or it was recorded by an
    /// earlier runtime, and is never loaded.
    Failed,
}

impl StreamListingState {
    /// The state as a listing spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
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
    /// Whether it is preparing, running, stopping, stopped or failed.
    pub state: StreamListingState,
    /// How many nodes its loaded graph holds; `None` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `None` when it is not.
    pub failed_because: Option<String>,
    /// The stream action working on it; `None` when none is.
    pub action_id: Option<String>,
    /// The step that action is on.
    pub step: Option<StreamActionStep>,
}

/// One `@stream` of a project, as its last compile gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOfTheProjectAsLastCompiled {
    /// The stream function as `module:qualname`.
    pub stream_function: String,
    /// Its docstring; `None` when it has none.
    pub description: Option<String>,
    /// The stream id its location computes to.
    pub stream_id: String,
    /// The state the runtime lists it in; `None` when the runtime holds it
    /// not.
    pub state: Option<StreamListingState>,
}

/// A project's streams as its last compile gave them, as
/// [`Runner::list_project_streams`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectStreamsAsLastCompiled {
    /// Each `@stream` of `stream.py`, in definition order.
    Compiled {
        /// The streams.
        streams: Vec<StreamOfTheProjectAsLastCompiled>,
    },
    /// The last compile gave no stream: `stream.py` did not import, or
    /// defines none, or the `.venv` was not made.
    GaveNoStream {
        /// Why.
        failed_because: String,
    },
    /// A load of the project is queued or under way and has not compiled yet.
    Preparing {
        /// The load's action id.
        action_id: String,
    },
    /// No load of the project ran in this runtime.
    NotLoaded,
}

/// An output port's exposure [`Runner::expose_port`] set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPortExposureOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// The node, as the owner named it.
    pub node: String,
    /// The output port, as the owner named it.
    pub port: String,
    /// The level the port is at now, or will be at when the stream loads.
    pub level: OutputPortExposureLevel,
    /// Whether the level was recorded as the owner's ruling on the stream's
    /// record.
    pub recorded: bool,
    /// Why the level, raised live on a loaded recorded stream, was not
    /// recorded as the owner's ruling; `None` when it was recorded, and for a
    /// stream its host process loaded, whose level is never recorded.
    pub ruling_not_recorded_because: Option<String>,
}

/// How many of the runtime's crashes in a row a recorded stream is implicated
/// in before it is failed.
pub const RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM: u32 = 2;

/// What [`Runner::reload_every_recorded_stream_neither_stopped_nor_failed`]
/// did: each project's re-load it queued, and each record it skipped; a
/// stopped record is not reported.
#[derive(Debug)]
pub enum StreamReloadAtTheStart {
    /// The re-load of a project's records neither stopped nor failed was
    /// queued.
    Queued {
        /// The re-load's action id.
        action_id: String,
        /// The project.
        project_directory: PathBuf,
        /// The records it re-loads.
        stream_ids: Vec<String>,
    },
    /// The re-load of a project's records could not be queued.
    NotQueued {
        /// The project.
        project_directory: PathBuf,
        /// Why.
        refusal: Error,
    },
    /// The recorded stream is failed, so it was skipped.
    SkippedAsFailed {
        /// The stream's id.
        stream_id: String,
        /// Why it is failed.
        failed_because: String,
    },
    /// A record that could not be read, left in place.
    RecordUnreadable(StreamRecordReadFailure),
}

/// What a project's last compile gave, kept for `list_project_streams`.
#[derive(Debug, Clone)]
enum WhatTheProjectsLastCompileGave {
    EveryStream(Vec<(String, Option<String>, String)>),
    NoStream(String),
}

/// Where a runtime records its streams, its projects' queues, each project's
/// last compile, and the lock that lets one load or unload step run at a time.
pub(crate) struct StreamActionsOfTheEngine {
    stream_records: OnceLock<StreamRecordsInTheStateDirectory>,
    /// Held across a load's or an unload's step — never across a venv or a
    /// compile, and never by a rename or an exposure.
    one_stream_action_at_a_time: Mutex<()>,
    pub(crate) stream_action_queues: StreamActionQueuesOfTheEngine,
    last_compile_of_each_project: Mutex<HashMap<PathBuf, WhatTheProjectsLastCompileGave>>,
}

impl StreamActionsOfTheEngine {
    /// The stream actions of the runtime `runner`, recording nothing yet.
    pub(crate) fn of_the_runner(runner: std::sync::Weak<Runner>) -> Self {
        Self {
            stream_records: OnceLock::new(),
            one_stream_action_at_a_time: Mutex::new(()),
            stream_action_queues: StreamActionQueuesOfTheEngine::of_the_runner(runner),
            last_compile_of_each_project: Mutex::default(),
        }
    }
}

/// One stream a stream action loads, recorded by the runtime under the id its
/// location computes to: its display name and location, the graph and the
/// owner's rulings, and where its interpreters start.
struct OneStreamAStreamActionLoads<'load> {
    display_name: &'load str,
    stream_location: &'load StreamLocation,
    graph_json: &'load serde_json::Value,
    rulings: &'load [OwnerExposureRuling],
    stream_environment: StreamEnvironment,
}

impl<'load> OneStreamAStreamActionLoads<'load> {
    /// The stream `record` records with the graph `graph_json` it holds, its
    /// rulings applied.
    fn recorded_by(record: &'load StreamRecord, graph_json: &'load serde_json::Value) -> Self {
        Self {
            display_name: &record.display_name,
            stream_location: &record.location,
            graph_json,
            rulings: &record.exposure_rulings,
            stream_environment: record.stream_environment(),
        }
    }
}

/// Note that `action`, when there is one, is on `step`.
fn enter_the_step(action: Option<&StreamActionOfAProject>, step: StreamActionStep) {
    if let Some(action) = action {
        action.enter_step(step);
    }
}

/// Note that `action`, when there is one, touched the stream `stream_id`.
fn note_the_stream_touched(
    action: Option<&StreamActionOfAProject>,
    stream_id: &str,
    display_name: &str,
) {
    if let Some(action) = action {
        action.touched_the_stream(stream_id, display_name);
    }
}

/// Refused when a cancel of `action` was asked for before its teardown
/// began; otherwise `action` is past cancelling from here.
fn go_past_cancelling(action: Option<&StreamActionOfAProject>) -> Result<()> {
    match action {
        Some(action) if !action.go_past_cancelling() => Err(Error::Runtime(format!(
            "the stream action `{}` was cancelled before its teardown began",
            action.action_id()
        ))),
        _ => Ok(()),
    }
}

/// Refused when the work on this thread is interrupted: the machine is
/// shutting down, or the stream action running here was cancelled.
fn refuse_once_this_threads_stream_work_is_interrupted(what_was_not_done: &str) -> Result<()> {
    if is_this_threads_stream_work_interrupted() {
        return Err(Error::Runtime(format!(
            "{what_was_not_done}: the runtime is shutting down, or the stream action was \
             cancelled"
        )));
    }
    Ok(())
}

/// `project_directory` with its symlinks resolved; as given when it cannot be
/// resolved.
fn the_resolved_project_directory(project_directory: &Path) -> PathBuf {
    std::fs::canonicalize(project_directory).unwrap_or_else(|_| project_directory.to_path_buf())
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
        let stream_records = StreamRecordsInTheStateDirectory::open(streams_directory)?;
        self.stream_actions
            .stream_records
            .set(stream_records)
            .map_err(|_| {
                Error::Configuration(format!(
                    "this runtime was handed a streams directory twice at once; it records its \
                     streams in {} only",
                    streams_directory.display()
                ))
            })
    }

    /// Queue a load of every `@stream` in the `stream.py` of the project in
    /// `project_directory`, answered with its action id at once. The load
    /// makes the project's `.venv` unless its interpreter runs, compiles
    /// every stream function, then records and loads each under the id its
    /// location computes to, and starts it — failing each stream whose
    /// function raised, or whose load or start is refused, and every record
    /// of the project when it gives no stream. Refused only for a relative
    /// directory, and on a runtime given no state directory.
    pub fn run_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        self.queue_a_load_of_the_project(project_directory, false)
    }

    /// Queue the creation of a project: in `project_directory`, or in a
    /// folder drawn under the streams root when none is given. The action
    /// writes the files a new project starts from — failing, with nothing
    /// written, when any is already there — then loads it as
    /// [`Self::run_project`] does, its new stream shown under the folder's
    /// name. Answers the action, the project's directory, and the id of the
    /// scaffold's `stream:main` there.
    pub fn create_project(&self, project_directory: Option<&Path>) -> Result<StreamActionQueued> {
        let project_directory = match project_directory {
            Some(project_directory) => {
                refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
                project_directory.to_path_buf()
            }
            None => draw_new_project_directory_under_the_streams_root()?,
        };
        let mut queued = self.queue_a_load_of_the_project(&project_directory, true)?;
        queued.stream_id = Some(
            StreamLocation {
                project_directory: queued.project_directory.clone(),
                stream_function: NEW_PROJECT_SCAFFOLD_STREAM_FUNCTION.to_string(),
            }
            .stream_id(),
        );
        Ok(queued)
    }

    fn queue_a_load_of_the_project(
        &self,
        project_directory: &Path,
        writing_the_scaffold_first: bool,
    ) -> Result<StreamActionQueued> {
        refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
        self.stream_records_or_refuse_naming(project_directory)?;
        self.queue_a_stream_action(
            the_resolved_project_directory(project_directory),
            StreamActionBody::LoadEveryStreamOfTheProject {
                writing_the_scaffold_first,
            },
            |_| Ok(()),
        )
    }

    /// Queue a stop of the stream whose id is `stream_id` on its project's
    /// queue, a pending stop marked on its record. The stop records it
    /// stopped, so a restart leaves it unloaded, and unloads it. Refused naming
    /// the streams the runtime holds when it holds none of that id.
    pub fn stop_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        self.queue_a_stream_action_on_one_stream(
            stream_id,
            PendingStreamActionKind::Stop,
            StreamActionBody::StopTheStream {
                stream_id: stream_id.to_string(),
            },
        )
    }

    /// Queue a start of the recorded stream whose id is `stream_id` on its
    /// project's queue, a pending start marked on its record. The start makes
    /// the project's `.venv` unless its interpreter runs, loads and starts the
    /// stream from its record, and records it neither stopped nor failed.
    /// Refused for an id no record holds, and a record an earlier runtime
    /// wrote.
    pub fn start_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        if self.current_stream_record_of_the_id(stream_id)?.is_none() {
            return Err(Error::NotFound(format!(
                "no stream with the id `{stream_id}` is recorded in this runtime, so there is \
                 none to start. Recorded: {}",
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        self.queue_a_stream_action_on_one_stream(
            stream_id,
            PendingStreamActionKind::Start,
            StreamActionBody::StartTheStream {
                stream_id: stream_id.to_string(),
            },
        )
    }

    /// Queue a remove of the project in `project_directory`, a pending remove
    /// marked on every record of it. The remove forgets every record of the
    /// project and unloads every stream of it, deleting no file. Refused only
    /// for a relative directory.
    pub fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
        let project_directory = the_resolved_project_directory(project_directory);
        let records_of_the_project = self.stream_ids_recorded_from_the_project(&project_directory);
        self.queue_a_stream_action(
            project_directory.clone(),
            StreamActionBody::RemoveTheProject,
            |action_id| {
                let Some(stream_records) = self.stream_records() else {
                    return Ok(());
                };
                for stream_id in &records_of_the_project {
                    if let Err(not_marked) = stream_records.update(stream_id, |record| {
                        record
                            .pending_stream_actions
                            .push(PendingStreamActionOnTheRecord {
                                action_id: action_id.to_string(),
                                kind: PendingStreamActionKind::Remove,
                            });
                        true
                    }) {
                        tracing::warn!(
                            "the remove `{action_id}` of {} is queued, and its pending mark on \
                             the stream `{stream_id}` was not written: {not_marked}",
                            project_directory.display()
                        );
                    }
                }
                Ok(())
            },
        )
    }

    /// Queue `body` on the queue of the project of the stream `stream_id`,
    /// a pending mark of `kind` written on its record when it has one.
    fn queue_a_stream_action_on_one_stream(
        &self,
        stream_id: &str,
        kind: PendingStreamActionKind,
        body: StreamActionBody,
    ) -> Result<StreamActionQueued> {
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        let record = match self.current_stream_record_of_the_id(stream_id) {
            Ok(record) => record,
            Err(unreadable) if loaded.is_none() => return Err(unreadable),
            Err(unreadable) => {
                tracing::warn!(
                    "the stream `{stream_id}` is loaded and its record cannot be read: \
                     {unreadable}"
                );
                None
            }
        };
        let project_directory = match (&record, &loaded) {
            (Some(record), _) => record.location.project_directory.clone(),
            (None, Some(loaded)) => the_project_directory_of_a_loaded_stream(loaded),
            (None, None) => return Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
        };
        self.queue_a_stream_action(project_directory, body, |action_id| {
            let (Some(_), Some(stream_records)) = (&record, self.stream_records()) else {
                return Ok(());
            };
            stream_records.update(stream_id, |record| {
                record
                    .pending_stream_actions
                    .push(PendingStreamActionOnTheRecord {
                        action_id: action_id.to_string(),
                        kind,
                    });
                true
            })?;
            Ok(())
        })
    }

    /// Run `body`, the turn of `action` come, on the calling thread.
    pub(crate) fn run_the_body_of_a_stream_action(
        &self,
        action: &StreamActionOfAProject,
        body: StreamActionBody,
    ) -> Result<()> {
        let project_directory = action.project_directory().to_path_buf();
        match body {
            StreamActionBody::LoadEveryStreamOfTheProject {
                writing_the_scaffold_first,
            } => {
                let loaded = self.load_the_project_now(
                    &project_directory,
                    Some(action),
                    writing_the_scaffold_first,
                )?;
                match loaded.gave_no_stream_because {
                    Some(gave_no_stream_because) => Err(Error::Runtime(format!(
                        "the project in {} gave no stream: {gave_no_stream_because}",
                        project_directory.display()
                    ))),
                    None => Ok(()),
                }
            }
            StreamActionBody::StartTheStream { stream_id } => self
                .start_the_stream_now(&stream_id, Some(action))
                .map(|_| ()),
            StreamActionBody::StopTheStream { stream_id } => self
                .stop_the_stream_now(&stream_id, Some(action))
                .map(|_| ()),
            StreamActionBody::RemoveTheProject => self
                .remove_the_project_now(&project_directory, Some(action))
                .map(|_| ()),
            StreamActionBody::ReloadTheRecordsAtStart { stream_ids } => self
                .reload_the_records_of_a_project_now(&project_directory, &stream_ids, Some(action)),
        }
    }

    /// Clear every pending mark `action` wrote, leaving each record otherwise
    /// as it was.
    pub(crate) fn clear_the_pending_marks_of_a_stream_action(
        &self,
        action: &StreamActionOfAProject,
    ) {
        let Some(stream_records) = self.stream_records() else {
            return;
        };
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(record)) = read else {
                continue;
            };
            if !record
                .pending_stream_actions
                .iter()
                .any(|mark| mark.action_id == action.action_id())
            {
                continue;
            }
            clear_the_pending_mark_of(stream_records, &record.stream_id, Some(action));
        }
    }

    /// The load of a project, run on the calling thread: write its scaffold
    /// first when `writing_the_scaffold_first`, make its `.venv`, compile
    /// every `@stream`, then record and load each, as
    /// [`Self::run_project`] says. Refused for a directory that is not a
    /// project, and for a compile that never reached the project's answer.
    pub(crate) fn load_the_project_now(
        &self,
        project_directory: &Path,
        action: Option<&StreamActionOfAProject>,
        writing_the_scaffold_first: bool,
    ) -> Result<ProjectLoadOutcome> {
        refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
        if writing_the_scaffold_first {
            enter_the_step(action, StreamActionStep::WritingTheScaffold);
            write_the_new_project_scaffold_into(project_directory)?;
        }
        refuse_a_directory_that_is_not_a_project(project_directory)?;
        let stream_records = self.stream_records_or_refuse_naming(project_directory)?;
        let lend_directory = self.lend_directory_or_refuse_naming(project_directory)?;
        let display_name_of_a_new_stream = writing_the_scaffold_first.then(|| {
            StreamLocation::of_a_project_that_gave_no_stream(project_directory.to_path_buf())
                .project_folder_name()
        });

        enter_the_step(action, StreamActionStep::MakingTheVenv);
        if let Err(venv_not_made) = self.make_the_projects_venv(project_directory) {
            let failed_because = match venv_not_made {
                ProjectVenvNotMade::Refused(refusal) => return Err(refusal),
                ProjectVenvNotMade::Failed { failed_because } => failed_because,
            };
            let stream_environment = StreamEnvironment {
                project_directory: the_resolved_project_directory(project_directory),
                interpreter: the_projects_venv_interpreter(project_directory),
            };
            return self.record_a_project_that_gave_no_stream(
                stream_records,
                &stream_environment,
                display_name_of_a_new_stream,
                failed_because,
                action,
            );
        }

        enter_the_step(action, StreamActionStep::Compiling);
        let StreamFunctionCompileInTheProjectsInterpreterReport {
            what_the_projects_code_gave,
            stream_environment,
        } = compile_the_stream_function_in_the_projects_interpreter(
            project_directory,
            None,
            lend_directory,
        )?;

        enter_the_step(action, StreamActionStep::Starting);
        let streams = match what_the_projects_code_gave {
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { streams }
                if !streams.is_empty() =>
            {
                streams
            }
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { .. } => {
                return self.record_a_project_that_gave_no_stream(
                    stream_records,
                    &stream_environment,
                    display_name_of_a_new_stream,
                    "its stream.py defines no @stream".to_string(),
                    action,
                );
            }
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDefinesNoStream {
                no_stream_defined_because: failed_because,
            }
            | WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                import_failed_because: failed_because,
            } => {
                return self.record_a_project_that_gave_no_stream(
                    stream_records,
                    &stream_environment,
                    display_name_of_a_new_stream,
                    failed_because,
                    action,
                );
            }
            WhatTheProjectsCodeGaveTheCompile::Compiled { .. }
            | WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised { .. }
            | WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined { .. } => {
                return Err(Error::Runtime(format!(
                    "the compile of every stream in {} answered for one stream function",
                    project_directory.display()
                )));
            }
        };
        self.keep_the_projects_last_compile(
            &stream_environment.project_directory,
            WhatTheProjectsLastCompileGave::EveryStream(
                streams
                    .iter()
                    .map(|stream| {
                        (
                            stream.stream_location.stream_function.clone(),
                            stream.description.clone(),
                            stream.stream_location.stream_id(),
                        )
                    })
                    .collect(),
            ),
        );

        let ids_the_project_defines: Vec<String> = streams
            .iter()
            .map(|stream| stream.stream_location.stream_id())
            .collect();
        let mut streams_loaded_or_failed = Vec::with_capacity(streams.len());
        for OneStreamOfTheProjectCompiled {
            stream_location,
            description: _,
            graph_or_why_none,
        } in streams
        {
            refuse_once_this_threads_stream_work_is_interrupted(&format!(
                "the load of {} stopped before {stream_location}",
                project_directory.display()
            ))?;
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let shown = stream_location.to_string();
            match self.record_and_load_one_stream_a_run_compiled(
                stream_records,
                StreamARunCompiled {
                    stream_location,
                    display_name_of_a_new_stream: display_name_of_a_new_stream.clone(),
                    stream_environment: &stream_environment,
                    graph_or_why_none,
                },
                action,
            ) {
                Ok(stream) => streams_loaded_or_failed.push(stream),
                Err(not_taken_in) => {
                    tracing::error!("the stream from {shown} was not taken in: {not_taken_in}")
                }
            }
        }
        {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            self.fail_every_record_of_the_project_its_stream_py_no_longer_defines(
                stream_records,
                &stream_environment.project_directory,
                &ids_the_project_defines,
                action,
            );
            self.forget_the_record_of_a_project_that_gives_streams_again(
                stream_records,
                &stream_environment.project_directory,
            );
        }
        Ok(ProjectLoadOutcome {
            project_directory: project_directory.to_path_buf(),
            streams: streams_loaded_or_failed,
            gave_no_stream_because: None,
        })
    }

    /// The stop of the stream `stream_id`, run on the calling thread: a
    /// recorded one is recorded stopped first, its mark of `action` finalised
    /// with it, then unloaded. A loaded recorded stream is unloaded even when
    /// its record cannot be read or written, and the outcome says why it was
    /// not recorded stopped. A stream already stopped is left as it is;
    /// refused for a failed stream, which is not loaded to stop.
    pub(crate) fn stop_the_stream_now(
        &self,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamStopOutcome> {
        let stopped_unrecorded = || StreamStopOutcome {
            stream_id: stream_id.to_string(),
            stop_not_recorded_because: None,
        };
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        if let Some(hosted) = loaded
            .as_ref()
            .filter(|loaded| loaded.holding() == LoadedStreamHolding::LivesWithTheHostProcess)
        {
            note_the_stream_touched(action, stream_id, &hosted.display_name());
            go_past_cancelling(action)?;
            enter_the_step(action, StreamActionStep::Stopping);
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            unload_a_stream_an_action_took_back(hosted, "its owner stopped it");
            return Ok(stopped_unrecorded());
        }
        let record = match self.current_stream_record_of_the_id(stream_id) {
            Ok(record) => record,
            Err(unreadable) => {
                let Some(loaded) = loaded else {
                    return Err(unreadable);
                };
                note_the_stream_touched(action, stream_id, &loaded.display_name());
                go_past_cancelling(action)?;
                enter_the_step(action, StreamActionStep::Stopping);
                let _one_stream_action_at_a_time =
                    self.stream_actions.one_stream_action_at_a_time.lock();
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                return Ok(a_recorded_stream_unloaded_and_not_recorded_stopped(
                    stream_id.to_string(),
                    &unreadable,
                ));
            }
        };
        if let Some(record) = &record {
            note_the_stream_touched(action, stream_id, &record.display_name);
        }
        match (loaded, record) {
            (None, None) => Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
            (None, Some(record)) if record.is_failed() => {
                self.clear_the_pending_mark_on_the_record(stream_id, action);
                Err(Error::Runtime(format!(
                    "the stream `{stream_id}` is failed, so it is not loaded to stop: {}. `start` \
                     retries it, `rm` removes its project",
                    record.failed_because.as_deref().unwrap_or_default()
                )))
            }
            (None, Some(record)) if record.stopped => {
                self.clear_the_pending_mark_on_the_record(stream_id, action);
                tracing::info!("the stream `{stream_id}` is already stopped");
                Ok(stopped_unrecorded())
            }
            (None, Some(_)) => {
                go_past_cancelling(action)?;
                self.record_the_stream_stopped(stream_id, action)?;
                Ok(stopped_unrecorded())
            }
            (Some(loaded), Some(_)) => {
                go_past_cancelling(action)?;
                enter_the_step(action, StreamActionStep::Stopping);
                let recorded_stopped = self.record_the_stream_stopped(stream_id, action);
                let _one_stream_action_at_a_time =
                    self.stream_actions.one_stream_action_at_a_time.lock();
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(match recorded_stopped {
                    Ok(()) => stopped_unrecorded(),
                    Err(not_written) => a_recorded_stream_unloaded_and_not_recorded_stopped(
                        stream_id.to_string(),
                        &not_written,
                    ),
                })
            }
            (Some(loaded), None) => {
                note_the_stream_touched(action, stream_id, &loaded.display_name());
                go_past_cancelling(action)?;
                enter_the_step(action, StreamActionStep::Stopping);
                let _one_stream_action_at_a_time =
                    self.stream_actions.one_stream_action_at_a_time.lock();
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(stopped_unrecorded())
            }
        }
    }

    /// The start of the recorded stream `stream_id`, run on the calling
    /// thread: make its project's `.venv` unless its interpreter runs, then
    /// load and start it from its record, the owner's rulings applied, and
    /// record it neither stopped nor failed, its crash count reset. A `.venv`
    /// that cannot be made records it failed for why. A record holding no
    /// graph — its compile failed — is compiled from its location, and the
    /// record of a project that gave no stream is a load of the project.
    /// Refused for a stream already loaded, an id no record holds, and a
    /// record an earlier runtime wrote. A failed stream whose retry is refused
    /// stays failed for the retry's reason, its crash count reset — or as it
    /// was, when the refusal was the engine's own. Its pending mark of
    /// `action` is cleared however it ends.
    pub(crate) fn start_the_stream_now(
        &self,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamStartOutcome> {
        let started = self.start_the_stream_leaving_its_mark(stream_id, action);
        self.clear_the_pending_mark_on_the_record(stream_id, action);
        started
    }

    fn start_the_stream_leaving_its_mark(
        &self,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamStartOutcome> {
        if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(stream_id) {
            return Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already loaded in this runtime, {}; only a stopped \
                 or failed stream starts",
                loaded.holding()
            )));
        }
        let record = self
            .current_stream_record_of_the_id(stream_id)
            .map_err(|unreadable| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` was not started: {unreadable}"
                ))
            })?;
        let Some(record) = record else {
            return Err(Error::NotFound(format!(
                "no stream with the id `{stream_id}` is recorded in this runtime, so there is \
                 none to start. Recorded: {}",
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        };
        note_the_stream_touched(action, stream_id, &record.display_name);
        let Some(graph_json) = record.graph.as_ref() else {
            let compiled_again =
                self.compile_and_run_a_recorded_stream_from_its_location(&record, action)?;
            return match compiled_again.state {
                StreamStateAfterItsRun::Running { node_count, .. } => Ok(StreamStartOutcome {
                    stream_id: compiled_again.stream_id,
                    node_count,
                }),
                StreamStateAfterItsRun::Failed { failed_because } => Err(Error::Runtime(format!(
                    "the stream `{}` was compiled from {}, and is failed: {failed_because}",
                    compiled_again.stream_id, record.location
                ))),
            };
        };
        enter_the_step(action, StreamActionStep::MakingTheVenv);
        if let Err(venv_not_made) = self.make_the_venv_of_a_recorded_stream(&record) {
            if !is_the_machines_shutdown_requested() {
                self.record_a_refused_start_failed_resetting_its_crash_count(
                    stream_id,
                    format!(
                        "`start` found its project's .venv not running, and it was not made: \
                         {venv_not_made}"
                    ),
                );
            }
            return Err(venv_not_made);
        }
        refuse_once_this_threads_stream_work_is_interrupted(&format!(
            "the stream `{stream_id}` was not started"
        ))?;
        enter_the_step(action, StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream = match self.load_a_recorded_stream_from_its_record(&record, graph_json) {
            Ok(stream) => stream,
            Err(retry_refusal)
                if record.is_failed()
                    && !matches!(
                        retry_refusal,
                        Error::EngineResourceRefusedAtAStreamsStart(_)
                    ) =>
            {
                self.record_a_refused_start_failed_resetting_its_crash_count(
                    stream_id,
                    format!("`start` retried it, and it did not load: {retry_refusal}"),
                );
                return Err(retry_refusal);
            }
            Err(start_refusal) => return Err(start_refusal),
        };
        let marked_running = self
            .stream_records_or_refuse_naming(&record.location.project_directory)
            .and_then(|stream_records| {
                stream_records.update(stream_id, |record| {
                    record.mark_running_resetting_its_crash_count()
                })
            });
        if let Err(write_refusal) = marked_running {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(write_refusal);
        }
        Ok(StreamStartOutcome {
            node_count: stream.node_count(),
            stream_id: stream_id.to_string(),
        })
    }

    /// The remove of the project in `project_directory`, run on the calling
    /// thread: forget every record of it — one an earlier runtime wrote
    /// included — then unload every stream of it side by side. A project with
    /// nothing loaded or recorded is no error.
    pub(crate) fn remove_the_project_now(
        &self,
        project_directory: &Path,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<ProjectRemoveOutcome> {
        let loaded_streams_of_the_project: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .every_loaded_stream()
            .into_iter()
            .filter(|stream| {
                the_same_directory(
                    &the_project_directory_of_a_loaded_stream(stream),
                    project_directory,
                )
            })
            .collect();
        let records_of_the_project = self.stream_ids_recorded_from_the_project(project_directory);
        for stream in &loaded_streams_of_the_project {
            note_the_stream_touched(action, stream.stream_id(), &stream.display_name());
        }
        go_past_cancelling(action)?;
        enter_the_step(action, StreamActionStep::Stopping);
        let mut records_forgotten = Vec::new();
        let mut records_not_forgotten = Vec::new();
        if let Some(stream_records) = self.stream_records() {
            for stream_id in records_of_the_project {
                match stream_records.remove(&stream_id) {
                    Ok(true) => records_forgotten.push(stream_id),
                    Ok(false) => {}
                    Err(not_removed) => {
                        tracing::error!("{not_removed}");
                        records_not_forgotten.push(not_removed.to_string());
                    }
                }
            }
        }
        self.stream_actions
            .last_compile_of_each_project
            .lock()
            .remove(project_directory);
        {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            unload_every_stream_an_action_took_back(
                &loaded_streams_of_the_project,
                "its owner removed its project",
            );
        }
        if !records_not_forgotten.is_empty() {
            return Err(Error::Runtime(format!(
                "the project in {} is unloaded, and a restart of the runtime loads what is still \
                 recorded of it: {}",
                project_directory.display(),
                records_not_forgotten.join("; ")
            )));
        }
        Ok(ProjectRemoveOutcome {
            project_directory: project_directory.to_path_buf(),
            streams_unloaded: loaded_streams_of_the_project
                .iter()
                .map(|stream| stream.stream_id().to_string())
                .collect(),
            records_forgotten,
        })
    }

    /// Show the stream whose id is `stream_id` under `display_name`, leaving
    /// its id, address and broadcast as they are, never waiting behind a
    /// stream action. A recorded stream's record is rewritten first, so a
    /// rename whose record cannot be written is refused with the stream shown
    /// as it was. Refused for a display name with nothing to show, a record
    /// an earlier runtime wrote, and an id the runtime holds no stream of.
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
        let renamed_record = match self.stream_records() {
            Some(stream_records) => stream_records.update(stream_id, |record| {
                record.display_name = display_name.to_string();
                true
            })?,
            None => None,
        };
        if renamed_record.is_none() && loaded.is_none() {
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
    /// record not loaded — one an earlier runtime wrote listed failed — each
    /// with the stream action working on it; and a project whose load is
    /// queued or under way and that lists no stream yet, as one `preparing`
    /// row under the project's id. A record that cannot be read is logged and
    /// left out.
    pub fn list_streams(&self) -> Vec<StreamListing> {
        let mut listings: Vec<StreamListing> = self
            .every_loaded_stream()
            .iter()
            .map(|stream| StreamListing {
                stream_id: stream.stream_id().to_string(),
                display_name: stream.display_name(),
                stream_location: stream.stream_location().cloned(),
                state: StreamListingState::Running,
                node_count: Some(stream.node_count()),
                failed_because: None,
                action_id: None,
                step: None,
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
                        action_id: None,
                        step: None,
                    },
                    Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => StreamListing {
                        state: StreamListingState::Failed,
                        failed_because: Some(earlier.failed_because()),
                        display_name: earlier.stream_id.clone(),
                        stream_id: earlier.stream_id,
                        stream_location: None,
                        node_count: None,
                        action_id: None,
                        step: None,
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
        for listing in &mut listings {
            let project_directory = listing
                .stream_location
                .as_ref()
                .map(|stream_location| stream_location.project_directory.as_path());
            if let Some((action_id, state, step)) =
                self.the_stream_action_working_on_the_stream(project_directory, &listing.stream_id)
            {
                listing.state = state;
                listing.action_id = Some(action_id);
                listing.step = step;
            }
        }
        for (project_directory, action_id, step) in self.projects_with_a_load_queued_or_running() {
            if listings.iter().any(|listing| {
                listing
                    .stream_location
                    .as_ref()
                    .is_some_and(|stream_location| {
                        stream_location.project_directory == project_directory
                    })
            }) {
                continue;
            }
            let stream_location =
                StreamLocation::of_a_project_that_gave_no_stream(project_directory);
            listings.push(StreamListing {
                stream_id: stream_location.stream_id(),
                display_name: stream_location.project_folder_name(),
                stream_location: Some(stream_location),
                state: StreamListingState::Preparing,
                node_count: None,
                failed_because: None,
                action_id: Some(action_id),
                step,
            });
        }
        listings.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        listings
    }

    /// The streams of the project in `project_directory` as its last compile
    /// in this runtime gave them, each with the state the runtime lists it
    /// in; no project code runs. With no compile yet, `preparing` while a
    /// load of it is queued or under way, and not loaded otherwise. Refused
    /// only for a relative directory.
    pub fn list_project_streams(
        &self,
        project_directory: &Path,
    ) -> Result<ProjectStreamsAsLastCompiled> {
        refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
        let project_directory = the_resolved_project_directory(project_directory);
        let last_compile = self
            .stream_actions
            .last_compile_of_each_project
            .lock()
            .get(&project_directory)
            .cloned();
        Ok(match last_compile {
            Some(WhatTheProjectsLastCompileGave::EveryStream(streams)) => {
                let listings = self.list_streams();
                ProjectStreamsAsLastCompiled::Compiled {
                    streams: streams
                        .into_iter()
                        .map(|(stream_function, description, stream_id)| {
                            StreamOfTheProjectAsLastCompiled {
                                state: listings
                                    .iter()
                                    .find(|listing| listing.stream_id == stream_id)
                                    .map(|listing| listing.state),
                                stream_function,
                                description,
                                stream_id,
                            }
                        })
                        .collect(),
                }
            }
            Some(WhatTheProjectsLastCompileGave::NoStream(failed_because)) => {
                ProjectStreamsAsLastCompiled::GaveNoStream { failed_because }
            }
            None => match self
                .projects_with_a_load_queued_or_running()
                .into_iter()
                .find(|(queued_project, _, _)| *queued_project == project_directory)
            {
                Some((_, action_id, _)) => ProjectStreamsAsLastCompiled::Preparing { action_id },
                None => ProjectStreamsAsLastCompiled::NotLoaded,
            },
        })
    }

    /// Put output port `port` of node `node` in the stream whose id is
    /// `stream_id` at `level`, never waiting behind a stream action: live on
    /// a loaded stream, where the runtime's publisher withdraws the port's
    /// track from each relay the level no longer allows, and recorded as the
    /// owner's ruling on the stream's record. On a loaded recorded stream a
    /// restriction is recorded before it changes live, and refused with the
    /// live level unchanged when its record cannot be written, so a restart
    /// never re-opens a port the owner closed; a promotion changes live first,
    /// and the outcome says why it was not recorded when it was not. A ruling
    /// on a stream not loaded is recorded once the recorded graph holds the
    /// node, and applies at its load. The level of a stream its host process
    /// loaded is never recorded.
    pub fn expose_port(
        &self,
        stream_id: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        let ruling = OwnerExposureRuling {
            node: node.to_string(),
            port: port.to_string(),
            level,
        };
        let set_live = |loaded: &LoadedStreamInThisRuntime| {
            loaded
                .log_route()
                .run_entered(|| loaded.set_output_port_exposure_level(node, port, level))
        };
        let (recorded, ruling_not_recorded_because) = match self
            .loaded_stream_of_the_id_when_loaded(stream_id)
        {
            Some(loaded) => {
                let live_level = loaded.output_port_exposure_level(node, port)?;
                match loaded.holding() {
                    LoadedStreamHolding::LivesWithTheHostProcess => {
                        set_live(&loaded)?;
                        (false, None)
                    }
                    LoadedStreamHolding::RecordedByTheRuntime
                        if level.is_narrower_than(live_level) =>
                    {
                        self.record_the_owners_ruling_on_a_stream_record(stream_id, ruling)
                            .map_err(|not_recorded| {
                                Error::Runtime(format!(
                                    "the port `{node}/{port}` of the stream `{stream_id}` was \
                                     not restricted to {level}, and is still {live_level}: the \
                                     owner's ruling could not be recorded, and a restriction \
                                     not recorded would be undone at the runtime's restart: \
                                     {not_recorded}"
                                ))
                            })?;
                        set_live(&loaded)?;
                        (true, None)
                    }
                    LoadedStreamHolding::RecordedByTheRuntime => {
                        set_live(&loaded)?;
                        match self.record_the_owners_ruling_on_a_stream_record(stream_id, ruling) {
                            Ok(()) => (true, None),
                            Err(not_recorded) => {
                                tracing::warn!(
                                    "the port `{node}/{port}` of the stream `{stream_id}` is \
                                     {level} now, live, and the owner's ruling was not \
                                     recorded: {not_recorded}"
                                );
                                (false, Some(not_recorded.to_string()))
                            }
                        }
                    }
                }
            }
            None => {
                let Some(record) = self.current_stream_record_of_the_id(stream_id)? else {
                    return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
                };
                refuse_a_node_the_recorded_graph_does_not_hold(&record, node)?;
                self.record_the_owners_ruling_on_a_stream_record(stream_id, ruling)?;
                (true, None)
            }
        };
        Ok(OutputPortExposureOutcome {
            stream_id: stream_id.to_string(),
            node: node.to_string(),
            port: port.to_string(),
            level,
            recorded,
            ruling_not_recorded_because,
        })
    }

    /// The re-load at the runtime's start: finish every pending mark a
    /// previous run left, in order — a pending stop recorded stopped, a
    /// pending start cleared of stopped and failed, a pending remove
    /// forgotten — then queue one re-load per project holding a record
    /// neither stopped nor failed, which loads only those records. A record
    /// an earlier runtime wrote is skipped as failed; a record that cannot be
    /// read is logged and left in place.
    pub fn reload_every_recorded_stream_neither_stopped_nor_failed(
        &self,
    ) -> Vec<StreamReloadAtTheStart> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        finish_the_pending_marks_a_previous_run_left(stream_records);
        let mut reloads = Vec::new();
        let mut records_to_reload_of_each_project: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        for read in stream_records.read_every() {
            match read {
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                }
                Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => {
                    let failed_because = earlier.failed_because();
                    tracing::warn!(
                        "the stream `{}` is failed, so it is not re-loaded: {failed_because}",
                        earlier.stream_id
                    );
                    reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                        stream_id: earlier.stream_id,
                        failed_because,
                    });
                }
                Ok(StreamRecordAsRead::Current(record)) => {
                    if let Some(failed_because) = record.failed_because {
                        tracing::warn!(
                            "the stream `{}` is failed, so it is not re-loaded: \
                             {failed_because}; `tatolab start` retries it",
                            record.stream_id
                        );
                        reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                            stream_id: record.stream_id,
                            failed_because,
                        });
                    } else if record.stopped {
                        tracing::info!(
                            "the stream `{}` is stopped, so it is not re-loaded",
                            record.stream_id
                        );
                    } else {
                        records_to_reload_of_each_project
                            .entry(record.location.project_directory)
                            .or_default()
                            .push(record.stream_id);
                    }
                }
            }
        }
        for (project_directory, stream_ids) in records_to_reload_of_each_project {
            match self.queue_a_stream_action(
                project_directory.clone(),
                StreamActionBody::ReloadTheRecordsAtStart {
                    stream_ids: stream_ids.clone(),
                },
                |_| Ok(()),
            ) {
                Ok(queued) => reloads.push(StreamReloadAtTheStart::Queued {
                    action_id: queued.action_id,
                    project_directory,
                    stream_ids,
                }),
                Err(refusal) => {
                    tracing::error!(
                        "the re-load of the streams of {} was not queued: {refusal}",
                        project_directory.display()
                    );
                    reloads.push(StreamReloadAtTheStart::NotQueued {
                        project_directory,
                        refusal,
                    });
                }
            }
        }
        reloads
    }

    /// The re-load at the runtime's start of the records `stream_ids` of the
    /// project in `project_directory`, run on the calling thread: its `.venv`
    /// made once, outside the one-action lock, then each record read again
    /// and loaded from that read — a stream stopped, failed or removed since
    /// stays unloaded, one loaded since is left as it is. A stream that does
    /// not re-load is recorded failed for the reason, unless the refusal was
    /// the engine's own — its GPU context — or a machine shutdown abandoned
    /// it. A record holding no graph is compiled from its location.
    pub(crate) fn reload_the_records_of_a_project_now(
        &self,
        project_directory: &Path,
        stream_ids: &[String],
        action: Option<&StreamActionOfAProject>,
    ) -> Result<()> {
        let stream_records = self.stream_records_or_refuse_naming(project_directory)?;
        let mut venv_made_of_each_project: HashMap<PathBuf, std::result::Result<(), String>> =
            HashMap::new();
        for stream_id in stream_ids {
            refuse_once_this_threads_stream_work_is_interrupted(&format!(
                "the re-load of {} stopped before the stream `{stream_id}`",
                project_directory.display()
            ))?;
            let record = match stream_records.read(stream_id) {
                Ok(Some(StreamRecordAsRead::Current(record))) => record,
                Ok(Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(_))) | Ok(None) => {
                    tracing::info!(
                        "the stream `{stream_id}` was removed before its re-load, so it is not \
                         re-loaded"
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    continue;
                }
            };
            if record.is_failed() || record.stopped {
                tracing::info!(
                    "the stream `{stream_id}` is {} since the re-load was queued, so it is not \
                     re-loaded",
                    record.listing_state()
                );
                continue;
            }
            if self
                .loaded_stream_of_the_id_when_loaded(stream_id)
                .is_some()
            {
                tracing::info!(
                    "the stream `{stream_id}` was loaded before its re-load reached it, so it is \
                     left as it is"
                );
                continue;
            }
            note_the_stream_touched(action, stream_id, &record.display_name);
            let Some(graph_json) = record.graph.as_ref() else {
                match self.compile_and_run_a_recorded_stream_from_its_location(&record, action) {
                    Ok(StreamLoadedOrFailedByARun {
                        state: StreamStateAfterItsRun::Running { node_count, .. },
                        ..
                    }) => tracing::info!(
                        "the stream `{stream_id}` was compiled from {} and re-loaded with \
                         {node_count} nodes",
                        record.location
                    ),
                    Ok(StreamLoadedOrFailedByARun {
                        state: StreamStateAfterItsRun::Failed { failed_because },
                        ..
                    }) => tracing::error!(
                        "the stream `{stream_id}` was compiled from {} and is failed: \
                         {failed_because}",
                        record.location
                    ),
                    Err(refusal) => tracing::error!(
                        "the stream `{stream_id}` was compiled from {} and did not re-load: \
                         {refusal}",
                        record.location
                    ),
                }
                continue;
            };
            enter_the_step(action, StreamActionStep::MakingTheVenv);
            let venv_project = the_project_directory_whose_venv_holds(&record.interpreter)
                .unwrap_or_else(|| project_directory.to_path_buf());
            let venv_made = venv_made_of_each_project
                .entry(venv_project)
                .or_insert_with(|| {
                    self.make_the_venv_of_a_recorded_stream(&record)
                        .map_err(|venv_not_made| venv_not_made.to_string())
                })
                .clone();
            enter_the_step(action, StreamActionStep::Starting);
            let reloaded = match venv_made {
                Ok(()) => {
                    let _one_stream_action_at_a_time =
                        self.stream_actions.one_stream_action_at_a_time.lock();
                    self.load_a_recorded_stream_from_its_record(&record, graph_json)
                }
                Err(venv_not_made) => Err(Error::Runtime(venv_not_made)),
            };
            match reloaded {
                Ok(stream) => tracing::info!(
                    "the stream `{stream_id}` re-loaded from {} with {} nodes",
                    record.location,
                    stream.node_count()
                ),
                Err(reload_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)) => {
                    tracing::error!(
                        "the stream `{stream_id}` was not re-loaded, and is re-loaded at the \
                         runtime's next start: {reload_refusal}"
                    );
                }
                Err(reload_refusal) if is_this_threads_stream_work_interrupted() => {
                    tracing::info!(
                        "the stream `{stream_id}` was not re-loaded, because the runtime is \
                         shutting down or the re-load was cancelled: {reload_refusal}"
                    );
                }
                Err(reload_refusal) => {
                    let failed_because =
                        format!("it did not re-load at the runtime's start: {reload_refusal}");
                    match stream_records.update(stream_id, |record| {
                        record.failed_because = Some(failed_because);
                        true
                    }) {
                        Ok(_) => tracing::error!(
                            "the stream `{stream_id}` did not re-load, and is failed: \
                             {reload_refusal}; `tatolab start` retries it"
                        ),
                        Err(not_written) => tracing::error!(
                            "the stream `{stream_id}` did not re-load: {reload_refusal}; and it \
                             was not recorded failed: {not_written}"
                        ),
                    }
                }
            }
        }
        Ok(())
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
        let mut streams_failed = Vec::new();
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(record)) = read else {
                continue;
            };
            if record.is_failed() {
                continue;
            }
            let cause = crash.causes_by_implicated_stream.get(&record.stream_id);
            let counted = stream_records.update(&record.stream_id, |record| match cause {
                Some(cause) => {
                    record.runtime_crashes_in_a_row_implicating_it += 1;
                    let crashes_in_a_row = record.runtime_crashes_in_a_row_implicating_it;
                    if crashes_in_a_row >= RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM {
                        record.failed_because = Some(format!(
                            "it was implicated in the runtime's last {crashes_in_a_row} crashes \
                             in a row, the last: {cause}"
                        ));
                    }
                    true
                }
                None if record.runtime_crashes_in_a_row_implicating_it == 0 => false,
                None => {
                    record.runtime_crashes_in_a_row_implicating_it = 0;
                    true
                }
            });
            match counted {
                Ok(Some(counted)) => {
                    if let Some(failed_because) = &counted.failed_because {
                        tracing::error!(
                            "the stream `{}` is failed: {failed_because}; it is not re-loaded \
                             until `tatolab start` retries it",
                            counted.stream_id
                        );
                        streams_failed.push(counted.stream_id);
                    }
                }
                Ok(None) => {}
                Err(not_written) => tracing::error!(
                    "the stream `{}`'s count of the runtime's crashes was not recorded: \
                     {not_written}",
                    record.stream_id
                ),
            }
        }
        streams_failed
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn stream_records(&self) -> Option<&StreamRecordsInTheStateDirectory> {
        self.stream_actions.stream_records.get()
    }

    /// The records of this runtime, refused naming the project a load or a
    /// start of it was asked for on a runtime given no state directory.
    fn stream_records_or_refuse_naming(
        &self,
        project_directory: &Path,
    ) -> Result<&StreamRecordsInTheStateDirectory> {
        self.stream_records().ok_or_else(|| {
            Error::Configuration(format!(
                "the project in {} was not run: this runtime records no stream, because its \
                 host gave it no state directory to record them in; run it in `tatolabd`",
                project_directory.display()
            ))
        })
    }

    /// The lend directory, refused naming the project a compile of it was
    /// asked for on a runtime handed none.
    fn lend_directory_or_refuse_naming(&self, project_directory: &Path) -> Result<&Path> {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "the stream functions in {} were not compiled: this runtime was handed no \
                     lend directory, which its host gives with \
                     `set_processor_interpreter_lend_directory` before any load",
                    project_directory.display()
                ))
            })
    }

    /// Keep what the last compile of the project in `project_directory` gave.
    fn keep_the_projects_last_compile(
        &self,
        project_directory: &Path,
        what_it_gave: WhatTheProjectsLastCompileGave,
    ) {
        self.stream_actions
            .last_compile_of_each_project
            .lock()
            .insert(project_directory.to_path_buf(), what_it_gave);
    }

    /// The id of every current record of the project in `project_directory`,
    /// and of every record an earlier runtime wrote naming it.
    fn stream_ids_recorded_from_the_project(&self, project_directory: &Path) -> Vec<String> {
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

    /// Record the stream `stream_id` stopped, finalising the pending mark of
    /// `action` on it.
    fn record_the_stream_stopped(
        &self,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<()> {
        let Some(stream_records) = self.stream_records() else {
            return Err(Error::Configuration(format!(
                "the stream `{stream_id}` was not recorded stopped: this runtime records no stream"
            )));
        };
        stream_records.update(stream_id, |record| {
            record.stopped = true;
            remove_the_pending_mark_of(record, action);
            true
        })?;
        Ok(())
    }

    /// Clear the pending mark of `action` on the record of `stream_id`; a
    /// record that cannot be written is logged.
    fn clear_the_pending_mark_on_the_record(
        &self,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) {
        if let Some(stream_records) = self.stream_records() {
            clear_the_pending_mark_of(stream_records, stream_id, action);
        }
    }

    /// Record `ruling` on the record of the stream `stream_id`, refused naming
    /// the record when it is gone.
    fn record_the_owners_ruling_on_a_stream_record(
        &self,
        stream_id: &str,
        ruling: OwnerExposureRuling,
    ) -> Result<()> {
        let recorded = match self.stream_records() {
            Some(stream_records) => stream_records.update(stream_id, |record| {
                record.record_the_owners_exposure_ruling(ruling);
                true
            })?,
            None => None,
        };
        if recorded.is_none() {
            let record_path = self
                .stream_records()
                .and_then(|stream_records| stream_records.record_path_of(stream_id))
                .map(|record_path| record_path.display().to_string())
                .unwrap_or_else(|| "in this runtime's state directory".to_string());
            return Err(Error::Runtime(format!(
                "the stream `{stream_id}` is loaded and its record {record_path} is gone, so the \
                 runtime's restart does not load it; `run` records it again"
            )));
        }
        Ok(())
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

    /// Record the stream a load compiled — failed with no graph when its
    /// function raised — keeping what the owner set on the record it
    /// replaces, and load and start its graph. A replace is recorded first,
    /// then the running stream unloaded; a new stream is recorded once loaded,
    /// before its start, so a load a machine shutdown abandons leaves nothing
    /// recorded. A load or start that is refused records the stream failed with
    /// the reason. A record that cannot be written leaves the running stream as
    /// it was.
    fn record_and_load_one_stream_a_run_compiled(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        compiled: StreamARunCompiled<'_>,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let StreamARunCompiled {
            stream_location,
            display_name_of_a_new_stream,
            stream_environment,
            graph_or_why_none,
        } = compiled;
        let stream_id = stream_location.stream_id();
        let (previous_record, previously_loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?;
        let display_name = previous_record
            .as_ref()
            .map(|previous| previous.display_name.clone())
            .or(display_name_of_a_new_stream)
            .unwrap_or_else(|| match &graph_or_why_none {
                Ok(graph_json) => graph_json
                    .get("stream")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&stream_id)
                    .to_string(),
                Err(_) => stream_location.stream_function_name(),
            });
        note_the_stream_touched(action, &stream_id, &display_name);
        let (mut stream_record, failed_because_its_code_gave_no_graph) = match graph_or_why_none {
            Ok(graph_json) => (
                StreamRecord::of_a_running_stream(
                    stream_location,
                    display_name,
                    stream_environment,
                    graph_json,
                ),
                None,
            ),
            Err(failed_because) => (
                StreamRecord::of_a_stream_whose_compile_failed(
                    stream_location,
                    display_name,
                    stream_environment,
                    failed_because.clone(),
                ),
                Some(failed_because),
            ),
        };
        let replaced_a_recorded_stream = previous_record.is_some();
        let a_new_stream_recorded_once_loaded =
            !replaced_a_recorded_stream && failed_because_its_code_gave_no_graph.is_none();
        if !a_new_stream_recorded_once_loaded {
            stream_records
                .write_keeping_what_the_owner_set_on_the_record_it_replaces(&mut stream_record)?;
            if let Some(previously_loaded) = previously_loaded {
                unload_a_stream_an_action_took_back(&previously_loaded, "its record is replaced");
            }
        }
        let state = match failed_because_its_code_gave_no_graph {
            Some(failed_because) => StreamStateAfterItsRun::Failed { failed_because },
            None => self.load_and_start_a_recorded_stream_recording_it_failed_when_refused(
                stream_records,
                &mut stream_record,
                a_new_stream_recorded_once_loaded,
            )?,
        };
        Ok(StreamLoadedOrFailedByARun::of_the_record(
            stream_record,
            state,
            replaced_a_recorded_stream,
        ))
    }

    /// Load the graph `stream_record` holds — writing the record once it is
    /// loaded when `record_it_once_loaded` — then start it. A refused load or
    /// start records the stream failed with the reason; a record that cannot
    /// be written unloads the stream and is refused.
    fn load_and_start_a_recorded_stream_recording_it_failed_when_refused(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_record: &mut StreamRecord,
        record_it_once_loaded: bool,
    ) -> Result<StreamStateAfterItsRun> {
        let loaded = match &stream_record.graph {
            Some(graph_json) => self.load_a_stream_with_the_owners_rulings(
                OneStreamAStreamActionLoads::recorded_by(stream_record, graph_json),
            ),
            None => Err(Error::Runtime(format!(
                "the stream `{}` holds no graph to load",
                stream_record.stream_id
            ))),
        };
        let stream = match loaded {
            Ok(stream) => stream,
            Err(load_refusal) => {
                return record_a_refused_load_failed(
                    stream_records,
                    stream_record,
                    load_refusal,
                    record_it_once_loaded,
                );
            }
        };
        if record_it_once_loaded
            && let Err(not_written) = stream_records
                .write_keeping_what_the_owner_set_on_the_record_it_replaces(stream_record)
        {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(not_written);
        }
        if let Err(start_refusal) = stream.start() {
            unload_a_stream_an_action_took_back(&stream, "its start was refused");
            return record_a_refused_load_failed(
                stream_records,
                stream_record,
                start_refusal,
                false,
            );
        }
        Ok(StreamStateAfterItsRun::Running {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        })
    }

    /// The record of `stream_location` a load of it replaces, and its stream
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

    /// Refuse a load of `stream_location` whose id another location holds, or
    /// a stream loaded without a record holds.
    fn refuse_a_run_of_a_location_whose_id_another_holds(
        &self,
        stream_location: &StreamLocation,
    ) -> Result<()> {
        self.the_recorded_stream_a_run_of_its_location_replaces(
            &stream_location.stream_id(),
            stream_location,
        )
        .map(|_| ())
    }

    /// Record failed the stream of `stream_location`, whose named function the
    /// project's entry no longer defines, unloading it; refused with
    /// `not_defined_because` when no record holds it.
    fn fail_the_record_of_a_stream_function_no_longer_defined(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_location: StreamLocation,
        not_defined_because: String,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let stream_id = stream_location.stream_id();
        let (Some(_), loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?
        else {
            return Err(Error::NotFound(not_defined_because));
        };
        let Some(record) = stream_records.update(&stream_id, |record| {
            record.failed_because = Some(not_defined_because.clone());
            true
        })?
        else {
            return Err(Error::NotFound(not_defined_because));
        };
        if let Some(loaded) = loaded {
            unload_a_stream_an_action_took_back(&loaded, "its function is no longer defined");
        }
        Ok(StreamLoadedOrFailedByARun::of_the_record(
            record,
            StreamStateAfterItsRun::Failed {
                failed_because: not_defined_because,
            },
            true,
        ))
    }

    /// Record failed every record of the project in `project_directory` whose
    /// function its `stream.py` no longer defines, naming the function, and
    /// unload each one loaded; never forgotten.
    fn fail_every_record_of_the_project_its_stream_py_no_longer_defines(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project_directory: &Path,
        ids_the_project_defines: &[String],
        action: Option<&StreamActionOfAProject>,
    ) {
        let mut streams_to_unload = Vec::new();
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(record)) = read else {
                continue;
            };
            if !the_same_directory(&record.location.project_directory, project_directory)
                || record.location.is_a_project_that_gave_no_stream()
                || ids_the_project_defines.contains(&record.stream_id)
            {
                continue;
            }
            note_the_stream_touched(action, &record.stream_id, &record.display_name);
            let failed_because = format!(
                "its function `{}` is no longer defined in stream.py",
                record.location.stream_function
            );
            match stream_records.update(&record.stream_id, |record| {
                record.failed_because = Some(failed_because.clone());
                true
            }) {
                Ok(_) => tracing::warn!(
                    "the stream `{}` is failed: {failed_because}",
                    record.stream_id
                ),
                Err(not_written) => tracing::error!(
                    "the stream `{}` is no longer defined, and was not recorded failed: \
                     {not_written}",
                    record.stream_id
                ),
            }
            streams_to_unload.extend(self.loaded_stream_of_the_id_when_loaded(&record.stream_id));
        }
        unload_every_stream_an_action_took_back(
            &streams_to_unload,
            "its function is no longer defined in stream.py",
        );
    }

    /// Record the project that gave no stream — its `.venv` was not made, or
    /// its entry did not import or defines none — as the project's last
    /// compile, and every record of it failed for `failed_because`.
    fn record_a_project_that_gave_no_stream(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_environment: &StreamEnvironment,
        display_name_of_a_new_stream: Option<String>,
        failed_because: String,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<ProjectLoadOutcome> {
        self.keep_the_projects_last_compile(
            &stream_environment.project_directory,
            WhatTheProjectsLastCompileGave::NoStream(failed_because.clone()),
        );
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let streams = self.fail_every_record_of_a_project_that_gave_no_stream(
            stream_records,
            stream_environment,
            display_name_of_a_new_stream,
            failed_because.clone(),
        )?;
        for stream in &streams {
            note_the_stream_touched(action, &stream.stream_id, &stream.display_name);
        }
        Ok(ProjectLoadOutcome {
            project_directory: stream_environment.project_directory.clone(),
            streams,
            gave_no_stream_because: Some(failed_because),
        })
    }

    /// Record failed, with `failed_because`, every stream recorded from the
    /// project that gave no stream, unloading each one loaded; with none,
    /// record the project itself failed under
    /// [`StreamLocation::of_a_project_that_gave_no_stream`], shown under
    /// `display_name_of_a_new_stream` or its folder's name.
    fn fail_every_record_of_a_project_that_gave_no_stream(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_environment: &StreamEnvironment,
        display_name_of_a_new_stream: Option<String>,
        failed_because: String,
    ) -> Result<Vec<StreamLoadedOrFailedByARun>> {
        let ids_of_the_project: Vec<String> = stream_records
            .read_every()
            .into_iter()
            .filter_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.project_directory
                        == stream_environment.project_directory =>
                {
                    Some(record.stream_id)
                }
                _ => None,
            })
            .collect();
        if ids_of_the_project.is_empty() {
            let stream_location = StreamLocation::of_a_project_that_gave_no_stream(
                stream_environment.project_directory.clone(),
            );
            self.refuse_a_run_of_a_location_whose_id_another_holds(&stream_location)?;
            let display_name = display_name_of_a_new_stream
                .unwrap_or_else(|| stream_location.project_folder_name());
            let mut record = StreamRecord::of_a_stream_whose_compile_failed(
                stream_location,
                display_name,
                stream_environment,
                failed_because.clone(),
            );
            stream_records
                .write_keeping_what_the_owner_set_on_the_record_it_replaces(&mut record)?;
            return Ok(vec![StreamLoadedOrFailedByARun::of_the_record(
                record,
                StreamStateAfterItsRun::Failed { failed_because },
                false,
            )]);
        }
        let mut streams_failed = Vec::with_capacity(ids_of_the_project.len());
        let mut streams_to_unload = Vec::new();
        for stream_id in ids_of_the_project {
            let Some(record) = stream_records.update(&stream_id, |record| {
                record.failed_because = Some(failed_because.clone());
                true
            })?
            else {
                continue;
            };
            streams_to_unload.extend(self.loaded_stream_of_the_id_when_loaded(&stream_id));
            streams_failed.push(StreamLoadedOrFailedByARun::of_the_record(
                record,
                StreamStateAfterItsRun::Failed {
                    failed_because: failed_because.clone(),
                },
                true,
            ));
        }
        unload_every_stream_an_action_took_back(&streams_to_unload, "its project gave no stream");
        Ok(streams_failed)
    }

    /// Forget the record of the project in `project_directory` written while
    /// it gave no stream, now that it gives streams; one that cannot be removed
    /// is logged and stays listed failed.
    fn forget_the_record_of_a_project_that_gives_streams_again(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project_directory: &Path,
    ) {
        let stream_location =
            StreamLocation::of_a_project_that_gave_no_stream(project_directory.to_path_buf());
        let stream_id = stream_location.stream_id();
        let Ok(Some(StreamRecordAsRead::Current(record))) = stream_records.read(&stream_id) else {
            return;
        };
        if record.location != stream_location {
            return;
        }
        if let Err(not_removed) = stream_records.remove(&stream_id) {
            tracing::error!(
                "the project in {} gives streams again, and the record `{stream_id}` written \
                 while it gave none was not removed, so it stays listed failed: {not_removed}",
                project_directory.display()
            );
        }
    }

    /// Make the `.venv` of the project in `project_directory` with the uv this
    /// runtime was handed, unless its interpreter runs.
    fn make_the_projects_venv(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        make_the_projects_venv_unless_its_interpreter_runs(
            project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_unit_uv_executable
                .get(),
        )
    }

    /// Make the `.venv` that `record`'s interpreter is in, unless it runs,
    /// refused for why when it cannot be made. An interpreter in no project's
    /// `.venv` is left as it is.
    fn make_the_venv_of_a_recorded_stream(&self, record: &StreamRecord) -> Result<()> {
        let Some(project_directory) = the_project_directory_whose_venv_holds(&record.interpreter)
        else {
            return Ok(());
        };
        Ok(self.make_the_projects_venv(&project_directory)?)
    }

    /// Record the stream `stream_id` failed for `failed_because`, a start of
    /// it refused, its crash count reset; a record that cannot be written is
    /// logged.
    fn record_a_refused_start_failed_resetting_its_crash_count(
        &self,
        stream_id: &str,
        failed_because: String,
    ) {
        let Some(stream_records) = self.stream_records() else {
            return;
        };
        if let Err(not_written) = stream_records.update(stream_id, |record| {
            record.failed_because = Some(failed_because);
            record.runtime_crashes_in_a_row_implicating_it = 0;
            true
        }) {
            tracing::warn!(
                "the stream `{stream_id}` was not started, and its failure was not recorded: \
                 {not_written}"
            );
        }
    }

    /// Compile `record`'s stream again from its location and load it — the
    /// project's load, for the record of a project that gave no stream. That
    /// record names no entry, so one written for an entry other than the
    /// project's own `stream.py` is refused, naming it.
    fn compile_and_run_a_recorded_stream_from_its_location(
        &self,
        record: &StreamRecord,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let project_directory = the_project_directory_whose_venv_holds(&record.interpreter)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "the stream `{}` was not compiled: its interpreter {} is not a project's \
                     `.venv/bin/python`",
                    record.stream_id,
                    record.interpreter.display()
                ))
            })?;
        if record.location.is_a_project_that_gave_no_stream() {
            if !the_same_directory(&project_directory, &record.location.project_directory) {
                return Err(Error::Configuration(format!(
                    "the stream `{}` records an entry in {} that did not import, and names no \
                     file to compile again; run that project again with `tatolab run` in {}",
                    record.stream_id,
                    record.location.project_directory.display(),
                    project_directory.display()
                )));
            }
            let loaded = self.load_the_project_now(&project_directory, action, false)?;
            if let Some(gave_no_stream_because) = loaded.gave_no_stream_because {
                return Err(Error::Runtime(format!(
                    "the project in {} gave no stream: {gave_no_stream_because}",
                    project_directory.display()
                )));
            }
            return loaded.streams.into_iter().next().ok_or_else(|| {
                Error::Runtime(format!(
                    "the project in {} was loaded, and took in no stream",
                    project_directory.display()
                ))
            });
        }
        self.load_one_stream_function_of_the_project_now(
            &project_directory,
            &the_stream_target_that_compiles_a_location_again(&record.location),
            &record.stream_id,
            action,
        )
    }

    /// Make the project's `.venv`, compile the one stream function `target`
    /// of it, then record and load it as the stream `stream_id` — the start or
    /// the re-load of a record whose compile failed.
    fn load_one_stream_function_of_the_project_now(
        &self,
        project_directory: &Path,
        target: &str,
        stream_id: &str,
        action: Option<&StreamActionOfAProject>,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let stream_records = self.stream_records_or_refuse_naming(project_directory)?;
        let lend_directory = self.lend_directory_or_refuse_naming(project_directory)?;
        enter_the_step(action, StreamActionStep::MakingTheVenv);
        self.make_the_projects_venv(project_directory)?;
        enter_the_step(action, StreamActionStep::Compiling);
        let StreamFunctionCompileInTheProjectsInterpreterReport {
            what_the_projects_code_gave,
            stream_environment,
        } = compile_the_stream_function_in_the_projects_interpreter(
            project_directory,
            Some(target),
            lend_directory,
        )?;
        enter_the_step(action, StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let (stream_location, graph_or_why_none) = match what_the_projects_code_gave {
            WhatTheProjectsCodeGaveTheCompile::Compiled {
                stream_location,
                graph_json,
            } => (stream_location, Ok(graph_json)),
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised {
                stream_location,
                failed_because,
            } => (stream_location, Err(failed_because)),
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined {
                stream_location,
                not_defined_because,
            } => {
                return self.fail_the_record_of_a_stream_function_no_longer_defined(
                    stream_records,
                    stream_location,
                    not_defined_because,
                );
            }
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                import_failed_because: failed_because,
            }
            | WhatTheProjectsCodeGaveTheCompile::StreamEntryDefinesNoStream {
                no_stream_defined_because: failed_because,
            } => {
                return self
                    .fail_every_record_of_a_project_that_gave_no_stream(
                        stream_records,
                        &stream_environment,
                        None,
                        failed_because.clone(),
                    )?
                    .into_iter()
                    .find(|stream| stream.stream_id == stream_id)
                    .ok_or_else(|| Error::Runtime(failed_because));
            }
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { .. } => {
                return Err(Error::Runtime(format!(
                    "the compile of `{target}` in {} answered for every stream of the project",
                    project_directory.display()
                )));
            }
        };
        self.record_and_load_one_stream_a_run_compiled(
            stream_records,
            StreamARunCompiled {
                stream_location,
                display_name_of_a_new_stream: None,
                stream_environment: &stream_environment,
                graph_or_why_none,
            },
            action,
        )
    }

    /// Load and start a recorded stream from `record` and the graph
    /// `graph_json` it holds.
    fn load_a_recorded_stream_from_its_record(
        &self,
        record: &StreamRecord,
        graph_json: &serde_json::Value,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_and_start_a_stream(OneStreamAStreamActionLoads::recorded_by(record, graph_json))
    }

    /// [`Self::load_a_stream_with_the_owners_rulings`], then start it; a
    /// refused start unloads it.
    fn load_and_start_a_stream(
        &self,
        stream_to_load: OneStreamAStreamActionLoads<'_>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let stream = self.load_a_stream_with_the_owners_rulings(stream_to_load)?;
        if let Err(start_refusal) = stream.start() {
            unload_a_stream_an_action_took_back(&stream, "its start was refused");
            return Err(start_refusal);
        }
        Ok(stream)
    }

    /// Load `stream_to_load`, the owner's rulings split around the load,
    /// without starting it: a ruling that waits for the load is skipped with a
    /// warning when its port is not there.
    fn load_a_stream_with_the_owners_rulings(
        &self,
        stream_to_load: OneStreamAStreamActionLoads<'_>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let OneStreamAStreamActionLoads {
            display_name,
            stream_location,
            graph_json,
            rulings,
            stream_environment,
        } = stream_to_load;
        let OwnerExposureRulingsSplitAroundTheLoad {
            graph_with_the_rulings_that_hold_before_the_load,
            rulings_applied_once_loaded,
        } = the_owners_exposure_rulings_split_around_the_load(graph_json, rulings);
        let graph =
            GraphSnapshot::from_graph_document(graph_with_the_rulings_that_hold_before_the_load)?;
        let stream = match self.load_stream_held_as_unless_a_machine_shutdown_is_requested(
            &graph,
            OptionsForLoadingOneStream::in_stream_environment(stream_environment)
                .with_display_name(display_name)
                .at_stream_location(stream_location.clone()),
            LoadedStreamHolding::RecordedByTheRuntime,
        )? {
            StreamLoadObservingMachineShutdownRequests::Loaded(stream) => stream,
            StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest => {
                return Err(Error::Runtime(format!(
                    "the stream `{}` was not loaded: the machine is shutting every stream down",
                    stream_location.stream_id()
                )));
            }
        };
        for ruling in rulings_applied_once_loaded {
            if let Err(not_applied) =
                stream.set_output_port_exposure_level(&ruling.node, &ruling.port, ruling.level)
            {
                stream.log_route().run_entered(|| {
                    tracing::warn!(
                        "the owner's exposure ruling on `{}/{}` is kept and not applied: \
                         {not_applied}",
                        ruling.node,
                        ruling.port
                    )
                });
            }
        }
        Ok(stream)
    }
}

/// How a load refused for an id a stream another caller loaded holds is told
/// to go on.
const STOP_IT_FIRST_TO_LOAD_THIS_ONE: &str = "stop it first to load this one";

/// Refuse a directory that is not a project: one that does not exist, or
/// holds no `pyproject.toml`.
fn refuse_a_directory_that_is_not_a_project(project_directory: &Path) -> Result<()> {
    if !project_directory.is_dir() {
        return Err(Error::Configuration(format!(
            "{} is not a directory, so there is no project there to run; `tatolab new` creates \
             one",
            project_directory.display()
        )));
    }
    if !project_directory.join("pyproject.toml").is_file() {
        return Err(Error::Configuration(format!(
            "{} holds no pyproject.toml, so it is not a project; `tatolab new` creates one",
            project_directory.display()
        )));
    }
    Ok(())
}

/// Finish the pending marks of every record in order, as a previous run of
/// the runtime left them: a stop recorded stopped, a start cleared of stopped
/// and failed, its crash count reset, and a remove forgotten.
fn finish_the_pending_marks_a_previous_run_left(stream_records: &StreamRecordsInTheStateDirectory) {
    for read in stream_records.read_every() {
        let Ok(StreamRecordAsRead::Current(record)) = read else {
            continue;
        };
        if record.pending_stream_actions.is_empty() {
            continue;
        }
        let mut forget_it = false;
        let finished = stream_records.update(&record.stream_id, |record| {
            for mark in std::mem::take(&mut record.pending_stream_actions) {
                match mark.kind {
                    PendingStreamActionKind::Stop => record.stopped = true,
                    PendingStreamActionKind::Start => {
                        record.mark_running_resetting_its_crash_count();
                    }
                    PendingStreamActionKind::Remove => forget_it = true,
                }
            }
            true
        });
        let finished = finished.and_then(|finished| {
            if forget_it {
                stream_records.remove(&record.stream_id)?;
            }
            Ok(finished)
        });
        match finished {
            Ok(_) => tracing::info!(
                "the stream `{}`'s actions a previous run left pending are finished: {}",
                record.stream_id,
                record
                    .pending_stream_actions
                    .iter()
                    .map(|mark| format!("{} `{}`", mark.kind.as_str(), mark.action_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Err(not_finished) => tracing::error!(
                "the stream `{}`'s actions a previous run left pending were not finished: \
                 {not_finished}",
                record.stream_id
            ),
        }
    }
}

/// Remove the pending mark of `action` from `record`.
fn remove_the_pending_mark_of(record: &mut StreamRecord, action: Option<&StreamActionOfAProject>) {
    if let Some(action) = action {
        record
            .pending_stream_actions
            .retain(|mark| mark.action_id != action.action_id());
    }
}

/// Clear the pending mark of `action` on the record of `stream_id` in
/// `stream_records`; a record that cannot be written is logged.
fn clear_the_pending_mark_of(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_id: &str,
    action: Option<&StreamActionOfAProject>,
) {
    let Some(action) = action else {
        return;
    };
    if let Err(not_written) = stream_records.update(stream_id, |record| {
        let marks_before = record.pending_stream_actions.len();
        remove_the_pending_mark_of(record, Some(action));
        record.pending_stream_actions.len() != marks_before
    }) {
        tracing::warn!(
            "the pending mark of `{}` on the stream `{stream_id}` was not cleared: {not_written}",
            action.action_id()
        );
    }
}

/// The project a loaded stream belongs to: its location's directory, or the
/// directory its host loaded it from.
fn the_project_directory_of_a_loaded_stream(stream: &LoadedStreamInThisRuntime) -> PathBuf {
    stream
        .stream_location()
        .map(|stream_location| stream_location.project_directory.clone())
        .unwrap_or_else(|| stream.project_directory().to_path_buf())
}

/// Refuse a load of `stream_location` under `stream_id` when the id is held by
/// a stream of another location — loaded, or recorded — naming both.
fn refuse_an_id_another_location_holds(
    stream_id: &str,
    stream_location: &StreamLocation,
    loaded: Option<&LoadedStreamInThisRuntime>,
    record: Option<&StreamRecordAsRead>,
) -> Result<()> {
    let refuse = |held_by: String| {
        Err(Error::GraphError(format!(
            "the stream id `{stream_id}` of {stream_location} is held by {held_by}; two \
             locations cannot share one id, so move or rename this project's folder or its \
             stream function, or `rm` the project that holds it"
        )))
    };
    if let Some(loaded) = loaded {
        match loaded.stream_location() {
            Some(loaded_location) if loaded_location == stream_location => {}
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
        Some(StreamRecordAsRead::Current(record)) if record.location != *stream_location => {
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

/// The stream function a load compiled, its location, the display name a new
/// stream of it is shown under, and the graph it compiled to or why its code
/// gave none.
struct StreamARunCompiled<'run> {
    stream_location: StreamLocation,
    display_name_of_a_new_stream: Option<String>,
    stream_environment: &'run StreamEnvironment,
    graph_or_why_none: std::result::Result<serde_json::Value, String>,
}

/// The project directory whose `.venv/bin/python` is `interpreter`; `None` when
/// it is not one. A stream compiled from a file in a subdirectory records that
/// subdirectory as its location, so the directory its compile ran in — and its
/// venv — is the interpreter's alone.
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

/// Whether `first` and `second` name one directory, symlinks resolved; spelled
/// alike when either cannot be resolved.
fn the_same_directory(first: &Path, second: &Path) -> bool {
    if first == second {
        return true;
    }
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

/// The target that compiles the stream at `location` again: the module's file
/// and the function when the module is one file of the location's project
/// directory — the location an entry file compiles to; the `module:qualname`
/// otherwise.
fn the_stream_target_that_compiles_a_location_again(location: &StreamLocation) -> String {
    if let Some((module, qualname)) = location.stream_function.split_once(':')
        && !module.contains('.')
    {
        let module_file = location.project_directory.join(format!("{module}.py"));
        if module_file.is_file() {
            return format!("{}:{qualname}", module_file.display());
        }
    }
    location.stream_function.clone()
}

/// Record `stream_record` failed for `refusal`, its load or start refused, and
/// answer it failed — unless a machine shutdown abandoned it, when the refusal
/// is returned and the record left as it was. A new stream not yet recorded is
/// recorded whole; a recorded one has only its failure written.
fn record_a_refused_load_failed(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_record: &mut StreamRecord,
    refusal: Error,
    not_recorded_yet: bool,
) -> Result<StreamStateAfterItsRun> {
    if is_the_machines_shutdown_requested() {
        return Err(refusal);
    }
    let failed_because = refusal.to_string();
    stream_record.failed_because = Some(failed_because.clone());
    let recorded_failed = if not_recorded_yet {
        stream_records.write_keeping_what_the_owner_set_on_the_record_it_replaces(stream_record)
    } else {
        stream_records
            .update(&stream_record.stream_id, |record| {
                record.failed_because = Some(failed_because.clone());
                true
            })
            .map(|_| ())
    };
    if let Err(not_written) = recorded_failed {
        tracing::error!(
            "the stream `{}` did not load, and was not recorded failed: {not_written}",
            stream_record.stream_id
        );
    }
    Ok(StreamStateAfterItsRun::Failed { failed_because })
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
        StreamListingState::Preparing
        | StreamListingState::Running
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

/// Refuse `node` when the graph `record` holds has no node of that name once
/// cast, naming the nodes it does hold.
fn refuse_a_node_the_recorded_graph_does_not_hold(record: &StreamRecord, node: &str) -> Result<()> {
    let node_cast = cast_exposed_name_to_url_safe(node)?;
    let recorded_node_names: Vec<&str> = record
        .graph
        .as_ref()
        .and_then(|graph_json| graph_json.get("nodes"))
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|recorded_node| recorded_node.get("name")?.as_str())
                .collect()
        })
        .unwrap_or_default();
    if recorded_node_names.iter().any(|recorded_name| {
        cast_exposed_name_to_url_safe(recorded_name)
            .is_ok_and(|recorded_cast| recorded_cast == node_cast)
    }) {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "the stream `{}` holds no node `{node}`. It holds: {}",
        record.stream_id,
        node_names_listed_for_a_refusal(recorded_node_names)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{
        MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode,
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

    /// The graph a stream function named `function_name` compiles to: one
    /// node, `source`, with the output ports `out1` and `out2`, exposing
    /// `exposed`.
    fn the_graph_of_a_function_named(
        function_name: &str,
        exposed: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "stream": function_name,
            "nodes": [{
                "name": "source",
                "type": MockOutputOnlyProcessor::Processor::processor_class_import_path().as_str(),
                "config": {},
            }],
            "links": [],
            "exposed": exposed,
        })
    }

    /// How a stub `.venv/bin/python` opens: the runtime's probe of whether the
    /// interpreter runs, `-I -c ""`, exits 0 before the stub's compile.
    const STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE: &str =
        "#!/bin/sh\n[ \"$2\" = -c ] && exit 0\n";

    /// A project whose `.venv/bin/python` is a shell script standing in for
    /// the compile entry.
    struct ProjectWithAStubCompile {
        project_directory: PathBuf,
        _directory_removed_on_drop: Arc<tempfile::TempDir>,
    }

    impl ProjectWithAStubCompile {
        fn compiling(graph: serde_json::Value) -> Self {
            Self::compiling_warning(graph, &[])
        }

        fn compiling_warning(graph: serde_json::Value, warnings: &[&str]) -> Self {
            Self::with_no_venv().given_a_venv_compiling_warning(graph, warnings)
        }

        fn with_no_venv() -> Self {
            let temporary_directory =
                a_temporary_directory_at_owner_only_mode().expect("a project directory");
            std::fs::write(
                temporary_directory.path().join("pyproject.toml"),
                "[project]\nname = \"project\"\n",
            )
            .expect("the project's pyproject.toml");
            Self {
                project_directory: temporary_directory.path().to_path_buf(),
                _directory_removed_on_drop: Arc::new(temporary_directory),
            }
        }

        /// Two projects with no venv, at two locations whose function
        /// `function_name` computes to one stream id: project folders of one
        /// name, searched under one parent until their hashes' six hex digits
        /// collide.
        fn two_with_no_venv_whose_ids_collide(function_name: &str) -> (Self, Self) {
            let parent =
                Arc::new(a_temporary_directory_at_owner_only_mode().expect("a parent directory"));
            let mut project_directory_by_stream_id = std::collections::HashMap::new();
            let (first, second) = (0u32..)
                .find_map(|attempt| {
                    let project_directory = parent.path().join(attempt.to_string()).join("rig");
                    let stream_id = StreamLocation {
                        project_directory: project_directory.clone(),
                        stream_function: format!("stream:{function_name}"),
                    }
                    .stream_id();
                    project_directory_by_stream_id
                        .insert(stream_id, project_directory.clone())
                        .map(|colliding| (colliding, project_directory))
                })
                .expect("two locations whose ids collide");
            let project_at = |project_directory: PathBuf| {
                directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                    &project_directory,
                    0o700,
                )
                .expect("the project directory");
                std::fs::write(
                    project_directory.join("pyproject.toml"),
                    "[project]\nname = \"rig\"\n",
                )
                .expect("the project's pyproject.toml");
                Self {
                    project_directory,
                    _directory_removed_on_drop: Arc::clone(&parent),
                }
            };
            (project_at(first), project_at(second))
        }

        /// This project, given a venv whose compile writes each of `warnings`
        /// and prints `graph`.
        fn given_a_venv_compiling_warning(
            self,
            graph: serde_json::Value,
            warnings: &[&str],
        ) -> Self {
            directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                &self.path().join(".venv").join("bin"),
                0o755,
            )
            .expect("the venv's bin directory");
            self.compile_to_warning(graph, warnings);
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

        /// Where the function `function_name` of this project's `stream.py` lives.
        fn location_of(&self, function_name: &str) -> StreamLocation {
            StreamLocation {
                project_directory: self.path().to_path_buf(),
                stream_function: format!("stream:{function_name}"),
            }
        }

        /// The stream id of the function `function_name` of this project.
        fn stream_id_of(&self, function_name: &str) -> String {
            self.location_of(function_name).stream_id()
        }

        /// From now on, the compile prints `graph` and this project.
        fn compile_to(&self, graph: serde_json::Value) {
            self.compile_to_warning(graph, &[]);
        }

        /// From now on, the compile writes each of `warnings` on a line of its
        /// standard error, then prints `graph`, this project, and the function
        /// of `stream.py` the graph's `stream` names.
        fn compile_to_warning(&self, graph: serde_json::Value, warnings: &[&str]) {
            let function_name = graph
                .get("stream")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("main")
                .to_string();
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "streams": [{
                        "stream_function": format!("stream:{function_name}"),
                        "stream_graph": graph,
                    }],
                }),
                warnings,
            );
        }

        /// From now on, the compile answers that the function `function_name`
        /// of `stream.py` raised, printing `traceback`.
        fn raise_in_the_function_printing(&self, function_name: &str, traceback: &str) {
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "streams": [{
                        "stream_function": format!("stream:{function_name}"),
                        "failed_because": traceback,
                    }],
                }),
                &[],
            );
        }

        /// From now on, the compile answers that `stream.py` defines no
        /// `@stream` named `function_name`.
        fn define_no_function_named(&self, function_name: &str) {
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "stream_function": format!("stream:{function_name}"),
                    "not_defined_because": format!(
                        "`stream.py` defines no @stream function named `{function_name}`"
                    ),
                }),
                &[],
            );
        }

        /// From now on, the compile answers that `stream.py` did not import,
        /// printing `traceback`.
        fn fail_to_import_printing(&self, traceback: &str) {
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "import_failed_because": traceback,
                }),
                &[],
            );
        }

        /// From now on, the compile writes its arguments to
        /// [`Self::the_last_compiles_arguments`], each of `warnings` on a line
        /// of its standard error, then prints `document`; a compile of one
        /// target prints the one stream of a `streams` document in the
        /// target's form.
        fn print_the_compile_document_warning(
            &self,
            document: serde_json::Value,
            warnings: &[&str],
        ) {
            self.write_the_compile_stub(document, warnings, "");
        }

        /// From now on, the compile prints `graph` and this project once
        /// [`Self::release_the_held_compiles`] is called.
        fn compile_to_once_released(&self, graph: serde_json::Value) {
            let function_name = graph
                .get("stream")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("main")
                .to_string();
            self.write_the_compile_stub(
                serde_json::json!({
                    "project_directory": self.path(),
                    "streams": [{
                        "stream_function": format!("stream:{function_name}"),
                        "stream_graph": graph,
                    }],
                }),
                &[],
                "while [ ! -f release-the-held-compiles ]; do sleep 0.02; done\n",
            );
        }

        /// Let every compile [`Self::compile_to_once_released`] holds print.
        fn release_the_held_compiles(&self) {
            std::fs::write(self.path().join("release-the-held-compiles"), "")
                .expect("the release is written");
        }

        fn write_the_compile_stub(
            &self,
            document: serde_json::Value,
            warnings: &[&str],
            before_printing: &str,
        ) {
            let warnings_written: String = warnings
                .iter()
                .map(|warning| format!("echo '{warning}' >&2\n"))
                .collect();
            let document_of_one_target = match document.get("streams").and_then(|streams| {
                streams.as_array().filter(|streams| streams.len() == 1)
            }) {
                Some(the_one_stream) => {
                    let mut of_the_target = the_one_stream[0].clone();
                    of_the_target["project_directory"] = document["project_directory"].clone();
                    if let Some(of_the_target) = of_the_target.as_object_mut() {
                        of_the_target.remove("description");
                    }
                    of_the_target
                }
                None => document.clone(),
            };
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                &format!(
                    "{STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE}printf '%s\\n' \"$@\" > \
                     compile-arguments\n{warnings_written}{before_printing}case \" $* \" in\n*\" --every-stream \"*) \
                     cat <<'COMPILED'\n{document}\nCOMPILED\n;;\n*) cat <<'COMPILED'\n\
                     {document_of_one_target}\nCOMPILED\n;;\nesac\n"
                ),
            );
        }

        /// The arguments the last compile was started with, one per line.
        fn the_last_compiles_arguments(&self) -> String {
            std::fs::read_to_string(self.path().join("compile-arguments"))
                .expect("a compile ran in the project")
        }

        /// From now on, the compile exits 1 printing `traceback`.
        fn fail_to_compile_printing(&self, traceback: &str) {
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                &format!(
                    "{STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE}echo \"{traceback}\" >&2\nexit 1\n"
                ),
            );
        }

        /// Where this project is recorded while its entry does not import.
        fn location_of_the_project(&self) -> StreamLocation {
            StreamLocation::of_a_project_that_gave_no_stream(self.path().to_path_buf())
        }

        /// A load of this project, run on the calling thread.
        fn run(&self, runner: &Runner) -> Result<ProjectLoadOutcome> {
            runner.load_the_project_now(self.path(), None, false)
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

    /// A record of the function `function_name` of `project`, shown under the
    /// function's name.
    fn a_record_of(
        project: &ProjectWithAStubCompile,
        function_name: &str,
        exposed: serde_json::Value,
    ) -> StreamRecord {
        StreamRecord::of_a_running_stream(
            project.location_of(function_name),
            function_name,
            &project.stream_environment(),
            the_graph_of_a_function_named(function_name, exposed),
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
    /// a loaded stream whose record is gone.
    fn a_stream_loaded_from_an_unwritten_record(
        runner: &Runner,
        record: &StreamRecord,
    ) -> Arc<LoadedStreamInThisRuntime> {
        runner
            .load_a_stream_with_the_owners_rulings(OneStreamAStreamActionLoads::recorded_by(
                record,
                record.graph.as_ref().expect("the record holds a graph"),
            ))
            .expect("the recorded stream loads")
    }

    /// The function `function_name` of `project` loaded by a library host
    /// under the id its location computes to, not started.
    fn a_stream_its_host_loaded(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        function_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        let graph = GraphSnapshot::from_graph_document(the_graph_of_a_function_named(
            function_name,
            serde_json::json!([]),
        ))
        .expect("the graph reads");
        runner
            .load_stream_from_graph_snapshot(
                &graph,
                OptionsForLoadingOneStream::in_project_directory(project.path())
                    .with_stream_id(project.stream_id_of(function_name)),
            )
            .expect("the host loads its stream")
    }

    fn the_exposures_graph_renders_for(stream: &LoadedStreamInThisRuntime) -> serde_json::Value {
        stream.to_json().expect("the graph renders")["exposed"].clone()
    }

    impl Runner {
        /// The re-load at the start, waited on until every re-load it queued
        /// has finished.
        fn reload_at_the_start_and_wait(&self) -> Vec<StreamReloadAtTheStart> {
            let reloads = self.reload_every_recorded_stream_neither_stopped_nor_failed();
            self.wait_until_every_queued_stream_action_has_finished();
            reloads
        }

        fn stop_the_stream_now_for_a_test(&self, stream_id: &str) -> Result<StreamStopOutcome> {
            self.stop_the_stream_now(stream_id, None)
        }

        fn start_the_stream_now_for_a_test(&self, stream_id: &str) -> Result<StreamStartOutcome> {
            self.start_the_stream_now(stream_id, None)
        }
    }

    /// Why a run of `function_name` in `project` loads no stream on `runner`.
    fn the_refusal_of_a_run_of(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        function_name: &str,
    ) -> String {
        refusal_of(
            runner
                .the_recorded_stream_a_run_of_its_location_replaces(
                    &project.stream_id_of(function_name),
                    &project.location_of(function_name),
                )
                .map(|_| ()),
        )
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
        run: &ProjectLoadOutcome,
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
    fn the_one_failed_stream_of(run: &ProjectLoadOutcome) -> (&StreamLoadedOrFailedByARun, &str) {
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = refusal_of(project.run(&runner));

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
            ProjectWithAStubCompile::two_with_no_venv_whose_ids_collide("camera");
        let project = project.given_a_venv_compiling_warning(
            the_graph_of_a_function_named("camera", serde_json::json!([])),
            &[],
        );
        let camera = project.stream_id_of("camera");
        let other_location = other_project.location_of("camera");
        assert_eq!(other_location.stream_id(), camera);
        let holder = a_stream_loaded_from_an_unwritten_record(
            &runner,
            &a_record_of(&other_project, "camera", serde_json::json!([])),
        );

        let loaded = project.run(&runner).expect("the load runs");
        let refusal = the_refusal_of_a_run_of(&runner, &project, "camera");

        assert!(loaded.streams.is_empty(), "{loaded:?}");
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let host_project = ProjectWithAStubCompile::with_no_venv();
        let graph = GraphSnapshot::from_graph_document(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ))
        .unwrap();
        let host_loaded = runner
            .load_stream_from_graph_snapshot(
                &graph,
                OptionsForLoadingOneStream::in_project_directory(host_project.path())
                    .with_stream_id(camera.clone()),
            )
            .expect("the host loads its stream under the id it names");

        let loaded = project.run(&runner).expect("the load runs");
        let refusal = the_refusal_of_a_run_of(&runner, &project, "camera");

        assert!(loaded.streams.is_empty(), "{loaded:?}");

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
    fn a_load_whose_id_a_record_of_another_location_holds_skips_it_naming_both_whatever_its_state()
    {
        let (project, other_project) =
            ProjectWithAStubCompile::two_with_no_venv_whose_ids_collide("camera");
        let project = project.given_a_venv_compiling_warning(
            the_graph_of_a_function_named("camera", serde_json::json!([])),
            &[],
        );
        let camera = project.stream_id_of("camera");
        let other_location = other_project.location_of("camera");
        assert_eq!(other_location.stream_id(), camera);

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
            let mut holder = a_record_of(&other_project, "camera", serde_json::json!([]));
            mark(&mut holder);
            records_in(state_directory.path()).write(&holder).unwrap();

            let loaded = project.run(&runner).expect("the load runs");
            assert!(loaded.streams.is_empty(), "{state}: {loaded:?}");
            let refusal = refusal_of(
                runner
                    .the_recorded_stream_a_run_of_its_location_replaces(
                        &camera,
                        &project.location_of("camera"),
                    )
                    .map(|_| ()),
            );

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
                let refusal = refusal_of(runner.stop_the_stream_now_for_a_test("nothing-here"));
                assert!(refusal.contains(&format!("{camera} (failed)")), "{refusal}");
            }
        }
    }

    #[test]
    #[serial]
    fn a_run_whose_compile_is_refused_leaves_the_running_stream_loaded_and_its_record_untouched() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let record = a_record_of(&project, "camera", serde_json::json!([]));
        let running =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);
        project.fail_to_compile_printing("NameError: name cmaera is not defined");

        let refusal = refusal_of(project.run(&runner));

        assert!(refusal.contains("cmaera"), "{refusal}");
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

    /// The graph of a function named `function_name` whose `exposed` names a
    /// port its one node lacks, which its load refuses.
    fn the_graph_of_a_function_exposing_a_port_its_node_lacks(
        function_name: &str,
    ) -> serde_json::Value {
        the_graph_of_a_function_named(
            function_name,
            serde_json::json!([{"node": "source", "port": "absent-port", "level": "private"}]),
        )
    }

    #[test]
    #[serial]
    fn a_replace_whose_load_is_refused_unloads_the_running_stream_and_records_it_failed_never_restoring_it()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("camera"),
        );
        let camera = project.stream_id_of("camera");
        let mut previous_record = a_record_of(&project, "camera", serde_json::json!([]));
        previous_record.display_name = "Front camera".to_string();
        previous_record.record_the_owners_exposure_ruling(OwnerExposureRuling {
            node: "source".to_string(),
            port: "out1".to_string(),
            level: OutputPortExposureLevel::Internal,
        });
        let previous = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &previous_record,
        );

        let run = project.run(&runner)
            .expect("a refused load is a failed stream, not a refused run");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(failed_because.contains("absent-port"), "{failed_because}");
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
        let recorded = the_record_of(state_directory.path(), &camera);
        assert_eq!(
            recorded.graph,
            Some(the_graph_of_a_function_exposing_a_port_its_node_lacks(
                "camera"
            )),
            "the record holds what the code compiles to now"
        );
        assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
        assert_eq!(recorded.exposure_rulings, previous_record.exposure_rulings);
    }

    #[test]
    #[serial]
    fn a_function_that_raises_while_it_compiles_is_recorded_failed_with_no_graph_and_its_traceback()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv().given_a_venv_compiling_warning(
            the_graph_of_a_function_named("camera", serde_json::json!([])),
            &[],
        );
        let camera = project.stream_id_of("camera");
        project.raise_in_the_function_printing(
            "camera",
            "Traceback (most recent call last):\nValueError: bad wiring",
        );

        let run = project.run(&runner)
            .expect("a raising function is a failed stream, not a refused run");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert_eq!(
            failed_because,
            "Traceback (most recent call last):\nValueError: bad wiring"
        );
        assert_eq!(stream.stream_id, camera);
        assert_eq!(
            stream.display_name, "camera",
            "shown under its function's name"
        );
        assert_eq!(stream.stream_location, project.location_of("camera"));
        assert!(!stream.replaced_a_recorded_stream);
        assert_eq!(run.project_directory, project.path());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(StreamRecord::of_a_stream_whose_compile_failed(
                project.location_of("camera"),
                "camera",
                &project.stream_environment(),
                "Traceback (most recent call last):\nValueError: bad wiring",
            ))
        );
        let listed = runner.list_streams();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, StreamListingState::Failed);
        assert_eq!(listed[0].failed_because.as_deref(), Some(failed_because));
    }

    #[test]
    #[serial]
    fn a_running_stream_whose_function_now_raises_is_unloaded_and_recorded_failed_keeping_its_rulings()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let mut previous_record = a_record_of(&project, "camera", serde_json::json!([]));
        previous_record.record_the_owners_exposure_ruling(OwnerExposureRuling {
            node: "source".to_string(),
            port: "out1".to_string(),
            level: OutputPortExposureLevel::Private,
        });
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &previous_record,
        );
        project.raise_in_the_function_printing("camera", "NameError: name 'cmaera' is not defined");

        let run = project.run(&runner).unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(failed_because.contains("cmaera"), "{failed_because}");
        assert!(stream.replaced_a_recorded_stream);
        assert!(running.has_ended(), "the old version is not running");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        let recorded = the_record_of(state_directory.path(), &camera);
        assert_eq!(recorded.graph, None);
        assert_eq!(recorded.exposure_rulings, previous_record.exposure_rulings);
    }

    #[test]
    #[serial]
    fn an_entry_that_does_not_import_records_the_project_failed_until_a_load_that_imports_forgets_it()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let project_stream_id = project.location_of_the_project().stream_id();
        let folder_name = project
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let folder_cast = cast_exposed_name_to_url_safe(&folder_name).unwrap();
        assert_eq!(
            project_stream_id
                .rsplit_once('-')
                .map(|(folder, hash)| (folder, hash.len())),
            Some((folder_cast.as_ref(), 6)),
            "the project's id is `<folder>-<hash>`"
        );
        project.fail_to_import_printing("  File \"stream.py\", line 5\nSyntaxError: expected ':'");

        let run = project.run(&runner)
            .expect("a project whose entry does not import is taken in failed");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(failed_because.contains("SyntaxError"), "{failed_because}");
        assert_eq!(stream.stream_id, project_stream_id);
        assert_eq!(stream.display_name, folder_name);
        assert_eq!(stream.stream_location, project.location_of_the_project());
        let recorded = the_record_of(state_directory.path(), &project_stream_id);
        assert_eq!(recorded.graph, None);
        assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
        assert_eq!(
            runner.list_streams()[0].state,
            StreamListingState::Failed,
            "the brand-new broken project is listed"
        );

        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");
        let imported_again = project.run(&runner).unwrap();

        let (stream, _) = the_one_failed_stream_of(&imported_again);
        assert_eq!(stream.stream_id, project.stream_id_of("camera"));
        assert_eq!(
            the_current_record_in(state_directory.path(), &project_stream_id),
            None,
            "a load whose entry imports forgets the project's record"
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
    }

    #[test]
    #[serial]
    fn an_entry_that_does_not_import_fails_every_record_of_the_project_and_unloads_each_one_loaded()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let other_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );
        let mut stopped_preview = a_record_of(&project, "preview", serde_json::json!([]));
        stopped_preview.stopped = true;
        records_in(state_directory.path())
            .write(&stopped_preview)
            .unwrap();
        let other = a_record_of(&other_project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&other).unwrap();
        project.fail_to_import_printing("ValueError: bad import");

        let run = project.run(&runner).unwrap();

        let mut failed_ids: Vec<&str> = run
            .streams
            .iter()
            .map(|stream| {
                assert_eq!(
                    stream.state,
                    StreamStateAfterItsRun::Failed {
                        failed_because: "ValueError: bad import".to_string()
                    }
                );
                stream.stream_id.as_str()
            })
            .collect();
        failed_ids.sort();
        let mut expected_ids = vec![
            project.stream_id_of("camera"),
            project.stream_id_of("preview"),
        ];
        expected_ids.sort();
        assert_eq!(failed_ids, expected_ids);
        assert!(camera.has_ended());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        for stream_id in &expected_ids {
            assert_eq!(
                the_record_of(state_directory.path(), stream_id)
                    .failed_because
                    .as_deref(),
                Some("ValueError: bad import")
            );
        }
        assert_eq!(
            the_current_record_in(state_directory.path(), &other.stream_id),
            Some(other),
            "another project's record is untouched"
        );
        assert_eq!(
            the_current_record_in(
                state_directory.path(),
                &project.location_of_the_project().stream_id()
            ),
            None,
            "a project with records gets no record of its own"
        );
    }

    #[test]
    #[serial]
    fn a_recorded_function_stream_py_no_longer_defines_is_unloaded_and_failed_never_forgotten() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "side",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );

        let run = project.run(&runner).expect("the load runs");

        let (stream, _, _) = the_one_running_stream_of(&run);
        assert_eq!(stream.stream_id, project.stream_id_of("side"));
        assert!(running.has_ended());
        let failed_because = the_record_of(state_directory.path(), &camera)
            .failed_because
            .expect("the record is failed");
        assert!(
            failed_because.contains("`stream:camera` is no longer defined in stream.py"),
            "{failed_because}"
        );
    }

    #[test]
    #[serial]
    fn starting_a_stream_whose_compile_failed_compiles_it_from_its_location() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");
        project.run(&runner).unwrap();
        project.raise_in_the_function_printing("camera", "ValueError: still bad");

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&camera));

        assert!(refusal.contains("still bad"), "{refusal}");
        assert_eq!(
            project.the_last_compiles_arguments(),
            "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\nstream:camera\n",
            "the compile names the recorded location's function"
        );
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .failed_because
                .as_deref(),
            Some("ValueError: still bad")
        );

        std::fs::write(project.path().join("stream.py"), "").unwrap();
        refusal_of(runner.start_the_stream_now_for_a_test(&camera));
        assert_eq!(
            project.the_last_compiles_arguments(),
            format!(
                "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\n{}:camera\n",
                project.path().join("stream.py").display()
            ),
            "a function of an entry file is compiled from that file"
        );
    }

    #[test]
    #[serial]
    fn starting_one_of_several_records_whose_entry_no_longer_imports_answers_for_that_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "aaa",
            serde_json::json!([]),
        ));
        records_in(state_directory.path())
            .write(&a_record_of(&project, "aaa", serde_json::json!([])))
            .unwrap();
        project.raise_in_the_function_printing("zzz", "ValueError: bad wiring");
        project.run(&runner).unwrap();
        let zzz = project.stream_id_of("zzz");
        assert!(
            project.stream_id_of("aaa") < zzz,
            "the record started sorts second"
        );
        project.fail_to_import_printing("SyntaxError: invalid syntax");

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&zzz));

        assert!(refusal.contains(&format!("`{zzz}`")), "{refusal}");
        assert!(
            !refusal.contains(&format!("`{}`", project.stream_id_of("aaa"))),
            "{refusal}"
        );
        assert!(refusal.contains("SyntaxError"), "{refusal}");
    }

    #[test]
    #[serial]
    fn starting_the_record_of_a_project_that_gave_no_stream_runs_the_project() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let project_stream_id = project.location_of_the_project().stream_id();
        project.fail_to_import_printing("SyntaxError: invalid syntax");
        project.run(&runner).unwrap();
        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&project_stream_id));

        assert!(refusal.contains("bad wiring"), "{refusal}");
        assert!(
            refusal.contains(&format!("`{}`", project.stream_id_of("camera"))),
            "{refusal}"
        );
        assert_eq!(
            project.the_last_compiles_arguments(),
            "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\n--every-stream\n",
            "the project's run names no function"
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &project_stream_id),
            None
        );
        assert!(
            the_current_record_in(state_directory.path(), &project.stream_id_of("camera"))
                .is_some()
        );
    }

    #[test]
    #[serial]
    fn starting_the_record_of_an_entry_other_than_the_projects_stream_py_is_refused_naming_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let rigs = project.path().join("rigs");
        std::fs::create_dir(&rigs).unwrap();
        let record_of_rigs = StreamRecord::of_a_stream_whose_compile_failed(
            StreamLocation::of_a_project_that_gave_no_stream(rigs.clone()),
            "rigs",
            &project.stream_environment(),
            "SyntaxError: invalid syntax",
        );
        records_in(state_directory.path())
            .write(&record_of_rigs)
            .unwrap();

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&record_of_rigs.stream_id));

        assert!(refusal.contains(&rigs.display().to_string()), "{refusal}");
        assert!(refusal.contains("tatolab run"), "{refusal}");
        assert!(
            !project.path().join("compile-arguments").exists(),
            "nothing is compiled in place of the entry that did not import"
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &record_of_rigs.stream_id),
            Some(record_of_rigs)
        );
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_compiles_a_record_holding_no_graph_from_its_location() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let mut graphless = StreamRecord::of_a_stream_whose_compile_failed(
            project.location_of("camera"),
            "camera",
            &project.stream_environment(),
            "an earlier failure",
        );
        graphless.failed_because = None;
        records_in(state_directory.path())
            .write(&graphless)
            .unwrap();
        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");

        let reloads = runner.reload_at_the_start_and_wait();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Queued { stream_ids, .. }] if *stream_ids == [camera.clone()]
            ),
            "{reloads:?}"
        );
        assert!(
            project
                .the_last_compiles_arguments()
                .ends_with("stream:camera\n")
        );
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .failed_because
                .as_deref(),
            Some("ValueError: bad wiring")
        );
    }

    #[test]
    #[serial]
    fn a_stopped_recorded_stream_is_recorded_stopped_and_a_fresh_runtime_leaves_it_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            let running = a_recorded_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_record_of(&project, "camera", serde_json::json!([])),
            );

            let stopped = runner
                .stop_the_stream_now_for_a_test(&camera)
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
            runner
                .stop_the_stream_now_for_a_test(&camera)
                .expect("a stop of a stopped stream leaves it stopped");
        }

        let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        assert!(
            fresh_runner
                .reload_at_the_start_and_wait()
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
                action_id: None,
                step: None,
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
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();

        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let running = runner
                    .load_a_stream_with_the_owners_rulings(
                        OneStreamAStreamActionLoads::recorded_by(
                            &record,
                            record.graph.as_ref().unwrap(),
                        ),
                    )
                    .expect("the recorded stream loads");

                let stopped = runner
                    .stop_the_stream_now_for_a_test(&camera)
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

                let refusal = refusal_of(runner.stop_the_stream_now_for_a_test(&camera));
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
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_record_of(&project, "camera", serde_json::json!([])),
        );

        let stopped = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            runner.stop_the_stream_now_for_a_test(&camera)
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
    fn raising_a_port_of_a_loaded_recorded_stream_whose_record_is_unreadable_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let recorded = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );

        let mut levels = [
            OutputPortExposureLevel::Private,
            OutputPortExposureLevel::Public,
        ]
        .into_iter();
        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let level = levels.next().unwrap();

                let exposed = runner
                    .expose_port(&camera, "source", "out1", level)
                    .expect("the exposure succeeds, since the level changed live");

                assert_eq!(exposed.level, level);
                assert!(!exposed.recorded);
                let not_recorded = exposed
                    .ruling_not_recorded_because
                    .expect("the outcome says why the ruling was not recorded");
                assert!(
                    not_recorded.contains(&record_path.display().to_string()),
                    "{not_recorded}"
                );
                assert_eq!(
                    the_exposures_graph_renders_for(&recorded),
                    serde_json::json!([{"node": "source", "port": "out1", "level": level}]),
                    "the level changed live"
                );
                assert_eq!(std::fs::read(record_path).unwrap(), unreadable_bytes);
            },
        );
    }

    #[test]
    #[serial]
    fn raising_a_port_of_a_loaded_recorded_stream_whose_record_cannot_be_written_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let recorded = a_recorded_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_record_of(&project, "camera", serde_json::json!([])),
        );

        let exposed = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            runner.expose_port(&camera, "source", "out1", OutputPortExposureLevel::Public)
        }
        .expect("the exposure succeeds, since the level changed live");

        assert!(!exposed.recorded);
        assert!(
            exposed.ruling_not_recorded_because.is_some(),
            "the outcome says why the ruling was not recorded: {exposed:?}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&recorded),
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
            "the level changed live"
        );
        assert!(
            the_record_of(&streams_directory, &camera)
                .exposure_rulings
                .is_empty(),
            "the record holds no ruling"
        );
    }

    #[test]
    #[serial]
    fn restricting_a_port_of_a_loaded_recorded_stream_whose_record_cannot_be_written_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_record_of(
            &project,
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
        );
        let recorded =
            a_recorded_stream_loaded_without_its_start(&runner, &streams_directory, &record);

        for restricted_level in [
            OutputPortExposureLevel::Private,
            OutputPortExposureLevel::Internal,
        ] {
            let refusal = {
                let _refusing_writes =
                    StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
                refusal_of(runner.expose_port(&camera, "source", "out1", restricted_level))
            };

            assert!(refusal.contains("not restricted"), "{refusal}");
            assert!(refusal.contains("still public"), "{refusal}");
            assert!(refusal.contains("was not written"), "{refusal}");
            assert_eq!(
                the_exposures_graph_renders_for(&recorded),
                serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
                "the live level is unchanged"
            );
            assert_eq!(
                the_current_record_in(&streams_directory, &camera),
                Some(record.clone())
            );
        }
    }

    #[test]
    #[serial]
    fn restricting_a_port_of_a_loaded_recorded_stream_whose_record_is_unreadable_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let recorded = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(
                &project,
                "camera",
                serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            ),
        );

        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let refusal = refusal_of(runner.expose_port(
                    &camera,
                    "source",
                    "out1",
                    OutputPortExposureLevel::Internal,
                ));

                assert!(
                    refusal.contains(&record_path.display().to_string()),
                    "{refusal}"
                );
                assert_eq!(
                    the_exposures_graph_renders_for(&recorded),
                    serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
                    "the live level is unchanged"
                );
                assert_eq!(std::fs::read(record_path).unwrap(), unreadable_bytes);
            },
        );
    }

    #[test]
    #[serial]
    fn a_loaded_recorded_stream_whose_record_is_gone_raises_live_naming_the_record_and_never_restricts()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let recorded = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );
        let record_path = records_in(state_directory.path())
            .record_path_of(&camera)
            .unwrap();
        std::fs::remove_file(&record_path).unwrap();

        let raised = runner
            .expose_port(&camera, "source", "out1", OutputPortExposureLevel::Private)
            .expect("the port is raised live");
        let refusal = refusal_of(runner.expose_port(
            &camera,
            "source",
            "out1",
            OutputPortExposureLevel::Internal,
        ));

        assert!(!raised.recorded);
        let not_recorded = raised
            .ruling_not_recorded_because
            .expect("the outcome names the missing record");
        assert!(
            not_recorded.contains(&record_path.display().to_string()),
            "{not_recorded}"
        );
        assert!(not_recorded.contains("gone"), "{not_recorded}");
        assert!(
            refusal.contains(&record_path.display().to_string()),
            "{refusal}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&recorded),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            "the raise held live and the refused restriction changed nothing"
        );
        assert!(!record_path.exists());
    }

    #[test]
    #[serial]
    fn starting_a_recorded_stream_whose_record_is_unreadable_is_refused_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let mut stopped = a_record_of(&project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        for_each_unreadable_record_of(state_directory.path(), &camera, |record_path, _| {
            let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&camera));

            assert!(refusal.contains(&format!("`{camera}`")), "{refusal}");
            assert!(refusal.contains("not started"), "{refusal}");
            assert!(
                refusal.contains(&record_path.display().to_string()),
                "{refusal}"
            );
            assert!(runner.ids_of_the_loaded_streams().is_empty());
        });
    }

    #[test]
    #[serial]
    fn stopping_a_stream_its_host_loaded_unloads_it_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "camera");

        let stopped = runner
            .stop_the_stream_now_for_a_test(&project.stream_id_of("camera"))
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
        let project = ProjectWithAStubCompile::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "hosted-one");
        let mut stopped = a_record_of(&project, "recorded-one", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let hosted_one = project.stream_id_of("hosted-one");
        let recorded_one = project.stream_id_of("recorded-one");

        let refusals = [
            refusal_of(runner.stop_the_stream_now_for_a_test("absent")),
            refusal_of(runner.stop_stream("absent")),
            refusal_of(runner.expose_port(
                "absent",
                "source",
                "out1",
                OutputPortExposureLevel::Public,
            )),
        ];

        for refusal in refusals {
            assert!(refusal.contains("`absent`"), "{refusal}");
            assert!(refusal.contains(&hosted_one), "{refusal}");
            assert!(
                refusal.contains(&format!("{recorded_one} (stopped)")),
                "{refusal}"
            );
        }
        let start_refusal = refusal_of(runner.start_the_stream_now_for_a_test(&hosted_one));
        assert!(
            start_refusal.contains("living with its host process"),
            "{start_refusal}"
        );
        let start_refusal = refusal_of(runner.start_the_stream_now_for_a_test("absent"));
        assert!(start_refusal.contains(&recorded_one), "{start_refusal}");
    }

    #[test]
    #[serial]
    fn a_stream_action_finds_a_stream_by_its_exact_id_alone() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let camera = project.stream_id_of("camera");
        let mut stopped = a_record_of(&project, "microphone", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let microphone = project.stream_id_of("microphone");

        for spelled_otherwise in [
            camera.to_uppercase(),
            format!(" {camera}"),
            "camera".to_string(),
        ] {
            let refusal = refusal_of(runner.stop_the_stream_now_for_a_test(&spelled_otherwise));

            assert!(refusal.contains("no stream with the id"), "{refusal}");
        }
        for spelled_otherwise in [microphone.to_uppercase(), "microphone".to_string()] {
            let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&spelled_otherwise));

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
    fn removing_a_project_unloads_every_stream_of_it_and_forgets_every_record_of_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let other_project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );
        let mut stopped = a_record_of(&project, "microphone", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let other = a_record_of(&other_project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&other).unwrap();

        let removed = runner
            .remove_the_project_now(project.path(), None)
            .unwrap();

        assert_eq!(removed.streams_unloaded, [camera.clone()]);
        let mut forgotten = removed.records_forgotten.clone();
        forgotten.sort();
        let mut expected = vec![camera, microphone];
        expected.sort();
        assert_eq!(forgotten, expected);
        assert!(running.has_ended());
        assert_eq!(
            runner
                .list_streams()
                .into_iter()
                .map(|listing| listing.stream_id)
                .collect::<Vec<_>>(),
            [other.stream_id.clone()],
            "another project's record stays"
        );

        let nothing_left = runner
            .remove_the_project_now(project.path(), None)
            .expect("a project with nothing left is no error");
        assert!(nothing_left.streams_unloaded.is_empty() && nothing_left.records_forgotten.is_empty());
    }

    #[test]
    #[serial]
    fn a_queued_remove_marks_every_record_of_its_project_and_runs_to_forget_them() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();

        let queued = runner.remove_project(project.path()).unwrap();
        runner.wait_until_every_queued_stream_action_has_finished();

        assert!(queued.action_id.starts_with(&format!("{}-", runner.runtime_id())));
        assert_eq!(the_current_record_in(state_directory.path(), &camera), None);
        let described = runner.describe_stream_action(&queued.action_id).unwrap();
        assert_eq!(described.state, super::super::StreamActionState::Done);
        assert_eq!(described.kind, super::super::StreamActionKind::Remove);
        assert!(
            refusal_of(runner.remove_project(Path::new("relative/project")))
                .contains("not an absolute path")
        );
    }

    /// Wait until the stream action `action_id` is on `step`.
    fn wait_until_the_stream_action_is_on_the_step(
        runner: &Runner,
        action_id: &str,
        step: super::super::StreamActionStep,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while runner.describe_stream_action(action_id).unwrap().step != Some(step) {
            assert!(Instant::now() < deadline, "`{action_id}` never reached {step}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[serial]
    fn a_queued_run_loads_every_stream_of_the_project_and_its_description_names_them() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");

        let queued = runner.run_project(project.path()).unwrap();
        runner.wait_until_every_queued_stream_action_has_finished();

        assert_eq!(queued.stream_id, None);
        let described = runner.describe_stream_action(&queued.action_id).unwrap();
        assert_eq!(described.state, super::super::StreamActionState::Done, "{described:?}");
        assert_eq!(described.kind, super::super::StreamActionKind::Load);
        assert_eq!(described.step, None);
        assert_eq!(
            described
                .streams
                .iter()
                .map(|stream| (stream.stream_id.as_str(), stream.state))
                .collect::<Vec<_>>(),
            [(camera.as_str(), Some(StreamListingState::Running))]
        );
        assert!(
            refusal_of(runner.run_project(Path::new("relative/project")))
                .contains("not an absolute path")
        );
        assert!(
            refusal_of(runner.describe_stream_action("another-runtime-1"))
                .contains("queued no stream action")
        );
    }

    #[test]
    #[serial]
    fn a_cancel_drops_a_queued_stop_clearing_its_mark_and_ends_a_running_load_at_its_step() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let graph = the_graph_of_a_function_named("camera", serde_json::json!([]));
        let project = ProjectWithAStubCompile::compiling(graph.clone());
        let camera = project.stream_id_of("camera");
        project.run(&runner).expect("the first load runs");
        project.compile_to_once_released(graph);

        let load = runner.run_project(project.path()).unwrap();
        wait_until_the_stream_action_is_on_the_step(
            &runner,
            &load.action_id,
            super::super::StreamActionStep::Compiling,
        );
        let stop = runner.stop_stream(&camera).unwrap();
        assert_eq!(
            the_record_of(state_directory.path(), &camera).pending_stream_actions,
            [PendingStreamActionOnTheRecord {
                action_id: stop.action_id.clone(),
                kind: PendingStreamActionKind::Stop,
            }]
        );
        let listed = runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == camera)
            .unwrap();
        assert_eq!(listed.state, StreamListingState::Preparing, "{listed:?}");
        assert_eq!(listed.action_id.as_deref(), Some(load.action_id.as_str()));

        let stop_cancelled = runner.cancel_stream_action(&stop.action_id).unwrap();
        let load_cancelled = runner.cancel_stream_action(&load.action_id).unwrap();
        project.release_the_held_compiles();
        runner.wait_until_every_queued_stream_action_has_finished();

        assert_eq!(stop_cancelled.state, super::super::StreamActionState::Cancelled);
        assert_eq!(stop_cancelled.not_cancelled_because, None);
        assert_eq!(load_cancelled.state, super::super::StreamActionState::InProgress);
        assert_eq!(load_cancelled.not_cancelled_because, None);
        let record = the_record_of(state_directory.path(), &camera);
        assert!(record.pending_stream_actions.is_empty(), "{record:?}");
        assert!(!record.stopped);
        assert_eq!(
            runner.describe_stream_action(&load.action_id).unwrap().state,
            super::super::StreamActionState::Cancelled
        );
        assert!(!runner.loaded_stream_of_the_id(&camera).unwrap().has_ended());
        let after_its_end = runner.cancel_stream_action(&load.action_id).unwrap();
        assert!(
            after_its_end
                .not_cancelled_because
                .as_ref()
                .is_some_and(|because| because.contains("already ended")),
            "{after_its_end:?}"
        );
    }

    #[test]
    #[serial]
    fn a_create_over_a_scaffold_file_fails_its_action_writing_nothing_and_names_the_new_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();

        let queued = runner.create_project(Some(project.path())).unwrap();
        runner.wait_until_every_queued_stream_action_has_finished();

        assert_eq!(queued.stream_id, Some(project.stream_id_of("main")));
        let described = runner.describe_stream_action(&queued.action_id).unwrap();
        assert_eq!(described.state, super::super::StreamActionState::Failed);
        assert!(
            described
                .failed_because
                .as_deref()
                .is_some_and(|because| because.contains("already has pyproject.toml")),
            "{described:?}"
        );
        assert!(!project.path().join("stream.py").exists());
        assert!(records_in(state_directory.path()).read_every().is_empty());
        assert!(
            refusal_of(runner.create_project(Some(Path::new("relative/project"))))
                .contains("not an absolute path")
        );
    }

    #[test]
    #[serial]
    fn the_start_finishes_the_stops_and_removes_a_previous_run_left_pending() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        let mut stop_pending = a_record_of(&project, "camera", serde_json::json!([]));
        stop_pending.pending_stream_actions = vec![PendingStreamActionOnTheRecord {
            action_id: "an-earlier-runtime-3".to_string(),
            kind: PendingStreamActionKind::Stop,
        }];
        let mut remove_pending = a_record_of(&project, "microphone", serde_json::json!([]));
        remove_pending.stopped = true;
        remove_pending.pending_stream_actions = vec![PendingStreamActionOnTheRecord {
            action_id: "an-earlier-runtime-4".to_string(),
            kind: PendingStreamActionKind::Remove,
        }];
        records_in(state_directory.path()).write(&stop_pending).unwrap();
        records_in(state_directory.path()).write(&remove_pending).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_at_the_start_and_wait();

        assert!(
            !reloads
                .iter()
                .any(|reload| matches!(reload, StreamReloadAtTheStart::Queued { .. })),
            "{reloads:?}"
        );
        let camera_record = the_record_of(state_directory.path(), &camera);
        assert!(camera_record.stopped, "{camera_record:?}");
        assert!(camera_record.pending_stream_actions.is_empty());
        assert_eq!(the_current_record_in(state_directory.path(), &microphone), None);
    }

    #[test]
    #[serial]
    fn renaming_a_recorded_stream_rewrites_its_record_then_its_table_entry_and_a_restart_keeps_the_name()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            let running = a_recorded_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_record_of(&project, "camera", serde_json::json!([])),
            );
            let mut stopped = a_record_of(&project, "microphone", serde_json::json!([]));
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
        expected.sort();
        assert_eq!(shown_as, expected);
    }

    #[test]
    #[serial]
    fn a_rename_whose_record_cannot_be_written_is_refused_and_the_stream_is_shown_as_it_was() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_record_of(&project, "camera", serde_json::json!([])),
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
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
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
        let project = ProjectWithAStubCompile::with_no_venv();
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
        let project = ProjectWithAStubCompile::with_no_venv();
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
        let project = ProjectWithAStubCompile::with_no_venv();
        a_stream_its_host_loaded(&runner, &project, "a-hosted");
        a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "b-recorded", serde_json::json!([])),
        );
        let mut stopped = a_record_of(&project, "c-stopped", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        records_in(state_directory.path())
            .write(&a_record_of(
                &project,
                "d-not-re-loaded",
                serde_json::json!([]),
            ))
            .unwrap();

        let listing_of = |function_name: &str, state, node_count| StreamListing {
            stream_id: project.stream_id_of(function_name),
            display_name: function_name.to_string(),
            stream_location: Some(project.location_of(function_name)),
            state,
            node_count,
            failed_because: None,
            action_id: None,
            step: None,
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
    fn a_record_an_earlier_runtime_wrote_is_listed_failed_never_loaded_and_removed_with_its_project()
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
        let reloads = runner.reload_at_the_start_and_wait();
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
        for refusal in [
            refusal_of(runner.start_the_stream_now_for_a_test("main")),
            refusal_of(runner.stop_the_stream_now_for_a_test("main")),
            refusal_of(runner.expose_port(
                "main",
                "source",
                "out1",
                OutputPortExposureLevel::Public,
            )),
        ] {
            assert!(
                refusal.contains("an earlier runtime recorded it"),
                "{refusal}"
            );
        }
        assert!(runner.ids_of_the_loaded_streams().is_empty());

        assert_eq!(
            runner
                .remove_the_project_now(Path::new("/home/someone/hello"), None)
                .unwrap()
                .records_forgotten,
            ["main".to_string()]
        );
        assert!(!earlier_record_path.exists());
        assert!(runner.list_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_port_of_a_loaded_recorded_stream_changes_live_and_its_ruling_is_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let recorded = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(
                &project,
                "camera",
                serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            ),
        );

        let exposed = runner
            .expose_port(&camera, "source", "out1", OutputPortExposureLevel::Internal)
            .expect("the port is made internal");

        assert!(exposed.recorded);
        assert_eq!(exposed.stream_id, camera);
        assert_eq!(exposed.ruling_not_recorded_because, None);
        assert_eq!(
            the_exposures_graph_renders_for(&recorded),
            serde_json::json!([])
        );
        assert_eq!(
            the_record_of(state_directory.path(), &camera).exposure_rulings,
            [OwnerExposureRuling {
                node: "source".to_string(),
                port: "out1".to_string(),
                level: OutputPortExposureLevel::Internal,
            }]
        );

        let refusal = refusal_of(runner.expose_port(
            &camera,
            "source",
            "absent-port",
            OutputPortExposureLevel::Public,
        ));
        assert!(refusal.contains("absent-port"), "{refusal}");
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .exposure_rulings
                .len(),
            1,
            "a refused live change records nothing"
        );
    }

    #[test]
    #[serial]
    fn a_port_of_a_stream_its_host_loaded_changes_live_and_nothing_is_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");

        let exposed = runner
            .expose_port(
                &project.stream_id_of("camera"),
                "source",
                "out2",
                OutputPortExposureLevel::Public,
            )
            .expect("the port is made public");

        assert!(!exposed.recorded);
        assert_eq!(
            exposed.ruling_not_recorded_because, None,
            "the level of a stream its host loaded is never recorded, and that is no failure"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&hosted),
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}])
        );
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_port_of_a_stopped_stream_is_recorded_once_the_recorded_graph_holds_its_node() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let mut stopped = a_record_of(&project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        let exposed = runner
            .expose_port(&camera, "Source", "out2", OutputPortExposureLevel::Public)
            .expect("the ruling is recorded");
        let refusal = refusal_of(runner.expose_port(
            &camera,
            "absent-node",
            "out1",
            OutputPortExposureLevel::Public,
        ));

        assert!(exposed.recorded);
        assert!(refusal.contains("absent-node"), "{refusal}");
        assert!(refusal.contains("source"), "{refusal}");
        let recorded = the_record_of(state_directory.path(), &camera);
        assert!(recorded.stopped);
        assert_eq!(
            recorded.exposure_rulings,
            [OwnerExposureRuling {
                node: "Source".to_string(),
                port: "out2".to_string(),
                level: OutputPortExposureLevel::Public,
            }]
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_recorded_streams_rulings_hold_at_its_load_and_one_on_a_port_its_node_lacks_is_skipped() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let mut record = a_record_of(
            &project,
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
        );
        for (port, level) in [
            ("out1", OutputPortExposureLevel::Internal),
            ("out2", OutputPortExposureLevel::Private),
            ("renamed-port", OutputPortExposureLevel::Public),
        ] {
            record.record_the_owners_exposure_ruling(OwnerExposureRuling {
                node: "source".to_string(),
                port: port.to_string(),
                level,
            });
        }

        let loaded =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        assert_eq!(
            the_exposures_graph_renders_for(&loaded),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}])
        );
        assert_eq!(loaded.holding(), LoadedStreamHolding::RecordedByTheRuntime);
        assert_eq!(loaded.stream_id(), project.stream_id_of("camera"));
        assert_eq!(loaded.display_name(), "camera");
        assert_eq!(
            loaded.stream_location(),
            Some(&project.location_of("camera"))
        );
    }

    #[test]
    #[serial]
    fn a_recorded_restriction_is_the_level_a_recorded_stream_holds_before_it_starts() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let mut record = a_record_of(
            &project,
            "camera",
            serde_json::json!([
                {"node": "source", "port": "out1", "level": "private"},
                {"node": "source", "port": "out2", "level": "private"}
            ]),
        );
        for (port, level) in [
            ("out1", OutputPortExposureLevel::Internal),
            ("out2", OutputPortExposureLevel::Public),
        ] {
            record.record_the_owners_exposure_ruling(OwnerExposureRuling {
                node: "source".to_string(),
                port: port.to_string(),
                level,
            });
        }

        let loaded =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        assert_eq!(
            the_exposures_graph_renders_for(&loaded),
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}])
        );
        assert_eq!(
            loaded
                .output_port_exposure_level("source", "out1")
                .expect("the port is there"),
            OutputPortExposureLevel::Internal,
            "the owner's internal holds over the function's private"
        );
    }

    #[test]
    #[serial]
    fn reloading_reports_each_recorded_stream_whose_venv_cannot_be_made_by_id_and_keeps_going() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        for function_name in ["a-camera", "c-microphone"] {
            records
                .write(&a_record_of(&project, function_name, serde_json::json!([])))
                .unwrap();
        }
        let malformed = state_directory.path().join("b-malformed.json");
        std::fs::write(&malformed, b"{").unwrap();
        let mut stopped = a_record_of(&project, "d-stopped", serde_json::json!([]));
        stopped.stopped = true;
        records.write(&stopped).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_at_the_start_and_wait();

        let mut queued_ids = Vec::new();
        let mut unreadable_paths = Vec::new();
        for reload in &reloads {
            match reload {
                StreamReloadAtTheStart::Queued { stream_ids, .. } => {
                    queued_ids.extend(stream_ids.iter().cloned())
                }
                StreamReloadAtTheStart::RecordUnreadable(unreadable) => {
                    unreadable_paths.push(unreadable.path.clone());
                }
                other => {
                    panic!("only the project's re-load and the malformed record: {other:?}")
                }
            }
        }
        let mut expected_ids = vec![
            project.stream_id_of("a-camera"),
            project.stream_id_of("c-microphone"),
        ];
        expected_ids.sort();
        assert_eq!(queued_ids, expected_ids);
        for stream_id in &expected_ids {
            let failed_because = the_record_of(state_directory.path(), stream_id)
                .failed_because
                .expect("the stream is recorded failed");
            assert!(failed_because.contains("carries no uv"), "{failed_because}");
        }
        assert_eq!(unreadable_paths, [malformed]);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(records.read_every().len(), 4, "every record stays");
    }

    #[test]
    #[serial]
    fn a_recorded_stream_stopped_or_removed_between_the_queue_and_its_reload_stays_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let records = records_in(state_directory.path());
        for function_name in ["a-stopped", "b-removed"] {
            records
                .write(&a_record_of(&project, function_name, serde_json::json!([])))
                .unwrap();
        }
        let listed = [
            project.stream_id_of("a-stopped"),
            project.stream_id_of("b-removed"),
        ];
        let mut stopped_since =
            the_record_of(state_directory.path(), &project.stream_id_of("a-stopped"));
        stopped_since.stopped = true;
        records.write(&stopped_since).unwrap();
        assert!(records.remove(&project.stream_id_of("b-removed")).unwrap());

        runner
            .reload_the_records_of_a_project_now(project.path(), &listed, None)
            .unwrap();

        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            records.read_every(),
            vec![Ok(StreamRecordAsRead::Current(stopped_since))]
        );
    }

    #[test]
    #[serial]
    fn a_record_unreadable_by_its_reload_is_skipped_and_left_in_place() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        records
            .write(&a_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let listed = [project.stream_id_of("camera")];
        let record_path_malformed_since_the_listing = records
            .record_path_of(&project.stream_id_of("camera"))
            .unwrap();
        std::fs::write(&record_path_malformed_since_the_listing, b"{").unwrap();

        runner
            .reload_the_records_of_a_project_now(project.path(), &listed, None)
            .unwrap();

        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(
            record_path_malformed_since_the_listing.is_file(),
            "the record is left in place"
        );
    }

    #[test]
    #[serial]
    fn a_recorded_stream_loaded_between_the_queue_and_its_reload_is_left_as_it_is() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let record = a_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();
        let listed = [record.stream_id.clone()];
        let loaded_since =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        runner
            .reload_the_records_of_a_project_now(project.path(), &listed, None)
            .unwrap();

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
    fn a_load_leaves_a_stream_its_host_loaded_and_fails_a_record_stream_py_no_longer_defines() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("studio"),
        );
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let record = a_record_of(&project, "recorded-camera", serde_json::json!([]));
        let recorded =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let run = project.run(&runner).unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(failed_because.contains("absent-port"), "{failed_because}");
        assert_eq!(stream.stream_id, project.stream_id_of("studio"));
        assert!(!hosted.has_ended());
        assert!(recorded.has_ended());
        assert!(
            the_record_of(state_directory.path(), &record.stream_id)
                .failed_because
                .is_some_and(|failed_because| failed_because.contains("no longer defined"))
        );
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [project.stream_id_of("camera")]
        );
    }

    #[test]
    #[serial]
    fn two_worktrees_of_one_project_run_side_by_side_under_two_ids_and_neither_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let main_worktree = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "main",
            serde_json::json!([]),
        ));
        let second_worktree = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "main",
            serde_json::json!([]),
        ));

        let first_run = main_worktree.run(&runner)
            .expect("the first worktree's stream runs");
        let second_run = second_worktree.run(&runner)
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

        let rerun = main_worktree.run(&runner)
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
        let camera_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let preview_project = ProjectWithAStubCompile::compiling_warning(
            the_graph_of_a_function_named("preview", serde_json::json!([])),
            &["tatolab: the cross-floor check found 1 thing binding this app to one floor."],
        );
        let camera = camera_project.stream_id_of("camera");
        let preview = preview_project.stream_id_of("preview");

        let camera_outcome = camera_project.run(&runner)
            .expect("the camera stream runs");
        let preview_outcome = preview_project.run(&runner)
            .expect("the preview stream runs");
        let (camera_run, _, camera_node_count) = the_one_running_stream_of(&camera_outcome);
        let (preview_run, _, _) = the_one_running_stream_of(&preview_outcome);

        assert_eq!(camera_run.stream_id, camera);
        assert_eq!(
            camera_run.display_name, "camera",
            "the function's name by default"
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
            Some(a_record_of(
                &camera_project,
                "camera",
                serde_json::json!([])
            ))
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &preview),
            Some(a_record_of(
                &preview_project,
                "preview",
                serde_json::json!([])
            ))
        );
        let mut expected_listing = [
            StreamListing {
                stream_id: camera.clone(),
                display_name: "camera".to_string(),
                stream_location: Some(camera_project.location_of("camera")),
                state: StreamListingState::Running,
                node_count: Some(1),
                failed_because: None,
                action_id: None,
                step: None,
            },
            StreamListing {
                stream_id: preview.clone(),
                display_name: "preview".to_string(),
                stream_location: Some(preview_project.location_of("preview")),
                state: StreamListingState::Running,
                node_count: Some(1),
                failed_because: None,
                action_id: None,
                step: None,
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
    fn a_run_of_the_records_location_replaces_it_under_its_id_keeping_its_renamed_display_name_and_rulings()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first_run = project.run(&runner).unwrap();
        let (first, first_stream_tag, _) = the_one_running_stream_of(&first_run);
        runner.rename_stream(&camera, "Front camera").unwrap();
        runner
            .expose_port(&camera, "source", "out1", OutputPortExposureLevel::Internal)
            .unwrap();
        let changed_source = the_graph_of_a_function_named(
            "camera",
            serde_json::json!([
                {"node": "source", "port": "out1", "level": "public"},
                {"node": "source", "port": "out2", "level": "private"}
            ]),
        );
        project.compile_to_warning(
            changed_source.clone(),
            &["a warning of the replacing compile"],
        );

        let replaced_run = project.run(&runner)
            .expect("the recorded stream is replaced");
        let (replaced, replaced_stream_tag, _) = the_one_running_stream_of(&replaced_run);

        assert_eq!(
            first.display_name, "camera",
            "a new stream is shown under its function's name"
        );
        assert!(replaced.replaced_a_recorded_stream);
        assert_eq!(replaced.stream_id, camera);
        assert_eq!(
            replaced.display_name, "Front camera",
            "a run giving no display name keeps the renamed one"
        );
        assert_ne!(replaced_stream_tag, first_stream_tag);
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(running.stream_tag(), replaced_stream_tag);
        assert_eq!(running.display_name(), "Front camera");
        assert_eq!(
            the_exposures_graph_renders_for(&running),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}]),
            "the owner's internal ruling wins over the changed function's public"
        );
        let recorded = the_record_of(state_directory.path(), &camera);
        assert_eq!(recorded.graph, Some(changed_source));
        assert_eq!(recorded.display_name, "Front camera");
        assert_eq!(
            recorded.exposure_rulings,
            [OwnerExposureRuling {
                node: "source".to_string(),
                port: "out1".to_string(),
                level: OutputPortExposureLevel::Internal,
            }]
        );

        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_stopped_recorded_stream_starts_again_and_re_loads_at_a_restart_with_its_rulings_applied() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
        ));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
            project.run(&runner).unwrap();
            runner
                .expose_port(&camera, "source", "out1", OutputPortExposureLevel::Private)
                .unwrap();
            runner.stop_the_stream_now_for_a_test(&camera).unwrap();

            let started = runner
                .start_the_stream_now_for_a_test(&camera)
                .expect("the stream starts again");

            assert_eq!(
                started,
                StreamStartOutcome {
                    stream_id: camera.clone(),
                    node_count: 1,
                }
            );
            assert_eq!(
                the_exposures_graph_renders_for(&runner.loaded_stream_of_the_id(&camera).unwrap()),
                serde_json::json!([{"node": "source", "port": "out1", "level": "private"}])
            );
            assert!(!the_record_of(state_directory.path(), &camera).stopped);
            let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&camera));
            assert!(refusal.contains("already loaded"), "{refusal}");
        }

        let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let reloads = restarted.reload_at_the_start_and_wait();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Queued { stream_ids, .. }] if *stream_ids == [camera.clone()]
            ),
            "{reloads:?}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&restarted.loaded_stream_of_the_id(&camera).unwrap()),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}])
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn an_owners_restriction_recorded_between_the_queue_and_the_reload_holds_at_the_load() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let records = records_in(state_directory.path());
        records
            .write(&a_record_of(
                &project,
                "camera",
                serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
            ))
            .unwrap();
        let listed = [camera.clone()];
        let mut restricted_since = the_record_of(state_directory.path(), &camera);
        restricted_since.record_the_owners_exposure_ruling(OwnerExposureRuling {
            node: "source".to_string(),
            port: "out1".to_string(),
            level: OutputPortExposureLevel::Internal,
        });
        records.write(&restricted_since).unwrap();

        runner
            .reload_the_records_of_a_project_now(project.path(), &listed, None)
            .unwrap();
        assert_eq!(
            the_exposures_graph_renders_for(&runner.loaded_stream_of_the_id(&camera).unwrap()),
            serde_json::json!([]),
            "the restriction recorded after the queue holds"
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(restricted_since)
        );
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let run = project.run(&runner)
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
        assert!(
            recorded.graph.is_some(),
            "`start` retries the recorded graph"
        );
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(project.run(&runner))
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
    fn a_project_fixed_after_its_entry_did_not_import_runs_its_stream_in_place_of_its_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "main",
            serde_json::json!([]),
        ));
        let project_stream_id = project.location_of_the_project().stream_id();
        project.fail_to_import_printing("SyntaxError: invalid syntax");
        project.run(&runner).unwrap();
        project.compile_to(the_graph_of_a_function_named("main", serde_json::json!([])));

        let run = project.run(&runner).unwrap();

        let (stream, _, node_count) = the_one_running_stream_of(&run);
        assert_eq!(stream.stream_id, project.stream_id_of("main"));
        assert_eq!(node_count, 1);
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [project.stream_id_of("main")]
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &project_stream_id),
            None
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn starting_a_stream_whose_compile_failed_runs_it_once_its_code_compiles() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");
        project.run(&runner).unwrap();
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let started = runner
            .start_the_stream_now_for_a_test(&camera)
            .expect("the stream compiles from its location and starts");

        assert_eq!(
            started,
            StreamStartOutcome {
                stream_id: camera.clone(),
                node_count: 1,
            }
        );
        let recorded = the_record_of(state_directory.path(), &camera);
        assert!(!recorded.is_failed());
        assert!(recorded.graph.is_some());
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first_run = project.run(&runner).unwrap();
        let (_, first_stream_tag, _) = the_one_running_stream_of(&first_run);
        let previous_record = the_record_of(&streams_directory, &camera);
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}]),
        ));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(project.run(&runner))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        let running = runner
            .loaded_stream_of_the_id(&camera)
            .expect("the running stream stays loaded");
        assert_eq!(running.stream_tag(), first_stream_tag);
        assert_eq!(
            the_exposures_graph_renders_for(&running),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
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
        let project = ProjectWithAStubCompile::compiling(serde_json::json!({}));
        for function_name in ["crasher", "steady"] {
            records_in(state_directory.path())
                .write(&a_record_of(&project, function_name, serde_json::json!([])))
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

        assert_eq!(failed_by_the_second, [crasher.clone()]);
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
        let project = ProjectWithAStubCompile::compiling(serde_json::json!({}));
        records_in(state_directory.path())
            .write(&a_record_of(&project, "crasher", serde_json::json!([])))
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
        let project = ProjectWithAStubCompile::compiling(serde_json::json!({}));
        let crasher = project.stream_id_of("crasher");
        let mut failed = a_record_of(&project, "crasher", serde_json::json!([]));
        failed.failed_because = Some("it crashed the runtime".to_string());
        failed.runtime_crashes_in_a_row_implicating_it = 2;
        records_in(state_directory.path()).write(&failed).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &HowThePreviousRuntimeRunEnded::Cleanly,
        );
        let reloads = runner.reload_at_the_start_and_wait();

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
        let refusal = refusal_of(runner.stop_the_stream_now_for_a_test(&crasher));
        assert!(refusal.contains("is failed"), "{refusal}");
        assert!(refusal.contains("it crashed the runtime"), "{refusal}");
        assert_eq!(the_record_of(state_directory.path(), &crasher), failed);
    }

    #[test]
    #[serial]
    fn a_recorded_stream_whose_venv_is_gone_is_failed_at_the_start_naming_its_interpreter() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_at_the_start_and_wait();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Queued { stream_ids, .. }] if *stream_ids == [camera.clone()]
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
                .reload_at_the_start_and_wait()
                .as_slice(),
            [StreamReloadAtTheStart::SkippedAsFailed { .. }]
        ));
    }

    #[test]
    #[serial]
    fn a_refused_retry_of_a_failed_stream_keeps_it_failed_for_the_retrys_reason_its_count_reset() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "crasher",
            serde_json::json!([]),
        ));
        let crasher = project.stream_id_of("crasher");
        let mut failed = a_record_of(&project, "crasher", serde_json::json!([]));
        failed.graph = Some(serde_json::json!({
            "stream": "crasher",
            "nodes": [{"name": "source", "type": "NoSuchBuiltInNodeType", "config": {}}],
            "links": [],
        }));
        failed.failed_because = Some("it crashed the runtime".to_string());
        failed.runtime_crashes_in_a_row_implicating_it = 2;
        records_in(state_directory.path()).write(&failed).unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&crasher));

        assert!(
            refusal.contains("no node type `NoSuchBuiltInNodeType`"),
            "{refusal}"
        );
        let retried = the_record_of(state_directory.path(), &crasher);
        assert_eq!(retried.runtime_crashes_in_a_row_implicating_it, 0);
        let failed_because = retried.failed_because.expect("the stream stays failed");
        assert!(
            failed_because.contains("`start` retried it"),
            "{failed_because}"
        );
        assert!(failed_because.contains(&refusal), "{failed_because}");
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
        fn making_the_venv_moved_aside_from(project: &ProjectWithAStubCompile) -> Self {
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);

        let run = project.run(&runner).unwrap();

        the_one_running_stream_of(&run);
        assert_eq!(stub_uv.runs(), ["sync"]);
        assert!(project.interpreter().is_file());
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_interpreter_runs_never_runs_uv() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = project.run(&runner).unwrap();

        the_one_running_stream_of(&run);
        assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_interpreter_does_not_run_has_its_venv_made_again() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);
        directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &project.path().join(".venv").join("bin"),
            0o755,
        )
        .unwrap();
        std::os::unix::fs::symlink("/nonexistent/python3.12", project.interpreter()).unwrap();

        let run = project.run(&runner).unwrap();

        the_one_running_stream_of(&run);
        assert_eq!(stub_uv.runs(), ["sync"]);
    }

    #[test]
    #[serial]
    fn a_run_whose_venv_uv_cannot_make_records_the_project_failed_in_uvs_words() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = project.run(&runner).unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains(UV_RESOLUTION_FAILURE),
            "{failed_because}"
        );
        assert_eq!(stream.stream_location, project.location_of_the_project());
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
    fn a_run_whose_venv_uv_cannot_make_unloads_and_fails_every_record_of_the_project() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        project.run(&runner).unwrap();
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = project.run(&runner).unwrap();

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
    fn a_run_on_a_runtime_given_no_uv_records_the_project_failed_saying_the_unit_carries_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();

        let run = project.run(&runner).unwrap();

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert!(
            failed_because.contains(
                "the runtime unit carries no uv; rebuild it with `cargo xtask build-runtime`"
            ),
            "{failed_because}"
        );
        assert_eq!(stream.stream_location, project.location_of_the_project());
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_in_a_missing_project_directory_is_refused_with_nothing_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let refusal =
            refusal_of(runner.load_the_project_now(&project.path().join("missing"), None, false));

        assert!(refusal.contains("is not a directory"), "{refusal}");
        assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn starting_a_stopped_stream_whose_venv_is_gone_makes_it_with_the_runtime_unit_uv() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        project.run(&runner).unwrap();
        runner.stop_the_stream_now_for_a_test(&camera).unwrap();
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);

        let started = runner.start_the_stream_now_for_a_test(&camera).expect("the stream starts");

        assert_eq!(started.stream_id, camera);
        assert_eq!(stub_uv.runs(), ["sync"]);
        assert!(!the_record_of(state_directory.path(), &camera).stopped);
    }

    #[test]
    #[serial]
    fn starting_a_stream_whose_venv_uv_cannot_make_is_refused_and_records_it_failed_in_uvs_words() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        project.run(&runner).unwrap();
        runner.stop_the_stream_now_for_a_test(&camera).unwrap();
        std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let refusal = refusal_of(runner.start_the_stream_now_for_a_test(&camera));

        assert!(refusal.contains(UV_RESOLUTION_FAILURE), "{refusal}");
        let failed_because = the_record_of(state_directory.path(), &camera)
            .failed_because
            .expect("the stream is failed");
        assert!(
            failed_because.contains(UV_RESOLUTION_FAILURE),
            "{failed_because}"
        );
        assert!(
            !project.path().join(".venv").exists(),
            "the half-made venv was left"
        );
        assert!(
            runner
                .loaded_stream_of_the_id_when_loaded(&camera)
                .is_none()
        );
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_makes_a_gone_venv_once_for_every_record_of_its_project() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let records = records_in(state_directory.path());
        for function_name in ["camera", "microphone"] {
            records
                .write(&a_record_of(&project, function_name, serde_json::json!([])))
                .unwrap();
        }
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        stub_uv.handed_to(&runner);

        let reloads = runner.reload_at_the_start_and_wait();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Queued { stream_ids, .. }] if stream_ids.len() == 2
            ),
            "{reloads:?}"
        );
        assert_eq!(runner.ids_of_the_loaded_streams().len(), 2);
        assert_eq!(stub_uv.runs(), ["sync"]);
    }

    #[test]
    #[serial]
    fn the_reload_at_the_start_records_failed_in_uvs_words_a_stream_whose_venv_uv_cannot_make() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let reloads = runner.reload_at_the_start_and_wait();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Queued { stream_ids, .. }] if *stream_ids == [camera.clone()]
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
}
