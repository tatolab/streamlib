// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions a runtime takes for its owner: run a project's stream
//! function, attached or kept, under the stream id its location computes to;
//! stop, start, remove and rename a stream; list what the runtime holds; set a port's
//! exposure; re-load the kept streams at start; and unload an attached stream
//! when what attached it goes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT, STREAM_LOAD_TOOL_CALL_TIMEOUT,
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
        STREAM_FUNCTION_COMPILE_BOUND, compile_the_stream_function_in_the_projects_interpreter,
    },
};
use crate::core::graph::{
    OutputPortExposureLevel, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
    node_names_listed_for_a_refusal,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::{Error, Result};

/// How a loaded stream is held: kept by the runtime, or attached to what
/// loaded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadedStreamHolding {
    /// Recorded in the runtime's state directory and re-loaded at its start.
    Kept,
    /// Lives as long as what loaded it: a local API connection, or the
    /// program that loaded it into a library [`Runner`].
    Attached,
}

impl std::fmt::Display for LoadedStreamHolding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Kept => "kept",
            Self::Attached => "attached",
        })
    }
}

/// What [`Runner::run_stream`] compiles and loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStreamRequest {
    /// The project whose `.venv/bin/python` compiles the stream function, by
    /// absolute path.
    pub project_directory: PathBuf,
    /// The stream function as `tatolab run` takes it — `stream.py:main`,
    /// `x.py`, `mod.sub:fn` — or `None` for the sole `@stream` in the
    /// project's `stream.py`.
    pub stream_function: Option<String>,
    /// The display name the load gives the stream. `None` keeps a replaced
    /// stream's display name, and shows a new one under its function's name.
    pub display_name: Option<String>,
    /// Whether the runtime keeps the stream, recording it in its state
    /// directory, or it is attached to what ran it.
    pub holding: LoadedStreamHolding,
    /// The tags of the streams the caller attached and still holds — a local
    /// API connection's — so an attached run of the location of one of them
    /// replaces it.
    pub stream_tags_attached_to_the_caller: Vec<LoadedStreamTag>,
}

/// What an attached [`Runner::run_stream`] compiled a loaded stream from, so
/// a refused replace re-loads it.
#[derive(Debug, Clone)]
pub(crate) struct AttachedStreamRunSource {
    /// The graph the compile printed.
    pub(crate) graph_json: serde_json::Value,
    /// Where its processor interpreters start.
    pub(crate) stream_environment: StreamEnvironment,
    /// The tags of the loads this one was re-loaded in place of after a
    /// refused replace; what attached any of them holds this one.
    pub(crate) re_loaded_in_place_of: Vec<LoadedStreamTag>,
}

/// A stream [`Runner::run_stream`] loaded and started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRunOutcome {
    /// The stream's id, computed from its location.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives.
    pub stream_location: StreamLocation,
    /// The tag of this load of it, which an attachment unloads by.
    pub stream_tag: LoadedStreamTag,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
    /// Whether the load replaced the kept stream of the same location.
    pub replaced_the_kept_record: bool,
    /// What the compile wrote to its standard error, line by line — the
    /// cross-floor check's warnings among it.
    pub compile_warnings: Vec<String>,
}

/// A stream [`Runner::stop_stream`] unloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStopOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Whether the stream is kept, and so recorded as stopped.
    pub kept: bool,
    /// Why a kept stream unloaded here was not recorded stopped, so a
    /// restart of the runtime loads it again; `None` when it was recorded,
    /// and for an attached stream.
    pub stop_not_recorded_because: Option<String>,
}

/// A kept stream [`Runner::start_stream`] loaded and started.
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
    /// Loaded, and lives as long as what loaded it.
    Attached,
    /// Kept by the runtime: loaded, or recorded and not loaded because it
    /// has ended — `start_stream` loads it again.
    Kept,
    /// Kept, and stopped by its owner.
    Stopped,
    /// Kept, and failed: it kept crashing the runtime or could not re-load at
    /// its start, and is skipped at the start until `start_stream` retries it;
    /// or it was recorded by an earlier runtime, and is never loaded.
    Failed,
}

impl StreamListingState {
    /// The state as a listing spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Kept => "kept",
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

impl From<LoadedStreamHolding> for StreamListingState {
    fn from(holding: LoadedStreamHolding) -> Self {
        match holding {
            LoadedStreamHolding::Kept => Self::Kept,
            LoadedStreamHolding::Attached => Self::Attached,
        }
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
    /// Whether it is attached, kept, stopped or failed.
    pub state: StreamListingState,
    /// How many nodes its loaded graph holds; `None` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `None` when it is not.
    pub failed_because: Option<String>,
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
    /// Whether the level was recorded as the owner's ruling on a kept stream.
    pub recorded: bool,
    /// Why the level, raised live on a loaded kept stream, was not recorded
    /// as the owner's ruling; `None` when it was recorded, and for an
    /// attached stream, whose level is never recorded.
    pub ruling_not_recorded_because: Option<String>,
}

/// How many of the runtime's crashes in a row a kept stream is implicated in
/// before it is failed (runtime-hosting decision 5).
pub const RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_KEPT_STREAM: u32 = 2;

/// What [`Runner::reload_every_kept_stream_neither_stopped_nor_failed`] did
/// with one record; a stream it left unloaded because it is stopped, removed
/// or already loaded is not reported.
#[derive(Debug)]
pub enum KeptStreamReloadAtTheStart {
    /// The kept stream loaded and started.
    Reloaded {
        /// The stream's id.
        stream_id: String,
    },
    /// The kept stream is failed, so it was skipped.
    SkippedAsFailed {
        /// The stream's id.
        stream_id: String,
        /// Why it is failed.
        failed_because: String,
    },
    /// The kept stream did not re-load, and is recorded failed for it — unless
    /// the refusal was the engine's own, or a machine shutdown abandoned the
    /// re-load, when it stays kept.
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
    /// Held across an action's check, load and record — never across a
    /// compile, which runs a project's code for up to its bound.
    one_stream_action_at_a_time: Mutex<()>,
}

/// One stream a stream action loads, under the id its location computes to:
/// its display name and location, the graph and the owner's rulings, where its
/// interpreters start, and how it is held.
struct OneStreamAStreamActionLoads<'load> {
    display_name: &'load str,
    stream_location: &'load StreamLocation,
    graph_json: &'load serde_json::Value,
    rulings: &'load [OwnerExposureRuling],
    stream_environment: StreamEnvironment,
    holding: LoadedStreamHolding,
}

impl<'load> OneStreamAStreamActionLoads<'load> {
    /// The kept stream `record` records, its rulings applied.
    fn recorded_by(record: &'load StreamRecord) -> Self {
        Self {
            display_name: &record.display_name,
            stream_location: &record.location,
            graph_json: &record.graph,
            rulings: &record.exposure_rulings,
            stream_environment: record.stream_environment(),
            holding: LoadedStreamHolding::Kept,
        }
    }
}

/// The longest one stream action holds the one-action lock, in milliseconds: a
/// replace, kept or attached, whose load and restore are both refused — the running
/// stream's teardown, then for each load a describe and a refused start's
/// teardown.
const LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_IN_MILLISECONDS: u128 =
    ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis()
        + 2 * (PROCESSOR_INTERPRETER_DESCRIBE_BOUND.as_millis()
            + ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis());

/// The longest a load waits for its answer with one stream action queued ahead
/// of it, in milliseconds: its compile runs outside the lock while the action
/// ahead holds it, then it holds the lock itself.
const LONGEST_LOAD_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS: u128 = {
    let compile = STREAM_FUNCTION_COMPILE_BOUND.as_millis();
    let compile_or_the_action_ahead =
        if compile > LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_IN_MILLISECONDS {
            compile
        } else {
            LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_IN_MILLISECONDS
        };
    compile_or_the_action_ahead + LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_IN_MILLISECONDS
};

const _: () = assert!(
    STREAM_FUNCTION_COMPILE_BOUND.as_millis() + PROCESSOR_INTERPRETER_DESCRIBE_BOUND.as_millis()
        < STREAM_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a load before the runtime's compile and describe bounds end"
);
const _: () = assert!(
    LONGEST_LOAD_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS
        < STREAM_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a load queued behind one stream action before the runtime answers"
);
const _: () = assert!(
    LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_IN_MILLISECONDS
        + ENGINE_TEARDOWN_WATCHDOG_BUDGET.as_millis()
        < STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT.as_millis(),
    "a client gives up on a stop, remove or expose queued behind one stream action before the \
     runtime answers"
);

impl Runner {
    /// Record this runtime's streams in `streams_directory`, created
    /// owner-only when absent. Refused a second time; a runtime never given
    /// one keeps no stream.
    pub fn keep_streams_in_the_state_directory(&self, streams_directory: &Path) -> Result<()> {
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

    /// Compile the request's stream function in its project's interpreter,
    /// then load and start the graph under the stream id its location — the
    /// resolved project directory and the function's `module:qualname` —
    /// computes to, keeping it when the request says so.
    ///
    /// An id held by another location is refused naming both. A load of a
    /// location already loaded or recorded replaces it under the same id,
    /// keeping its display name unless the request gives one: a kept load
    /// replaces the kept stream, the owner's rulings kept, and an attached
    /// load replaces the stream the caller attached. The running stream is
    /// unloaded only once the compile succeeded, so a compile that fails leaves
    /// it running; a refused load re-loads the previous one. A kept load of a
    /// location another connection holds attached, and an attached load of a
    /// kept one, are refused naming the holder.
    pub fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        let RunStreamRequest {
            project_directory,
            stream_function,
            display_name: display_name_the_run_gives,
            holding,
            stream_tags_attached_to_the_caller,
        } = request;
        if let Some(display_name) = &display_name_the_run_gives {
            refuse_a_display_name_with_nothing_to_show(display_name)?;
        }
        let how_this_run_is_held = match (holding, self.stream_records()) {
            (LoadedStreamHolding::Attached, _) => HowARunIsHeld::Attached,
            (LoadedStreamHolding::Kept, Some(stream_records)) => {
                HowARunIsHeld::KeptIn(stream_records)
            }
            (LoadedStreamHolding::Kept, None) => {
                return Err(Error::Configuration(format!(
                    "the stream in {} was not kept: this runtime keeps no stream, because its \
                     host gave it no state directory to keep them in; load it attached, or run \
                     it in `tatolabd`",
                    project_directory.display()
                )));
            }
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
        let compiled = compile_the_stream_function_in_the_projects_interpreter(
            &project_directory,
            stream_function.as_deref(),
            lend_directory,
        )?;
        let stream_location = compiled.stream_location;
        let stream_id = stream_location.stream_id();
        let stream_environment = compiled.stream_environment;
        let graph_json = compiled.graph_json;
        let compile_warnings = compiled.compile_warnings;

        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(&stream_id);
        let record = self.stream_record_of_the_id(&stream_id)?;
        refuse_an_id_another_location_holds(
            &stream_id,
            &stream_location,
            loaded.as_deref(),
            record.as_ref(),
        )?;
        let record = match record {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(_)) | None => None,
        };
        match (how_this_run_is_held, record, loaded) {
            (HowARunIsHeld::KeptIn(records_to_keep_it_in), Some(previous_record), loaded)
                if loaded
                    .as_ref()
                    .is_none_or(|loaded| loaded.holding() == LoadedStreamHolding::Kept) =>
            {
                let display_name = display_name_the_run_gives
                    .unwrap_or_else(|| previous_record.display_name.clone());
                self.replace_a_kept_stream(
                    records_to_keep_it_in,
                    previous_record,
                    loaded,
                    StreamRecord::of_a_running_stream(
                        stream_location,
                        display_name,
                        &stream_environment,
                        graph_json,
                    ),
                    compile_warnings,
                )
            }
            (HowARunIsHeld::Attached, Some(kept_record), _) => Err(Error::GraphError(format!(
                "the stream `{stream_id}` from {stream_location} is kept by this runtime{}; \
                     `run -d` replaces it, and `rm` forgets it",
                the_state_a_refusal_names_a_kept_record_by(&kept_record)
            ))),
            (HowARunIsHeld::Attached, None, Some(loaded))
                if loaded.holding() == LoadedStreamHolding::Attached
                    && stream_tags_attached_to_the_caller
                        .iter()
                        .any(|stream_tag| loaded.is_held_by_the_attachment_of(*stream_tag)) =>
            {
                let Some(previous_run_source) = loaded.attached_stream_run_source().cloned() else {
                    return Err(a_stream_id_already_loaded_refusal(
                        &loaded,
                        STOP_IT_FIRST_TO_LOAD_THIS_ONE,
                    ));
                };
                let display_name =
                    display_name_the_run_gives.unwrap_or_else(|| loaded.display_name());
                self.replace_an_attached_stream(
                    loaded,
                    previous_run_source,
                    AttachedRunOfOneLocation {
                        stream_location,
                        display_name,
                        run_source: AttachedStreamRunSource {
                            graph_json,
                            stream_environment,
                            re_loaded_in_place_of: Vec::new(),
                        },
                    },
                    compile_warnings,
                )
            }
            (_, _, Some(loaded)) => Err(a_stream_id_already_loaded_refusal(
                &loaded,
                STOP_IT_FIRST_TO_LOAD_THIS_ONE,
            )),
            (how_this_run_is_held, _, None) => {
                let display_name_of_a_new_stream =
                    display_name_the_run_gives.unwrap_or_else(|| {
                        graph_json
                            .get("stream")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(&stream_id)
                            .to_string()
                    });
                let stream = self.load_and_start_a_stream(OneStreamAStreamActionLoads {
                    display_name: &display_name_of_a_new_stream,
                    stream_location: &stream_location,
                    graph_json: &graph_json,
                    rulings: &[],
                    stream_environment: stream_environment.clone(),
                    holding,
                })?;
                match how_this_run_is_held {
                    HowARunIsHeld::KeptIn(records_to_keep_it_in) => {
                        let record = StreamRecord::of_a_running_stream(
                            stream_location.clone(),
                            display_name_of_a_new_stream.clone(),
                            &stream_environment,
                            graph_json,
                        );
                        if let Err(write_refusal) = records_to_keep_it_in.write(&record) {
                            unload_a_stream_an_action_took_back(
                                &stream,
                                "its record was not written",
                            );
                            return Err(write_refusal);
                        }
                    }
                    HowARunIsHeld::Attached => {
                        stream.remember_the_attached_stream_run_source(AttachedStreamRunSource {
                            graph_json,
                            stream_environment,
                            re_loaded_in_place_of: Vec::new(),
                        })
                    }
                }
                Ok(StreamRunOutcome {
                    node_count: stream.node_count(),
                    stream_tag: stream.stream_tag(),
                    stream_id,
                    display_name: display_name_of_a_new_stream,
                    stream_location,
                    replaced_the_kept_record: false,
                    compile_warnings,
                })
            }
        }
    }

