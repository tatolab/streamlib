// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions a runtime takes for its owner: run a project's stream
//! function under the stream id its location computes to, recording it — failed
//! when the project's code gives no graph or its load is refused; stop,
//! start, remove and rename a stream; list what the runtime holds; set a port's
//! exposure; and re-load the recorded streams at start.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT, STREAM_LOAD_TOOL_CALL_TIMEOUT,
};

use super::project_stream_action_queue::{
    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT, ProjectStreamsListing, StreamAction,
    StreamActionKind, StreamActionProgress, StreamActionQueueOfEachProject, StreamActionQueued,
    StreamActionState, StreamActionStep, StreamActionWorkingOnAStream, StreamOfAStreamAction,
    StreamTheLastCompileOfTheProjectNamed, WhatTheLastCompileOfTheProjectGave,
};
use super::project_venv_made_with_the_runtime_unit_uv::{
    PROJECT_VENV_MAKING_BOUND, PROJECT_VENV_SYNC_BOUND, ProjectVenvNotMade,
    make_the_projects_venv_unless_its_interpreter_runs_within,
};
use super::runtime::{
    a_stream_id_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
};
use super::{
    CrashOfThePreviousRuntimeRun, ENGINE_TEARDOWN_WATCHDOG_BUDGET, HowThePreviousRuntimeRunEnded,
    LoadedStreamInThisRuntime, LoadedStreamTag, OptionsForLoadingOneStream, OwnerExposureRuling,
    OwnerExposureRulingsSplitAroundTheLoad, Runner, StreamEnvironment,
    StreamLoadObservingMachineShutdownRequests, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordReadFailure, StreamRecordsInTheStateDirectory, is_the_machines_shutdown_requested,
    the_owners_exposure_rulings_split_around_the_load,
};
use crate::core::compiler::compiler_ops::{
    processor_interpreter_describe::PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
    stream_function_compile_in_the_projects_interpreter::{
        OneStreamOfTheProjectCompiled, STREAM_FUNCTION_COMPILE_BOUND,
        StreamFunctionCompileInTheProjectsInterpreterReport, WhatTheProjectsCodeGaveTheCompile,
        compile_the_stream_function_in_the_projects_interpreter, the_projects_venv_interpreter,
    },
};
use crate::core::graph::{
    OutputPortExposureLevel, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
    node_names_listed_for_a_refusal,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::logging::{ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread};
use crate::core::{Error, Result};

/// How a loaded stream is held: recorded by the runtime, or living as long as
/// the process that hosts its library [`Runner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadedStreamHolding {
    /// A `run_stream` load, recorded in the runtime's state directory and
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

/// The project [`Runner::run_project`] queues a load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProjectRequest {
    /// The project whose every `@stream` in `stream.py` is loaded, by absolute
    /// path.
    pub project_directory: PathBuf,
}

/// What [`Runner::run_stream`] compiles and loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStreamRequest {
    /// The project whose `.venv/bin/python` compiles the stream function, by
    /// absolute path.
    pub project_directory: PathBuf,
    /// The stream function as `tatolab run` takes it — `stream.py:main`,
    /// `x.py`, `mod.sub:fn` — or `None` for every `@stream` in the project's
    /// `stream.py`, each its own stream.
    pub stream_function: Option<String>,
    /// The display name the load gives the stream. `None` keeps a replaced
    /// stream's display name, and shows a new one under its function's name.
    pub display_name: Option<String>,
}

/// What [`Runner::run_stream`] did with a project: each stream it loaded and
/// started, or recorded failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRunOutcome {
    /// The project the run was given.
    pub project_directory: PathBuf,
    /// Each stream the run loaded or recorded failed.
    pub streams: Vec<StreamLoadedOrFailedByARun>,
    /// What the compile wrote to its standard error, line by line — the
    /// cross-floor check's warnings among it.
    pub compile_warnings: Vec<String>,
}

/// One stream [`Runner::run_stream`] loaded and started, or recorded failed.
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

/// Whether a stream [`Runner::run_stream`] took in runs, or why it is failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStateAfterItsRun {
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

/// A stream [`Runner::stop_stream`] unloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStopOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Why a recorded stream unloaded here was not recorded stopped, so a
    /// restart of the runtime loads it again; `None` when it was recorded,
    /// and for a stream its host process loaded.
    pub stop_not_recorded_because: Option<String>,
}

/// A recorded stream [`Runner::start_stream`] loaded and started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStartOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
}

/// What [`Runner::remove_stream`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRemoveOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Whether the stream was loaded and is now unloaded.
    pub unloaded: bool,
    /// Whether the stream was recorded and its record is now gone.
    pub forgotten: bool,
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
    /// Stopped by its owner, and left unloaded until `start_stream`.
    Stopped,
    /// It kept crashing the runtime or could not re-load at its start, and is
    /// skipped at the start until `start_stream` retries it; or it was
    /// recorded by an earlier runtime, and is never loaded.
    Failed,
    /// A stream action queued or running works on it.
    Preparing,
}

impl StreamListingState {
    /// The state as a listing spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Preparing => "preparing",
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
    /// The stream action working on it while one is queued or running.
    pub stream_action_under_way: Option<StreamActionWorkingOnAStream>,
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
    stream_records: OnceLock<StreamRecordsInTheStateDirectory>,
    /// Held across an action's check, load and record, and across the re-load
    /// at the runtime's start of one record, its venv made included — never
    /// across a load's venv or compile.
    one_stream_action_at_a_time: Mutex<()>,
    /// Each project's serial queue of stream actions, and the actions held.
    pub(super) stream_action_queue_of_each_project: StreamActionQueueOfEachProject,
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

/// The longest one stream action holds the one-action lock, in milliseconds: a
/// replace whose start is refused — the running stream's teardown, then the
/// load's describe and the refused start's teardown. An entry that does not
/// import unloads every stream of its project side by side, within one
/// teardown.
const LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_A_STREAM_ACTION_IN_MILLISECONDS: u128 =
    ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis()
        + PROCESSOR_INTERPRETER_DESCRIBE_BOUND.as_millis()
        + ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis();

/// The longest the re-load at the runtime's start holds the one-action lock for
/// one record, in milliseconds: its project's venv made, then its load's
/// describe and a refused start's teardown. The local API serves meanwhile.
const LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_THE_RELOAD_OF_ONE_RECORD_IN_MILLISECONDS: u128 =
    PROJECT_VENV_MAKING_BOUND.as_millis()
        + PROCESSOR_INTERPRETER_DESCRIBE_BOUND.as_millis()
        + ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis();

/// The longest whatever is queued ahead holds the one-action lock, in
/// milliseconds: a stream action, or the re-load of one record at the start.
const LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_AHEAD_IN_THE_QUEUE_IN_MILLISECONDS: u128 =
    the_longer_of(
        LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_A_STREAM_ACTION_IN_MILLISECONDS,
        LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_THE_RELOAD_OF_ONE_RECORD_IN_MILLISECONDS,
    );

const fn the_longer_of(first_in_milliseconds: u128, second_in_milliseconds: u128) -> u128 {
    if first_in_milliseconds > second_in_milliseconds {
        first_in_milliseconds
    } else {
        second_in_milliseconds
    }
}

/// The longest a load waits for its answer with one holder of the lock queued
/// ahead of it, in milliseconds: the project's venv is made and its compile
/// runs outside the lock while the one ahead holds it, then it holds the lock
/// itself.
const LONGEST_LOAD_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS: u128 = the_longer_of(
    PROJECT_VENV_MAKING_BOUND.as_millis() + STREAM_FUNCTION_COMPILE_BOUND.as_millis(),
    LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_AHEAD_IN_THE_QUEUE_IN_MILLISECONDS,
)
    + LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_A_STREAM_ACTION_IN_MILLISECONDS;

/// The longest a start of a stream holding a graph waits for its answer with
/// one holder of the lock queued ahead of it, in milliseconds: the project's
/// venv is made outside the lock while the one ahead holds it, then it holds
/// the lock itself. A start of a stream holding no graph is a load.
const LONGEST_START_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS: u128 = the_longer_of(
    PROJECT_VENV_MAKING_BOUND.as_millis(),
    LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_AHEAD_IN_THE_QUEUE_IN_MILLISECONDS,
)
    + LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_A_STREAM_ACTION_IN_MILLISECONDS;

const _: () = assert!(
    PROJECT_VENV_MAKING_BOUND.as_millis()
        + STREAM_FUNCTION_COMPILE_BOUND.as_millis()
        + PROCESSOR_INTERPRETER_DESCRIBE_BOUND.as_millis()
        < STREAM_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a load before the runtime's venv, compile and describe bounds end"
);
const _: () = assert!(
    LONGEST_START_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS
        < STREAM_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a start queued behind one stream action before the runtime answers"
);
const _: () = assert!(
    LONGEST_LOAD_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS
        < STREAM_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a load queued behind one stream action before the runtime answers"
);
const _: () = assert!(
    LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_AHEAD_IN_THE_QUEUE_IN_MILLISECONDS
        + ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis()
        < STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a stop, remove or expose queued behind one stream action before the \
     runtime answers"
);

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

