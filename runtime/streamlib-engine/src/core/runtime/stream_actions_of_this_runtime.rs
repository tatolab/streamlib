// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions a runtime takes for its owner: run a project's stream
//! function, attached or kept; stop, start and remove a stream; list what the
//! runtime holds; set a port's exposure; re-load the kept streams at start;
//! and unload an attached stream when what attached it goes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use super::runtime::{
    a_stream_name_already_loaded_refusal, request_a_streams_shutdown_and_wait_until_it_has_ended,
    the_cast_name_of_the_stream_a_load_names,
};
use super::{
    KeptStreamRecord, KeptStreamRecordReadFailure, KeptStreamRecordsInTheStateDirectory,
    LoadedStreamInThisRuntime, LoadedStreamTag, OptionsForLoadingOneStream, OwnerExposureRuling,
    OwnerExposureRulingsSplitAroundTheLoad, Runner, StreamEnvironment,
    StreamLoadObservingMachineShutdownRequests,
    compile_the_stream_function_in_the_projects_interpreter,
    the_owners_exposure_rulings_split_around_the_load,
};
use crate::core::graph::{
    OutputPortExposureLevel, cast_exposed_name_to_url_safe, names_listed_for_a_refusal,
    node_names_listed_for_a_refusal,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::{Error, Result};

/// How a loaded stream is held: kept by the runtime, or attached to what
/// loaded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
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
    /// The name to load the stream as, overriding the function's.
    pub stream_name: Option<String>,
    /// Whether the runtime keeps the stream, recording it in its state
    /// directory, or it is attached to what ran it.
    pub holding: LoadedStreamHolding,
}

/// A stream [`Runner::run_stream`] loaded and started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRunOutcome {
    /// The stream's URL-safe cast name.
    pub stream_name: String,
    /// The tag of this load of it, which an attachment unloads by.
    pub stream_tag: LoadedStreamTag,
    /// The project directory the compile reported.
    pub project_directory: PathBuf,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
    /// Whether the load replaced the kept stream of the same project and
    /// function.
    pub replaced_the_kept_record: bool,
    /// What the compile wrote to its standard error, line by line — the
    /// cross-floor check's warnings among it.
    pub compile_warnings: Vec<String>,
}

/// A stream [`Runner::stop_stream`] unloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStopOutcome {
    /// The stream's URL-safe cast name.
    pub stream_name: String,
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
    /// The stream's URL-safe cast name.
    pub stream_name: String,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
}

/// What [`Runner::remove_stream`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRemoveOutcome {
    /// The stream's URL-safe cast name.
    pub stream_name: String,
    /// Whether the stream was loaded and is now unloaded.
    pub unloaded: bool,
    /// Whether the stream was kept and its record is now gone.
    pub forgotten: bool,
}

/// The state one stream of [`Runner::list_streams`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamListingState {
    /// Loaded, and lives as long as what loaded it.
    Attached,
    /// Kept by the runtime: loaded, or recorded and not loaded because it
    /// could not re-load or has ended — `start_stream` retries it.
    Kept,
    /// Kept, and stopped by its owner.
    Stopped,
}

impl From<LoadedStreamHolding> for StreamListingState {
    fn from(holding: LoadedStreamHolding) -> Self {
        match holding {
            LoadedStreamHolding::Kept => Self::Kept,
            LoadedStreamHolding::Attached => Self::Attached,
        }
    }
}

impl std::fmt::Display for StreamListingState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Attached => "attached",
            Self::Kept => "kept",
            Self::Stopped => "stopped",
        })
    }
}

/// One stream the runtime holds, as [`Runner::list_streams`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamListing {
    /// The stream's URL-safe cast name.
    pub name: String,
    /// Whether it is attached, kept or stopped.
    pub state: StreamListingState,
    /// The stream's project directory.
    pub project_directory: PathBuf,
    /// How many nodes its loaded graph holds; `None` when it is not loaded.
    pub node_count: Option<usize>,
}

/// An output port's exposure [`Runner::expose_port`] set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPortExposureOutcome {
    /// The stream's URL-safe cast name.
    pub stream_name: String,
    /// The node, as the owner named it.
    pub node: String,
    /// The output port, as the owner named it.
    pub port: String,
    /// The level the port is at now, or will be at when the stream loads.
    pub level: OutputPortExposureLevel,
    /// Whether the level was recorded as the owner's ruling on a kept stream.
    pub recorded: bool,
    /// Why the level, changed live on a loaded kept stream, was not recorded
    /// as the owner's ruling; `None` when it was recorded, and for an
    /// attached stream, whose level is never recorded.
    pub ruling_not_recorded_because: Option<String>,
}

/// How a run refused for a name a loaded or kept stream holds is told to go on.
const RUN_THIS_ONE_UNDER_ANOTHER_NAME: &str = "load this one under another name with `--name`";

/// What [`Runner::reload_every_kept_stream_not_stopped`] did with one kept
/// stream's record; a stream it left unloaded because it is stopped, removed
/// or already loaded is not reported.
#[derive(Debug)]
pub enum KeptStreamReloadAtTheStart {
    /// The kept stream loaded and started.
    Reloaded {
        /// The stream's URL-safe cast name.
        stream_name: String,
    },
    /// The kept stream did not re-load, and its record is kept.
    NotReloaded {
        /// The stream's URL-safe cast name.
        stream_name: String,
        /// Why it did not re-load.
        refusal: Error,
    },
    /// A record that could not be read, left in place.
    RecordUnreadable(KeptStreamRecordReadFailure),
}

/// Where a runtime keeps its streams, and the lock that lets one stream action
/// run at a time.
#[derive(Default)]
pub(crate) struct StreamActionsOfTheEngine {
    kept_stream_records: OnceLock<KeptStreamRecordsInTheStateDirectory>,
    /// Held across an action's check, load and record — never across a
    /// compile, which runs a project's code for up to its bound.
    one_stream_action_at_a_time: Mutex<()>,
}

impl Runner {
    /// Keep this runtime's streams as records in `kept_streams_directory`,
    /// created owner-only when absent. Refused a second time; a runtime never
    /// given one keeps no stream.
    pub fn keep_streams_in_the_state_directory(&self, kept_streams_directory: &Path) -> Result<()> {
        if let Some(kept_stream_records) = self.kept_stream_records() {
            return Err(Error::Configuration(format!(
                "this runtime already keeps its streams in {}; it keeps them in one directory \
                 for its life",
                kept_stream_records.kept_streams_directory().display()
            )));
        }
        let kept_stream_records =
            KeptStreamRecordsInTheStateDirectory::open(kept_streams_directory)?;
        self.stream_actions
            .kept_stream_records
            .set(kept_stream_records)
            .map_err(|_| {
                Error::Configuration(format!(
                    "this runtime was handed a kept-streams directory twice at once; it keeps \
                     its streams in {} only",
                    kept_streams_directory.display()
                ))
            })
    }