    /// Unload the stream whose id is `stream_id`; a kept one is recorded
    /// stopped first, so a restart leaves it unloaded. A loaded kept stream is
    /// unloaded even when its record cannot be read or written, and the
    /// outcome says why it was not recorded stopped. Refused — with nothing
    /// changed — naming the streams the runtime holds when it holds none of
    /// that id, and for a kept stream already stopped or failed.
    pub fn stop_stream(&self, stream_id: &str) -> Result<StreamStopOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        if let Some(attached) = loaded
            .as_ref()
            .filter(|loaded| loaded.holding() == LoadedStreamHolding::Attached)
        {
            unload_a_stream_an_action_took_back(attached, "its owner stopped it");
            return Ok(StreamStopOutcome {
                stream_id: stream_id.to_string(),
                kept: false,
                stop_not_recorded_because: None,
            });
        }
        let kept_record = match self.current_stream_record_of_the_id(stream_id) {
            Ok(kept_record) => kept_record,
            Err(unreadable) => {
                let Some(loaded) = loaded else {
                    return Err(unreadable);
                };
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                return Ok(a_kept_stream_unloaded_and_not_recorded_stopped(
                    stream_id.to_string(),
                    &unreadable,
                ));
            }
        };
        match (loaded, kept_record) {
            (None, None) => Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
            (None, Some(kept_record)) if kept_record.is_failed() => Err(Error::Runtime(format!(
                "the kept stream `{stream_id}` is failed, so it is not loaded to stop: {}. \
                 `start` retries it, `rm` forgets it",
                kept_record.failed_because.as_deref().unwrap_or_default()
            ))),
            (None, Some(kept_record)) if kept_record.stopped => Err(Error::Runtime(format!(
                "the kept stream `{stream_id}` is already stopped; `start` loads it again"
            ))),
            (None, Some(mut kept_record)) => {
                kept_record.stopped = true;
                self.write_a_stream_record(&kept_record)?;
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    kept: true,
                    stop_not_recorded_because: None,
                })
            }
            (Some(loaded), Some(mut kept_record)) => {
                kept_record.stopped = true;
                let recorded_stopped = self.write_a_stream_record(&kept_record);
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(match recorded_stopped {
                    Ok(()) => StreamStopOutcome {
                        stream_id: stream_id.to_string(),
                        kept: true,
                        stop_not_recorded_because: None,
                    },
                    Err(not_written) => a_kept_stream_unloaded_and_not_recorded_stopped(
                        stream_id.to_string(),
                        &not_written,
                    ),
                })
            }
            (Some(loaded), None) => {
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    kept: false,
                    stop_not_recorded_because: None,
                })
            }
        }
    }

    /// Load and start the kept stream whose id is `stream_id` from its record,
    /// the owner's rulings applied, and record it neither stopped nor failed,
    /// its crash count reset. Refused for a stream already loaded, an attached
    /// one, an id no record holds, and a record an earlier runtime wrote. A
    /// failed stream whose retry is refused stays failed for the retry's
    /// reason, its crash count reset — or as it was, when the refusal was the
    /// engine's own; one whose retry crashes the runtime stays failed as it was.
    pub fn start_stream(&self, stream_id: &str) -> Result<StreamStartOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(stream_id) {
            return Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already loaded in this runtime, {}; only a kept \
                 stream that is not loaded starts",
                loaded.holding()
            )));
        }
        let kept_record =
            self.current_stream_record_of_the_id(stream_id)
                .map_err(|unreadable| {
                    Error::Runtime(format!(
                        "the kept stream `{stream_id}` was not started: {unreadable}"
                    ))
                })?;
        let Some(mut kept_record) = kept_record else {
            return Err(Error::NotFound(format!(
                "no kept stream with the id `{stream_id}` is in this runtime, so there is none to \
                 start; an attached stream is loaded again with `run`. Kept: {}",
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        };
        let stream = match self.load_a_kept_stream_from_its_record(&kept_record) {
            Ok(stream) => stream,
            Err(retry_refusal)
                if kept_record.is_failed()
                    && !matches!(
                        retry_refusal,
                        Error::EngineResourceRefusedAtAStreamsStart(_)
                    ) =>
            {
                kept_record.failed_because = Some(format!(
                    "`start` retried it, and it did not load: {retry_refusal}"
                ));
                kept_record.runtime_crashes_in_a_row_implicating_it = 0;
                if let Err(not_written) = self.write_a_stream_record(&kept_record) {
                    tracing::warn!(
                        "the kept stream `{stream_id}` stays failed, and its refused retry was \
                         not recorded: {not_written}"
                    );
                }
                return Err(retry_refusal);
            }
            Err(start_refusal) => return Err(start_refusal),
        };
        if kept_record.mark_running_resetting_its_crash_count()
            && let Err(write_refusal) = self.write_a_stream_record(&kept_record)
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
        match (record, &loaded) {
            (None, None) => return Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
            (Some(mut record), _) => {
                record.display_name = display_name.to_string();
                self.write_a_stream_record(&record)?;
            }
            (None, Some(_)) => {}
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
                state: stream.holding().into(),
                node_count: Some(stream.node_count()),
                failed_because: None,
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
                    },
                    Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => StreamListing {
                        state: StreamListingState::Failed,
                        failed_because: Some(earlier.failed_because()),
                        display_name: earlier.stream_id.clone(),
                        stream_id: earlier.stream_id,
                        stream_location: None,
                        node_count: None,
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
        listings.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        listings
    }

    /// Put output port `port` of node `node` in the stream whose id is
    /// `stream_id` at `level`: live on a loaded stream, where the runtime's
    /// publisher withdraws the port's track from each relay the level no
    /// longer allows, and recorded as the owner's ruling on a kept stream. On a loaded kept
    /// stream a restriction is recorded before it changes live, and refused
    /// with the live level unchanged when its record cannot be written, so a
    /// restart never re-opens a port the owner closed; a promotion changes
    /// live first, and the outcome says why it was not recorded when it was
    /// not. A stopped stream's ruling is recorded once the recorded graph
    /// holds the node. An attached stream's level is never recorded.
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
                    LoadedStreamHolding::Attached => {
                        set_live(&loaded)?;
                        (false, None)
                    }
                    LoadedStreamHolding::Kept if level.is_narrower_than(live_level) => {
                        self.record_the_owners_ruling_on_a_kept_record(stream_id, ruling)
                            .map_err(|not_recorded| {
                                Error::Runtime(format!(
                                    "the port `{node}/{port}` of the kept stream `{stream_id}` \
                                     was not restricted to {level}, and is still {live_level}: \
                                     the owner's ruling could not be recorded, and a restriction \
                                     not recorded would be undone at the runtime's restart: \
                                     {not_recorded}"
                                ))
                            })?;
                        set_live(&loaded)?;
                        (true, None)
                    }
                    LoadedStreamHolding::Kept => {
                        set_live(&loaded)?;
                        match self.record_the_owners_ruling_on_a_kept_record(stream_id, ruling) {
                            Ok(()) => (true, None),
                            Err(not_recorded) => {
                                tracing::warn!(
                                    "the port `{node}/{port}` of the kept stream `{stream_id}` \
                                     is {level} now, live, and the owner's ruling was not \
                                     recorded: {not_recorded}"
                                );
                                (false, Some(not_recorded.to_string()))
                            }
                        }
                    }
                }
            }
            None => {
                let Some(mut kept_record) = self.current_stream_record_of_the_id(stream_id)? else {
                    return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
                };
                refuse_a_node_the_recorded_graph_does_not_hold(&kept_record, node)?;
                kept_record.record_the_owners_exposure_ruling(ruling);
                self.write_a_stream_record(&kept_record)?;
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

    /// Load and start every kept stream neither stopped nor failed, as a
    /// runtime does at its start, reporting each record it tried in file-name
    /// order. A stream that does not re-load is recorded failed for the
    /// reason, unless the refusal was the engine's own — its GPU context — or
    /// a machine shutdown abandoned the re-load; a record an earlier runtime
    /// wrote is skipped as failed; a record that cannot be read is logged and
    /// left in place.
    pub fn reload_every_kept_stream_neither_stopped_nor_failed(
        &self,
    ) -> Vec<KeptStreamReloadAtTheStart> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        self.reload_each_kept_stream_listed(stream_records, stream_records.read_every())
    }

    /// Re-load each record of `listed`. The local API serves while this runs,
    /// so each record is read again under the one-action lock and loaded from
    /// that read: a stream stopped or removed since the listing stays
    /// unloaded, one loaded since is left as it is, and the owner's rulings
    /// recorded since hold from its load.
    fn reload_each_kept_stream_listed(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        listed: Vec<std::result::Result<StreamRecordAsRead, StreamRecordReadFailure>>,
    ) -> Vec<KeptStreamReloadAtTheStart> {
        let mut reloads = Vec::new();
        for listed_record in listed {
            let stream_id = match listed_record {
                Ok(listed_record) => listed_record.stream_id().to_string(),
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(KeptStreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let kept_record = match stream_records.read(&stream_id) {
                Ok(Some(StreamRecordAsRead::Current(kept_record))) => kept_record,
                Ok(Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))) => {
                    let failed_because = earlier.failed_because();
                    tracing::warn!(
                        "the stream `{stream_id}` is failed, so it is not re-loaded: \
                         {failed_because}"
                    );
                    reloads.push(KeptStreamReloadAtTheStart::SkippedAsFailed {
                        stream_id,
                        failed_because,
                    });
                    continue;
                }
                Ok(None) => {
                    tracing::info!(
                        "the kept stream `{stream_id}` was removed before its re-load, so it \
                         is not re-loaded"
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(KeptStreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            if let Some(failed_because) = kept_record.failed_because {
                tracing::warn!(
                    "the kept stream `{stream_id}` is failed, so it is not re-loaded: \
                     {failed_because}; `tatolab start` retries it"
                );
                reloads.push(KeptStreamReloadAtTheStart::SkippedAsFailed {
                    stream_id,
                    failed_because,
                });
                continue;
            }
            if kept_record.stopped {
                tracing::info!("the kept stream `{stream_id}` is stopped, so it is not re-loaded");
                continue;
            }
            if self
                .loaded_stream_of_the_id_when_loaded(&stream_id)
                .is_some()
            {
                tracing::info!(
                    "the kept stream `{stream_id}` was loaded before its re-load reached it, so \
                     it is left as it is"
                );
                continue;
            }
            match self.load_a_kept_stream_from_its_record(&kept_record) {
                Ok(stream) => {
                    tracing::info!(
                        "the kept stream `{stream_id}` re-loaded from {} with {} nodes",
                        kept_record.location,
                        stream.node_count()
                    );
                    reloads.push(KeptStreamReloadAtTheStart::Reloaded { stream_id });
                }
                Err(reload_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)) => {
                    tracing::error!(
                        "the kept stream `{stream_id}` was not re-loaded, and stays kept for \
                         the runtime's next start: {reload_refusal}"
                    );
                    reloads.push(KeptStreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) if is_the_machines_shutdown_requested() => {
                    tracing::info!(
                        "the kept stream `{stream_id}` was not re-loaded, because the machine is \
                         shutting every stream down: {reload_refusal}"
                    );
                    reloads.push(KeptStreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) => {
                    let mut failed_record = kept_record;
                    failed_record.failed_because = Some(format!(
                        "it did not re-load at the runtime's start: {reload_refusal}"
                    ));
                    match stream_records.write(&failed_record) {
                        Ok(()) => tracing::error!(
                            "the kept stream `{stream_id}` did not re-load, and is failed: \
                             {reload_refusal}; `tatolab start` retries it"
                        ),
                        Err(not_written) => tracing::error!(
                            "the kept stream `{stream_id}` did not re-load: {reload_refusal}; \
                             and it was not recorded failed: {not_written}"
                        ),
                    }
                    reloads.push(KeptStreamReloadAtTheStart::NotReloaded {
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
    /// [`RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_KEPT_STREAM`] — and resets every
    /// other's. Returns the ids of the streams it failed.
    pub fn count_the_previous_runtime_runs_end_against_the_kept_streams(
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
            let Ok(StreamRecordAsRead::Current(mut kept_record)) = read else {
                continue;
            };
            if kept_record.is_failed() {
                continue;
            }
            let crashes_in_a_row_before = kept_record.runtime_crashes_in_a_row_implicating_it;
            match crash
                .causes_by_implicated_stream
                .get(&kept_record.stream_id)
            {
                Some(cause) => {
                    kept_record.runtime_crashes_in_a_row_implicating_it += 1;
                    let crashes_in_a_row = kept_record.runtime_crashes_in_a_row_implicating_it;
                    if crashes_in_a_row >= RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_KEPT_STREAM {
                        kept_record.failed_because = Some(format!(
                            "it was implicated in the runtime's last {crashes_in_a_row} crashes \
                             in a row, the last: {cause}"
                        ));
                    }
                }
                None if crashes_in_a_row_before == 0 => continue,
                None => kept_record.runtime_crashes_in_a_row_implicating_it = 0,
            }
            if let Err(not_written) = stream_records.write(&kept_record) {
                tracing::error!(
                    "the kept stream `{}`'s count of the runtime's crashes was not recorded: \
                     {not_written}",
                    kept_record.stream_id
                );
                continue;
            }
            if let Some(failed_because) = &kept_record.failed_because {
                tracing::error!(
                    "the kept stream `{}` is failed: {failed_because}; it is not re-loaded until \
                     `tatolab start` retries it",
                    kept_record.stream_id
                );
                streams_failed.push(kept_record.stream_id);
            }
        }
        streams_failed
    }

    /// Unload the stream whose id is `stream_id` when it is still the load
    /// `stream_tag` tags, or a re-load of it after a refused replace — what a
    /// closed local API connection does for the streams it attached. Does
    /// nothing, and says so with `false`, when the id is not loaded or now
    /// holds another stream.
    pub fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_id: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(stream_id) else {
            return false;
        };
        if !loaded.is_held_by_the_attachment_of(stream_tag) {
            return false;
        }
        unload_a_stream_an_action_took_back(&loaded, "the connection that attached it closed");
        true
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

    /// Record `ruling` on the record of the loaded kept stream `stream_id`,
    /// refused naming the record when it is gone.
    fn record_the_owners_ruling_on_a_kept_record(
        &self,
        stream_id: &str,
        ruling: OwnerExposureRuling,
    ) -> Result<()> {
        let Some(mut kept_record) = self.current_stream_record_of_the_id(stream_id)? else {
            let record_path = self
                .stream_records()
                .and_then(|stream_records| stream_records.record_path_of(stream_id))
                .map(|record_path| record_path.display().to_string())
                .unwrap_or_else(|| "in this runtime's state directory".to_string());
            return Err(Error::Runtime(format!(
                "the kept stream `{stream_id}` is loaded and its record {record_path} is gone, \
                 so the runtime's restart does not load it; `run -d` keeps it again"
            )));
        };
        kept_record.record_the_owners_exposure_ruling(ruling);
        self.write_a_stream_record(&kept_record)
    }

    fn write_a_stream_record(&self, record: &StreamRecord) -> Result<()> {
        match self.stream_records() {
            Some(stream_records) => stream_records.write(record),
            None => Err(Error::Configuration(format!(
                "the kept stream `{}` was not recorded: this runtime keeps no stream",
                record.stream_id
            ))),
        }
    }

    fn a_stream_this_runtime_does_not_hold(&self, stream_id: &str) -> Error {
        Error::NotFound(format!(
            "no stream with the id `{stream_id}` is loaded or kept in this runtime. Loaded: {}. \
             Kept: {}",
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
                            StreamListingState::Kept => record.stream_id,
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

    /// Replace `previous_record` by `replacement`, whose compile already
    /// succeeded writing `compile_warnings`: unload the running stream, load
    /// and start the new graph with the previous rulings, and record it. A
    /// refused load re-loads the previous record when its stream was running.
    fn replace_a_kept_stream(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        previous_record: StreamRecord,
        previously_loaded: Option<Arc<LoadedStreamInThisRuntime>>,
        mut replacement: StreamRecord,
        compile_warnings: Vec<String>,
    ) -> Result<StreamRunOutcome> {
        replacement.exposure_rulings = previous_record.exposure_rulings.clone();
        let was_running = previously_loaded.is_some();
        if let Some(previously_loaded) = previously_loaded {
            unload_a_stream_an_action_took_back(&previously_loaded, "its kept record is replaced");
        }
        let replaced = self
            .load_and_start_a_stream(OneStreamAStreamActionLoads::recorded_by(&replacement))
            .and_then(|stream| match stream_records.write(&replacement) {
                Ok(()) => Ok(stream),
                Err(write_refusal) => {
                    unload_a_stream_an_action_took_back(&stream, "its record was not written");
                    Err(write_refusal)
                }
            });
        match replaced {
            Ok(stream) => Ok(StreamRunOutcome {
                node_count: stream.node_count(),
                stream_tag: stream.stream_tag(),
                stream_id: replacement.stream_id,
                display_name: replacement.display_name,
                stream_location: replacement.location,
                replaced_the_kept_record: true,
                compile_warnings,
            }),
            Err(replace_refusal) if was_running => {
                match self.load_a_kept_stream_from_its_record(&previous_record) {
                    Ok(_) => Err(replace_refusal),
                    Err(restore_refusal) => Err(Error::Runtime(format!(
                        "{replace_refusal}; and the stream it was to replace did not re-load \
                         from its record: {restore_refusal}"
                    ))),
                }
            }
            Err(replace_refusal) => Err(replace_refusal),
        }
    }

    /// Replace the attached stream `previously_loaded`, which an attached run
    /// compiled from `previous_run_source`, by `replacement`, whose compile
    /// already succeeded writing `compile_warnings`: unload it, then load and
    /// start the new graph attached under the same id. A refused load re-loads
    /// the previous graph, held by what attached the stream it replaces.
    fn replace_an_attached_stream(
        &self,
        previously_loaded: Arc<LoadedStreamInThisRuntime>,
        mut previous_run_source: AttachedStreamRunSource,
        replacement: AttachedRunOfOneLocation,
        compile_warnings: Vec<String>,
    ) -> Result<StreamRunOutcome> {
        let AttachedRunOfOneLocation {
            stream_location,
            display_name,
            run_source,
        } = replacement;
        let stream_id = previously_loaded.stream_id().to_string();
        unload_a_stream_an_action_took_back(
            &previously_loaded,
            "what attached it ran its stream function again",
        );
        match self.load_and_start_a_stream(OneStreamAStreamActionLoads {
            display_name: &display_name,
            stream_location: &stream_location,
            graph_json: &run_source.graph_json,
            rulings: &[],
            stream_environment: run_source.stream_environment.clone(),
            holding: LoadedStreamHolding::Attached,
        }) {
            Ok(stream) => {
                stream.remember_the_attached_stream_run_source(run_source);
                Ok(StreamRunOutcome {
                    node_count: stream.node_count(),
                    stream_tag: stream.stream_tag(),
                    stream_id,
                    display_name,
                    stream_location,
                    replaced_the_kept_record: false,
                    compile_warnings,
                })
            }
            Err(replace_refusal) => {
                let previous_display_name = previously_loaded.display_name();
                match self.load_and_start_a_stream(OneStreamAStreamActionLoads {
                    display_name: &previous_display_name,
                    stream_location: previously_loaded
                        .stream_location()
                        .unwrap_or(&stream_location),
                    graph_json: &previous_run_source.graph_json,
                    rulings: &[],
                    stream_environment: previous_run_source.stream_environment.clone(),
                    holding: LoadedStreamHolding::Attached,
                }) {
                    Ok(re_loaded) => {
                        previous_run_source
                            .re_loaded_in_place_of
                            .push(previously_loaded.stream_tag());
                        re_loaded.remember_the_attached_stream_run_source(previous_run_source);
                        Err(replace_refusal)
                    }
                    Err(re_load_refusal) => Err(Error::Runtime(format!(
                        "{replace_refusal}; and the stream it was to replace did not re-load: \
                     {re_load_refusal}"
                    ))),
                }
            }
        }
    }

    /// Load and start a kept stream from `kept_record`, refused naming the
    /// interpreter when the recorded one is gone.
    fn load_a_kept_stream_from_its_record(
        &self,
        kept_record: &StreamRecord,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        if !kept_record.interpreter.is_file() {
            return Err(Error::Configuration(format!(
                "the kept stream `{}` was not loaded: its interpreter {} is gone; run `uv sync` \
                 in {}",
                kept_record.stream_id,
                kept_record.interpreter.display(),
                kept_record.location.project_directory.display()
            )));
        }
        self.load_and_start_a_stream(OneStreamAStreamActionLoads::recorded_by(kept_record))
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
            holding,
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
            holding,
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

/// How a run holds the stream it loads: recorded in the runtime's streams
/// directory, or attached to its caller.
enum HowARunIsHeld<'records> {
    KeptIn(&'records StreamRecordsInTheStateDirectory),
    Attached,
}

/// An attached run's compile of one location, ready to replace the stream the
/// caller attached there.
struct AttachedRunOfOneLocation {
    stream_location: StreamLocation,
    display_name: String,
    run_source: AttachedStreamRunSource,
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
                "a{} kept stream from {}",
                the_state_a_refusal_names_a_kept_record_by(record),
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

/// Log what the runtime's previous run crashed on, a crash no stream owns as
/// the runtime bug it is.
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

fn log_the_previous_runtime_runs_crash(crash: &CrashOfThePreviousRuntimeRun) {
    for (stream_id, cause) in &crash.causes_by_implicated_stream {
        tracing::error!(
            "the runtime's previous run crashed, implicating the kept stream `{stream_id}`: \
             {cause}"
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

/// The state a refusal names a kept record by, a leading space before it:
/// ` stopped`, ` failed`, or nothing for one the runtime re-loads.
fn the_state_a_refusal_names_a_kept_record_by(kept_record: &StreamRecord) -> &'static str {
    match kept_record.listing_state() {
        StreamListingState::Failed => " failed",
        StreamListingState::Stopped => " stopped",
        StreamListingState::Kept | StreamListingState::Attached => "",
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

/// The stop of the kept stream `stream_id`, unloaded already, whose record
/// could not be marked stopped because of `not_recorded`.
fn a_kept_stream_unloaded_and_not_recorded_stopped(
    stream_id: String,
    not_recorded: &dyn std::fmt::Display,
) -> StreamStopOutcome {
    tracing::warn!(
        "the kept stream `{stream_id}` was unloaded, and is not recorded stopped, so a restart \
         of this runtime loads it again: {not_recorded}"
    );
    StreamStopOutcome {
        stream_id,
        kept: true,
        stop_not_recorded_because: Some(not_recorded.to_string()),
    }
}

/// Refuse `node` when the graph `kept_record` holds has no node of that name
/// once cast, naming the nodes it does hold.
fn refuse_a_node_the_recorded_graph_does_not_hold(
    kept_record: &StreamRecord,
    node: &str,
) -> Result<()> {
    let node_cast = cast_exposed_name_to_url_safe(node)?;
    let recorded_node_names: Vec<&str> = kept_record
        .graph
        .get("nodes")
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
        "the kept stream `{}` holds no node `{node}`. It holds: {}",
        kept_record.stream_id,
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

    use crate::core::runtime::STREAM_RECORD_SCHEMA_VERSION;
    use streamlib_runtime_client_contract::directory_at_an_explicit_mode;

    const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

    /// A runner handed a lend directory, recording its streams in
    /// `streams_directory` when one is given.
    fn a_runner_keeping_its_streams_in(streams_directory: Option<&Path>) -> Arc<Runner> {
        ensure_test_mocks_registered();
        let runner = Runner::new().expect("the runner builds");
        runner
            .set_processor_interpreter_lend_directory(PathBuf::from(LEND_DIRECTORY_FOR_TEST))
            .expect("the lend directory is handed over");
        if let Some(streams_directory) = streams_directory {
            runner
                .keep_streams_in_the_state_directory(streams_directory)
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
            let compile_document = serde_json::json!({
                "stream_graph": graph,
                "project_directory": self.path(),
                "stream_function": format!("stream:{function_name}"),
            });
            let warnings_written: String = warnings
                .iter()
                .map(|warning| format!("echo '{warning}' >&2\n"))
                .collect();
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                &format!(
                    "#!/bin/sh\n{warnings_written}cat <<'COMPILED'\n{compile_document}\nCOMPILED\n"
                ),
            );
        }

        /// From now on, the compile exits 1 printing `traceback`.
        fn fail_to_compile_printing(&self, traceback: &str) {
            write_an_executable_script_from_a_child_process(
                &self.interpreter(),
                &format!("#!/bin/sh\necho \"{traceback}\" >&2\nexit 1\n"),
            );
        }

        fn run_request(&self, holding: LoadedStreamHolding) -> RunStreamRequest {
            RunStreamRequest {
                project_directory: self.path().to_path_buf(),
                stream_function: None,
                display_name: None,
                holding,
                stream_tags_attached_to_the_caller: Vec::new(),
            }
        }

        /// An attached run by a caller that attached the loads `stream_tags`.
        fn attached_run_request_of_the_caller_holding(
            &self,
            stream_tags: &[LoadedStreamTag],
        ) -> RunStreamRequest {
            RunStreamRequest {
                stream_tags_attached_to_the_caller: stream_tags.to_vec(),
                ..self.run_request(LoadedStreamHolding::Attached)
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

    /// A kept record of the function `function_name` of `project`, shown under
    /// the function's name.
    fn a_kept_record_of(
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

    /// Record `record` and load it held as kept, not started — what a kept
    /// stream is between its load and its start.
    fn a_kept_stream_loaded_without_its_start(
        runner: &Runner,
        streams_directory: &Path,
        record: &StreamRecord,
    ) -> Arc<LoadedStreamInThisRuntime> {
        records_in(streams_directory)
            .write(record)
            .expect("the record is written");
        runner
            .load_a_stream_with_the_owners_rulings(OneStreamAStreamActionLoads::recorded_by(record))
            .expect("the kept stream loads")
    }

    /// The function `function_name` of `project` loaded attached under the id
    /// its location computes to, not started.
    fn a_stream_loaded_attached_at(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        function_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        let graph_json = the_graph_of_a_function_named(function_name, serde_json::json!([]));
        runner
            .load_a_stream_with_the_owners_rulings(OneStreamAStreamActionLoads {
                display_name: function_name,
                stream_location: &project.location_of(function_name),
                graph_json: &graph_json,
                rulings: &[],
                stream_environment: project.stream_environment(),
                holding: LoadedStreamHolding::Attached,
            })
            .expect("the attached stream loads")
    }

    /// The function `function_name` of `project` loaded attached under the id
    /// its location computes to, not started.
    fn an_attached_stream_loaded_without_its_start(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        function_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        a_stream_loaded_attached_at(runner, project, function_name)
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

    #[test]
    #[serial]
    fn a_kept_run_on_a_runtime_given_no_state_directory_is_refused_by_name() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("keeps no stream"), "{refusal}");
        assert!(refusal.contains("state directory"), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_second_streams_directory_is_refused_naming_the_first() {
        let state_directory = tempfile::tempdir().unwrap();
        let first = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&first));

        let refusal = refusal_of(
            runner.keep_streams_in_the_state_directory(&state_directory.path().join("other")),
        );

        assert!(refusal.contains(&first.display().to_string()), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_run_in_a_project_with_no_venv_interpreter_is_refused_pointing_at_uv_sync() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();

        for holding in [LoadedStreamHolding::Attached, LoadedStreamHolding::Kept] {
            let refusal = refusal_of(runner.run_stream(project.run_request(holding)));

            assert!(refusal.contains("uv sync"), "{refusal}");
        }
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_whose_id_a_loaded_stream_of_another_location_holds_is_refused_naming_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let (project, other_project) =
            ProjectWithAStubCompile::two_with_no_venv_whose_ids_collide("camera");
        let project = project.given_a_venv_compiling_warning(
            the_graph_of_a_function_named("camera", serde_json::json!([])),
            &[],
        );
        let camera = project.stream_id_of("camera");
        let other_location = other_project.location_of("camera");
        assert_eq!(other_location.stream_id(), camera);
        let holder = a_stream_loaded_attached_at(&runner, &other_project, "camera");

        for holding in [LoadedStreamHolding::Attached, LoadedStreamHolding::Kept] {
            let refusal = refusal_of(runner.run_stream(RunStreamRequest {
                stream_tags_attached_to_the_caller: vec![holder.stream_tag()],
                ..project.run_request(holding)
            }));

            assert!(refusal.contains(&format!("`{camera}`")), "{refusal}");
            assert!(
                refusal.contains(&project.location_of("camera").to_string()),
                "{refusal}"
            );
            assert!(refusal.contains(&other_location.to_string()), "{refusal}");
            assert!(refusal.contains("cannot share one id"), "{refusal}");
        }
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
        let runner = a_runner_keeping_its_streams_in(None);
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

        let refusal =
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Attached)));

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
            ("kept", |_| {}),
        ];
        for (state, mark) in holder_states {
            let state_directory = tempfile::tempdir().unwrap();
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            let mut holder = a_kept_record_of(&other_project, "camera", serde_json::json!([]));
            mark(&mut holder);
            records_in(state_directory.path()).write(&holder).unwrap();

            for holding in [LoadedStreamHolding::Kept, LoadedStreamHolding::Attached] {
                let refusal = refusal_of(runner.run_stream(project.run_request(holding)));

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
                if state != "kept" {
                    assert!(refusal.contains(state), "{state}: {refusal}");
                }
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
    fn an_attached_run_of_a_kept_location_is_refused_naming_it_kept() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let mut stopped = a_kept_record_of(&project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        let refusal =
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Attached)));

        assert!(
            refusal.contains("is kept by this runtime stopped"),
            "{refusal}"
        );
        assert!(refusal.contains("`run -d` replaces it"), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            the_current_record_in(state_directory.path(), &project.stream_id_of("camera")),
            Some(stopped)
        );
    }

    #[test]
    #[serial]
    fn a_kept_run_whose_compile_fails_leaves_the_running_stream_loaded_and_its_record_untouched() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let record = a_kept_record_of(&project, "camera", serde_json::json!([]));
        let running =
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &record);
        project.fail_to_compile_printing("NameError: name cmaera is not defined");

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

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
    fn a_replace_whose_load_is_refused_and_whose_previous_stream_cannot_re_load_reports_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("camera"),
        );
        let camera = project.stream_id_of("camera");
        let gone_interpreter = project.path().join("relocated-venv").join("python");
        let previous_record = StreamRecord {
            interpreter: gone_interpreter.clone(),
            ..a_kept_record_of(&project, "camera", serde_json::json!([]))
        };
        let previous = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &previous_record,
        );

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("absent-port"), "{refusal}");
        assert!(refusal.contains("did not re-load"), "{refusal}");
        assert!(
            refusal.contains(&gone_interpreter.display().to_string()),
            "{refusal}"
        );
        assert!(previous.has_ended(), "the replace unloaded it first");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(previous_record),
            "a refused replace leaves the record as it was"
        );
    }

    #[test]
    #[serial]
    fn a_stopped_kept_stream_is_recorded_stopped_and_a_fresh_runtime_leaves_it_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            let running = a_kept_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_kept_record_of(&project, "camera", serde_json::json!([])),
            );

            let stopped = runner.stop_stream(&camera).expect("the kept stream stops");

            assert_eq!(
                stopped,
                StreamStopOutcome {
                    stream_id: camera.clone(),
                    kept: true,
                    stop_not_recorded_because: None,
                }
            );
            assert!(running.has_ended());
            assert!(runner.ids_of_the_loaded_streams().is_empty());
            let refusal = refusal_of(runner.stop_stream(&camera));
            assert!(refusal.contains("already stopped"), "{refusal}");
        }

        let fresh_runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        assert!(
            fresh_runner
                .reload_every_kept_stream_neither_stopped_nor_failed()
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
    fn stopping_a_loaded_kept_stream_whose_record_is_unreadable_unloads_it_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_kept_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();

        for_each_unreadable_record_of(
            state_directory.path(),
            &camera,
            |record_path, unreadable_bytes| {
                let running = runner
                    .load_a_stream_with_the_owners_rulings(
                        OneStreamAStreamActionLoads::recorded_by(&record),
                    )
                    .expect("the kept stream loads");

                let stopped = runner
                    .stop_stream(&camera)
                    .expect("the stop succeeds, since the stream is unloaded");

                assert_eq!(stopped.stream_id, camera);
                assert!(stopped.kept);
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
    fn stopping_a_loaded_kept_stream_whose_record_cannot_be_written_still_unloads_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
        );

        let stopped = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            runner.stop_stream(&camera)
        }
        .expect("the stop succeeds, since the stream is unloaded");

        assert!(stopped.kept);
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
    fn raising_a_port_of_a_loaded_kept_stream_whose_record_is_unreadable_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
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
                    the_exposures_graph_renders_for(&kept),
                    serde_json::json!([{"node": "source", "port": "out1", "level": level}]),
                    "the level changed live"
                );
                assert_eq!(std::fs::read(record_path).unwrap(), unreadable_bytes);
            },
        );
    }

    #[test]
    #[serial]
    fn raising_a_port_of_a_loaded_kept_stream_whose_record_cannot_be_written_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
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
            the_exposures_graph_renders_for(&kept),
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
    fn restricting_a_port_of_a_loaded_kept_stream_whose_record_cannot_be_written_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let record = a_kept_record_of(
            &project,
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
        );
        let kept = a_kept_stream_loaded_without_its_start(&runner, &streams_directory, &record);

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
                the_exposures_graph_renders_for(&kept),
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
    fn restricting_a_port_of_a_loaded_kept_stream_whose_record_is_unreadable_is_refused() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(
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
                    the_exposures_graph_renders_for(&kept),
                    serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
                    "the live level is unchanged"
                );
                assert_eq!(std::fs::read(record_path).unwrap(), unreadable_bytes);
            },
        );
    }

    #[test]
    #[serial]
    fn a_loaded_kept_stream_whose_record_is_gone_raises_live_naming_the_record_and_never_restricts()
    {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
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
            the_exposures_graph_renders_for(&kept),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            "the raise held live and the refused restriction changed nothing"
        );
        assert!(!record_path.exists());
    }

    #[test]
    #[serial]
    fn starting_a_kept_stream_whose_record_is_unreadable_is_refused_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let mut stopped = a_kept_record_of(&project, "camera", serde_json::json!([]));
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
    fn stopping_an_attached_stream_unloads_it_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        let stopped = runner
            .stop_stream(&project.stream_id_of("camera"))
            .expect("the attached stream stops");

        assert!(!stopped.kept);
        assert_eq!(stopped.stop_not_recorded_because, None);
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn an_action_naming_no_stream_the_runtime_holds_is_refused_naming_the_loaded_and_kept_ones() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        an_attached_stream_loaded_without_its_start(&runner, &project, "attached-one");
        let mut stopped = a_kept_record_of(&project, "kept-one", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let attached_one = project.stream_id_of("attached-one");
        let kept_one = project.stream_id_of("kept-one");

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
            assert!(refusal.contains(&attached_one), "{refusal}");
            assert!(
                refusal.contains(&format!("{kept_one} (stopped)")),
                "{refusal}"
            );
        }
        let start_refusal = refusal_of(runner.start_stream(&attached_one));
        assert!(start_refusal.contains("attached"), "{start_refusal}");
        let start_refusal = refusal_of(runner.start_stream("absent"));
        assert!(start_refusal.contains(&kept_one), "{start_refusal}");
    }

    #[test]
    #[serial]
    fn a_stream_action_finds_a_stream_by_its_exact_id_alone() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let attached = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");
        let camera = project.stream_id_of("camera");
        let mut stopped = a_kept_record_of(&project, "microphone", serde_json::json!([]));
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

            assert!(refusal.contains("no kept stream with the id"), "{refusal}");
        }
        assert!(!attached.has_ended());
        assert_eq!(runner.ids_of_the_loaded_streams(), [camera]);
        assert_eq!(
            the_current_record_in(state_directory.path(), &microphone),
            Some(stopped)
        );
    }

    #[test]
    #[serial]
    fn removing_a_kept_stream_unloads_it_and_forgets_its_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
        );
        let mut stopped = a_kept_record_of(&project, "microphone", serde_json::json!([]));
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
    fn renaming_a_kept_stream_rewrites_its_record_then_its_table_entry_and_a_restart_keeps_the_name()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let microphone = project.stream_id_of("microphone");
        {
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            let running = a_kept_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_kept_record_of(&project, "camera", serde_json::json!([])),
            );
            let mut stopped = a_kept_record_of(&project, "microphone", serde_json::json!([]));
            stopped.stopped = true;
            records_in(state_directory.path()).write(&stopped).unwrap();

            let renamed = runner
                .rename_stream(&camera, "Hello camera")
                .expect("the loaded kept stream is renamed");
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

        let fresh_runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

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
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            &streams_directory,
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
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
    fn renaming_a_loaded_kept_stream_whose_record_is_unreadable_is_refused_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
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
    fn renaming_an_attached_stream_shows_it_under_the_name_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let attached = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        runner
            .rename_stream(&project.stream_id_of("camera"), "Hello camera")
            .expect("the attached stream is renamed");

        assert_eq!(attached.display_name(), "Hello camera");
        assert_eq!(runner.list_streams()[0].display_name, "Hello camera");
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_rename_is_refused_for_a_name_showing_nothing_an_id_held_by_no_stream_and_an_earlier_record()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let attached = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");
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
        assert_eq!(attached.display_name(), "camera");
    }

    #[test]
    #[serial]
    fn the_listing_names_each_stream_by_id_display_name_and_location_and_one_not_loaded_counts_no_node()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        an_attached_stream_loaded_without_its_start(&runner, &project, "a-attached");
        a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "b-kept", serde_json::json!([])),
        );
        let mut stopped = a_kept_record_of(&project, "c-stopped", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        records_in(state_directory.path())
            .write(&a_kept_record_of(
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
        };
        let mut expected = [
            listing_of("a-attached", StreamListingState::Attached, Some(1)),
            listing_of("b-kept", StreamListingState::Kept, Some(1)),
            listing_of("c-stopped", StreamListingState::Stopped, None),
            listing_of("d-not-re-loaded", StreamListingState::Kept, None),
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
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        runner.count_the_previous_runtime_runs_end_against_the_kept_streams(&a_crash_pinned_on(&[
            "main",
        ]));
        let reloads = runner.reload_every_kept_stream_neither_stopped_nor_failed();
        let listing = runner.list_streams();

        assert_eq!(
            std::fs::read(&earlier_record_path).unwrap(),
            earlier_record_bytes,
            "neither the crash count nor the re-load rewrites it"
        );
        let [
            KeptStreamReloadAtTheStart::SkippedAsFailed {
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
    fn a_port_of_a_loaded_kept_stream_changes_live_and_its_ruling_is_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(
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
            the_exposures_graph_renders_for(&kept),
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
    fn a_port_of_an_attached_stream_changes_live_and_nothing_is_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let attached = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

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
            "an attached stream's level is never recorded, and that is no failure"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&attached),
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}])
        );
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_port_of_a_stopped_stream_is_recorded_once_the_recorded_graph_holds_its_node() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let mut stopped = a_kept_record_of(&project, "camera", serde_json::json!([]));
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
    fn a_kept_streams_rulings_hold_at_its_load_and_one_on_a_port_its_node_lacks_is_skipped() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let mut record = a_kept_record_of(
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
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        assert_eq!(
            the_exposures_graph_renders_for(&loaded),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}])
        );
        assert_eq!(loaded.holding(), LoadedStreamHolding::Kept);
        assert_eq!(loaded.stream_id(), project.stream_id_of("camera"));
        assert_eq!(loaded.display_name(), "camera");
        assert_eq!(
            loaded.stream_location(),
            Some(&project.location_of("camera"))
        );
    }

    #[test]
    #[serial]
    fn a_recorded_restriction_is_the_level_a_kept_stream_holds_before_it_starts() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let mut record = a_kept_record_of(
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
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

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
    fn reloading_reports_each_kept_stream_whose_interpreter_is_gone_by_id_and_keeps_going() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        for function_name in ["a-camera", "c-microphone"] {
            records
                .write(&a_kept_record_of(
                    &project,
                    function_name,
                    serde_json::json!([]),
                ))
                .unwrap();
        }
        let malformed = state_directory.path().join("b-malformed.json");
        std::fs::write(&malformed, b"{").unwrap();
        let mut stopped = a_kept_record_of(&project, "d-stopped", serde_json::json!([]));
        stopped.stopped = true;
        records.write(&stopped).unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_every_kept_stream_neither_stopped_nor_failed();

        assert_eq!(reloads.len(), 3, "{reloads:?}");
        let mut ids_not_reloaded = Vec::new();
        let mut unreadable_paths = Vec::new();
        for reload in &reloads {
            match reload {
                KeptStreamReloadAtTheStart::NotReloaded { stream_id, refusal } => {
                    let refusal = refusal.to_string();
                    assert!(refusal.contains(stream_id.as_str()), "{refusal}");
                    assert!(
                        refusal.contains(&project.interpreter().display().to_string()),
                        "{refusal}"
                    );
                    assert!(refusal.contains("uv sync"), "{refusal}");
                    ids_not_reloaded.push(stream_id.clone());
                }
                KeptStreamReloadAtTheStart::RecordUnreadable(unreadable) => {
                    unreadable_paths.push(unreadable.path.clone());
                }
                other => panic!("only the two kept streams and the malformed record: {other:?}"),
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
        assert_eq!(records.read_every().len(), 4, "every record is kept");
    }

    #[test]
    #[serial]
    fn a_kept_stream_stopped_or_removed_between_the_listing_and_its_reload_stays_unloaded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let records = records_in(state_directory.path());
        for function_name in ["a-stopped", "b-removed"] {
            records
                .write(&a_kept_record_of(
                    &project,
                    function_name,
                    serde_json::json!([]),
                ))
                .unwrap();
        }
        let listed = records.read_every();
        let mut stopped_since =
            the_record_of(state_directory.path(), &project.stream_id_of("a-stopped"));
        stopped_since.stopped = true;
        records.write(&stopped_since).unwrap();
        assert!(records.remove(&project.stream_id_of("b-removed")).unwrap());

        let reloads = runner.reload_each_kept_stream_listed(&records, listed);

        assert!(reloads.is_empty(), "neither is reported: {reloads:?}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            records.read_every(),
            vec![Ok(StreamRecordAsRead::Current(stopped_since))]
        );
    }

    #[test]
    #[serial]
    fn a_kept_record_unreadable_by_its_reload_is_reported_unreadable_by_its_path() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        records
            .write(&a_kept_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let listed = records.read_every();
        let record_path_malformed_since_the_listing = records
            .record_path_of(&project.stream_id_of("camera"))
            .unwrap();
        std::fs::write(&record_path_malformed_since_the_listing, b"{").unwrap();

        let reloads = runner.reload_each_kept_stream_listed(&records, listed);

        let [KeptStreamReloadAtTheStart::RecordUnreadable(unreadable)] = reloads.as_slice() else {
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
    fn a_kept_stream_loaded_between_the_listing_and_its_reload_is_left_as_it_is() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let record = a_kept_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();
        let listed = records_in(state_directory.path()).read_every();
        let loaded_since =
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

        let reloads =
            runner.reload_each_kept_stream_listed(&records_in(state_directory.path()), listed);

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
    fn an_attached_stream_is_unloaded_by_its_own_tag_and_never_one_that_took_its_id_since() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        let first = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        assert!(runner.unload_the_attached_stream_if_still_the_same(&camera, first.stream_tag()));
        assert!(first.has_ended());
        assert!(runner.ids_of_the_loaded_streams().is_empty());

        let second = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        assert!(!runner.unload_the_attached_stream_if_still_the_same(&camera, first.stream_tag()));
        assert!(!runner.unload_the_attached_stream_if_still_the_same("absent", first.stream_tag()));
        assert!(!second.has_ended());
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&camera)
                .unwrap()
                .stream_tag(),
            second.stream_tag()
        );
    }

    /// The attached stream of `project`'s function `function_name` loaded
    /// under its id, not started, remembering an attached run compiled it to
    /// `run_source_graph`.
    fn an_attached_run_loaded_without_its_start(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        function_name: &str,
        run_source_graph: serde_json::Value,
    ) -> Arc<LoadedStreamInThisRuntime> {
        let stream = an_attached_stream_loaded_without_its_start(runner, project, function_name);
        stream.remember_the_attached_stream_run_source(AttachedStreamRunSource {
            graph_json: run_source_graph,
            stream_environment: project.stream_environment(),
            re_loaded_in_place_of: Vec::new(),
        });
        stream
    }

    #[test]
    #[serial]
    fn an_attached_run_whose_compile_fails_leaves_the_callers_stream_loaded_and_attached() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let running = an_attached_run_loaded_without_its_start(
            &runner,
            &project,
            "camera",
            the_graph_of_a_function_named("camera", serde_json::json!([])),
        );
        project.fail_to_compile_printing("SyntaxError: invalid syntax");

        let refusal = refusal_of(runner.run_stream(
            project.attached_run_request_of_the_caller_holding(&[running.stream_tag()]),
        ));

        assert!(refusal.contains("SyntaxError"), "{refusal}");
        assert!(!running.has_ended());
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&camera)
                .unwrap()
                .stream_tag(),
            running.stream_tag(),
            "the running stream is the one loaded before the save"
        );
        assert!(runner.unload_the_attached_stream_if_still_the_same(&camera, running.stream_tag()));
    }

    #[test]
    #[serial]
    fn an_attached_run_of_a_location_another_caller_attached_is_refused_and_leaves_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let running = an_attached_run_loaded_without_its_start(
            &runner,
            &project,
            "camera",
            the_graph_of_a_function_named("camera", serde_json::json!([])),
        );
        let another_callers_stream =
            an_attached_stream_loaded_without_its_start(&runner, &project, "preview");

        for stream_tags_of_another_caller in [Vec::new(), vec![another_callers_stream.stream_tag()]]
        {
            let refusal = refusal_of(runner.run_stream(
                project.attached_run_request_of_the_caller_holding(&stream_tags_of_another_caller),
            ));

            assert!(refusal.contains("already loaded"), "{refusal}");
            assert!(refusal.contains(&format!("`{camera}`")), "{refusal}");
            assert!(
                refusal.contains(&project.location_of("camera").to_string()),
                "{refusal}"
            );
            assert!(refusal.contains("stop it first"), "{refusal}");
        }
        let refusal = refusal_of(runner.run_stream(RunStreamRequest {
            stream_tags_attached_to_the_caller: vec![running.stream_tag()],
            ..project.run_request(LoadedStreamHolding::Kept)
        }));
        assert!(
            refusal.contains("already loaded"),
            "a kept run never replaces an attached stream: {refusal}"
        );
        assert!(!running.has_ended());
        assert_eq!(
            runner
                .loaded_stream_of_the_id(&camera)
                .unwrap()
                .stream_tag(),
            running.stream_tag()
        );
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_refused_attached_replace_whose_previous_graph_cannot_re_load_reports_both() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("camera"),
        );
        let previous = an_attached_run_loaded_without_its_start(
            &runner,
            &project,
            "camera",
            the_graph_of_a_function_named(
                "camera",
                serde_json::json!([{"node": "source", "port": "gone-port", "level": "public"}]),
            ),
        );

        let refusal = refusal_of(runner.run_stream(
            project.attached_run_request_of_the_caller_holding(&[previous.stream_tag()]),
        ));

        assert!(refusal.contains("absent-port"), "{refusal}");
        assert!(refusal.contains("did not re-load"), "{refusal}");
        assert!(refusal.contains("gone-port"), "{refusal}");
        assert!(previous.has_ended(), "the replace unloaded it first");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_re_load_after_a_refused_replace_is_held_by_the_attachment_of_the_load_it_replaced() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::with_no_venv();
        let replaced_load_tag =
            an_attached_stream_loaded_without_its_start(&runner, &project, "preview").stream_tag();
        let re_loaded = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");
        re_loaded.remember_the_attached_stream_run_source(AttachedStreamRunSource {
            graph_json: the_graph_of_a_function_named("camera", serde_json::json!([])),
            stream_environment: project.stream_environment(),
            re_loaded_in_place_of: vec![replaced_load_tag],
        });

        assert!(re_loaded.is_held_by_the_attachment_of(re_loaded.stream_tag()));
        assert!(re_loaded.is_held_by_the_attachment_of(replaced_load_tag));
        assert!(runner.unload_the_attached_stream_if_still_the_same(
            &project.stream_id_of("camera"),
            replaced_load_tag
        ));
        assert!(re_loaded.has_ended());
    }

    #[test]
    #[serial]
    fn a_run_of_another_function_replaces_no_stream_whatever_the_caller_holds() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(
            the_graph_of_a_function_exposing_a_port_its_node_lacks("studio"),
        );
        let attached = an_attached_run_loaded_without_its_start(
            &runner,
            &project,
            "camera",
            the_graph_of_a_function_named("camera", serde_json::json!([])),
        );
        let kept_record = a_kept_record_of(&project, "kept-camera", serde_json::json!([]));
        let kept =
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &kept_record);

        for (run_request, what_the_run_is) in [
            (
                project.attached_run_request_of_the_caller_holding(&[]),
                "another caller's attached run",
            ),
            (
                RunStreamRequest {
                    stream_tags_attached_to_the_caller: vec![
                        attached.stream_tag(),
                        kept.stream_tag(),
                    ],
                    ..project.run_request(LoadedStreamHolding::Kept)
                },
                "a kept run",
            ),
            (
                project.attached_run_request_of_the_caller_holding(&[kept.stream_tag()]),
                "an attached run by a caller naming the kept stream's tag",
            ),
            (
                project.attached_run_request_of_the_caller_holding(&[attached.stream_tag()]),
                "the caller's run of another function",
            ),
            (
                RunStreamRequest {
                    display_name: Some("camera".to_string()),
                    ..project.attached_run_request_of_the_caller_holding(&[attached.stream_tag()])
                },
                "the caller's run giving the display name of the stream it holds",
            ),
        ] {
            let refusal = refusal_of(runner.run_stream(run_request));

            assert!(
                refusal.contains("absent-port"),
                "{what_the_run_is}: {refusal}"
            );
            assert!(
                !refusal.contains("did not re-load"),
                "{what_the_run_is} replaced nothing: {refusal}"
            );
            assert!(!attached.has_ended(), "{what_the_run_is}");
            assert!(!kept.has_ended(), "{what_the_run_is}");
        }
        let mut expected_ids = vec![
            project.stream_id_of("camera"),
            project.stream_id_of("kept-camera"),
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
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let main_worktree = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "main",
            serde_json::json!([]),
        ));
        let second_worktree = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "main",
            serde_json::json!([]),
        ));

        let first = runner
            .run_stream(main_worktree.run_request(LoadedStreamHolding::Kept))
            .expect("the first worktree's stream runs");
        let second = runner
            .run_stream(second_worktree.run_request(LoadedStreamHolding::Kept))
            .expect("the second worktree's stream runs beside it");

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
            .run_stream(main_worktree.run_request(LoadedStreamHolding::Kept))
            .expect("the first worktree runs again");
        assert_eq!(rerun.stream_id, first.stream_id, "a re-run keeps its id");
        assert!(rerun.replaced_the_kept_record);
        assert_eq!(records_in(state_directory.path()).read_every().len(), 2);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_starts_the_stream_under_its_locations_id_and_records_it_when_kept_and_not_when_attached()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let kept_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let attached_project = ProjectWithAStubCompile::compiling_warning(
            the_graph_of_a_function_named("preview", serde_json::json!([])),
            &["tatolab: the cross-floor check found 1 thing binding this app to one floor."],
        );
        let camera = kept_project.stream_id_of("camera");
        let preview = attached_project.stream_id_of("preview");

        let kept = runner
            .run_stream(kept_project.run_request(LoadedStreamHolding::Kept))
            .expect("the kept stream runs");
        let attached = runner
            .run_stream(attached_project.run_request(LoadedStreamHolding::Attached))
            .expect("the attached stream runs");

        assert_eq!(kept.stream_id, camera);
        assert_eq!(
            kept.display_name, "camera",
            "the function's name by default"
        );
        assert_eq!(kept.stream_location, kept_project.location_of("camera"));
        assert_eq!(kept.node_count, 1);
        assert!(!kept.replaced_the_kept_record);
        assert_eq!(kept.compile_warnings, Vec::<String>::new());
        assert_eq!(attached.stream_id, preview);
        assert_eq!(attached.display_name, "preview");
        assert_eq!(
            attached.compile_warnings,
            ["tatolab: the cross-floor check found 1 thing binding this app to one floor."]
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(a_kept_record_of(
                &kept_project,
                "camera",
                serde_json::json!([])
            ))
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
        let mut expected_listing = [
            StreamListing {
                stream_id: camera.clone(),
                display_name: "camera".to_string(),
                stream_location: Some(kept_project.location_of("camera")),
                state: StreamListingState::Kept,
                node_count: Some(1),
                failed_because: None,
            },
            StreamListing {
                stream_id: preview.clone(),
                display_name: "preview".to_string(),
                stream_location: Some(attached_project.location_of("preview")),
                state: StreamListingState::Attached,
                node_count: Some(1),
                failed_because: None,
            },
        ];
        expected_listing.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        assert_eq!(runner.list_streams(), expected_listing);
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(
            running.status(),
            crate::core::runtime::RuntimeStatus::Started
        );
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
    fn a_kept_run_of_the_records_location_replaces_it_under_its_id_keeping_its_renamed_display_name_and_rulings()
     {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
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

        let replaced = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .expect("the kept stream is replaced");

        assert_eq!(
            first.display_name, "camera",
            "a new stream is shown under its function's name"
        );
        assert!(replaced.replaced_the_kept_record);
        assert_eq!(replaced.stream_id, camera);
        assert_eq!(
            replaced.display_name, "Front camera",
            "a run giving no display name keeps the renamed one"
        );
        assert_eq!(
            replaced.compile_warnings,
            ["a warning of the replacing compile"]
        );
        assert_ne!(replaced.stream_tag, first.stream_tag);
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(running.stream_tag(), replaced.stream_tag);
        assert_eq!(running.display_name(), "Front camera");
        assert_eq!(
            the_exposures_graph_renders_for(&running),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}]),
            "the owner's internal ruling wins over the changed function's public"
        );
        let recorded = the_record_of(state_directory.path(), &camera);
        assert_eq!(recorded.graph, changed_source);
        assert_eq!(recorded.display_name, "Front camera");
        assert_eq!(
            recorded.exposure_rulings,
            [OwnerExposureRuling {
                node: "source".to_string(),
                port: "out1".to_string(),
                level: OutputPortExposureLevel::Internal,
            }]
        );

        let renamed = runner
            .run_stream(RunStreamRequest {
                display_name: Some("Desk camera".to_string()),
                ..project.run_request(LoadedStreamHolding::Kept)
            })
            .expect("the kept stream is replaced again");

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
    fn a_stopped_kept_stream_starts_again_and_re_loads_at_a_restart_with_its_rulings_applied() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
        ));
        let camera = project.stream_id_of("camera");
        {
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            runner
                .run_stream(project.run_request(LoadedStreamHolding::Kept))
                .unwrap();
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

        let restarted = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let reloads = restarted.reload_every_kept_stream_neither_stopped_nor_failed();

        assert!(
            matches!(
                reloads.as_slice(),
                [KeptStreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
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
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let records = records_in(state_directory.path());
        records
            .write(&a_kept_record_of(
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

        let reloads = runner.reload_each_kept_stream_listed(&records, listed);

        assert!(
            matches!(
                reloads.as_slice(),
                [KeptStreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
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
    fn a_run_whose_start_is_refused_unloads_the_stream_it_loaded_and_records_nothing() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        runner
            .install_setup_hook(|_| Err(Error::Configuration("the setup hook refuses".into())))
            .unwrap();
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("the setup hook refuses"), "{refusal}");
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_kept_run_whose_record_is_not_written_unloads_the_stream_it_started() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)))
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
    fn a_replace_whose_load_is_refused_after_the_unload_re_loads_the_previous_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
        let previous_record = the_record_of(state_directory.path(), &camera);
        project.compile_to(the_graph_of_a_function_exposing_a_port_its_node_lacks(
            "camera",
        ));

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("absent-port"), "{refusal}");
        assert!(!refusal.contains("did not re-load"), "{refusal}");
        let restored = runner
            .loaded_stream_of_the_id(&camera)
            .expect("the previous record is loaded again");
        assert_ne!(restored.stream_tag(), first.stream_tag);
        assert_eq!(
            restored.status(),
            crate::core::runtime::RuntimeStatus::Started
        );
        assert_eq!(restored.holding(), LoadedStreamHolding::Kept);
        assert_eq!(
            the_exposures_graph_renders_for(&restored),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}])
        );
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(previous_record)
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_replace_whose_record_is_not_written_unloads_the_new_stream_and_re_loads_the_previous() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&streams_directory));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
        let previous_record = the_record_of(&streams_directory, &camera);
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}]),
        ));

        let refusal = {
            let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        assert!(!refusal.contains("did not re-load"), "{refusal}");
        let restored = runner
            .loaded_stream_of_the_id(&camera)
            .expect("the previous record is loaded again");
        assert_ne!(restored.stream_tag(), first.stream_tag);
        assert_eq!(
            the_exposures_graph_renders_for(&restored),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            "the stream loaded is the previous record's, not the one whose record was refused"
        );
        assert_eq!(
            the_current_record_in(&streams_directory, &camera),
            Some(previous_record)
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn an_attached_run_of_the_stream_its_caller_attached_replaces_it() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let camera = project.stream_id_of("camera");
        let first = runner
            .run_stream(RunStreamRequest {
                display_name: Some("Front camera".to_string()),
                ..project.run_request(LoadedStreamHolding::Attached)
            })
            .expect("the attached stream runs");
        let first_stream = runner.loaded_stream_of_the_id(&camera).unwrap();
        project.compile_to_warning(
            the_graph_of_a_function_named(
                "camera",
                serde_json::json!([{"node": "source", "port": "out2", "level": "private"}]),
            ),
            &["a warning of the replacing compile"],
        );

        let replaced = runner
            .run_stream(project.attached_run_request_of_the_caller_holding(&[first.stream_tag]))
            .expect("the attached stream is replaced");

        assert_ne!(replaced.stream_tag, first.stream_tag);
        assert_eq!(replaced.stream_id, camera);
        assert_eq!(replaced.display_name, "Front camera");
        assert!(!replaced.replaced_the_kept_record);
        assert_eq!(
            replaced.compile_warnings,
            ["a warning of the replacing compile"]
        );
        assert!(first_stream.has_ended());
        let running = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert_eq!(running.stream_tag(), replaced.stream_tag);
        assert_eq!(running.holding(), LoadedStreamHolding::Attached);
        assert_eq!(
            running.status(),
            crate::core::runtime::RuntimeStatus::Started
        );
        assert_eq!(
            the_exposures_graph_renders_for(&running),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}])
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn an_attached_replace_whose_load_is_refused_re_loads_the_previous_graph_for_the_same_caller() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let camera = project.stream_id_of("camera");
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Attached))
            .unwrap();
        project.compile_to(the_graph_of_a_function_exposing_a_port_its_node_lacks(
            "camera",
        ));

        let refusal =
            refusal_of(runner.run_stream(
                project.attached_run_request_of_the_caller_holding(&[first.stream_tag]),
            ));

        assert!(refusal.contains("absent-port"), "{refusal}");
        assert!(!refusal.contains("did not re-load"), "{refusal}");
        let re_loaded = runner
            .loaded_stream_of_the_id(&camera)
            .expect("the previous graph is loaded again");
        assert_ne!(re_loaded.stream_tag(), first.stream_tag);
        assert_eq!(re_loaded.holding(), LoadedStreamHolding::Attached);
        assert_eq!(
            re_loaded.status(),
            crate::core::runtime::RuntimeStatus::Started
        );
        assert_eq!(
            the_exposures_graph_renders_for(&re_loaded),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}])
        );
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        runner
            .run_stream(project.attached_run_request_of_the_caller_holding(&[first.stream_tag]))
            .expect("the caller's next run replaces the re-loaded stream");
        assert!(re_loaded.has_ended());
        let replacing = runner.loaded_stream_of_the_id(&camera).unwrap();
        assert!(
            !runner.unload_the_attached_stream_if_still_the_same(&camera, first.stream_tag),
            "the first load's attachment holds the re-load, never the stream that replaced it"
        );
        assert!(!replacing.has_ended());
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
    fn a_kept_stream_implicated_in_two_crashes_in_a_row_is_failed_naming_the_last_cause() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(serde_json::json!({}));
        for function_name in ["crasher", "steady"] {
            records_in(state_directory.path())
                .write(&a_kept_record_of(
                    &project,
                    function_name,
                    serde_json::json!([]),
                ))
                .unwrap();
        }
        let crasher = project.stream_id_of("crasher");
        let steady = project.stream_id_of("steady");
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        let failed_by_the_first = runner
            .count_the_previous_runtime_runs_end_against_the_kept_streams(&a_crash_pinned_on(&[
                &crasher,
            ]));

        assert!(failed_by_the_first.is_empty(), "{failed_by_the_first:?}");
        let crasher_record = the_record_of(state_directory.path(), &crasher);
        assert_eq!(crasher_record.runtime_crashes_in_a_row_implicating_it, 1);
        assert!(!crasher_record.is_failed());

        let failed_by_the_second = runner
            .count_the_previous_runtime_runs_end_against_the_kept_streams(&a_crash_pinned_on(&[
                &crasher,
            ]));

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
                (steady, StreamListingState::Kept, false),
            ]
        );
    }

    #[test]
    #[serial]
    fn a_clean_end_or_a_crash_pinned_elsewhere_between_two_crashes_resets_the_count() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::compiling(serde_json::json!({}));
        records_in(state_directory.path())
            .write(&a_kept_record_of(
                &project,
                "crasher",
                serde_json::json!([]),
            ))
            .unwrap();
        let crasher = project.stream_id_of("crasher");
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
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
            runner.count_the_previous_runtime_runs_end_against_the_kept_streams(
                &a_crash_pinned_on(&[&crasher]),
            );
            runner.count_the_previous_runtime_runs_end_against_the_kept_streams(
                &previous_run_between_the_crashes,
            );
            assert_eq!(
                the_record_of(state_directory.path(), &crasher)
                    .runtime_crashes_in_a_row_implicating_it,
                0,
                "{previous_run_between_the_crashes:?} did not reset the count"
            );
            let failed = runner.count_the_previous_runtime_runs_end_against_the_kept_streams(
                &a_crash_pinned_on(&[&crasher]),
            );

            assert!(
                failed.is_empty(),
                "{previous_run_between_the_crashes:?}: {failed:?}"
            );
            assert!(!the_record_of(state_directory.path(), &crasher).is_failed());
            runner.count_the_previous_runtime_runs_end_against_the_kept_streams(
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
        let mut failed = a_kept_record_of(&project, "crasher", serde_json::json!([]));
        failed.failed_because = Some("it crashed the runtime".to_string());
        failed.runtime_crashes_in_a_row_implicating_it = 2;
        records_in(state_directory.path()).write(&failed).unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        runner.count_the_previous_runtime_runs_end_against_the_kept_streams(
            &HowThePreviousRuntimeRunEnded::Cleanly,
        );
        let reloads = runner.reload_every_kept_stream_neither_stopped_nor_failed();

        assert_eq!(the_record_of(state_directory.path(), &crasher), failed);
        assert!(
            matches!(
                reloads.as_slice(),
                [KeptStreamReloadAtTheStart::SkippedAsFailed { stream_id, failed_because }]
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
    fn a_kept_stream_whose_venv_is_gone_is_failed_at_the_start_naming_its_interpreter() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let camera = project.stream_id_of("camera");
        records_in(state_directory.path())
            .write(&a_kept_record_of(&project, "camera", serde_json::json!([])))
            .unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_every_kept_stream_neither_stopped_nor_failed();

        assert!(
            matches!(
                reloads.as_slice(),
                [KeptStreamReloadAtTheStart::NotReloaded { stream_id, .. }] if *stream_id == camera
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
        let fresh_runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        assert!(matches!(
            fresh_runner
                .reload_every_kept_stream_neither_stopped_nor_failed()
                .as_slice(),
            [KeptStreamReloadAtTheStart::SkippedAsFailed { .. }]
        ));
    }

    #[test]
    #[serial]
    fn a_refused_retry_of_a_failed_stream_keeps_it_failed_for_the_retrys_reason_its_count_reset() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let crasher = project.stream_id_of("crasher");
        let mut failed = a_kept_record_of(&project, "crasher", serde_json::json!([]));
        failed.failed_because = Some("it crashed the runtime".to_string());
        failed.runtime_crashes_in_a_row_implicating_it = 2;
        records_in(state_directory.path()).write(&failed).unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        let refusal = refusal_of(runner.start_stream(&crasher));

        assert!(refusal.contains("interpreter"), "{refusal}");
        let retried = the_record_of(state_directory.path(), &crasher);
        assert_eq!(retried.runtime_crashes_in_a_row_implicating_it, 0);
        let failed_because = retried.failed_because.expect("the stream stays failed");
        assert!(
            failed_because.contains("`start` retried it"),
            "{failed_because}"
        );
        assert!(failed_because.contains("interpreter"), "{failed_because}");
    }
}