    /// Make the project's `.venv` unless its interpreter runs, compile the
    /// request's stream function in it, then record and load the stream under
    /// the id its location — the resolved project directory and the function's
    /// `module:qualname` — computes to, and start it.
    ///
    /// A load is never refused for what the project's code or environment
    /// does. A `.venv` that cannot be made fails the project's records as an
    /// entry that does not import does. A function
    /// that raised while it compiled is recorded failed with its traceback and
    /// no graph; a load or start that is refused leaves the stream recorded
    /// failed with the reason; a named function the entry no longer defines
    /// fails its record. An entry that does not import or defines no
    /// `@stream` fails every record of the project, or with none records the
    /// project itself failed, under
    /// [`StreamLocation::of_a_project_that_gave_no_stream`], until a load
    /// that gives a stream forgets it.
    ///
    /// A load of a location already recorded replaces it under the same id,
    /// keeping its display name unless the request gives one, and the owner's
    /// rulings: the running stream is unloaded once the new record is written,
    /// and never put back. Refused, with nothing changed, for what the caller
    /// gets wrong — a project the compile refuses, a named function no record
    /// holds, an id held by another location — and before the compile on a
    /// runtime given no state directory.
    pub fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        self.run_the_project_reporting_to(request, None)
    }

    /// [`Runner::run_stream`], for the stream action `progress` reports when
    /// one runs it: the project's log entered with the action's id, each step
    /// reported, the venv's sync unbounded, and a refusal written to the
    /// project's log.
    fn run_the_project_reporting_to(
        &self,
        request: RunStreamRequest,
        progress: Option<&StreamActionProgress<'_>>,
    ) -> Result<StreamRunOutcome> {
        let _in_the_projects_log = self.enter_the_log_of_the_project_a_load_takes_in(
            &request.project_directory,
            progress.map(|progress| progress.action_id.as_str()),
        );
        let run_outcome = self.run_the_project_in_its_log(request, progress);
        match (&run_outcome, &_in_the_projects_log) {
            (Ok(run_outcome), Some(_)) => self.log_each_stream_a_run_failed_in_its_projects_log(
                run_outcome,
                progress.map(|progress| progress.action_id.as_str()),
            ),
            (Err(refusal), _) => {
                if let Some(progress) = progress {
                    tracing::warn!("the stream action {} failed: {refusal}", progress.action_id);
                }
            }
            (Ok(_), None) => {}
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
        request: RunStreamRequest,
        progress: Option<&StreamActionProgress<'_>>,
    ) -> Result<StreamRunOutcome> {
        let report_the_step = |step: StreamActionStep| {
            if let Some(progress) = progress {
                progress.is_on(step);
            }
        };
        let RunStreamRequest {
            project_directory,
            stream_function,
            display_name: display_name_the_run_gives,
        } = request;
        if let Some(display_name) = &display_name_the_run_gives {
            refuse_a_display_name_with_nothing_to_show(display_name)?;
        }
        let Some(stream_records) = self.stream_records() else {
            return Err(Error::Configuration(format!(
                "the stream in {} was not run: this runtime records no stream, because its \
                 host gave it no state directory to record them in; run it in `tatolabd`",
                project_directory.display()
            )));
        };
        let lend_directory = self
            .engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "the stream function in {} was not compiled: this runtime was handed no lend \
                     directory, which its host gives with \
                     `set_processor_interpreter_lend_directory` before any load",
                    project_directory.display()
                ))
            })?;
        report_the_step(StreamActionStep::MakingTheVenv);
        let venv_sync_bound = progress.is_none().then_some(PROJECT_VENV_SYNC_BOUND);
        if let Err(venv_not_made) =
            self.make_the_projects_venv_within(&project_directory, venv_sync_bound)
        {
            let failed_because = match venv_not_made {
                ProjectVenvNotMade::Refused(refusal) => return Err(refusal),
                ProjectVenvNotMade::Failed { failed_because } => failed_because,
            };
            let stream_environment = StreamEnvironment {
                project_directory: std::fs::canonicalize(&project_directory)
                    .unwrap_or_else(|_| project_directory.clone()),
                interpreter: the_projects_venv_interpreter(&project_directory),
            };
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            return Ok(StreamRunOutcome {
                streams: self.fail_every_record_of_a_project_that_gave_no_stream(
                    stream_records,
                    &stream_environment,
                    display_name_the_run_gives,
                    failed_because,
                )?,
                project_directory,
                compile_warnings: Vec::new(),
            });
        }
        report_the_step(StreamActionStep::Compiling);
        let StreamFunctionCompileInTheProjectsInterpreterReport {
            what_the_projects_code_gave,
            stream_environment,
            compile_warnings,
        } = compile_the_stream_function_in_the_projects_interpreter(
            &project_directory,
            stream_function.as_deref(),
            lend_directory,
        )?;
        if stream_function.is_none() {
            self.remember_what_the_every_stream_compile_gave(
                &stream_environment.project_directory,
                &what_the_projects_code_gave,
                progress,
            );
        }

        report_the_step(StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let streams = match what_the_projects_code_gave {
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { streams }
                if !streams.is_empty() =>
            {
                let ids_the_project_defines: Vec<String> = streams
                    .iter()
                    .map(|stream| stream.stream_location.stream_id())
                    .collect();
                for stream in &streams {
                    self.refuse_a_run_of_a_location_whose_id_another_holds(
                        &stream.stream_location,
                    )?;
                }
                let mut streams_loaded_or_failed = Vec::with_capacity(streams.len());
                for OneStreamOfTheProjectCompiled {
                    stream_location,
                    description: _,
                    graph_or_why_none,
                } in streams
                {
                    streams_loaded_or_failed.push(self.record_and_load_one_stream_a_run_compiled(
                        stream_records,
                        StreamARunCompiled {
                            stream_location,
                            display_name_the_run_gives: display_name_the_run_gives.clone(),
                            stream_environment: &stream_environment,
                            graph_or_why_none,
                        },
                    )?);
                }
                streams_loaded_or_failed.extend(
                    self.fail_every_record_of_the_project_its_stream_py_no_longer_defines(
                        stream_records,
                        &stream_environment.project_directory,
                        &ids_the_project_defines,
                    )?,
                );
                streams_loaded_or_failed
            }
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { .. } => {
                return Ok(StreamRunOutcome {
                    streams: self.fail_every_record_of_a_project_that_gave_no_stream(
                        stream_records,
                        &stream_environment,
                        display_name_the_run_gives,
                        "its stream.py defines no @stream".to_string(),
                    )?,
                    project_directory,
                    compile_warnings,
                });
            }
            WhatTheProjectsCodeGaveTheCompile::Compiled {
                stream_location,
                graph_json,
            } => vec![self.record_and_load_one_stream_a_run_compiled(
                stream_records,
                StreamARunCompiled {
                    stream_location,
                    display_name_the_run_gives,
                    stream_environment: &stream_environment,
                    graph_or_why_none: Ok(graph_json),
                },
            )?],
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised {
                stream_location,
                failed_because,
            } => vec![self.record_and_load_one_stream_a_run_compiled(
                stream_records,
                StreamARunCompiled {
                    stream_location,
                    display_name_the_run_gives,
                    stream_environment: &stream_environment,
                    graph_or_why_none: Err(failed_because),
                },
            )?],
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined {
                stream_location,
                not_defined_because,
            } => vec![self.fail_the_record_of_a_stream_function_no_longer_defined(
                stream_records,
                stream_location,
                display_name_the_run_gives,
                not_defined_because,
            )?],
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                import_failed_because: failed_because,
            }
            | WhatTheProjectsCodeGaveTheCompile::StreamEntryDefinesNoStream {
                no_stream_defined_because: failed_because,
            } => {
                return Ok(StreamRunOutcome {
                    streams: self.fail_every_record_of_a_project_that_gave_no_stream(
                        stream_records,
                        &stream_environment,
                        display_name_the_run_gives,
                        failed_because,
                    )?,
                    project_directory,
                    compile_warnings,
                });
            }
        };
        self.forget_the_record_of_a_project_that_gives_streams_again(
            stream_records,
            &stream_environment.project_directory,
        );
        Ok(StreamRunOutcome {
            project_directory,
            streams,
            compile_warnings,
        })
    }

    /// Unload the stream whose id is `stream_id`; a recorded one is recorded
    /// stopped first, so a restart leaves it unloaded. A loaded recorded
    /// stream is unloaded even when its record cannot be read or written, and
    /// the outcome says why it was not recorded stopped. Refused — with nothing
    /// changed — naming the streams the runtime holds when it holds none of
    /// that id, and for a stream already stopped or failed.
    pub fn stop_stream(&self, stream_id: &str) -> Result<StreamStopOutcome> {
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
                "the stream `{stream_id}` is failed, so it is not loaded to stop: {}. `start` \
                 retries it, `rm` forgets it",
                record.failed_because.as_deref().unwrap_or_default()
            ))),
            (None, Some(record)) if record.stopped => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already stopped; `start` loads it again"
            ))),
            (None, Some(mut record)) => {
                record.stopped = true;
                self.write_a_stream_record(&record)?;
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    stop_not_recorded_because: None,
                })
            }
            (Some(loaded), Some(mut record)) => {
                record.stopped = true;
                let recorded_stopped = self.write_a_stream_record(&record);
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

    /// Make the recorded stream's project `.venv` unless its interpreter runs,
    /// then load and start the recorded stream whose id is `stream_id` from its
    /// record, the owner's rulings applied, and record it neither stopped nor
    /// failed, its crash count reset. A `.venv` that cannot be made records it
    /// failed for why, and refuses the start. A record holding no graph — its compile
    /// failed — is compiled from its location as [`Self::run_stream`] compiles
    /// it, and the project's own `<folder>-<hash>` record is a run of the
    /// project. Refused for a stream already loaded, an id no record holds,
    /// and a record an earlier runtime wrote. A failed stream whose retry is
    /// refused stays failed for the retry's reason, its crash count reset — or
    /// as it was, when the refusal was the engine's own; one whose retry
    /// crashes the runtime stays failed as it was.
    pub fn start_stream(&self, stream_id: &str) -> Result<StreamStartOutcome> {
        let _in_the_projects_log = self
            .current_stream_record_of_the_id(stream_id)
            .ok()
            .flatten()
            .and_then(|record| {
                self.enter_the_log_of_the_project_a_load_takes_in(
                    &record.location.project_directory,
                    None,
                )
            });
        self.make_the_venv_of_a_recorded_stream_holding_a_graph_before_its_start(stream_id)?;
        let one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
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
        let Some(mut record) = record else {
            return Err(Error::NotFound(format!(
                "no stream with the id `{stream_id}` is recorded in this runtime, so there is \
                 none to start. Recorded: {}",
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        };
        let Some(graph_json) = record.graph.as_ref() else {
            drop(one_stream_action_at_a_time);
            let compiled_again =
                self.compile_and_run_a_recorded_stream_from_its_location(&record)?;
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
                    &mut record,
                    format!("`start` retried it, and it did not load: {retry_refusal}"),
                );
                return Err(retry_refusal);
            }
            Err(start_refusal) => return Err(start_refusal),
        };
        if record.mark_running_resetting_its_crash_count()
            && let Err(write_refusal) = self.write_a_stream_record(&record)
        {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(write_refusal);
        }
        Ok(StreamStartOutcome {
            node_count: stream.node_count(),
            stream_id: stream_id.to_string(),
        })
    }

    /// Forget the record of the stream whose id is `stream_id` — one an earlier
    /// runtime wrote included — then unload it if it is loaded. Refused when the
    /// runtime holds no stream of that id.
    pub fn remove_stream(&self, stream_id: &str) -> Result<StreamRemoveOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
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
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        let record = self
            .current_stream_record_of_the_id(stream_id)
            .map_err(|unreadable| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` was not renamed: {unreadable}"
                ))
            })?;
        if record.is_none() && loaded.is_none() {
            return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
        }
        if let Some(mut record) = record {
            record.display_name = display_name.to_string();
            self.write_a_stream_record(&record)?;
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
                state: StreamListingState::Running,
                node_count: Some(stream.node_count()),
                failed_because: None,
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
                        stream_action_under_way: None,
                    },
                    Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => StreamListing {
                        state: StreamListingState::Failed,
                        failed_because: Some(earlier.failed_because()),
                        display_name: earlier.stream_id.clone(),
                        stream_id: earlier.stream_id,
                        stream_location: None,
                        node_count: None,
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

    /// Put output port `port` of node `node` in the stream whose id is
    /// `stream_id` at `level`: live on a loaded stream, where the runtime's
    /// publisher withdraws the port's track from each relay the level no
    /// longer allows, and recorded as the owner's ruling on the stream's
    /// record. On a loaded recorded stream a restriction is recorded before it
    /// changes live, and refused with the live level unchanged when its record
    /// cannot be written, so a restart never re-opens a port the owner closed;
    /// a promotion changes live first, and the outcome says why it was not
    /// recorded when it was not. A stopped stream's ruling is recorded once the
    /// recorded graph holds the node. The level of a stream its host process
    /// loaded is never recorded.
    pub fn expose_port(
        &self,
        stream_id: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
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
                let Some(mut record) = self.current_stream_record_of_the_id(stream_id)? else {
                    return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
                };
                refuse_a_node_the_recorded_graph_does_not_hold(&record, node)?;
                record.record_the_owners_exposure_ruling(ruling);
                self.write_a_stream_record(&record)?;
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

    /// Load and start every recorded stream neither stopped nor failed, as a
    /// runtime does at its start, reporting each record it tried in file-name
    /// order. A stream that does not re-load is recorded failed for the
    /// reason, unless the refusal was the engine's own — its GPU context — or
    /// a machine shutdown abandoned the re-load; a record an earlier runtime
    /// wrote is skipped as failed; a record that cannot be read is logged and
    /// left in place.
    pub fn reload_every_recorded_stream_neither_stopped_nor_failed(
        &self,
    ) -> Vec<StreamReloadAtTheStart> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        self.reload_each_recorded_stream_listed(stream_records, stream_records.read_every())
    }

    /// Re-load each record of `listed`. The local API serves while this runs,
    /// so each record is read again under the one-action lock and loaded from
    /// that read: a stream stopped or removed since the listing stays
    /// unloaded, one loaded since is left as it is, and the owner's rulings
    /// recorded since hold from its load. A record holding no graph is
    /// compiled from its location as `start_stream` compiles it.
    fn reload_each_recorded_stream_listed(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        listed: Vec<std::result::Result<StreamRecordAsRead, StreamRecordReadFailure>>,
    ) -> Vec<StreamReloadAtTheStart> {
        let mut reloads = Vec::new();
        for listed_record in listed {
            let stream_id = match listed_record {
                Ok(listed_record) => listed_record.stream_id().to_string(),
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
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
                     {failed_because}; `tatolab start` retries it"
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
            let Some(graph_json) = record.graph.as_ref() else {
                drop(one_stream_action_at_a_time);
                reloads
                    .push(self.reload_a_recorded_stream_holding_no_graph_by_compiling_it(&record));
                continue;
            };
            let reloaded = self
                .make_the_venv_of_a_recorded_stream(&record)
                .and_then(|()| self.load_a_recorded_stream_from_its_record(&record, graph_json));
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
                Err(reload_refusal) if is_the_machines_shutdown_requested() => {
                    tracing::info!(
                        "the stream `{stream_id}` was not re-loaded, because the machine is \
                         shutting every stream down: {reload_refusal}"
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) => {
                    let mut failed_record = record;
                    failed_record.failed_because = Some(format!(
                        "it did not re-load at the runtime's start: {reload_refusal}"
                    ));
                    match stream_records.write(&failed_record) {
                        Ok(()) => tracing::error!(
                            "the stream `{stream_id}` did not re-load, and is failed: \
                             {reload_refusal}; `tatolab start` retries it"
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
            if let Err(not_written) = stream_records.write(&record) {
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
                     `tatolab start` retries it",
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
        self.stream_actions.stream_records.get()
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

    /// Record `ruling` on the record of the loaded stream `stream_id`, refused
    /// naming the record when it is gone.
    fn record_the_owners_ruling_on_a_stream_record(
        &self,
        stream_id: &str,
        ruling: OwnerExposureRuling,
    ) -> Result<()> {
        let Some(mut record) = self.current_stream_record_of_the_id(stream_id)? else {
            let record_path = self
                .stream_records()
                .and_then(|stream_records| stream_records.record_path_of(stream_id))
                .map(|record_path| record_path.display().to_string())
                .unwrap_or_else(|| "in this runtime's state directory".to_string());
            return Err(Error::Runtime(format!(
                "the stream `{stream_id}` is loaded and its record {record_path} is gone, so the \
                 runtime's restart does not load it; `run` records it again"
            )));
        };
        record.record_the_owners_exposure_ruling(ruling);
        self.write_a_stream_record(&record)
    }

    fn write_a_stream_record(&self, record: &StreamRecord) -> Result<()> {
        match self.stream_records() {
            Some(stream_records) => stream_records.write(record),
            None => Err(Error::Configuration(format!(
                "the stream `{}` was not recorded: this runtime records no stream",
                record.stream_id
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

    /// Record the stream a run compiled — failed with no graph when its
    /// function raised — keeping the owner's rulings of the record it
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
    ) -> Result<StreamLoadedOrFailedByARun> {
        let StreamARunCompiled {
            stream_location,
            display_name_the_run_gives,
            stream_environment,
            graph_or_why_none,
        } = compiled;
        let stream_id = stream_location.stream_id();
        let (previous_record, previously_loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?;
        let display_name = display_name_the_run_gives
            .or_else(|| {
                previous_record
                    .as_ref()
                    .map(|previous| previous.display_name.clone())
            })
            .unwrap_or_else(|| match &graph_or_why_none {
                Ok(graph_json) => graph_json
                    .get("stream")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&stream_id)
                    .to_string(),
                Err(_) => stream_location.stream_function_name(),
            });
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
        if let Some(previous_record) = previous_record {
            stream_record.exposure_rulings = previous_record.exposure_rulings;
        }
        let a_new_stream_recorded_once_loaded =
            !replaced_a_recorded_stream && failed_because_its_code_gave_no_graph.is_none();
        if !a_new_stream_recorded_once_loaded {
            stream_records.write(&stream_record)?;
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
                return record_a_refused_load_failed(stream_records, stream_record, load_refusal);
            }
        };
        if record_it_once_loaded && let Err(not_written) = stream_records.write(stream_record) {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(not_written);
        }
        if let Err(start_refusal) = stream.start() {
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

    /// Refuse a run of `stream_location` whose id another location holds, or a
    /// stream loaded without a record holds.
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
        display_name_the_run_gives: Option<String>,
        not_defined_because: String,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let stream_id = stream_location.stream_id();
        let (Some(mut record), loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?
        else {
            return Err(Error::NotFound(not_defined_because));
        };
        if let Some(display_name) = display_name_the_run_gives {
            record.display_name = display_name;
        }
        record.failed_because = Some(not_defined_because.clone());
        stream_records.write(&record)?;
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
    /// function in its `stream.py` the project's every-stream compile no longer
    /// lists, naming the function, and unload each one loaded; never
    /// forgotten.
    fn fail_every_record_of_the_project_its_stream_py_no_longer_defines(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project_directory: &Path,
        ids_the_project_defines: &[String],
    ) -> Result<Vec<StreamLoadedOrFailedByARun>> {
        let mut streams_failed = Vec::new();
        let mut streams_to_unload = Vec::new();
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(mut record)) = read else {
                continue;
            };
            let defined_in_stream_py = record
                .location
                .stream_function
                .split_once(':')
                .is_some_and(|(module, _)| module == STREAM_PY_ENTRY_MODULE_NAME);
            if !the_same_directory(&record.location.project_directory, project_directory)
                || !defined_in_stream_py
                || ids_the_project_defines.contains(&record.stream_id)
            {
                continue;
            }
            let failed_because = format!(
                "`{}` is no longer defined in stream.py",
                record.location.stream_function
            );
            record.failed_because = Some(failed_because.clone());
            stream_records.write(&record)?;
            streams_to_unload.extend(self.loaded_stream_of_the_id_when_loaded(&record.stream_id));
            streams_failed.push(StreamLoadedOrFailedByARun::of_the_record(
                record,
                StreamStateAfterItsRun::Failed { failed_because },
                true,
            ));
        }
        unload_every_stream_an_action_took_back(
            &streams_to_unload,
            "its function is no longer defined in stream.py",
        );
        Ok(streams_failed)
    }

    /// Record failed, with `failed_because`, every stream recorded from the
    /// project that gave no stream — its `.venv` was not made, or its entry did
    /// not import or defines no `@stream` — unloading each one loaded; with none, record the project
    /// itself failed under
    /// [`StreamLocation::of_a_project_that_gave_no_stream`], shown under
    /// its folder's name unless the run gives one.
    fn fail_every_record_of_a_project_that_gave_no_stream(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_environment: &StreamEnvironment,
        display_name_the_run_gives: Option<String>,
        failed_because: String,
    ) -> Result<Vec<StreamLoadedOrFailedByARun>> {
        let records_of_the_project: Vec<StreamRecord> = stream_records
            .read_every()
            .into_iter()
            .filter_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.project_directory
                        == stream_environment.project_directory =>
                {
                    Some(record)
                }
                _ => None,
            })
            .collect();
        if records_of_the_project.is_empty() {
            let stream_location = StreamLocation::of_a_project_that_gave_no_stream(
                stream_environment.project_directory.clone(),
            );
            self.refuse_a_run_of_a_location_whose_id_another_holds(&stream_location)?;
            let display_name =
                display_name_the_run_gives.unwrap_or_else(|| stream_location.project_folder_name());
            let record = StreamRecord::of_a_stream_whose_compile_failed(
                stream_location,
                display_name,
                stream_environment,
                failed_because.clone(),
            );
            stream_records.write(&record)?;
            return Ok(vec![StreamLoadedOrFailedByARun::of_the_record(
                record,
                StreamStateAfterItsRun::Failed { failed_because },
                false,
            )]);
        }
        let mut streams_failed = Vec::with_capacity(records_of_the_project.len());
        let mut streams_to_unload = Vec::new();
        for mut record in records_of_the_project {
            record.failed_because = Some(failed_because.clone());
            stream_records.write(&record)?;
            streams_to_unload.extend(self.loaded_stream_of_the_id_when_loaded(&record.stream_id));
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

    /// Enter, on this thread, the log of the project in `project_directory`
    /// for a load of it, so its venv making and compile land there under no
    /// stream; `None` for a directory that is not a project — one that does not
    /// resolve or holds neither `stream.py` nor `pyproject.toml` — whose load
    /// logs to the runtime's own log.
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

    /// Make the `.venv` of the project in `project_directory` with the uv this
    /// runtime was handed, unless its interpreter runs.
    fn make_the_projects_venv(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        self.make_the_projects_venv_within(project_directory, Some(PROJECT_VENV_SYNC_BOUND))
    }

    /// [`Self::make_the_projects_venv`], its sync bounded by `sync_bound`, or
    /// unbounded when it is `None`.
    fn make_the_projects_venv_within(
        &self,
        project_directory: &Path,
        sync_bound: Option<std::time::Duration>,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        make_the_projects_venv_unless_its_interpreter_runs_within(
            project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_unit_uv_executable
                .get(),
            sync_bound,
            &is_the_machines_shutdown_requested,
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

    /// Before `start_stream` takes the one-action lock: make the `.venv` of the
    /// recorded stream `stream_id` when its record holds a graph — a record
    /// holding none is compiled by a load, which makes it — and, when it cannot
    /// be made, record the stream failed for why and refuse the start.
    fn make_the_venv_of_a_recorded_stream_holding_a_graph_before_its_start(
        &self,
        stream_id: &str,
    ) -> Result<()> {
        let Ok(Some(StreamRecordAsRead::Current(record))) = self.stream_record_of_the_id(stream_id)
        else {
            return Ok(());
        };
        if record.graph.is_none() {
            return Ok(());
        }
        let Err(venv_not_made) = self.make_the_venv_of_a_recorded_stream(&record) else {
            return Ok(());
        };
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        if let Ok(Some(mut record)) = self.current_stream_record_of_the_id(stream_id)
            && self
                .loaded_stream_of_the_id_when_loaded(stream_id)
                .is_none()
            && !is_the_machines_shutdown_requested()
        {
            self.record_a_refused_start_failed_resetting_its_crash_count(
                &mut record,
                format!(
                    "`start` found its project's .venv not running, and it was not made: \
                     {venv_not_made}"
                ),
            );
        }
        Err(venv_not_made)
    }

    /// Record `record` failed for `failed_because`, a start of it refused, its
    /// crash count reset; a record that cannot be written is logged.
    fn record_a_refused_start_failed_resetting_its_crash_count(
        &self,
        record: &mut StreamRecord,
        failed_because: String,
    ) {
        record.failed_because = Some(failed_because);
        record.runtime_crashes_in_a_row_implicating_it = 0;
        if let Err(not_written) = self.write_a_stream_record(record) {
            tracing::warn!(
                "the stream `{}` was not started, and its failure was not recorded: {not_written}",
                record.stream_id
            );
        }
    }

    /// Compile `record`'s stream again from its location, as a run of it, and
    /// load it — the project's run, for the record of a project that gave no
    /// stream. That record names no entry, so one written for an entry
    /// other than the project's own `stream.py` is refused, naming it.
    fn compile_and_run_a_recorded_stream_from_its_location(
        &self,
        record: &StreamRecord,
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
        if record.location.is_a_project_that_gave_no_stream()
            && !the_same_directory(&project_directory, &record.location.project_directory)
        {
            return Err(Error::Configuration(format!(
                "the stream `{}` records an entry in {} that gave no stream, and names no file \
                 to compile again; run that entry again with `tatolab run` in {}",
                record.stream_id,
                record.location.project_directory.display(),
                project_directory.display()
            )));
        }
        let run = self.run_stream(RunStreamRequest {
            project_directory,
            stream_function: the_stream_target_that_compiles_a_location_again(&record.location),
            display_name: None,
        })?;
        let mut streams = run.streams.into_iter();
        let the_project_took_in_one_stream =
            record.location.is_a_project_that_gave_no_stream() && streams.len() == 1;
        streams
            .find(|stream| the_project_took_in_one_stream || stream.stream_id == record.stream_id)
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "the stream `{}` was compiled from {}, and its run did not take it in",
                    record.stream_id, record.location
                ))
            })
    }

    /// The re-load at the runtime's start of `record`, which holds no graph:
    /// compiled from its location, then loaded.
    fn reload_a_recorded_stream_holding_no_graph_by_compiling_it(
        &self,
        record: &StreamRecord,
    ) -> StreamReloadAtTheStart {
        let stream_id = record.stream_id.clone();
        let refusal = match self.compile_and_run_a_recorded_stream_from_its_location(record) {
            Ok(StreamLoadedOrFailedByARun {
                stream_id,
                state: StreamStateAfterItsRun::Running { node_count, .. },
                ..
            }) => {
                tracing::info!(
                    "the stream `{stream_id}` was compiled from {} and re-loaded with \
                     {node_count} nodes",
                    record.location
                );
                return StreamReloadAtTheStart::Reloaded { stream_id };
            }
            Ok(StreamLoadedOrFailedByARun {
                state: StreamStateAfterItsRun::Failed { failed_because },
                ..
            }) => Error::Runtime(failed_because),
            Err(refusal) => refusal,
        };
        tracing::error!(
            "the stream `{stream_id}` was compiled from {} and did not re-load: {refusal}",
            record.location
        );
        StreamReloadAtTheStart::NotReloaded { stream_id, refusal }
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

impl Runner {
    /// Queue a load of every `@stream` in the `stream.py` of the project in
    /// `request`'s directory on the project's stream action queue, answering
    /// the action's id at once; [`Runner::describe_stream_action`] follows it.
    /// The load makes the project's `.venv` with no bound of its own, compiles
    /// and loads or fails each stream as [`Runner::run_stream`] does, and
    /// refuses nothing: a directory that does not exist or is no project, and
    /// an id another location holds, fail the action. Refused only for a
    /// directory not named by its absolute path.
    pub fn run_project(&self, request: RunProjectRequest) -> Result<StreamActionQueued> {
        let RunProjectRequest { project_directory } = request;
        if !project_directory.is_absolute() {
            return Err(Error::Configuration(format!(
                "`project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            )));
        }
        let project_directory =
            std::fs::canonicalize(&project_directory).unwrap_or(project_directory);
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
        Ok(queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.load_the_project_for_its_stream_action(&action_id, project_directory);
            }),
            &self.tokio_handle(),
        ))
    }

    /// The stream action `action_id`, refused when the engine never held it or
    /// let it go among its project's older finished actions; its records stay
    /// in the project's log.
    pub fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .held_stream_action(action_id)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "the stream action `{action_id}` is not held: this runtime never queued it, or it is \
                     older than the last {FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT} finished \
                     actions of its project; its records stay in the project's log"
                ))
            })
    }

    /// The streams of the project in `project_directory` as its last
    /// every-stream compile gave them, and the action preparing it when one is
    /// queued or running; refused for a project with neither. Runs no project
    /// code.
    pub fn list_project_streams(&self, project_directory: &Path) -> Result<ProjectStreamsListing> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let stream_action_under_way = queue_of_each_project
            .actions_under_way()
            .into_iter()
            .find(|action| action.project_directory == project_directory)
            .map(|action| StreamActionWorkingOnAStream {
                action_id: action.action_id,
                step: action.step,
            });
        let what_the_last_compile_gave =
            queue_of_each_project.the_last_compile_of(&project_directory);
        if stream_action_under_way.is_none() && what_the_last_compile_gave.is_none() {
            return Err(Error::Configuration(format!(
                "the project in {} is not loaded: `run_project` loads it",
                project_directory.display()
            )));
        }
        Ok(ProjectStreamsListing {
            project_directory,
            stream_action_under_way,
            what_the_last_compile_gave,
        })
    }

    /// Run the load the stream action `action_id` queued of the project in
    /// `project_directory`, reporting its steps, and finish the action done or
    /// failed.
    fn load_the_project_for_its_stream_action(&self, action_id: &str, project_directory: PathBuf) {
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        queue_of_each_project.change_the_held_stream_action(action_id, |action| {
            action.state = StreamActionState::InProgress;
        });
        let run_outcome = match why_a_directory_is_no_project(&project_directory) {
            Some(why_it_is_no_project) => {
                tracing::warn!("the stream action {action_id} failed: {why_it_is_no_project}");
                Err(why_it_is_no_project)
            }
            None => self
                .run_the_project_reporting_to(
                    RunStreamRequest {
                        project_directory,
                        stream_function: None,
                        display_name: None,
                    },
                    Some(&StreamActionProgress {
                        queue_of_each_project,
                        action_id: action_id.to_string(),
                    }),
                )
                .map_err(|refusal| refusal.to_string()),
        };
        queue_of_each_project.change_the_held_stream_action(action_id, |action| {
            action.step = None;
            match run_outcome {
                Ok(run_outcome) => {
                    action.state = StreamActionState::Done;
                    action.streams = run_outcome
                        .streams
                        .into_iter()
                        .map(|stream| StreamOfAStreamAction {
                            state: Some(match stream.state {
                                StreamStateAfterItsRun::Running { .. } => {
                                    StreamListingState::Running
                                }
                                StreamStateAfterItsRun::Failed { .. } => StreamListingState::Failed,
                            }),
                            stream_id: stream.stream_id,
                            display_name: stream.display_name,
                            stream_location: stream.stream_location,
                        })
                        .collect();
                }
                Err(failed_because) => {
                    action.state = StreamActionState::Failed;
                    action.failed_because = Some(failed_because);
                }
            }
        });
    }

    /// Remember what the every-stream compile of the project in
    /// `project_directory` gave, and hand the streams it named to the action
    /// `progress` reports for, so a listing shows them preparing.
    fn remember_what_the_every_stream_compile_gave(
        &self,
        project_directory: &Path,
        what_the_projects_code_gave: &WhatTheProjectsCodeGaveTheCompile,
        progress: Option<&StreamActionProgress<'_>>,
    ) {
        let what_it_gave = match what_the_projects_code_gave {
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { streams }
                if !streams.is_empty() =>
            {
                WhatTheLastCompileOfTheProjectGave::Streams(
                    streams
                        .iter()
                        .map(|stream| StreamTheLastCompileOfTheProjectNamed {
                            stream_location: stream.stream_location.clone(),
                            description: stream.description.clone(),
                        })
                        .collect(),
                )
            }
            WhatTheProjectsCodeGaveTheCompile::EveryStreamOfTheProject { .. } => {
                WhatTheLastCompileOfTheProjectGave::EntryDefinesNoStream(
                    "its stream.py defines no @stream".to_string(),
                )
            }
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                import_failed_because,
            } => {
                WhatTheLastCompileOfTheProjectGave::EntryDidNotImport(import_failed_because.clone())
            }
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDefinesNoStream {
                no_stream_defined_because,
            } => WhatTheLastCompileOfTheProjectGave::EntryDefinesNoStream(
                no_stream_defined_because.clone(),
            ),
            WhatTheProjectsCodeGaveTheCompile::Compiled { .. }
            | WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised { .. }
            | WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined { .. } => return,
        };
        if let (Some(progress), WhatTheLastCompileOfTheProjectGave::Streams(streams)) =
            (progress, &what_it_gave)
        {
            progress.works_on(
                streams
                    .iter()
                    .map(|stream| StreamOfAStreamAction {
                        stream_id: stream.stream_location.stream_id(),
                        display_name: self
                            .current_stream_record_of_the_id(&stream.stream_location.stream_id())
                            .ok()
                            .flatten()
                            .map_or_else(
                                || stream.stream_location.stream_function_name(),
                                |record| record.display_name,
                            ),
                        stream_location: stream.stream_location.clone(),
                        state: None,
                    })
                    .collect(),
            );
        }
        self.stream_actions
            .stream_action_queue_of_each_project
            .remember_the_last_compile_of(project_directory, what_it_gave);
    }

    /// Show each stream an action under way works on `preparing`, with the
    /// action and its step: the streams its compile named, else the project's
    /// recorded ones, else the project itself under
    /// [`StreamLocation::of_a_project_that_gave_no_stream`].
    fn show_the_streams_actions_under_way_prepare(&self, listings: &mut Vec<StreamListing>) {
        let mut projects_an_earlier_action_works_on: Vec<PathBuf> = Vec::new();
        for action in self
            .stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
        {
            if projects_an_earlier_action_works_on
                .iter()
                .any(|project| the_same_directory(project, &action.project_directory))
            {
                continue;
            }
            projects_an_earlier_action_works_on.push(action.project_directory.clone());
            let working_on_it = StreamActionWorkingOnAStream {
                action_id: action.action_id.clone(),
                step: action.step,
            };
            let streams_it_works_on: Vec<(String, String, StreamLocation)> = if action
                .streams
                .is_empty()
            {
                let recorded_of_the_project: Vec<(String, String, StreamLocation)> = listings
                    .iter()
                    .filter_map(|listing| {
                        let location = listing.stream_location.as_ref()?;
                        the_same_directory(&location.project_directory, &action.project_directory)
                            .then(|| {
                                (
                                    listing.stream_id.clone(),
                                    listing.display_name.clone(),
                                    location.clone(),
                                )
                            })
                    })
                    .collect();
                if recorded_of_the_project.is_empty() {
                    let project = StreamLocation::of_a_project_that_gave_no_stream(
                        action.project_directory.clone(),
                    );
                    vec![(project.stream_id(), project.project_folder_name(), project)]
                } else {
                    recorded_of_the_project
                }
            } else {
                action
                    .streams
                    .iter()
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
                match listings
                    .iter_mut()
                    .find(|listing| listing.stream_id == stream_id)
                {
                    Some(listing) => {
                        listing.state = StreamListingState::Preparing;
                        listing.stream_action_under_way = Some(working_on_it.clone());
                    }
                    None => listings.push(StreamListing {
                        stream_id,
                        display_name,
                        stream_location: Some(stream_location),
                        state: StreamListingState::Preparing,
                        node_count: None,
                        failed_because: None,
                        stream_action_under_way: Some(working_on_it.clone()),
                    }),
                }
            }
        }
    }
}