    /// Compile the request's stream function in its project's interpreter,
    /// then load and start the graph, keeping it when the request says so.
    ///
    /// A name already loaded, or held by a kept record — stopped included —
    /// is refused naming the project that holds it, except a kept load of
    /// the record's own project and function, which replaces the record: the
    /// running stream is unloaded only once the compile succeeded, the new
    /// graph loads with the record's rulings, and a refused load re-loads the
    /// previous one.
    pub fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        let RunStreamRequest {
            project_directory,
            stream_function,
            stream_name,
            holding,
        } = request;
        let kept_stream_records = self.kept_stream_records();
        let records_to_keep_it_in = match (holding, kept_stream_records) {
            (LoadedStreamHolding::Attached, _) => None,
            (LoadedStreamHolding::Kept, Some(kept_stream_records)) => Some(kept_stream_records),
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
            stream_name.as_deref(),
            lend_directory,
        )?;
        let stream_name = the_cast_name_of_the_stream_a_load_names(
            stream_name.as_deref().or(compiled
                .graph_json
                .get("stream")
                .and_then(serde_json::Value::as_str)),
            "the compiled stream function",
        )?;
        let stream_environment = compiled.stream_environment;
        let graph_json = compiled.graph_json;
        let compile_warnings = compiled.compile_warnings;

        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_cast_name(&stream_name);
        let kept_record = match kept_stream_records {
            Some(kept_stream_records) => kept_stream_records.read(&stream_name)?,
            None => None,
        };
        match (kept_record, loaded) {
            (Some(kept_record), loaded) => match records_to_keep_it_in {
                Some(records_to_keep_it_in)
                    if kept_record.project_directory == stream_environment.project_directory
                        && kept_record.stream_function == stream_function
                        && loaded
                            .as_ref()
                            .is_none_or(|loaded| loaded.holding() == LoadedStreamHolding::Kept) =>
                {
                    self.replace_a_kept_stream(
                        records_to_keep_it_in,
                        kept_record,
                        loaded,
                        KeptStreamRecord::of_a_running_stream(
                            stream_name,
                            &stream_environment,
                            stream_function,
                            graph_json,
                        ),
                        compile_warnings,
                    )
                }
                _ => Err(Error::GraphError(format!(
                    "the stream name `{stream_name}` is held by a{} kept stream from {}; \
                     {RUN_THIS_ONE_UNDER_ANOTHER_NAME}",
                    if kept_record.stopped { " stopped" } else { "" },
                    kept_record.project_directory.display()
                ))),
            },
            (None, Some(loaded)) => Err(a_stream_name_already_loaded_refusal(
                &loaded,
                RUN_THIS_ONE_UNDER_ANOTHER_NAME,
            )),
            (None, None) => {
                let stream = self.load_and_start_a_stream(
                    &stream_name,
                    &graph_json,
                    &[],
                    stream_environment.clone(),
                    holding,
                )?;
                if let Some(records_to_keep_it_in) = records_to_keep_it_in {
                    let record = KeptStreamRecord::of_a_running_stream(
                        stream_name.clone(),
                        &stream_environment,
                        stream_function,
                        graph_json,
                    );
                    if let Err(write_refusal) = records_to_keep_it_in.write(&record) {
                        unload_a_stream_an_action_took_back(&stream, "its record was not written");
                        return Err(write_refusal);
                    }
                }
                Ok(StreamRunOutcome {
                    node_count: stream.node_count(),
                    stream_tag: stream.stream_tag(),
                    stream_name,
                    project_directory: stream_environment.project_directory,
                    replaced_the_kept_record: false,
                    compile_warnings,
                })
            }
        }
    }

    /// Unload the stream `stream_name` names; a kept one is recorded stopped
    /// first, so a restart leaves it unloaded. A loaded kept stream is
    /// unloaded even when its record cannot be read or written, and the
    /// outcome says why it was not recorded stopped. Refused — with nothing
    /// changed — naming the streams the runtime holds when it holds none of
    /// that name, and for a kept stream already stopped.
    pub fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_cast = self.the_cast_name_of_a_stream_an_action_names(stream_name)?;
        let loaded = self.loaded_stream_of_the_cast_name(&stream_cast);
        if let Some(attached) = loaded
            .as_ref()
            .filter(|loaded| loaded.holding() == LoadedStreamHolding::Attached)
        {
            unload_a_stream_an_action_took_back(attached, "its owner stopped it");
            return Ok(StreamStopOutcome {
                stream_name: stream_cast,
                kept: false,
                stop_not_recorded_because: None,
            });
        }
        let kept_record = match self.kept_record_of_the_cast_name(&stream_cast) {
            Ok(kept_record) => kept_record,
            Err(unreadable) => {
                let Some(loaded) = loaded else {
                    return Err(unreadable);
                };
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                return Ok(a_kept_stream_unloaded_and_not_recorded_stopped(
                    stream_cast,
                    &unreadable,
                ));
            }
        };
        match (loaded, kept_record) {
            (None, None) => Err(self.a_stream_this_runtime_does_not_hold(stream_name)),
            (None, Some(kept_record)) if kept_record.stopped => Err(Error::Runtime(format!(
                "the kept stream `{stream_cast}` is already stopped; `start` loads it again"
            ))),
            (None, Some(mut kept_record)) => {
                kept_record.stopped = true;
                self.write_a_kept_record(&kept_record)?;
                Ok(StreamStopOutcome {
                    stream_name: stream_cast,
                    kept: true,
                    stop_not_recorded_because: None,
                })
            }
            (Some(loaded), Some(mut kept_record)) => {
                kept_record.stopped = true;
                let recorded_stopped = self.write_a_kept_record(&kept_record);
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(match recorded_stopped {
                    Ok(()) => StreamStopOutcome {
                        stream_name: stream_cast,
                        kept: true,
                        stop_not_recorded_because: None,
                    },
                    Err(not_written) => {
                        a_kept_stream_unloaded_and_not_recorded_stopped(stream_cast, &not_written)
                    }
                })
            }
            (Some(loaded), None) => {
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(StreamStopOutcome {
                    stream_name: stream_cast,
                    kept: false,
                    stop_not_recorded_because: None,
                })
            }
        }
    }

    /// Load and start the kept stream `stream_name` names from its record,
    /// the owner's rulings applied, and record it not stopped. Refused for a
    /// stream already loaded, an attached one, and a name no record holds.
    pub fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_cast = self.the_cast_name_of_a_stream_an_action_names(stream_name)?;
        if let Some(loaded) = self.loaded_stream_of_the_cast_name(&stream_cast) {
            return Err(Error::Runtime(format!(
                "the stream `{stream_cast}` is already loaded in this runtime, {}; only a kept \
                 stream that is not loaded starts",
                loaded.holding()
            )));
        }
        let kept_record =
            self.kept_record_of_the_cast_name(&stream_cast)
                .map_err(|unreadable| {
                    Error::Runtime(format!(
                        "the kept stream `{stream_cast}` was not started: {unreadable}"
                    ))
                })?;
        let Some(mut kept_record) = kept_record else {
            return Err(Error::NotFound(format!(
                "no kept stream named `{stream_name}` is in this runtime, so there is none to \
                 start; an attached stream is loaded again with `run`. Kept: {}",
                self.kept_stream_names_listed_for_a_refusal()
            )));
        };
        let stream = self.load_a_kept_stream_from_its_record(&kept_record)?;
        if kept_record.stopped {
            kept_record.stopped = false;
            if let Err(write_refusal) = self.write_a_kept_record(&kept_record) {
                unload_a_stream_an_action_took_back(&stream, "its record was not written");
                return Err(write_refusal);
            }
        }
        Ok(StreamStartOutcome {
            node_count: stream.node_count(),
            stream_name: stream_cast,
        })
    }

    /// Forget the kept record of the stream `stream_name` names, then unload
    /// it if it is loaded. Refused when the runtime holds no stream of that
    /// name.
    pub fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_cast = self.the_cast_name_of_a_stream_an_action_names(stream_name)?;
        let loaded = self.loaded_stream_of_the_cast_name(&stream_cast);
        let forgotten = match self.kept_stream_records() {
            Some(kept_stream_records) => kept_stream_records.remove(&stream_cast)?,
            None => false,
        };
        if loaded.is_none() && !forgotten {
            return Err(self.a_stream_this_runtime_does_not_hold(stream_name));
        }
        if let Some(loaded) = &loaded {
            unload_a_stream_an_action_took_back(loaded, "its owner removed it");
        }
        Ok(StreamRemoveOutcome {
            stream_name: stream_cast,
            unloaded: loaded.is_some(),
            forgotten,
        })
    }

    /// Every stream the runtime holds, by name: each loaded stream, then each
    /// kept record not loaded. A record that cannot be read is logged and
    /// left out.
    pub fn list_streams(&self) -> Vec<StreamListing> {
        let mut listings: Vec<StreamListing> = self
            .every_loaded_stream()
            .iter()
            .map(|stream| StreamListing {
                name: stream.stream_name().to_string(),
                state: stream.holding().into(),
                project_directory: stream.project_directory().to_path_buf(),
                node_count: Some(stream.node_count()),
            })
            .collect();
        if let Some(kept_stream_records) = self.kept_stream_records() {
            for read in kept_stream_records.read_every() {
                match read {
                    Ok(kept_record) => {
                        if listings
                            .iter()
                            .any(|listing| listing.name == kept_record.stream_name)
                        {
                            continue;
                        }
                        listings.push(StreamListing {
                            state: if kept_record.stopped {
                                StreamListingState::Stopped
                            } else {
                                StreamListingState::Kept
                            },
                            name: kept_record.stream_name,
                            project_directory: kept_record.project_directory,
                            node_count: None,
                        });
                    }
                    Err(unreadable) => {
                        tracing::warn!("{unreadable}; it is left out of the stream listing")
                    }
                }
            }
        }
        listings.sort_by(|first, second| first.name.cmp(&second.name));
        listings
    }

    /// Put output port `port` of node `node` in the stream `stream_name`
    /// names at `level`: live on a loaded stream, cutting off at once every
    /// reader from outside the stream the level no longer allows, and
    /// recorded as the owner's ruling on a kept stream — only once the live
    /// change succeeded, and on a stopped stream once the recorded graph
    /// holds the node. An attached stream's level is never recorded. A loaded
    /// kept stream whose record cannot be read or written keeps the live
    /// change, and the outcome says why it was not recorded.
    pub fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_cast = self.the_cast_name_of_a_stream_an_action_names(stream_name)?;
        let ruling = OwnerExposureRuling {
            node: node.to_string(),
            port: port.to_string(),
            level,
        };
        let (recorded, ruling_not_recorded_because) = match self
            .loaded_stream_of_the_cast_name(&stream_cast)
        {
            Some(loaded) => {
                loaded
                    .log_route()
                    .run_entered(|| loaded.set_output_port_exposure_level(node, port, level))?;
                match loaded.holding() {
                    LoadedStreamHolding::Attached => (false, None),
                    LoadedStreamHolding::Kept => {
                        match self.record_the_owners_ruling_on_a_kept_record(&stream_cast, ruling) {
                            Ok(recorded) => (recorded, None),
                            Err(not_recorded) => {
                                tracing::warn!(
                                    "the port `{node}/{port}` of the kept stream `{stream_cast}` \
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
                let Some(mut kept_record) = self.kept_record_of_the_cast_name(&stream_cast)? else {
                    return Err(self.a_stream_this_runtime_does_not_hold(stream_name));
                };
                refuse_a_node_the_recorded_graph_does_not_hold(&kept_record, node)?;
                kept_record.record_the_owners_exposure_ruling(ruling);
                self.write_a_kept_record(&kept_record)?;
                (true, None)
            }
        };
        Ok(OutputPortExposureOutcome {
            stream_name: stream_cast,
            node: node.to_string(),
            port: port.to_string(),
            level,
            recorded,
            ruling_not_recorded_because,
        })
    }

    /// Load and start every kept stream not stopped, as a runtime does at its
    /// start, reporting each record it tried in file-name order. A stream that
    /// does not re-load, and a record that cannot be read, is logged with the
    /// reason and skipped, its record kept.
    pub fn reload_every_kept_stream_not_stopped(&self) -> Vec<KeptStreamReloadAtTheStart> {
        let Some(kept_stream_records) = self.kept_stream_records() else {
            return Vec::new();
        };
        self.reload_each_kept_stream_listed(kept_stream_records, kept_stream_records.read_every())
    }

    /// Re-load each record of `listed`. The local API serves while this runs,
    /// so each record is read again under the one-action lock and loaded from
    /// that read: a stream stopped or removed since the listing stays
    /// unloaded, one loaded since is left as it is, and the owner's rulings
    /// recorded since hold from its load.
    fn reload_each_kept_stream_listed(
        &self,
        kept_stream_records: &KeptStreamRecordsInTheStateDirectory,
        listed: Vec<std::result::Result<KeptStreamRecord, KeptStreamRecordReadFailure>>,
    ) -> Vec<KeptStreamReloadAtTheStart> {
        let mut reloads = Vec::new();
        for listed_record in listed {
            let listed_record = match listed_record {
                Ok(listed_record) => listed_record,
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(KeptStreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            let stream_name = listed_record.stream_name;
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let kept_record = match kept_stream_records.read(&stream_name) {
                Ok(Some(kept_record)) => kept_record,
                Ok(None) => {
                    tracing::info!(
                        "the kept stream `{stream_name}` was removed before its re-load, so it \
                         is not re-loaded"
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(KeptStreamReloadAtTheStart::NotReloaded {
                        stream_name,
                        refusal: unreadable,
                    });
                    continue;
                }
            };
            if kept_record.stopped {
                tracing::info!(
                    "the kept stream `{stream_name}` is stopped, so it is not re-loaded"
                );
                continue;
            }
            if self.loaded_stream_of_the_cast_name(&stream_name).is_some() {
                tracing::info!(
                    "the kept stream `{stream_name}` was loaded before its re-load reached it, \
                     so it is left as it is"
                );
                continue;
            }
            match self.load_a_kept_stream_from_its_record(&kept_record) {
                Ok(stream) => {
                    tracing::info!(
                        "the kept stream `{stream_name}` re-loaded from {} with {} nodes",
                        kept_record.project_directory.display(),
                        stream.node_count()
                    );
                    reloads.push(KeptStreamReloadAtTheStart::Reloaded { stream_name });
                }
                Err(reload_refusal) => {
                    tracing::error!(
                        "the kept stream `{stream_name}` did not re-load, and is skipped with its \
                         record kept: {reload_refusal}"
                    );
                    reloads.push(KeptStreamReloadAtTheStart::NotReloaded {
                        stream_name,
                        refusal: reload_refusal,
                    });
                }
            }
        }
        reloads
    }

    /// Unload the stream `stream_name` names when it is still the load
    /// `stream_tag` tags — what a closed local API connection does for the
    /// streams it attached. Does nothing, and says so with `false`, when the
    /// name is not loaded or now holds another stream.
    pub fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let Some(loaded) = cast_exposed_name_to_url_safe(stream_name)
            .ok()
            .and_then(|stream_cast| self.loaded_stream_of_the_cast_name(&stream_cast))
        else {
            return false;
        };
        if loaded.stream_tag() != stream_tag {
            return false;
        }
        unload_a_stream_an_action_took_back(&loaded, "the connection that attached it closed");
        true
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn kept_stream_records(&self) -> Option<&KeptStreamRecordsInTheStateDirectory> {
        self.stream_actions.kept_stream_records.get()
    }

    fn kept_record_of_the_cast_name(&self, stream_cast: &str) -> Result<Option<KeptStreamRecord>> {
        match self.kept_stream_records() {
            Some(kept_stream_records) => kept_stream_records.read(stream_cast),
            None => Ok(None),
        }
    }

    /// Record `ruling` on the kept record of `stream_cast`; `false` when the
    /// stream has no record.
    fn record_the_owners_ruling_on_a_kept_record(
        &self,
        stream_cast: &str,
        ruling: OwnerExposureRuling,
    ) -> Result<bool> {
        let Some(mut kept_record) = self.kept_record_of_the_cast_name(stream_cast)? else {
            return Ok(false);
        };
        kept_record.record_the_owners_exposure_ruling(ruling);
        self.write_a_kept_record(&kept_record)?;
        Ok(true)
    }

    fn write_a_kept_record(&self, kept_record: &KeptStreamRecord) -> Result<()> {
        match self.kept_stream_records() {
            Some(kept_stream_records) => kept_stream_records.write(kept_record),
            None => Err(Error::Configuration(format!(
                "the kept stream `{}` was not recorded: this runtime keeps no stream",
                kept_record.stream_name
            ))),
        }
    }

    fn the_cast_name_of_a_stream_an_action_names(&self, stream_name: &str) -> Result<String> {
        cast_exposed_name_to_url_safe(stream_name)
            .map(|stream_cast| stream_cast.into_owned())
            .map_err(|casts_to_nothing| {
                Error::NotFound(format!(
                    "cannot name a stream `{stream_name}`: {casts_to_nothing}"
                ))
            })
    }

    fn a_stream_this_runtime_does_not_hold(&self, stream_name: &str) -> Error {
        Error::NotFound(format!(
            "no stream named `{stream_name}` is loaded or kept in this runtime. Loaded: {}. \
             Kept: {}",
            self.loaded_stream_names_listed_for_a_refusal(),
            self.kept_stream_names_listed_for_a_refusal()
        ))
    }

    fn kept_stream_names_listed_for_a_refusal(&self) -> String {
        let kept_names: Vec<String> = self
            .kept_stream_records()
            .map(|kept_stream_records| {
                kept_stream_records
                    .read_every()
                    .into_iter()
                    .filter_map(|read| read.ok())
                    .map(|kept_record| {
                        if kept_record.stopped {
                            format!("{} (stopped)", kept_record.stream_name)
                        } else {
                            kept_record.stream_name
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        names_listed_for_a_refusal(kept_names, "none")
    }

    /// Replace `previous_record` by `replacement`, whose compile already
    /// succeeded writing `compile_warnings`: unload the running stream, load
    /// and start the new graph with the previous rulings, and record it. A
    /// refused load re-loads the previous record when its stream was running.
    fn replace_a_kept_stream(
        &self,
        kept_stream_records: &KeptStreamRecordsInTheStateDirectory,
        previous_record: KeptStreamRecord,
        previously_loaded: Option<Arc<LoadedStreamInThisRuntime>>,
        mut replacement: KeptStreamRecord,
        compile_warnings: Vec<String>,
    ) -> Result<StreamRunOutcome> {
        replacement.exposure_rulings = previous_record.exposure_rulings.clone();
        let was_running = previously_loaded.is_some();
        if let Some(previously_loaded) = previously_loaded {
            unload_a_stream_an_action_took_back(&previously_loaded, "its kept record is replaced");
        }
        let replaced = self
            .load_and_start_a_stream(
                &replacement.stream_name,
                &replacement.graph,
                &replacement.exposure_rulings,
                replacement.stream_environment(),
                LoadedStreamHolding::Kept,
            )
            .and_then(|stream| match kept_stream_records.write(&replacement) {
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
                stream_name: replacement.stream_name,
                project_directory: replacement.project_directory,
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

    /// Load and start a kept stream from `kept_record`, refused naming the
    /// interpreter when the recorded one is gone.
    fn load_a_kept_stream_from_its_record(
        &self,
        kept_record: &KeptStreamRecord,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        if !kept_record.interpreter.is_file() {
            return Err(Error::Configuration(format!(
                "the kept stream `{}` was not loaded: its interpreter {} is gone; run `uv sync` \
                 in {}",
                kept_record.stream_name,
                kept_record.interpreter.display(),
                kept_record.project_directory.display()
            )));
        }
        self.load_and_start_a_stream(
            &kept_record.stream_name,
            &kept_record.graph,
            &kept_record.exposure_rulings,
            kept_record.stream_environment(),
            LoadedStreamHolding::Kept,
        )
    }

    /// [`Self::load_a_stream_with_the_owners_rulings`], then start it; a
    /// refused start unloads it.
    fn load_and_start_a_stream(
        &self,
        stream_name: &str,
        graph_json: &serde_json::Value,
        rulings: &[OwnerExposureRuling],
        stream_environment: StreamEnvironment,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let stream = self.load_a_stream_with_the_owners_rulings(
            stream_name,
            graph_json,
            rulings,
            stream_environment,
            holding,
        )?;
        if let Err(start_refusal) = stream.start() {
            unload_a_stream_an_action_took_back(&stream, "its start was refused");
            return Err(start_refusal);
        }
        Ok(stream)
    }

    /// Load `graph_json` as the stream `stream_name` held as `holding`, the
    /// owner's `rulings` split around the load, without starting it: a ruling
    /// that waits for the load is skipped with a warning when its port is not
    /// there.
    fn load_a_stream_with_the_owners_rulings(
        &self,
        stream_name: &str,
        graph_json: &serde_json::Value,
        rulings: &[OwnerExposureRuling],
        stream_environment: StreamEnvironment,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let OwnerExposureRulingsSplitAroundTheLoad {
            graph_with_the_rulings_that_hold_before_the_load,
            rulings_applied_once_loaded,
        } = the_owners_exposure_rulings_split_around_the_load(graph_json, rulings);
        let graph =
            GraphSnapshot::from_graph_document(graph_with_the_rulings_that_hold_before_the_load)?;
        let stream = match self.load_stream_held_as_unless_a_machine_shutdown_is_requested(
            &graph,
            OptionsForLoadingOneStream::in_stream_environment(stream_environment)
                .named(stream_name),
            holding,
        )? {
            StreamLoadObservingMachineShutdownRequests::Loaded(stream) => stream,
            StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest => {
                return Err(Error::Runtime(format!(
                    "the stream `{stream_name}` was not loaded: the machine is shutting every \
                     stream down"
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

/// Ask `stream` for its shutdown, wait until it has ended, and log a failing
/// end in its own log — the action that unloaded it goes on regardless.
fn unload_a_stream_an_action_took_back(stream: &Arc<LoadedStreamInThisRuntime>, reason: &str) {
    request_a_streams_shutdown_and_wait_until_it_has_ended(stream, reason);
    if let Err(failing_end) = stream.how_this_stream_ended_as_a_waiter_reports_it() {
        stream.log_route().run_entered(|| {
            tracing::warn!(
                "the stream `{}` was unloaded because {reason}, and its end reported: \
                 {failing_end}",
                stream.stream_name()
            )
        });
    }
}

/// The stop of the kept stream `stream_cast`, unloaded already, whose record
/// could not be marked stopped because of `not_recorded`.
fn a_kept_stream_unloaded_and_not_recorded_stopped(
    stream_cast: String,
    not_recorded: &dyn std::fmt::Display,
) -> StreamStopOutcome {
    tracing::warn!(
        "the kept stream `{stream_cast}` was unloaded, and is not recorded stopped, so a restart \
         of this runtime loads it again: {not_recorded}"
    );
    StreamStopOutcome {
        stream_name: stream_cast,
        kept: true,
        stop_not_recorded_because: Some(not_recorded.to_string()),
    }
}

/// Refuse `node` when the graph `kept_record` holds has no node of that name
/// once cast, naming the nodes it does hold.
fn refuse_a_node_the_recorded_graph_does_not_hold(
    kept_record: &KeptStreamRecord,
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
        kept_record.stream_name,
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
    use streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode;

    const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

    /// A runner handed a lend directory, keeping its streams in
    /// `kept_streams_directory` when one is given.
    fn a_runner_keeping_its_streams_in(kept_streams_directory: Option<&Path>) -> Arc<Runner> {
        ensure_test_mocks_registered();
        let runner = Runner::new().expect("the runner builds");
        runner
            .set_processor_interpreter_lend_directory(PathBuf::from(LEND_DIRECTORY_FOR_TEST))
            .expect("the lend directory is handed over");
        if let Some(kept_streams_directory) = kept_streams_directory {
            runner
                .keep_streams_in_the_state_directory(kept_streams_directory)
                .expect("the runner keeps its streams there");
        }
        runner
    }

    /// The graph a stream function named `stream_name` compiles to: one node,
    /// `source`, with the output ports `out1` and `out2`, exposing `exposed`.
    fn the_graph_of_a_function_named(
        stream_name: &str,
        exposed: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "stream": stream_name,
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
        project_directory: tempfile::TempDir,
    }

    impl ProjectWithAStubCompile {
        fn compiling(graph: serde_json::Value) -> Self {
            Self::compiling_warning(graph, &[])
        }

        fn compiling_warning(graph: serde_json::Value, warnings: &[&str]) -> Self {
            let project = Self::with_no_venv();
            create_directory_and_its_missing_parents_at_mode(
                &project.path().join(".venv").join("bin"),
                0o755,
            )
            .expect("the venv's bin directory");
            project.compile_to_warning(graph, warnings);
            project
        }

        fn with_no_venv() -> Self {
            Self {
                project_directory: a_temporary_directory_at_owner_only_mode()
                    .expect("a project directory"),
            }
        }

        fn path(&self) -> &Path {
            self.project_directory.path()
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

        /// From now on, the compile prints `graph` and this project.
        fn compile_to(&self, graph: serde_json::Value) {
            self.compile_to_warning(graph, &[]);
        }

        /// From now on, the compile writes each of `warnings` on a line of its
        /// standard error, then prints `graph` and this project.
        fn compile_to_warning(&self, graph: serde_json::Value, warnings: &[&str]) {
            let compile_document = serde_json::json!({
                "stream_graph": graph,
                "project_directory": self.path(),
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
                stream_name: None,
                holding,
            }
        }
    }

    fn records_in(kept_streams_directory: &Path) -> KeptStreamRecordsInTheStateDirectory {
        KeptStreamRecordsInTheStateDirectory::open(kept_streams_directory)
            .expect("the kept streams open")
    }

    fn a_kept_record_of(
        project: &ProjectWithAStubCompile,
        stream_name: &str,
        exposed: serde_json::Value,
    ) -> KeptStreamRecord {
        KeptStreamRecord::of_a_running_stream(
            stream_name,
            &project.stream_environment(),
            None,
            the_graph_of_a_function_named(stream_name, exposed),
        )
    }

    /// Record `record` and load it held as kept, not started — what a kept
    /// stream is between its load and its start.
    fn a_kept_stream_loaded_without_its_start(
        runner: &Runner,
        kept_streams_directory: &Path,
        record: &KeptStreamRecord,
    ) -> Arc<LoadedStreamInThisRuntime> {
        records_in(kept_streams_directory)
            .write(record)
            .expect("the record is written");
        runner
            .load_a_stream_with_the_owners_rulings(
                &record.stream_name,
                &record.graph,
                &record.exposure_rulings,
                record.stream_environment(),
                LoadedStreamHolding::Kept,
            )
            .expect("the kept stream loads")
    }

    fn an_attached_stream_loaded_without_its_start(
        runner: &Runner,
        project: &ProjectWithAStubCompile,
        stream_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        runner
            .load_a_stream_with_the_owners_rulings(
                stream_name,
                &the_graph_of_a_function_named(stream_name, serde_json::json!([])),
                &[],
                project.stream_environment(),
                LoadedStreamHolding::Attached,
            )
            .expect("the attached stream loads")
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
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    #[test]
    #[serial]
    fn a_second_kept_streams_directory_is_refused_naming_the_first() {
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
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_under_a_name_already_loaded_is_refused_naming_the_project_that_holds_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let first_project = ProjectWithAStubCompile::with_no_venv();
        let first = an_attached_stream_loaded_without_its_start(&runner, &first_project, "camera");
        let second_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        for holding in [LoadedStreamHolding::Attached, LoadedStreamHolding::Kept] {
            let refusal = refusal_of(runner.run_stream(second_project.run_request(holding)));

            assert!(
                refusal.contains(&first_project.path().display().to_string()),
                "{refusal}"
            );
            assert!(refusal.contains("--name"), "{refusal}");
        }
        assert_eq!(
            runner.loaded_stream_named("camera").unwrap().stream_tag(),
            first.stream_tag()
        );
        assert!(records_in(state_directory.path()).read_every().is_empty());
    }

    #[test]
    #[serial]
    fn a_run_under_a_name_a_kept_record_holds_is_refused_naming_its_project_stopped_included() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let recorded_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));
        let mut stopped = a_kept_record_of(&recorded_project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();
        let other_project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusals = [
            refusal_of(runner.run_stream(other_project.run_request(LoadedStreamHolding::Kept))),
            refusal_of(runner.run_stream(other_project.run_request(LoadedStreamHolding::Attached))),
            refusal_of(
                runner.run_stream(recorded_project.run_request(LoadedStreamHolding::Attached)),
            ),
            refusal_of(runner.run_stream(RunStreamRequest {
                stream_function: Some("other.py".to_string()),
                ..recorded_project.run_request(LoadedStreamHolding::Kept)
            })),
        ];

        for refusal in refusals {
            assert!(
                refusal.contains(&recorded_project.path().display().to_string()),
                "{refusal}"
            );
            assert!(refusal.contains("stopped"), "{refusal}");
            assert!(refusal.contains("--name"), "{refusal}");
        }
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert_eq!(
            records_in(state_directory.path()).read("camera").unwrap(),
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
        let record = a_kept_record_of(&project, "camera", serde_json::json!([]));
        let running =
            a_kept_stream_loaded_without_its_start(&runner, state_directory.path(), &record);
        project.fail_to_compile_printing("NameError: name cmaera is not defined");

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("cmaera"), "{refusal}");
        assert_eq!(
            runner.loaded_stream_named("camera").unwrap().stream_tag(),
            running.stream_tag(),
            "the running stream is the one loaded before the replace"
        );
        assert!(!running.has_ended());
        assert_eq!(
            records_in(state_directory.path()).read("camera").unwrap(),
            Some(record)
        );
    }

    /// The graph of a function named `stream_name` whose `exposed` names a
    /// port its one node lacks, which its load refuses.
    fn the_graph_of_a_function_exposing_a_port_its_node_lacks(
        stream_name: &str,
    ) -> serde_json::Value {
        the_graph_of_a_function_named(
            stream_name,
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
        let gone_interpreter = project.path().join("relocated-venv").join("python");
        let previous_record = KeptStreamRecord {
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
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert_eq!(
            records_in(state_directory.path()).read("camera").unwrap(),
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
        {
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            let running = a_kept_stream_loaded_without_its_start(
                &runner,
                state_directory.path(),
                &a_kept_record_of(&project, "camera", serde_json::json!([])),
            );

            let stopped = runner.stop_stream("camera").expect("the kept stream stops");

            assert_eq!(
                stopped,
                StreamStopOutcome {
                    stream_name: "camera".to_string(),
                    kept: true,
                    stop_not_recorded_because: None,
                }
            );
            assert!(running.has_ended());
            assert!(runner.names_of_the_loaded_streams().is_empty());
            let refusal = refusal_of(runner.stop_stream("camera"));
            assert!(refusal.contains("already stopped"), "{refusal}");
        }

        let fresh_runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        assert!(
            fresh_runner
                .reload_every_kept_stream_not_stopped()
                .is_empty()
        );
        assert!(fresh_runner.names_of_the_loaded_streams().is_empty());
        assert_eq!(
            fresh_runner.list_streams(),
            [StreamListing {
                name: "camera".to_string(),
                state: StreamListingState::Stopped,
                project_directory: project.path().to_path_buf(),
                node_count: None,
            }]
        );
        assert!(
            records_in(state_directory.path())
                .read("camera")
                .unwrap()
                .unwrap()
                .stopped
        );
    }

    /// Makes `kept_streams_directory` refuse every new file until it drops, so
    /// a record write fails.
    struct KeptStreamsDirectoryRefusingWrites<'a> {
        kept_streams_directory: &'a Path,
    }

    impl<'a> KeptStreamsDirectoryRefusingWrites<'a> {
        fn from_now_on(kept_streams_directory: &'a Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                kept_streams_directory,
                std::fs::Permissions::from_mode(0o500),
            )
            .expect("the kept-streams directory is made read-only");
            Self {
                kept_streams_directory,
            }
        }
    }

    impl Drop for KeptStreamsDirectoryRefusingWrites<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                self.kept_streams_directory,
                std::fs::Permissions::from_mode(0o700),
            );
        }
    }

    /// Overwrite the record of `stream_name` in `kept_streams_directory` with
    /// a file of each unreadable kind in turn — not JSON, then a record of
    /// another schema version — handing each one's bytes to `observe`.
    fn for_each_unreadable_record_of(
        kept_streams_directory: &Path,
        stream_name: &str,
        mut observe: impl FnMut(&Path, &[u8]),
    ) {
        let record_path = records_in(kept_streams_directory)
            .record_path_of(stream_name)
            .unwrap();
        let mut later_schema = serde_json::to_value(
            records_in(kept_streams_directory)
                .read(stream_name)
                .unwrap()
                .expect("a readable record to make unreadable"),
        )
        .unwrap();
        later_schema["schema_version"] = serde_json::json!(2);
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
        let record = a_kept_record_of(&project, "camera", serde_json::json!([]));
        records_in(state_directory.path()).write(&record).unwrap();

        for_each_unreadable_record_of(
            state_directory.path(),
            "camera",
            |record_path, unreadable_bytes| {
                let running = runner
                    .load_a_stream_with_the_owners_rulings(
                        "camera",
                        &record.graph,
                        &[],
                        record.stream_environment(),
                        LoadedStreamHolding::Kept,
                    )
                    .expect("the kept stream loads");

                let stopped = runner
                    .stop_stream("camera")
                    .expect("the stop succeeds, since the stream is unloaded");

                assert_eq!(stopped.stream_name, "camera");
                assert!(stopped.kept);
                let not_recorded = stopped
                    .stop_not_recorded_because
                    .expect("the outcome says the stop was not recorded");
                assert!(
                    not_recorded.contains(&record_path.display().to_string()),
                    "{not_recorded}"
                );
                assert!(running.has_ended());
                assert!(runner.names_of_the_loaded_streams().is_empty());
                assert_eq!(
                    std::fs::read(record_path).unwrap(),
                    unreadable_bytes,
                    "the unreadable record is left as it was"
                );

                let refusal = refusal_of(runner.stop_stream("camera"));
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
        let kept_streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&kept_streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            &kept_streams_directory,
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
        );

        let stopped = {
            let _refusing_writes =
                KeptStreamsDirectoryRefusingWrites::from_now_on(&kept_streams_directory);
            runner.stop_stream("camera")
        }
        .expect("the stop succeeds, since the stream is unloaded");

        assert!(stopped.kept);
        assert!(
            stopped.stop_not_recorded_because.is_some(),
            "the outcome says the stop was not recorded: {stopped:?}"
        );
        assert!(running.has_ended());
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert!(
            !records_in(&kept_streams_directory)
                .read("camera")
                .unwrap()
                .unwrap()
                .stopped
        );
    }

    #[test]
    #[serial]
    fn exposing_a_port_of_a_loaded_kept_stream_whose_record_is_unreadable_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
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
            "camera",
            |record_path, unreadable_bytes| {
                let level = levels.next().unwrap();

                let exposed = runner
                    .expose_port("camera", "source", "out1", level)
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
    fn exposing_a_port_of_a_loaded_kept_stream_whose_record_cannot_be_written_changes_it_live() {
        let state_directory = tempfile::tempdir().unwrap();
        let kept_streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&kept_streams_directory));
        let project = ProjectWithAStubCompile::with_no_venv();
        let kept = a_kept_stream_loaded_without_its_start(
            &runner,
            &kept_streams_directory,
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
        );

        let exposed = {
            let _refusing_writes =
                KeptStreamsDirectoryRefusingWrites::from_now_on(&kept_streams_directory);
            runner.expose_port("camera", "source", "out1", OutputPortExposureLevel::Public)
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
            records_in(&kept_streams_directory)
                .read("camera")
                .unwrap()
                .unwrap()
                .exposure_rulings
                .is_empty(),
            "the record holds no ruling"
        );
    }

    #[test]
    #[serial]
    fn starting_a_kept_stream_whose_record_is_unreadable_is_refused_naming_the_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let mut stopped = a_kept_record_of(&project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        for_each_unreadable_record_of(state_directory.path(), "camera", |record_path, _| {
            let refusal = refusal_of(runner.start_stream("camera"));

            assert!(refusal.contains("`camera`"), "{refusal}");
            assert!(refusal.contains("not started"), "{refusal}");
            assert!(
                refusal.contains(&record_path.display().to_string()),
                "{refusal}"
            );
            assert!(runner.names_of_the_loaded_streams().is_empty());
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
            .stop_stream("camera")
            .expect("the attached stream stops");

        assert!(!stopped.kept);
        assert_eq!(stopped.stop_not_recorded_because, None);
        assert!(runner.names_of_the_loaded_streams().is_empty());
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
            assert!(refusal.contains("attached-one"), "{refusal}");
            assert!(refusal.contains("kept-one (stopped)"), "{refusal}");
        }
        let start_refusal = refusal_of(runner.start_stream("attached-one"));
        assert!(start_refusal.contains("attached"), "{start_refusal}");
        let start_refusal = refusal_of(runner.start_stream("absent"));
        assert!(start_refusal.contains("kept-one"), "{start_refusal}");
    }

    #[test]
    #[serial]
    fn removing_a_kept_stream_unloads_it_and_forgets_its_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
        let running = a_kept_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_kept_record_of(&project, "camera", serde_json::json!([])),
        );
        let mut stopped = a_kept_record_of(&project, "microphone", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        assert_eq!(
            runner.remove_stream("camera").unwrap(),
            StreamRemoveOutcome {
                stream_name: "camera".to_string(),
                unloaded: true,
                forgotten: true,
            }
        );
        assert_eq!(
            runner.remove_stream("microphone").unwrap(),
            StreamRemoveOutcome {
                stream_name: "microphone".to_string(),
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
    fn the_listing_names_each_stream_attached_kept_or_stopped_and_one_not_loaded_counts_no_node() {
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

        let listing_of = |name: &str, state, node_count| StreamListing {
            name: name.to_string(),
            state,
            project_directory: project.path().to_path_buf(),
            node_count,
        };
        assert_eq!(
            runner.list_streams(),
            [
                listing_of("a-attached", StreamListingState::Attached, Some(1)),
                listing_of("b-kept", StreamListingState::Kept, Some(1)),
                listing_of("c-stopped", StreamListingState::Stopped, None),
                listing_of("d-not-re-loaded", StreamListingState::Kept, None),
            ]
        );
    }

    #[test]
    #[serial]
    fn a_port_of_a_loaded_kept_stream_changes_live_and_its_ruling_is_recorded() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::with_no_venv();
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
            .expose_port(
                "camera",
                "source",
                "out1",
                OutputPortExposureLevel::Internal,
            )
            .expect("the port is made internal");

        assert!(exposed.recorded);
        assert_eq!(exposed.ruling_not_recorded_because, None);
        assert_eq!(
            the_exposures_graph_renders_for(&kept),
            serde_json::json!([])
        );
        assert_eq!(
            records_in(state_directory.path())
                .read("camera")
                .unwrap()
                .unwrap()
                .exposure_rulings,
            [OwnerExposureRuling {
                node: "source".to_string(),
                port: "out1".to_string(),
                level: OutputPortExposureLevel::Internal,
            }]
        );

        let refusal = refusal_of(runner.expose_port(
            "camera",
            "source",
            "absent-port",
            OutputPortExposureLevel::Public,
        ));
        assert!(refusal.contains("absent-port"), "{refusal}");
        assert_eq!(
            records_in(state_directory.path())
                .read("camera")
                .unwrap()
                .unwrap()
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
            .expose_port("camera", "source", "out2", OutputPortExposureLevel::Public)
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
        let mut stopped = a_kept_record_of(&project, "camera", serde_json::json!([]));
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        let exposed = runner
            .expose_port("camera", "Source", "out2", OutputPortExposureLevel::Public)
            .expect("the ruling is recorded");
        let refusal = refusal_of(runner.expose_port(
            "camera",
            "absent-node",
            "out1",
            OutputPortExposureLevel::Public,
        ));

        assert!(exposed.recorded);
        assert!(refusal.contains("absent-node"), "{refusal}");
        assert!(refusal.contains("source"), "{refusal}");
        let recorded = records_in(state_directory.path())
            .read("camera")
            .unwrap()
            .unwrap();
        assert!(recorded.stopped);
        assert_eq!(
            recorded.exposure_rulings,
            [OwnerExposureRuling {
                node: "Source".to_string(),
                port: "out2".to_string(),
                level: OutputPortExposureLevel::Public,
            }]
        );
        assert!(runner.names_of_the_loaded_streams().is_empty());
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
        let refusal = loaded
            .register_a_reader_of_an_exposed_output_port(
                "source",
                "out1",
                crate::core::graph::OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
                Box::new(|| {}),
            )
            .expect_err("the owner's internal holds over the function's private");
        assert!(
            matches!(refusal, Error::OutputPortNotExposedToTheReader(_)),
            "{refusal}"
        );
    }

    #[test]
    #[serial]
    fn reloading_reports_each_kept_stream_whose_interpreter_is_gone_by_name_and_keeps_going() {
        let state_directory = tempfile::tempdir().unwrap();
        let project = ProjectWithAStubCompile::with_no_venv();
        let records = records_in(state_directory.path());
        records
            .write(&a_kept_record_of(
                &project,
                "a-camera",
                serde_json::json!([]),
            ))
            .unwrap();
        records
            .write(&a_kept_record_of(
                &project,
                "c-microphone",
                serde_json::json!([]),
            ))
            .unwrap();
        let malformed = state_directory.path().join("b-malformed.json");
        std::fs::write(&malformed, b"{").unwrap();
        let mut stopped = a_kept_record_of(&project, "d-stopped", serde_json::json!([]));
        stopped.stopped = true;
        records.write(&stopped).unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));

        let reloads = runner.reload_every_kept_stream_not_stopped();

        assert_eq!(reloads.len(), 3, "{reloads:?}");
        for (reload, expected_stream_name) in
            [(&reloads[0], "a-camera"), (&reloads[2], "c-microphone")]
        {
            let KeptStreamReloadAtTheStart::NotReloaded {
                stream_name,
                refusal,
            } = reload
            else {
                panic!("`{expected_stream_name}` is reported not re-loaded: {reload:?}");
            };
            assert_eq!(stream_name, expected_stream_name);
            let refusal = refusal.to_string();
            assert!(refusal.contains(expected_stream_name), "{refusal}");
            assert!(
                refusal.contains(&project.interpreter().display().to_string()),
                "{refusal}"
            );
            assert!(refusal.contains("uv sync"), "{refusal}");
        }
        let KeptStreamReloadAtTheStart::RecordUnreadable(unreadable) = &reloads[1] else {
            panic!(
                "the malformed record is reported by its path: {:?}",
                reloads[1]
            );
        };
        assert_eq!(unreadable.path, malformed);
        assert!(runner.names_of_the_loaded_streams().is_empty());
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
        records
            .write(&a_kept_record_of(
                &project,
                "a-stopped",
                serde_json::json!([]),
            ))
            .unwrap();
        records
            .write(&a_kept_record_of(
                &project,
                "b-removed",
                serde_json::json!([]),
            ))
            .unwrap();
        let listed = records.read_every();
        let mut stopped_since = records.read("a-stopped").unwrap().unwrap();
        stopped_since.stopped = true;
        records.write(&stopped_since).unwrap();
        assert!(records.remove("b-removed").unwrap());

        let reloads = runner.reload_each_kept_stream_listed(&records, listed);

        assert!(reloads.is_empty(), "neither is reported: {reloads:?}");
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert_eq!(records.read_every(), vec![Ok(stopped_since)]);
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
            runner.loaded_stream_named("camera").unwrap().stream_tag(),
            loaded_since.stream_tag()
        );
        assert!(!loaded_since.has_ended());
    }

    #[test]
    #[serial]
    fn an_attached_stream_is_unloaded_by_its_own_tag_and_never_one_that_took_its_name_since() {
        let runner = a_runner_keeping_its_streams_in(None);
        let project = ProjectWithAStubCompile::with_no_venv();
        let first = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        assert!(runner.unload_the_attached_stream_if_still_the_same("camera", first.stream_tag()));
        assert!(first.has_ended());
        assert!(runner.names_of_the_loaded_streams().is_empty());

        let second = an_attached_stream_loaded_without_its_start(&runner, &project, "camera");

        assert!(!runner.unload_the_attached_stream_if_still_the_same("camera", first.stream_tag()));
        assert!(!runner.unload_the_attached_stream_if_still_the_same("absent", first.stream_tag()));
        assert!(!second.has_ended());
        assert_eq!(
            runner.loaded_stream_named("camera").unwrap().stream_tag(),
            second.stream_tag()
        );
    }

    // The tests below start streams, which needs the engine's GPU context.

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_run_starts_the_stream_and_records_it_when_kept_and_not_when_attached() {
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

        let kept = runner
            .run_stream(kept_project.run_request(LoadedStreamHolding::Kept))
            .expect("the kept stream runs");
        let attached = runner
            .run_stream(attached_project.run_request(LoadedStreamHolding::Attached))
            .expect("the attached stream runs");

        assert_eq!(kept.stream_name, "camera");
        assert_eq!(kept.project_directory, kept_project.path());
        assert_eq!(kept.node_count, 1);
        assert!(!kept.replaced_the_kept_record);
        assert_eq!(kept.compile_warnings, Vec::<String>::new());
        assert_eq!(attached.stream_name, "preview");
        assert_eq!(
            attached.compile_warnings,
            ["tatolab: the cross-floor check found 1 thing binding this app to one floor."]
        );
        let recorded = records_in(state_directory.path())
            .read("camera")
            .unwrap()
            .expect("the kept stream is recorded");
        assert_eq!(
            recorded,
            a_kept_record_of(&kept_project, "camera", serde_json::json!([]))
        );
        assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
        let states: Vec<_> = runner
            .list_streams()
            .into_iter()
            .map(|listing| (listing.name, listing.state, listing.node_count))
            .collect();
        assert_eq!(
            states,
            [
                ("camera".to_string(), StreamListingState::Kept, Some(1)),
                ("preview".to_string(), StreamListingState::Attached, Some(1)),
            ]
        );
        assert_eq!(
            runner.loaded_stream_named("camera").unwrap().status(),
            crate::core::runtime::RuntimeStatus::Started
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_kept_run_of_the_records_project_and_function_replaces_it_and_keeps_its_rulings() {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
        runner
            .expose_port(
                "camera",
                "source",
                "out1",
                OutputPortExposureLevel::Internal,
            )
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

        assert!(replaced.replaced_the_kept_record);
        assert_eq!(
            replaced.compile_warnings,
            ["a warning of the replacing compile"]
        );
        assert_ne!(replaced.stream_tag, first.stream_tag);
        let running = runner.loaded_stream_named("camera").unwrap();
        assert_eq!(running.stream_tag(), replaced.stream_tag);
        assert_eq!(
            the_exposures_graph_renders_for(&running),
            serde_json::json!([{"node": "source", "port": "out2", "level": "private"}]),
            "the owner's internal ruling wins over the changed function's public"
        );
        let recorded = records_in(state_directory.path())
            .read("camera")
            .unwrap()
            .unwrap();
        assert_eq!(recorded.graph, changed_source);
        assert_eq!(
            recorded.exposure_rulings,
            [OwnerExposureRuling {
                node: "source".to_string(),
                port: "out1".to_string(),
                level: OutputPortExposureLevel::Internal,
            }]
        );
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
        {
            let runner = a_runner_keeping_its_streams_in(Some(state_directory.path()));
            runner
                .run_stream(project.run_request(LoadedStreamHolding::Kept))
                .unwrap();
            runner
                .expose_port("camera", "source", "out1", OutputPortExposureLevel::Private)
                .unwrap();
            runner.stop_stream("camera").unwrap();

            let started = runner
                .start_stream("camera")
                .expect("the stream starts again");

            assert_eq!(
                started,
                StreamStartOutcome {
                    stream_name: "camera".to_string(),
                    node_count: 1,
                }
            );
            assert_eq!(
                the_exposures_graph_renders_for(&runner.loaded_stream_named("camera").unwrap()),
                serde_json::json!([{"node": "source", "port": "out1", "level": "private"}])
            );
            assert!(
                !records_in(state_directory.path())
                    .read("camera")
                    .unwrap()
                    .unwrap()
                    .stopped
            );
            let refusal = refusal_of(runner.start_stream("camera"));
            assert!(refusal.contains("already loaded"), "{refusal}");
        }

        let restarted = a_runner_keeping_its_streams_in(Some(state_directory.path()));
        let reloads = restarted.reload_every_kept_stream_not_stopped();

        assert!(
            matches!(
                reloads.as_slice(),
                [KeptStreamReloadAtTheStart::Reloaded { stream_name }] if stream_name == "camera"
            ),
            "{reloads:?}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&restarted.loaded_stream_named("camera").unwrap()),
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
        let records = records_in(state_directory.path());
        records
            .write(&a_kept_record_of(
                &project,
                "camera",
                serde_json::json!([{"node": "source", "port": "out1", "level": "public"}]),
            ))
            .unwrap();
        let listed = records.read_every();
        let mut restricted_since = records.read("camera").unwrap().unwrap();
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
                [KeptStreamReloadAtTheStart::Reloaded { stream_name }] if stream_name == "camera"
            ),
            "{reloads:?}"
        );
        assert_eq!(
            the_exposures_graph_renders_for(&runner.loaded_stream_named("camera").unwrap()),
            serde_json::json!([]),
            "the restriction recorded after the listing holds"
        );
        assert_eq!(records.read("camera").unwrap(), Some(restricted_since));
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
        assert!(runner.names_of_the_loaded_streams().is_empty());
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
        let kept_streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&kept_streams_directory));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([]),
        ));

        let refusal = {
            let _refusing_writes =
                KeptStreamsDirectoryRefusingWrites::from_now_on(&kept_streams_directory);
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        assert!(runner.names_of_the_loaded_streams().is_empty());
        assert!(records_in(&kept_streams_directory).read_every().is_empty());
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
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
        let previous_record = records_in(state_directory.path())
            .read("camera")
            .unwrap()
            .unwrap();
        project.compile_to(the_graph_of_a_function_exposing_a_port_its_node_lacks(
            "camera",
        ));

        let refusal = refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)));

        assert!(refusal.contains("absent-port"), "{refusal}");
        assert!(!refusal.contains("did not re-load"), "{refusal}");
        let restored = runner
            .loaded_stream_named("camera")
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
            records_in(state_directory.path()).read("camera").unwrap(),
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
        let kept_streams_directory = state_directory.path().join("streams");
        let runner = a_runner_keeping_its_streams_in(Some(&kept_streams_directory));
        let project = ProjectWithAStubCompile::compiling(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
        ));
        let first = runner
            .run_stream(project.run_request(LoadedStreamHolding::Kept))
            .unwrap();
        let previous_record = records_in(&kept_streams_directory)
            .read("camera")
            .unwrap()
            .unwrap();
        project.compile_to(the_graph_of_a_function_named(
            "camera",
            serde_json::json!([{"node": "source", "port": "out2", "level": "public"}]),
        ));

        let refusal = {
            let _refusing_writes =
                KeptStreamsDirectoryRefusingWrites::from_now_on(&kept_streams_directory);
            refusal_of(runner.run_stream(project.run_request(LoadedStreamHolding::Kept)))
        };

        assert!(refusal.contains("was not written"), "{refusal}");
        assert!(!refusal.contains("did not re-load"), "{refusal}");
        let restored = runner
            .loaded_stream_named("camera")
            .expect("the previous record is loaded again");
        assert_ne!(restored.stream_tag(), first.stream_tag);
        assert_eq!(
            the_exposures_graph_renders_for(&restored),
            serde_json::json!([{"node": "source", "port": "out1", "level": "private"}]),
            "the stream loaded is the previous record's, not the one whose record was refused"
        );
        assert_eq!(
            records_in(&kept_streams_directory).read("camera").unwrap(),
            Some(previous_record)
        );
    }
}