/// Why `project_directory` is no project a load takes in — it does not
/// exist, or holds neither `stream.py` nor `pyproject.toml` — or `None` when it
/// is one.
fn why_a_directory_is_no_project(project_directory: &Path) -> Option<String> {
    if !project_directory.is_dir() {
        return Some(format!(
            "the project directory {} does not exist",
            project_directory.display()
        ));
    }
    if !project_directory.join("stream.py").is_file()
        && !project_directory.join("pyproject.toml").is_file()
    {
        return Some(format!(
            "{} is not a project: it holds neither stream.py nor pyproject.toml",
            project_directory.display()
        ));
    }
    None
}

/// How a run refused for an id a stream another caller loaded holds is told to
/// go on.
const STOP_IT_FIRST_TO_LOAD_THIS_ONE: &str = "stop it first to load this one";

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
             stream function, or `rm` the stream that holds it"
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

/// The stream function a run compiled, its location and display name, and the
/// graph it compiled to or why its code gave none.
struct StreamARunCompiled<'run> {
    stream_location: StreamLocation,
    display_name_the_run_gives: Option<String>,
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

/// The module a project's `stream.py` imports as, the module part of every
/// stream function it defines.
const STREAM_PY_ENTRY_MODULE_NAME: &str = "stream";

/// Whether `first` and `second` name one directory, symlinks resolved; spelled
/// alike when either cannot be resolved.
fn the_same_directory(first: &Path, second: &Path) -> bool {
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => first == second,
    }
}

/// The target that compiles the stream at `location` again: none for a
/// project that gave no stream; the module's file and the function when
/// the module is one file of the location's project directory — the location
/// an entry file compiles to; the `module:qualname` otherwise.
fn the_stream_target_that_compiles_a_location_again(location: &StreamLocation) -> Option<String> {
    if location.is_a_project_that_gave_no_stream() {
        return None;
    }
    if let Some((module, qualname)) = location.stream_function.split_once(':')
        && !module.contains('.')
    {
        let module_file = location.project_directory.join(format!("{module}.py"));
        if module_file.is_file() {
            return Some(format!("{}:{qualname}", module_file.display()));
        }
    }
    Some(location.stream_function.clone())
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
    if let Err(not_written) = stream_records.write(stream_record) {
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
        StreamListingState::Running | StreamListingState::Preparing => "",
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

    /// What a compile of every stream answers where a compile of one stream
    /// function answers `document`: a graph or a traceback as the one entry
    /// of `streams`; any other answer as it is.
    fn the_every_stream_answer_of(document: &serde_json::Value) -> serde_json::Value {
        let Some(stream_function) = document.get("stream_function") else {
            return document.clone();
        };
        let mut one_stream = serde_json::json!({
            "stream_function": stream_function,
            "description": null,
        });
        for answer_key in ["stream_graph", "failed_because"] {
            if let Some(answer) = document.get(answer_key) {
                one_stream[answer_key] = answer.clone();
            }
        }
        serde_json::json!({
            "project_directory": document["project_directory"],
            "streams": [one_stream],
        })
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
                    "stream_graph": graph,
                    "project_directory": self.path(),
                    "stream_function": format!("stream:{function_name}"),
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
                    "stream_function": format!("stream:{function_name}"),
                    "failed_because": traceback,
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

        /// From now on, the compile answers every `@stream` of `stream.py`:
        /// each of `functions_compiled` to its graph, each of
        /// `functions_raising` raising its name.
        fn compile_every_stream_to(&self, functions_compiled: &[&str], functions_raising: &[&str]) {
            let streams: Vec<serde_json::Value> = functions_compiled
                .iter()
                .map(|function_name| {
                    serde_json::json!({
                        "stream_function": format!("stream:{function_name}"),
                        "description": null,
                        "stream_graph": the_graph_of_a_function_named(
                            function_name,
                            serde_json::json!([]),
                        ),
                    })
                })
                .chain(functions_raising.iter().map(|function_name| {
                    serde_json::json!({
                        "stream_function": format!("stream:{function_name}"),
                        "description": null,
                        "failed_because": format!("ValueError: {function_name}"),
                    })
                }))
                .collect();
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "streams": streams,
                }),
                &[],
            );
        }

        /// From now on, the compile answers that `stream.py` imported and
        /// defines no `@stream`, printing `no_stream_defined_because`.
        fn define_no_stream_printing(&self, no_stream_defined_because: &str) {
            self.print_the_compile_document_warning(
                serde_json::json!({
                    "project_directory": self.path(),
                    "no_stream_defined_because": no_stream_defined_because,
                }),
                &[],
            );
        }

        /// From now on, the compile writes its arguments to
        /// [`Self::the_last_compiles_arguments`], each of `warnings` on a line
        /// of its standard error, then prints `document` — to a compile of
        /// every stream, a document answering one stream function as the one
        /// entry of its `streams`.
        fn print_the_compile_document_warning(
            &self,
            document: serde_json::Value,
            warnings: &[&str],
        ) {
            let warnings_written: String = warnings
                .iter()
                .map(|warning| format!("echo '{warning}' >&2\n"))
                .collect();
            let every_stream_document = the_every_stream_answer_of(&document);
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                &format!(
                    "{STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE}printf '%s\\n' \"$@\" > \
                     compile-arguments\n{warnings_written}case \" $* \" in\n*' --every-stream \
                     '*) cat <<'COMPILED'\n{every_stream_document}\nCOMPILED\n;;\n*) cat \
                     <<'COMPILED'\n{document}\nCOMPILED\n;;\nesac\n"
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

        /// Where this project is recorded while it gives no stream.
        fn location_of_the_project(&self) -> StreamLocation {
            StreamLocation::of_a_project_that_gave_no_stream(self.path().to_path_buf())
        }

        fn run_request(&self) -> RunStreamRequest {
            RunStreamRequest {
                project_directory: self.path().to_path_buf(),
                stream_function: None,
                display_name: None,
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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = refusal_of(runner.run_stream(project.run_request()));

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

        let refusal = refusal_of(runner.run_stream(project.run_request()));

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

        let refusal = refusal_of(runner.run_stream(project.run_request()));

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

            let refusal = refusal_of(runner.run_stream(project.run_request()));

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
                let refusal = refusal_of(runner.remove_stream("nothing-here"));
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

        let refusal = refusal_of(runner.run_stream(project.run_request()));

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

        let run = runner
            .run_stream(project.run_request())
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

        let run = runner
            .run_stream(project.run_request())
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

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let run = runner
            .run_stream(project.run_request())
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
        let imported_again = runner.run_stream(project.run_request()).unwrap();

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
    fn a_stream_py_defining_no_stream_is_a_failed_project_until_a_load_defines_one() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let project_stream_id = project.location_of_the_project().stream_id();
        let no_stream_defined = "`stream.py` defines no @stream function\n    @stream";
        project.define_no_stream_printing(no_stream_defined);

        let run = runner
            .run_stream(project.run_request())
            .expect("a stream.py defining no @stream is taken in failed");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert_eq!(failed_because, no_stream_defined);
        assert_eq!(stream.stream_id, project_stream_id);
        assert_eq!(
            the_record_of(state_directory.path(), &project_stream_id)
                .failed_because
                .as_deref(),
            Some(no_stream_defined)
        );

        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let defined_again = runner.run_stream(project.run_request()).unwrap();
        assert_eq!(
            defined_again
                .streams
                .iter()
                .map(|stream| stream.stream_id.as_str())
                .collect::<Vec<_>>(),
            [project.stream_id_of("camera").as_str()]
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &project_stream_id),
            None,
            "a load that defines a stream forgets the project's record"
        );

        project.define_no_stream_printing(no_stream_defined);
        let emptied = runner.run_stream(project.run_request()).unwrap();
        let (stream, failed_because) = the_one_failed_stream_of(&emptied);
        assert_eq!(stream.stream_id, project.stream_id_of("camera"));
        assert_eq!(failed_because, no_stream_defined);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_of_every_stream_one_of_whose_ids_the_host_holds_is_refused_loading_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let hosted = a_stream_its_host_loaded(&runner, &project, "preview");
        project.compile_every_stream_to(&["camera", "preview"], &[]);

        let refusal = refusal_of(runner.run_stream(project.run_request()));

        assert!(
            refusal.contains(&project.stream_id_of("preview")),
            "{refusal}"
        );
        assert_eq!(runner.ids_of_the_loaded_streams(), [hosted.stream_id()]);
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_takes_in_every_stream_of_stream_py_and_fails_one_whose_function_is_no_longer_defined()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        project.compile_every_stream_to(&["camera", "preview"], &["broken"]);

        let run = runner
            .run_stream(project.run_request())
            .expect("every stream of stream.py is taken in");

        let states: Vec<(&str, Option<&str>)> = run
            .streams
            .iter()
            .map(|stream| {
                (
                    stream.stream_id.as_str(),
                    match &stream.state {
                        StreamStateAfterItsRun::Running { .. } => None,
                        StreamStateAfterItsRun::Failed { failed_because } => {
                            Some(failed_because.as_str())
                        }
                    },
                )
            })
            .collect();
        assert_eq!(
            states,
            [
                (project.stream_id_of("camera").as_str(), None),
                (project.stream_id_of("preview").as_str(), None),
                (
                    project.stream_id_of("broken").as_str(),
                    Some("ValueError: broken")
                ),
            ]
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

        project.compile_every_stream_to(&["camera"], &["broken"]);
        let rerun = runner.run_stream(project.run_request()).unwrap();

        let preview_failed = rerun
            .streams
            .iter()
            .find(|stream| stream.stream_id == project.stream_id_of("preview"))
            .expect("the vanished stream is answered");
        assert_eq!(
            preview_failed.state,
            StreamStateAfterItsRun::Failed {
                failed_because: "`stream:preview` is no longer defined in stream.py".to_string()
            }
        );
        assert_eq!(
            the_record_of(state_directory.path(), &project.stream_id_of("preview"))
                .failed_because
                .as_deref(),
            Some("`stream:preview` is no longer defined in stream.py")
        );
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            [project.stream_id_of("camera")]
        );
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

        let run = runner.run_stream(project.run_request()).unwrap();

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
    fn a_named_function_no_longer_defined_fails_its_record_and_one_no_record_holds_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera", serde_json::json!([])),
        );
        project.define_no_function_named("camera");

        let run = runner
            .run_stream(RunStreamRequest {
                stream_function: Some("stream.py:camera".to_string()),
                ..project.run_request()
            })
            .expect("a recorded function no longer defined is a failed stream");

        let (stream, failed_because) = the_one_failed_stream_of(&run);
        assert_eq!(stream.stream_id, camera);
        assert!(
            failed_because.contains("defines no @stream function named `camera`"),
            "{failed_because}"
        );
        assert!(running.has_ended());
        assert_eq!(
            the_record_of(state_directory.path(), &camera)
                .failed_because
                .as_deref(),
            Some(failed_because)
        );

        project.define_no_function_named("side");
        let records_before = records_in(state_directory.path()).read_every();
        let refusal = refusal_of(runner.run_stream(RunStreamRequest {
            stream_function: Some("stream.py:side".to_string()),
            ..project.run_request()
        }));

        assert!(
            refusal.contains("defines no @stream function named `side`"),
            "{refusal}"
        );
        assert_eq!(
            records_in(state_directory.path()).read_every(),
            records_before,
            "a function nothing defines nor records is refused with nothing changed"
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
        runner.run_stream(project.run_request()).unwrap();
        project.raise_in_the_function_printing("camera", "ValueError: still bad");

        let refusal = refusal_of(runner.start_stream(&camera));

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
        refusal_of(runner.start_stream(&camera));
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
        runner
            .run_stream(RunStreamRequest {
                stream_function: Some("stream.py:zzz".to_string()),
                ..project.run_request()
            })
            .unwrap();
        let zzz = project.stream_id_of("zzz");
        assert!(
            project.stream_id_of("aaa") < zzz,
            "the record started sorts second"
        );
        project.fail_to_import_printing("SyntaxError: invalid syntax");

        let refusal = refusal_of(runner.start_stream(&zzz));

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
        runner.run_stream(project.run_request()).unwrap();
        project.raise_in_the_function_printing("camera", "ValueError: bad wiring");

        let refusal = refusal_of(runner.start_stream(&project_stream_id));

        assert!(refusal.contains("bad wiring"), "{refusal}");
        assert!(
            refusal.contains(&format!("`{}`", project.stream_id_of("camera"))),
            "{refusal}"
        );
        assert_eq!(
            project.the_last_compiles_arguments(),
            "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\n--every-stream\n",
            "the project's run compiles every stream"
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

        let refusal = refusal_of(runner.start_stream(&record_of_rigs.stream_id));

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

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::NotReloaded { stream_id, refusal }]
                    if *stream_id == camera && refusal.to_string().contains("bad wiring")
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
                .stop_stream(&camera)
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
            let refusal = refusal_of(runner.stop_stream(&camera));
            assert!(refusal.contains("already stopped"), "{refusal}");
        }

        let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

        assert!(
            fresh_runner
                .reload_every_recorded_stream_neither_stopped_nor_failed()
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
                    .stop_stream(&camera)
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

                let refusal = refusal_of(runner.stop_stream(&camera));
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
            runner.stop_stream(&camera)
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
            let refusal = refusal_of(runner.start_stream(&camera));

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
            .stop_stream(&project.stream_id_of("camera"))
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
            refusal_of(runner.stop_stream("absent")),
            refusal_of(runner.remove_stream("absent")),
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
        let start_refusal = refusal_of(runner.start_stream(&hosted_one));
        assert!(
            start_refusal.contains("living with its host process"),
            "{start_refusal}"
        );
        let start_refusal = refusal_of(runner.start_stream("absent"));
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
            let refusal = refusal_of(runner.stop_stream(&spelled_otherwise));

            assert!(refusal.contains("no stream with the id"), "{refusal}");
        }
        for spelled_otherwise in [microphone.to_uppercase(), "microphone".to_string()] {
            let refusal = refusal_of(runner.start_stream(&spelled_otherwise));

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
        let project = ProjectWithAStubCompile::with_no_venv();
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

        assert_eq!(
            runner.remove_stream(&camera).unwrap(),
            StreamRemoveOutcome {
                stream_id: camera.clone(),
                unloaded: true,
                forgotten: true,
            }
        );
        assert_eq!(
            runner.remove_stream(&microphone).unwrap(),
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
    fn a_run_giving_a_display_name_that_shows_nothing_is_refused_before_its_compile() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_record_of(&project, "camera", serde_json::json!([]));
        let running =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        for showing_nothing in ["", "  "] {
            let refusal = refusal_of(runner.run_stream(RunStreamRequest {
                display_name: Some(showing_nothing.to_string()),
                ..project.run_request()
            }));

            assert!(refusal.contains("shows nothing"), "{refusal}");
            assert!(
                !refusal.contains("carries no uv"),
                "refused before its venv is made, which this runtime cannot: {refusal}"
            );
        }
        assert!(!running.has_ended());
        assert_eq!(running.display_name(), "camera");
        assert_eq!(the_record_of(state_directory.path(), &camera), record);
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
        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();
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
            refusal_of(runner.start_stream("main")),
            refusal_of(runner.stop_stream("main")),
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
            runner.remove_stream("main").unwrap(),
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

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

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
        let listed = records.read_every();
        let mut stopped_since =
            the_record_of(state_directory.path(), &project.stream_id_of("a-stopped"));
        stopped_since.stopped = true;
        records.write(&stopped_since).unwrap();
        assert!(records.remove(&project.stream_id_of("b-removed")).unwrap());

        let reloads = runner.reload_each_recorded_stream_listed(&records, listed);

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
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        records
            .write(&a_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let listed = records.read_every();
        let record_path_malformed_since_the_listing = records
            .record_path_of(&project.stream_id_of("camera"))
            .unwrap();
        std::fs::write(&record_path_malformed_since_the_listing, b"{").unwrap();

        let reloads = runner.reload_each_recorded_stream_listed(&records, listed);

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
        let project = ProjectWithAStubCompile::with_no_venv();
        let record = a_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();
        let listed = records_in(state_directory.path()).read_every();
        let loaded_since =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let reloads =
            runner.reload_each_recorded_stream_listed(&records_in(state_directory.path()), listed);

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
    fn a_run_of_another_function_replaces_no_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("studio"),
        );
        let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
        let record = a_record_of(&project, "recorded-camera", serde_json::json!([]));
        let recorded =
            a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let run_of_studio = RunStreamRequest {
            stream_function: Some("stream.py:studio".to_string()),
            ..project.run_request()
        };
        for (run_request, what_the_run_is) in [
            (run_of_studio.clone(), "a run of another function"),
            (
                RunStreamRequest {
                    display_name: Some("recorded-camera".to_string()),
                    ..run_of_studio.clone()
                },
                "a run giving the display name of a recorded stream",
            ),
        ] {
            let run = runner.run_stream(run_request).unwrap();

            let (stream, failed_because) = the_one_failed_stream_of(&run);
            assert!(
                failed_because.contains("absent-port"),
                "{what_the_run_is}: {failed_because}"
            );
            assert_eq!(
                stream.stream_id,
                project.stream_id_of("studio"),
                "{what_the_run_is}"
            );
            assert!(!hosted.has_ended(), "{what_the_run_is}");
            assert!(!recorded.has_ended(), "{what_the_run_is}");
        }
        let mut expected_ids = vec![
            project.stream_id_of("camera"),
            project.stream_id_of("recorded-camera"),
        ];
        expected_ids.sort();
        assert_eq!(runner.ids_of_the_loaded_streams(), expected_ids);
    }

    // The tests below start streams, which needs the engine's GPU context.

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
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

        let first_run = runner
            .run_stream(main_worktree.run_request())
            .expect("the first worktree's stream runs");
        let second_run = runner
            .run_stream(second_worktree.run_request())
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
            .run_stream(main_worktree.run_request())
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

        let camera_outcome = runner
            .run_stream(camera_project.run_request())
            .expect("the camera stream runs");
        let preview_outcome = runner
            .run_stream(preview_project.run_request())
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
        assert_eq!(camera_outcome.compile_warnings, Vec::<String>::new());
        assert_eq!(camera_outcome.project_directory, camera_project.path());
        assert_eq!(preview_run.stream_id, preview);
        assert_eq!(preview_run.display_name, "preview");
        assert_eq!(
            preview_outcome.compile_warnings,
            ["tatolab: the cross-floor check found 1 thing binding this app to one floor."]
        );
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
                stream_action_under_way: None,
            },
            StreamListing {
                stream_id: preview.clone(),
                display_name: "preview".to_string(),
                stream_location: Some(preview_project.location_of("preview")),
                state: StreamListingState::Running,
                node_count: Some(1),
                failed_because: None,
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
    fn a_run_of_the_records_location_replaces_it_under_its_id_keeping_its_renamed_display_name_and_rulings()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first_run = runner.run_stream(project.run_request()).unwrap();
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

        let replaced_run = runner
            .run_stream(project.run_request())
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
        assert_eq!(
            replaced_run.compile_warnings,
            ["a warning of the replacing compile"]
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

        let renamed_run = runner
            .run_stream(RunStreamRequest {
                display_name: Some("Desk camera".to_string()),
                ..project.run_request()
            })
            .expect("the recorded stream is replaced again");
        let (renamed, _, _) = the_one_running_stream_of(&renamed_run);

        assert_eq!(renamed.stream_id, camera);
        assert_eq!(renamed.display_name, "Desk camera");
        assert_eq!(
            the_record_of(state_directory.path(), &camera).display_name,
            "Desk camera"
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
            runner.run_stream(project.run_request()).unwrap();
            runner
                .expose_port(&camera, "source", "out1", OutputPortExposureLevel::Private)
                .unwrap();
            runner.stop_stream(&camera).unwrap();

            let started = runner
                .start_stream(&camera)
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
            let refusal = refusal_of(runner.start_stream(&camera));
            assert!(refusal.contains("already loaded"), "{refusal}");
        }

        let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let reloads = restarted.reload_every_recorded_stream_neither_stopped_nor_failed();

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
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
    fn an_owners_restriction_recorded_between_the_listing_and_the_reload_holds_at_the_load() {
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
        let listed = records.read_every();
        let mut restricted_since = the_record_of(state_directory.path(), &camera);
        restricted_since.record_the_owners_exposure_ruling(OwnerExposureRuling {
            node: "source".to_string(),
            port: "out1".to_string(),
            level: OutputPortExposureLevel::Internal,
        });
        records.write(&restricted_since).unwrap();

        let reloads = runner.reload_each_recorded_stream_listed(&records, listed);

        assert!(
            matches!(
                reloads.as_slice(),
                [StreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
            ),
            "{reloads:?}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&runner.loaded_stream_of_the_id(&camera).unwrap()),
            serde_json::json!([]),
            "the restriction recorded after the listing holds"
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

        let run = runner
            .run_stream(project.run_request())
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
            refusal_of(runner.run_stream(project.run_request()))
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
        runner.run_stream(project.run_request()).unwrap();
        project.compile_to(the_graph_of_a_function_named("main", serde_json::json!([])));

        let run = runner.run_stream(project.run_request()).unwrap();

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
        runner.run_stream(project.run_request()).unwrap();
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let started = runner
            .start_stream(&camera)
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
        let first_run = runner.run_stream(project.run_request()).unwrap();
        let (_, first_stream_tag, _) = the_one_running_stream_of(&first_run);
        let previous_record = the_record_of(&streams_directory, &camera);
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}]),
        ));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(runner.run_stream(project.run_request()))
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
        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

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
        let refusal = refusal_of(runner.stop_stream(&crasher));
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

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

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
                .reload_every_recorded_stream_neither_stopped_nor_failed()
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

        let refusal = refusal_of(runner.start_stream(&crasher));

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

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let run = runner.run_stream(project.run_request()).unwrap();

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
    fn a_run_whose_venv_uv_cannot_make_writes_uvs_lines_to_the_projects_log_under_no_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        runner.run_stream(project.run_request()).unwrap();

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
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        runner.run_stream(project.run_request()).unwrap();
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let run = runner.run_stream(project.run_request()).unwrap();

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

        let refusal = refusal_of(runner.run_stream(RunStreamRequest {
            project_directory: project.path().join("missing"),
            ..project.run_request()
        }));

        assert!(refusal.contains("does not exist"), "{refusal}");
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
        runner.run_stream(project.run_request()).unwrap();
        runner.stop_stream(&camera).unwrap();
        let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
        stub_uv.handed_to(&runner);

        let started = runner.start_stream(&camera).expect("the stream starts");

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
        runner.run_stream(project.run_request()).unwrap();
        runner.stop_stream(&camera).unwrap();
        std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
        let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
        stub_uv.handed_to(&runner);

        let refusal = refusal_of(runner.start_stream(&camera));

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

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

        assert!(
            reloads
                .iter()
                .all(|reload| matches!(reload, StreamReloadAtTheStart::Reloaded { .. })),
            "{reloads:?}"
        );
        assert_eq!(reloads.len(), 2, "{reloads:?}");
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

        let reloads = runner.reload_every_recorded_stream_neither_stopped_nor_failed();

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
        project: &ProjectWithAStubCompile,
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

    fn run_project_request_of(project_directory: &Path) -> RunProjectRequest {
        RunProjectRequest {
            project_directory: project_directory.to_path_buf(),
        }
    }

    #[test]
    #[serial]
    fn a_run_project_answers_its_action_at_once_and_the_action_walks_to_done_with_its_streams() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        project.compile_every_stream_to(&["camera", "preview"], &[]);
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
        stub_uv.handed_to(&runner);
        let project_directory = std::fs::canonicalize(project.path()).unwrap();

        let queued = runner
            .run_project(run_project_request_of(project.path()))
            .expect("a run of a project is queued");

        assert_eq!(queued.project_directory, project_directory);
        let making_the_venv = the_stream_action_once(&runner, &queued.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        assert_eq!(making_the_venv.state, StreamActionState::InProgress);
        assert_eq!(making_the_venv.kind, StreamActionKind::Load);
        let the_project_preparing = runner
            .list_streams()
            .into_iter()
            .find(|listing| listing.state == StreamListingState::Preparing)
            .expect("the project is listed preparing while its venv is made");
        assert_eq!(
            the_project_preparing.stream_id,
            StreamLocation::of_a_project_that_gave_no_stream(project_directory.clone()).stream_id()
        );
        assert_eq!(
            the_project_preparing.stream_action_under_way,
            Some(StreamActionWorkingOnAStream {
                action_id: queued.action_id.clone(),
                step: Some(StreamActionStep::MakingTheVenv),
            })
        );
        let preparing_before_its_compile = runner
            .list_project_streams(project.path())
            .expect("a project with an action under way is listed");
        assert_eq!(
            preparing_before_its_compile
                .stream_action_under_way
                .map(|it| it.action_id),
            Some(queued.action_id.clone())
        );
        assert_eq!(
            preparing_before_its_compile.what_the_last_compile_gave,
            None
        );

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
        let listed_once_compiled = runner.list_project_streams(project.path()).unwrap();
        assert_eq!(listed_once_compiled.stream_action_under_way, None);
        let Some(WhatTheLastCompileOfTheProjectGave::Streams(streams_compiled)) =
            listed_once_compiled.what_the_last_compile_gave
        else {
            panic!("the compile named the project's streams");
        };
        assert_eq!(
            streams_compiled
                .iter()
                .map(|stream| stream.stream_location.stream_function.as_str())
                .collect::<Vec<_>>(),
            ["stream:camera", "stream:preview"]
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
        let waiting_on_its_venv = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_named("camera", serde_json::json!([])),
        );
        let (stub_uv, gate) = a_uv_making_the_venv_once_released(&waiting_on_its_venv);
        stub_uv.handed_to(&runner);
        let beside_it = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "preview",
            serde_json::json!([]),
        ));

        let first = runner
            .run_project(run_project_request_of(waiting_on_its_venv.path()))
            .unwrap();
        let second = runner
            .run_project(run_project_request_of(waiting_on_its_venv.path()))
            .unwrap();
        the_stream_action_once(&runner, &first.action_id, |action| {
            action.step == Some(StreamActionStep::MakingTheVenv)
        });
        let other_project = runner
            .run_project(run_project_request_of(beside_it.path()))
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
    fn a_run_project_of_a_folder_that_is_no_project_fails_its_action_saying_why() {
        let runner = a_runner_recording_its_streams_in(None);
        let no_project = a_temporary_directory_at_owner_only_mode().unwrap();

        for (project_directory, why) in [
            (no_project.path().join("missing"), "does not exist"),
            (
                no_project.path().to_path_buf(),
                "is not a project: it holds neither stream.py nor pyproject.toml",
            ),
        ] {
            let queued = runner
                .run_project(run_project_request_of(&project_directory))
                .expect("a load refuses nothing at the call");
            let failed = the_stream_action_once_finished(&runner, &queued.action_id);

            assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
            let failed_because = failed.failed_because.unwrap_or_default();
            assert!(failed_because.contains(why), "{failed_because}");
        }
        assert!(
            refusal_of(runner.run_project(run_project_request_of(Path::new("relative/desk"))))
                .contains("`project_directory` must be an absolute path"),
        );
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_queued_run_of_a_stream_py_defining_no_stream_records_the_project_failed() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let no_stream_defined = "`stream.py` defines no @stream function";
        project.define_no_stream_printing(no_stream_defined);

        let queued = runner
            .run_project(run_project_request_of(project.path()))
            .unwrap();
        let done = the_stream_action_once_finished(&runner, &queued.action_id);

        assert_eq!(done.state, StreamActionState::Done, "{done:?}");
        let project_stream_id = project.location_of_the_project().stream_id();
        assert_eq!(
            done.streams
                .iter()
                .map(|stream| (stream.stream_id.as_str(), stream.state))
                .collect::<Vec<_>>(),
            [(project_stream_id.as_str(), Some(StreamListingState::Failed))]
        );
        assert_eq!(
            the_record_of(state_directory.path(), &project_stream_id)
                .failed_because
                .as_deref(),
            Some(no_stream_defined)
        );
        assert_eq!(
            runner
                .list_project_streams(project.path())
                .unwrap()
                .what_the_last_compile_gave,
            Some(WhatTheLastCompileOfTheProjectGave::EntryDefinesNoStream(
                no_stream_defined.to_string()
            ))
        );
    }

    #[test]
    #[serial]
    fn an_action_never_held_and_a_project_never_run_are_refused_naming_what_to_do() {
        let runner = a_runner_recording_its_streams_in(None);
        let project = ProjectWithAStubCompile::with_no_venv();

        assert!(
            refusal_of(runner.describe_stream_action("Rgone-7"))
                .contains("the stream action `Rgone-7` is not held")
        );
        assert!(
            refusal_of(runner.list_project_streams(project.path()))
                .contains("is not loaded: `run_project` loads it")
        );
    }
}
