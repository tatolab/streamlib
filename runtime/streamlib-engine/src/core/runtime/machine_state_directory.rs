// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The streams a runtime records in its state directory: one record per
//! stream, named by its id and written whole or not at all, changed only
//! through one read-edit-write at a time, the stream actions queued on it, and
//! the owner's exposure rulings each record carries, split around the stream's
//! load.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};
use streamlib_runtime_client_contract::owner_only_file_written_whole::write_an_owner_only_file_whole;

use super::{StreamEnvironment, StreamListingState, StreamLocation};
use crate::core::graph::{
    OutputPortExposureLevel, cast_exposed_name_to_url_safe, is_in_exposed_name_cast_form,
};
use crate::core::{Error, Result};

/// The schema version of a stream record this runtime writes and reads.
pub const STREAM_RECORD_SCHEMA_VERSION: u32 = 4;

/// The schema version whose records hold no pending stream actions; a record
/// of it reads as current, holding none.
const STREAM_RECORD_SCHEMA_VERSION_HOLDING_NO_PENDING_STREAM_ACTION: u32 = 3;

/// The extension every stream record's file name carries.
const STREAM_RECORD_FILE_EXTENSION: &str = "json";

/// One stream, as its runtime re-loads it: its id and location, its display
/// name, the graph compiled at its load, the interpreter its processors start
/// in, whether it was stopped or has failed, and the owner's exposure rulings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRecord {
    /// [`STREAM_RECORD_SCHEMA_VERSION`] for a record this runtime wrote.
    pub schema_version: u32,
    /// The stream id its location computes to, which its file is named after.
    pub stream_id: String,
    /// Where the stream's code lives.
    pub location: StreamLocation,
    /// The label the stream is shown under.
    pub display_name: String,
    /// The project's venv interpreter, as it was spelled at the load.
    pub interpreter: PathBuf,
    /// The graph compiled at the load, before any ruling was applied; `None`
    /// for a stream whose compile failed, which `start_stream` compiles from
    /// its location.
    pub graph: Option<serde_json::Value>,
    /// Whether the owner stopped the stream, so a restart leaves it unloaded.
    pub stopped: bool,
    /// Why the stream is failed, so the runtime's start skips it until
    /// `start_stream` retries it; `None` when it is not failed.
    pub failed_because: Option<String>,
    /// How many of the runtime's crashes in a row, the last included, the
    /// stream was implicated in.
    pub runtime_crashes_in_a_row_implicating_it: u32,
    /// The owner's exposure rulings, each overriding the level the stream
    /// function gave its port.
    pub exposure_rulings: Vec<OwnerExposureRuling>,
    /// The stop, remove and start actions queued on the stream and not yet
    /// finished, in queue order; each is finalised only by its own action.
    #[serde(default)]
    pub pending_stream_actions: Vec<PendingStreamActionOnTheRecord>,
}

/// A stream action queued on a record, which the runtime's start finishes when
/// a crash left it unfinished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingStreamActionOnTheRecord {
    /// The action's id.
    pub action_id: String,
    /// What the action does to the record once it runs.
    pub kind: PendingStreamActionKind,
}

/// What a pending stream action does to its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingStreamActionKind {
    /// Records the stream stopped.
    Stop,
    /// Forgets the record.
    Remove,
    /// Records the stream neither stopped nor failed.
    Start,
}

impl StreamRecord {
    /// A record of the stream at `location`, shown as `display_name` and
    /// running from `stream_environment`, neither stopped nor failed,
    /// implicated in no crash, and carrying no ruling.
    pub fn of_a_running_stream(
        location: StreamLocation,
        display_name: impl Into<String>,
        stream_environment: &StreamEnvironment,
        graph: serde_json::Value,
    ) -> Self {
        Self {
            schema_version: STREAM_RECORD_SCHEMA_VERSION,
            stream_id: location.stream_id(),
            location,
            display_name: display_name.into(),
            interpreter: stream_environment.interpreter.clone(),
            graph: Some(graph),
            stopped: false,
            failed_because: None,
            runtime_crashes_in_a_row_implicating_it: 0,
            exposure_rulings: Vec::new(),
            pending_stream_actions: Vec::new(),
        }
    }

    /// A record of the stream at `location`, shown as `display_name`, whose
    /// compile in `stream_environment` gave no graph: failed because of
    /// `failed_because`, implicated in no crash, and carrying no ruling.
    pub fn of_a_stream_whose_compile_failed(
        location: StreamLocation,
        display_name: impl Into<String>,
        stream_environment: &StreamEnvironment,
        failed_because: impl Into<String>,
    ) -> Self {
        Self {
            graph: None,
            failed_because: Some(failed_because.into()),
            ..Self::of_a_running_stream(
                location,
                display_name,
                stream_environment,
                serde_json::Value::Null,
            )
        }
    }

    /// Whether the stream is failed.
    pub fn is_failed(&self) -> bool {
        self.failed_because.is_some()
    }

    /// The state the stream is listed in while it is not loaded: failed
    /// before stopped, and running otherwise.
    pub fn listing_state(&self) -> StreamListingState {
        if self.is_failed() {
            StreamListingState::Failed
        } else if self.stopped {
            StreamListingState::Stopped
        } else {
            StreamListingState::Running
        }
    }

    /// Record the stream neither stopped nor failed, its crash count reset;
    /// `false` when it already was.
    pub fn mark_running_resetting_its_crash_count(&mut self) -> bool {
        let changed =
            self.stopped || self.is_failed() || self.runtime_crashes_in_a_row_implicating_it != 0;
        self.stopped = false;
        self.failed_because = None;
        self.runtime_crashes_in_a_row_implicating_it = 0;
        changed
    }

    /// The environment the stream's interpreters start in.
    pub fn stream_environment(&self) -> StreamEnvironment {
        StreamEnvironment {
            project_directory: self.location.project_directory.clone(),
            interpreter: self.interpreter.clone(),
        }
    }

    /// Whether the stream is stopped once its pending stops and starts have
    /// run: the last of them decides, and the record's own state otherwise.
    pub fn is_stopped_once_its_pending_stream_actions_run(&self) -> bool {
        self.pending_stream_actions
            .iter()
            .rev()
            .find_map(|pending| match pending.kind {
                PendingStreamActionKind::Stop => Some(true),
                PendingStreamActionKind::Start => Some(false),
                PendingStreamActionKind::Remove => None,
            })
            .unwrap_or(self.stopped)
    }

    /// Whether the stream is failed once its pending stops and starts have
    /// run: a pending start retries it, and the record's own state decides
    /// otherwise.
    pub fn is_failed_once_its_pending_stream_actions_run(&self) -> bool {
        self.is_failed()
            && !self
                .pending_stream_actions
                .iter()
                .any(|pending| pending.kind == PendingStreamActionKind::Start)
    }

    /// Queue the pending action `action_id` of `kind` on the record.
    pub fn queue_the_pending_stream_action(
        &mut self,
        action_id: &str,
        kind: PendingStreamActionKind,
    ) {
        self.pending_stream_actions
            .push(PendingStreamActionOnTheRecord {
                action_id: action_id.to_string(),
                kind,
            });
    }

    /// Take the pending action `action_id` off the record, its action having
    /// finished it.
    pub fn finish_the_pending_stream_action(&mut self, action_id: &str) {
        self.pending_stream_actions
            .retain(|pending| pending.action_id != action_id);
    }

    /// Take the record's attributes from `current`, the record as it is on
    /// disk: the display name unless `keep_this_display_name`, the owner's
    /// rulings and the pending actions, none of which a load changes.
    pub fn keep_the_attributes_of(&mut self, current: &StreamRecord, keep_this_display_name: bool) {
        if !keep_this_display_name {
            self.display_name = current.display_name.clone();
        }
        self.exposure_rulings = current.exposure_rulings.clone();
        self.pending_stream_actions = current.pending_stream_actions.clone();
    }

    /// Record `ruling`, replacing an earlier ruling on the same port.
    pub fn record_the_owners_exposure_ruling(&mut self, ruling: OwnerExposureRuling) {
        self.exposure_rulings
            .retain(|recorded| !recorded.names_the_same_port_as(&ruling));
        self.exposure_rulings.push(ruling);
    }
}

/// A record an earlier runtime wrote under an earlier schema, which this
/// runtime lists failed and never loads: it stays, keyed by its file name,
/// until it is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecordOfAnEarlierRuntime {
    /// The record's file name without its extension, which names it.
    pub stream_id: String,
    /// The schema version it was written under.
    pub schema_version: u64,
    /// The project directory it records, when it records one.
    pub project_directory: Option<PathBuf>,
}

impl StreamRecordOfAnEarlierRuntime {
    /// Why the record is listed failed, and how to run its stream again.
    pub fn failed_because(&self) -> String {
        let run_it_again = match &self.project_directory {
            Some(project_directory) => {
                format!(
                    "run it again with `tatolab run` in {}",
                    project_directory.display()
                )
            }
            None => "run it again with `tatolab run` in its project".to_string(),
        };
        format!(
            "an earlier runtime recorded it, under record schema {}, and this runtime does not \
             load it; {run_it_again}, and `tatolab rm {}` forgets this record",
            self.schema_version, self.stream_id
        )
    }
}

/// One file of the state directory's `streams/`, read.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamRecordAsRead {
    /// A record this runtime writes.
    Current(StreamRecord),
    /// A record an earlier runtime wrote, listed failed.
    RecordedByAnEarlierRuntime(StreamRecordOfAnEarlierRuntime),
}

impl StreamRecordAsRead {
    /// The id the record is keyed by.
    pub fn stream_id(&self) -> &str {
        match self {
            Self::Current(record) => &record.stream_id,
            Self::RecordedByAnEarlierRuntime(earlier) => &earlier.stream_id,
        }
    }
}

/// The owner's decision on how far one output port of a stream may be
/// read, which wins over the level the stream function gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerExposureRuling {
    /// The node the port belongs to, as the owner named it.
    pub node: String,
    /// The output port, as the owner named it.
    pub port: String,
    /// The level the owner set.
    pub level: OutputPortExposureLevel,
}

impl OwnerExposureRuling {
    /// Whether `other` rules on the same port, the names compared cast.
    pub fn names_the_same_port_as(&self, other: &OwnerExposureRuling) -> bool {
        self.names_the_port(&other.node, &other.port)
    }

    /// Whether this ruling's port is output port `port` of node `node`, the
    /// names compared cast; a name that casts to nothing is compared as
    /// spelled.
    pub fn names_the_port(&self, node: &str, port: &str) -> bool {
        CastPortOfAnOwnerExposureRuling::of(self).is_the_port(node, port)
    }
}

/// An owner's ruling's node and port, cast once to be compared against many
/// ports.
enum CastPortOfAnOwnerExposureRuling<'ruling> {
    /// Both names cast.
    Cast { node: String, port: String },
    /// A name casts to nothing, so both are compared as spelled.
    AsSpelled {
        node: &'ruling str,
        port: &'ruling str,
    },
}

impl<'ruling> CastPortOfAnOwnerExposureRuling<'ruling> {
    fn of(ruling: &'ruling OwnerExposureRuling) -> Self {
        match (
            cast_exposed_name_to_url_safe(&ruling.node),
            cast_exposed_name_to_url_safe(&ruling.port),
        ) {
            (Ok(node), Ok(port)) => Self::Cast {
                node: node.into_owned(),
                port: port.into_owned(),
            },
            _ => Self::AsSpelled {
                node: &ruling.node,
                port: &ruling.port,
            },
        }
    }

    fn is_the_port(&self, node: &str, port: &str) -> bool {
        match self {
            Self::Cast {
                node: ruling_node,
                port: ruling_port,
            } => match (
                cast_exposed_name_to_url_safe(node),
                cast_exposed_name_to_url_safe(port),
            ) {
                (Ok(node), Ok(port)) => node == *ruling_node && port == *ruling_port,
                _ => false,
            },
            Self::AsSpelled {
                node: ruling_node,
                port: ruling_port,
            } => node == *ruling_node && port == *ruling_port,
        }
    }
}

/// The owner's rulings on one stream, split around its load: the graph the
/// stream loads from, and the rulings its load applies once it has the nodes'
/// ports.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OwnerExposureRulingsSplitAroundTheLoad {
    /// The graph with every ruling applied that makes a port internal or rules
    /// on a port the stream function exposes, so a restriction holds before
    /// the stream admits any reader.
    pub(crate) graph_with_the_rulings_that_hold_before_the_load: serde_json::Value,
    /// The `private` and `public` rulings on ports the stream function left
    /// internal, which the graph cannot show exist; each is applied to the
    /// loaded stream, and skipped when its port is not there.
    pub(crate) rulings_applied_once_loaded: Vec<OwnerExposureRuling>,
}

/// Split `rulings` around the load of `graph_json`: an `internal` ruling, and
/// one on a port the function's own `exposed` names, is applied to the graph;
/// a ruling that opens a port the function left internal waits for the load.
pub(crate) fn the_owners_exposure_rulings_split_around_the_load(
    graph_json: &serde_json::Value,
    rulings: &[OwnerExposureRuling],
) -> OwnerExposureRulingsSplitAroundTheLoad {
    let (rulings_before_the_load, rulings_applied_once_loaded): (Vec<_>, Vec<_>) =
        rulings.iter().cloned().partition(|ruling| {
            ruling.level == OutputPortExposureLevel::Internal
                || the_function_exposes_the_port_of(graph_json, ruling)
        });
    OwnerExposureRulingsSplitAroundTheLoad {
        graph_with_the_rulings_that_hold_before_the_load:
            graph_with_the_owners_exposure_rulings_applied(graph_json, &rulings_before_the_load),
        rulings_applied_once_loaded,
    }
}

/// Whether the function's own `exposed` in `graph_json` names the port
/// `ruling` rules on.
fn the_function_exposes_the_port_of(
    graph_json: &serde_json::Value,
    ruling: &OwnerExposureRuling,
) -> bool {
    let ruled_port = CastPortOfAnOwnerExposureRuling::of(ruling);
    graph_json
        .get("exposed")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|exposed| {
            exposed
                .iter()
                .any(|entry| the_exposed_entry_is_the_port(entry, &ruled_port))
        })
}

/// Whether one entry of a graph's `exposed` names `ruled_port`.
fn the_exposed_entry_is_the_port(
    exposed_entry: &serde_json::Value,
    ruled_port: &CastPortOfAnOwnerExposureRuling<'_>,
) -> bool {
    let named = |key: &str| {
        exposed_entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    ruled_port.is_the_port(named("node"), named("port"))
}

/// `graph_json` with each of `rulings` applied to the entries of its
/// `exposed` that name the ruling's port: an `internal` ruling removes them, a
/// `private` or `public` one takes the place of the first. A ruling on a port
/// `exposed` does not name changes nothing.
fn graph_with_the_owners_exposure_rulings_applied(
    graph_json: &serde_json::Value,
    rulings: &[OwnerExposureRuling],
) -> serde_json::Value {
    let mut ruled_graph = graph_json.clone();
    let Some(exposed) = ruled_graph
        .get_mut("exposed")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return ruled_graph;
    };
    for ruling in rulings {
        let ruled_port = CastPortOfAnOwnerExposureRuling::of(ruling);
        let mut first_entry_of_the_port = None;
        let mut position = 0;
        exposed.retain(|entry| {
            let is_the_port = the_exposed_entry_is_the_port(entry, &ruled_port);
            if is_the_port && first_entry_of_the_port.is_none() {
                first_entry_of_the_port = Some(position);
            }
            if !is_the_port {
                position += 1;
            }
            !is_the_port
        });
        if let Some(position) = first_entry_of_the_port
            && ruling.level != OutputPortExposureLevel::Internal
        {
            exposed.insert(
                position,
                serde_json::json!({
                    "node": ruling.node,
                    "port": ruling.port,
                    "level": ruling.level,
                }),
            );
        }
    }
    ruled_graph
}

/// A stream record that could not be read, by the path it sits at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecordReadFailure {
    /// The record's file, or the streams directory when no record file is reached.
    pub path: PathBuf,
    /// Why it could not be read.
    pub reason: String,
}

impl std::fmt::Display for StreamRecordReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the stream record {} cannot be read: {}",
            self.path.display(),
            self.reason
        )
    }
}

impl From<StreamRecordReadFailure> for Error {
    fn from(unreadable: StreamRecordReadFailure) -> Self {
        Error::Runtime(unreadable.to_string())
    }
}

/// The stream records in a state directory's `streams/`, one `<stream id>.json`
/// per stream.
#[derive(Debug, Clone)]
pub struct StreamRecordsInTheStateDirectory {
    streams_directory: PathBuf,
    one_record_change_at_a_time: Arc<Mutex<()>>,
}

impl StreamRecordsInTheStateDirectory {
    /// The records in `streams_directory`, created owner-only when it is
    /// absent; a path there that is not a directory is refused by name.
    pub fn open(streams_directory: &Path) -> Result<Self> {
        create_directory_and_its_missing_parents_at_mode(
            streams_directory,
            OWNER_ONLY_DIRECTORY_MODE,
        )
        .map_err(|cannot_create| {
            Error::Configuration(format!(
                "the streams directory {} cannot be opened: {cannot_create}",
                streams_directory.display()
            ))
        })?;
        Ok(Self {
            streams_directory: streams_directory.to_path_buf(),
            one_record_change_at_a_time: Arc::new(Mutex::new(())),
        })
    }

    /// The directory the records sit in.
    pub fn streams_directory(&self) -> &Path {
        &self.streams_directory
    }

    /// The file the record of `stream_id` sits at, `<stream id>.json`; `None`
    /// when `stream_id` is not in the cast form every id takes, so it names
    /// no record.
    pub fn record_path_of(&self, stream_id: &str) -> Option<PathBuf> {
        is_in_exposed_name_cast_form(stream_id).then(|| {
            self.streams_directory
                .join(format!("{stream_id}.{STREAM_RECORD_FILE_EXTENSION}"))
        })
    }

    /// Change the record of `stream_id` with `edit`, read and written under
    /// one lock so no other change is lost: `edit` is handed the record — `None`
    /// when there is none, or an earlier runtime wrote it — and what it leaves
    /// is written whole, or the record forgotten when it leaves `None`. A
    /// record that cannot be read is refused naming its path.
    pub fn update<EditAnswer>(
        &self,
        stream_id: &str,
        edit: impl FnOnce(&mut Option<StreamRecord>) -> EditAnswer,
    ) -> Result<EditAnswer> {
        let _one_record_change_at_a_time = self.one_record_change_at_a_time.lock();
        let read = self.read(stream_id)?;
        let a_record_was_there = read.is_some();
        let mut record = match read {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(_)) | None => None,
        };
        let before = record.clone();
        let answer = edit(&mut record);
        match record {
            Some(record) if Some(&record) != before.as_ref() => self.write_whole(&record)?,
            Some(_) => {}
            None if a_record_was_there && before.is_some() => {
                self.remove_the_record_file(stream_id)?;
            }
            None => {}
        }
        Ok(answer)
    }

    /// Change the record of `stream_id` with `edit` when there is one, and
    /// answer it as written; `None` when there is none.
    pub fn change_the_record(
        &self,
        stream_id: &str,
        edit: impl FnOnce(&mut StreamRecord),
    ) -> Result<Option<StreamRecord>> {
        self.update(stream_id, |record| {
            record.as_mut().map(|record| {
                edit(record);
                record.clone()
            })
        })
    }

    /// Write `record` whole as a load recorded it, its attributes — display
    /// name unless `keep_this_display_name`, rulings and pending actions —
    /// kept from the record on disk, and answer it as written.
    pub fn write_the_record_a_load_made(
        &self,
        mut record: StreamRecord,
        keep_this_display_name: bool,
    ) -> Result<StreamRecord> {
        let stream_id = record.stream_id.clone();
        self.update(&stream_id, |current| {
            if let Some(current) = current.as_ref() {
                record.keep_the_attributes_of(current, keep_this_display_name);
            }
            *current = Some(record.clone());
            record
        })
    }

    /// Write `record` whole or not at all, owner-only, replacing what is on
    /// disk under the record lock.
    pub fn write(&self, record: &StreamRecord) -> Result<()> {
        let _one_record_change_at_a_time = self.one_record_change_at_a_time.lock();
        self.write_whole(record)
    }

    fn write_whole(&self, record: &StreamRecord) -> Result<()> {
        let refuse = |what_failed: String| {
            Error::Runtime(format!(
                "the record of the stream `{}` was not written to {}: {what_failed}",
                record.stream_id,
                self.streams_directory.display()
            ))
        };
        let record_path = self.record_path_of(&record.stream_id).ok_or_else(|| {
            refuse(format!(
                "`{}` is not a stream id, so it names no record file",
                record.stream_id
            ))
        })?;
        let mut record_json = serde_json::to_vec_pretty(record)
            .map_err(|cannot_serialize| refuse(cannot_serialize.to_string()))?;
        record_json.push(b'\n');

        write_an_owner_only_file_whole(&record_path, &record_json)
            .map_err(|write_failure| refuse(write_failure.to_string()))?;
        Ok(())
    }

    /// The record of `stream_id`, `None` when there is none; a record that
    /// cannot be read is refused naming its path.
    pub fn read(
        &self,
        stream_id: &str,
    ) -> std::result::Result<Option<StreamRecordAsRead>, StreamRecordReadFailure> {
        let Some(record_path) = self.record_path_of(stream_id) else {
            return Ok(None);
        };
        match std::fs::read(&record_path) {
            Ok(record_bytes) => read_a_stream_record(&record_path, &record_bytes).map(Some),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(not_read) => Err(StreamRecordReadFailure {
                path: record_path,
                reason: not_read.to_string(),
            }),
        }
    }

    /// Every record in the directory, ordered by file name; each that cannot
    /// be read — malformed, or of a later schema version — is reported by its
    /// path and the rest are still read.
    pub fn read_every(
        &self,
    ) -> Vec<std::result::Result<StreamRecordAsRead, StreamRecordReadFailure>> {
        let directory_entries = match std::fs::read_dir(&self.streams_directory) {
            Ok(directory_entries) => directory_entries,
            Err(not_listed) => {
                return vec![Err(StreamRecordReadFailure {
                    path: self.streams_directory.clone(),
                    reason: format!("the directory cannot be listed: {not_listed}"),
                })];
            }
        };
        let mut record_paths: Vec<PathBuf> = directory_entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_a_stream_record_file_name(path))
            .collect();
        record_paths.sort();
        record_paths
            .into_iter()
            .map(|record_path| match std::fs::read(&record_path) {
                Ok(record_bytes) => read_a_stream_record(&record_path, &record_bytes),
                Err(not_read) => Err(StreamRecordReadFailure {
                    path: record_path,
                    reason: not_read.to_string(),
                }),
            })
            .collect()
    }

    /// Remove the record of `stream_id` — one an earlier runtime wrote
    /// included — under the record lock; `false` when there was none.
    pub fn remove(&self, stream_id: &str) -> Result<bool> {
        let _one_record_change_at_a_time = self.one_record_change_at_a_time.lock();
        self.remove_the_record_file(stream_id)
    }

    fn remove_the_record_file(&self, stream_id: &str) -> Result<bool> {
        let Some(record_path) = self.record_path_of(stream_id) else {
            return Ok(false);
        };
        match std::fs::remove_file(&record_path) {
            Ok(()) => {
                self.sync_the_streams_directory();
                Ok(true)
            }
            Err(not_removed) if not_removed.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(not_removed) => Err(Error::Runtime(format!(
                "the stream record {} was not removed: {not_removed}",
                record_path.display()
            ))),
        }
    }

    /// Sync the records' directory, best effort, so a rename or a removal in
    /// it reaches the disk.
    fn sync_the_streams_directory(&self) {
        if let Ok(directory) = std::fs::File::open(&self.streams_directory) {
            let _ = directory.sync_all();
        }
    }
}

/// Whether `path` names a record: `<stream id>.json`. A stream id may begin
/// with `.`, and a whole-file write's temporary ends in `.tmp`, so the
/// extension alone tells them apart.
fn is_a_stream_record_file_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == STREAM_RECORD_FILE_EXTENSION)
}

/// Read one record from `record_bytes`: a record of an earlier schema version
/// is read as one an earlier runtime wrote, keyed by its file name, and one of
/// a later version is refused by that version before its fields are read.
fn read_a_stream_record(
    record_path: &Path,
    record_bytes: &[u8],
) -> std::result::Result<StreamRecordAsRead, StreamRecordReadFailure> {
    let failure = |reason: String| StreamRecordReadFailure {
        path: record_path.to_path_buf(),
        reason,
    };
    let record_document: serde_json::Value = serde_json::from_slice(record_bytes)
        .map_err(|not_json| failure(format!("it is not JSON: {not_json}")))?;
    let record_file_stem = record_path
        .file_stem()
        .map(|file_stem| file_stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let current_version = u64::from(STREAM_RECORD_SCHEMA_VERSION);
    match record_document
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version)
            if version == current_version
                || version
                    == u64::from(STREAM_RECORD_SCHEMA_VERSION_HOLDING_NO_PENDING_STREAM_ACTION) => {
        }
        Some(earlier_version) if earlier_version < current_version => {
            return Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(
                StreamRecordOfAnEarlierRuntime {
                    stream_id: record_file_stem,
                    schema_version: earlier_version,
                    project_directory: record_document
                        .get("project_directory")
                        .and_then(serde_json::Value::as_str)
                        .map(PathBuf::from),
                },
            ));
        }
        Some(later_version) => {
            return Err(failure(format!(
                "it is a record of schema version {later_version}, and this runtime reads \
                 version {STREAM_RECORD_SCHEMA_VERSION}"
            )));
        }
        None => {
            return Err(failure(
                "it carries no `schema_version`, so it is not a stream record".to_string(),
            ));
        }
    }
    let mut record = StreamRecord::deserialize(&record_document)
        .map_err(|not_a_record| failure(format!("it is not a stream record: {not_a_record}")))?;
    record.schema_version = STREAM_RECORD_SCHEMA_VERSION;
    let recorded_stream_id = &record.stream_id;
    if *recorded_stream_id != record_file_stem {
        return Err(failure(format!(
            "it records the stream `{recorded_stream_id}`, whose record is \
             `{recorded_stream_id}.{STREAM_RECORD_FILE_EXTENSION}` and not this file, so no \
             stream action on `{record_file_stem}` reaches it; rename it or remove it"
        )));
    }
    let location_stream_id = record.location.stream_id();
    if *recorded_stream_id != location_stream_id {
        return Err(failure(format!(
            "it records the stream `{recorded_stream_id}` at {}, whose stream id is \
             `{location_stream_id}`; a stream's id is the one its location computes to, so \
             remove this record and run the stream again",
            record.location
        )));
    }
    Ok(StreamRecordAsRead::Current(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A record of the stream `function` in the project folder `folder`.
    fn a_record_of(folder: &str, function: &str) -> StreamRecord {
        let project_directory = PathBuf::from(format!("/home/someone/{folder}"));
        StreamRecord::of_a_running_stream(
            StreamLocation {
                project_directory: project_directory.clone(),
                stream_function: format!("stream:{function}"),
            },
            function,
            &StreamEnvironment {
                interpreter: project_directory.join(".venv/bin/python"),
                project_directory,
            },
            serde_json::json!({
                "stream": function,
                "nodes": [{"name": "camera", "type": "tatolab.stream:CameraSource", "config": {}}],
                "links": [],
                "exposed": [{"node": "camera", "port": "video", "level": "private"}],
            }),
        )
    }

    fn current(record: StreamRecord) -> StreamRecordAsRead {
        StreamRecordAsRead::Current(record)
    }

    fn file_names_in(directory: &Path) -> Vec<String> {
        let mut file_names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        file_names.sort();
        file_names
    }

    fn a_ruling(node: &str, port: &str, level: OutputPortExposureLevel) -> OwnerExposureRuling {
        OwnerExposureRuling {
            node: node.to_string(),
            port: port.to_string(),
            level,
        }
    }

    #[test]
    fn a_written_record_reads_back_whole_from_a_file_named_after_its_stream_id() {
        let state_directory = tempfile::tempdir().unwrap();
        let records =
            StreamRecordsInTheStateDirectory::open(&state_directory.path().join("streams"))
                .unwrap();
        let mut record = a_record_of("camera-app", "preview");
        record.stopped = true;
        record.record_the_owners_exposure_ruling(a_ruling(
            "camera",
            "video",
            OutputPortExposureLevel::Internal,
        ));
        let stream_id = record.location.stream_id();
        assert_eq!(record.stream_id, stream_id);

        records.write(&record).unwrap();

        assert_eq!(
            records.read(&stream_id).unwrap(),
            Some(current(record.clone()))
        );
        assert_eq!(records.read_every(), vec![Ok(current(record))]);
        assert_eq!(
            file_names_in(records.streams_directory()),
            [format!("{stream_id}.json")]
        );
    }

    #[test]
    fn a_schema_3_record_holding_a_graph_reads_it_as_present() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of("camera-app", "preview");
        let schema_3_file = serde_json::json!({
            "schema_version": 3,
            "stream_id": record.stream_id,
            "location": record.location,
            "display_name": "preview",
            "interpreter": record.interpreter,
            "graph": {
                "stream": "preview",
                "nodes": [{"name": "camera", "type": "tatolab.stream:CameraSource", "config": {}}],
                "links": [],
                "exposed": [{"node": "camera", "port": "video", "level": "private"}],
            },
            "stopped": false,
            "failed_because": null,
            "runtime_crashes_in_a_row_implicating_it": 0,
            "exposure_rulings": [],
        });
        std::fs::write(
            state_directory
                .path()
                .join(format!("{}.json", record.stream_id)),
            serde_json::to_vec(&schema_3_file).unwrap(),
        )
        .unwrap();

        let read = records.read(&record.stream_id).unwrap();

        assert_eq!(read, Some(current(record)));
    }

    #[test]
    fn a_record_of_a_stream_whose_compile_failed_holds_no_graph_and_reads_back_whole() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let compiled = a_record_of("camera-app", "preview");
        let record = StreamRecord::of_a_stream_whose_compile_failed(
            compiled.location.clone(),
            "preview",
            &compiled.stream_environment(),
            "Traceback (most recent call last):\nValueError: bad wiring",
        );

        records.write(&record).unwrap();

        assert_eq!(record.graph, None);
        assert_eq!(record.listing_state(), StreamListingState::Failed);
        assert_eq!(
            records.read(&record.stream_id).unwrap(),
            Some(current(record))
        );
    }

    #[test]
    fn a_record_whose_id_begins_with_a_dot_is_read_like_any_other() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of(".config-app", "preview");
        assert!(
            record.stream_id.starts_with(".config-app-"),
            "{}",
            record.stream_id
        );

        records.write(&record).unwrap();

        assert_eq!(records.read_every(), vec![Ok(current(record))]);
    }

    #[test]
    fn a_record_holds_its_id_location_and_display_name_under_schema_4() {
        let record = a_record_of("camera-app", "preview");

        let written = serde_json::to_value(&record).unwrap();

        assert_eq!(written["schema_version"], 4);
        assert_eq!(written["pending_stream_actions"], serde_json::json!([]));
        assert_eq!(written["stream_id"], record.location.stream_id());
        assert_eq!(
            written["location"],
            serde_json::json!({
                "project_directory": "/home/someone/camera-app",
                "stream_function": "stream:preview",
            })
        );
        assert_eq!(written["display_name"], "preview");
    }

    #[test]
    fn the_streams_directory_is_owner_only_and_each_record_is_its_owners_alone() {
        let state_directory = tempfile::tempdir().unwrap();
        let streams_directory = state_directory.path().join("streams");
        let records = StreamRecordsInTheStateDirectory::open(&streams_directory).unwrap();
        let record = a_record_of("camera-app", "preview");

        records.write(&record).unwrap();

        let mode_of = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_of(&streams_directory), 0o700);
        assert_eq!(
            mode_of(&records.record_path_of(&record.stream_id).unwrap()),
            0o600
        );
    }

    #[test]
    fn a_rewrite_replaces_the_record_whole_and_leaves_no_temporary_file() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let first = a_record_of("camera-app", "preview");
        records.write(&first).unwrap();

        let mut stopped = first.clone();
        stopped.stopped = true;
        records.write(&stopped).unwrap();

        assert_eq!(
            records.read(&first.stream_id).unwrap(),
            Some(current(stopped))
        );
        assert_eq!(
            file_names_in(records.streams_directory()),
            [format!("{}.json", first.stream_id)]
        );
    }

    #[test]
    fn every_record_is_read_and_each_unreadable_one_is_reported_by_its_path() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let directory = records.streams_directory().to_path_buf();
        let camera = a_record_of("a-camera", "main");
        records.write(&camera).unwrap();
        std::fs::write(directory.join("b-malformed.json"), b"{ not json").unwrap();
        let mut later_schema = serde_json::to_value(a_record_of("c-later", "main")).unwrap();
        later_schema["schema_version"] = serde_json::json!(5);
        later_schema["crashed_at"] = serde_json::json!("2026-10-10T00:00:00Z");
        std::fs::write(
            directory.join("c-later.json"),
            serde_json::to_vec(&later_schema).unwrap(),
        )
        .unwrap();
        std::fs::write(directory.join("notes.txt"), b"not a record").unwrap();
        std::fs::write(directory.join(".e-half.json.1.0.tmp"), b"{").unwrap();

        let every_record = records.read_every();

        assert_eq!(every_record.len(), 3, "{every_record:?}");
        assert_eq!(every_record[0], Ok(current(camera)));
        let malformed = every_record[1].as_ref().unwrap_err();
        assert_eq!(malformed.path, directory.join("b-malformed.json"));
        assert!(malformed.reason.contains("not JSON"), "{malformed}");
        let later = every_record[2].as_ref().unwrap_err();
        assert_eq!(later.path, directory.join("c-later.json"));
        assert!(later.reason.contains("schema version 5"), "{later}");
    }

    #[test]
    fn a_schema_3_record_reads_as_holding_no_pending_action_and_is_rewritten_under_schema_4() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of("camera-app", "preview");
        let mut schema_3 = serde_json::to_value(&record).unwrap();
        schema_3["schema_version"] = serde_json::json!(3);
        schema_3
            .as_object_mut()
            .unwrap()
            .remove("pending_stream_actions");
        std::fs::write(
            records.record_path_of(&record.stream_id).unwrap(),
            serde_json::to_vec(&schema_3).unwrap(),
        )
        .unwrap();

        assert_eq!(
            records.read(&record.stream_id).unwrap(),
            Some(current(record.clone()))
        );
        records
            .change_the_record(&record.stream_id, |recorded| {
                recorded.display_name = "Desk".to_string();
            })
            .unwrap();
        let rewritten: serde_json::Value = serde_json::from_slice(
            &std::fs::read(records.record_path_of(&record.stream_id).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(rewritten["schema_version"], 4);
        assert_eq!(rewritten["display_name"], "Desk");
    }

    #[test]
    fn updates_from_many_threads_each_keep_what_the_others_changed() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of("camera-app", "preview");
        records.write(&record).unwrap();

        std::thread::scope(|scope| {
            for action_number in 0..16 {
                let records = records.clone();
                let stream_id = record.stream_id.clone();
                scope.spawn(move || {
                    records
                        .change_the_record(&stream_id, |recorded| {
                            recorded.queue_the_pending_stream_action(
                                &format!("R-{action_number}"),
                                PendingStreamActionKind::Stop,
                            );
                        })
                        .unwrap();
                });
            }
        });

        let Some(StreamRecordAsRead::Current(updated)) = records.read(&record.stream_id).unwrap()
        else {
            panic!("the record is gone");
        };
        assert_eq!(updated.pending_stream_actions.len(), 16);
    }

    #[test]
    fn a_loads_record_keeps_the_display_name_rulings_and_marks_written_since_it_read() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let mut renamed_while_it_loaded = a_record_of("camera-app", "preview");
        renamed_while_it_loaded.display_name = "Desk".to_string();
        renamed_while_it_loaded
            .queue_the_pending_stream_action("R-3", PendingStreamActionKind::Stop);
        records.write(&renamed_while_it_loaded).unwrap();
        let mut what_the_load_made = a_record_of("camera-app", "preview");
        what_the_load_made.failed_because = Some("it raised".to_string());

        let written = records
            .write_the_record_a_load_made(what_the_load_made, false)
            .unwrap();

        assert_eq!(written.display_name, "Desk");
        assert_eq!(written.failed_because.as_deref(), Some("it raised"));
        assert_eq!(
            written.pending_stream_actions,
            renamed_while_it_loaded.pending_stream_actions
        );
        assert_eq!(
            records.read(&written.stream_id).unwrap(),
            Some(current(written))
        );
    }

    #[test]
    fn the_last_pending_stop_or_start_decides_whether_a_record_is_stopped_or_failed() {
        let mut record = a_record_of("camera-app", "preview");
        assert!(!record.is_stopped_once_its_pending_stream_actions_run());

        record.queue_the_pending_stream_action("R-1", PendingStreamActionKind::Stop);
        assert!(record.is_stopped_once_its_pending_stream_actions_run());
        record.queue_the_pending_stream_action("R-2", PendingStreamActionKind::Start);
        assert!(!record.is_stopped_once_its_pending_stream_actions_run());

        record.failed_because = Some("it crashed".to_string());
        assert!(!record.is_failed_once_its_pending_stream_actions_run());
        record.finish_the_pending_stream_action("R-2");
        assert!(record.is_failed_once_its_pending_stream_actions_run());
        assert_eq!(record.pending_stream_actions.len(), 1);
    }

    #[test]
    fn a_schema_2_record_is_read_as_an_earlier_runtimes_keyed_by_its_file_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        std::fs::write(
            records.streams_directory().join("main.json"),
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

        let earlier = StreamRecordOfAnEarlierRuntime {
            stream_id: "main".to_string(),
            schema_version: 2,
            project_directory: Some(PathBuf::from("/home/someone/hello")),
        };
        assert_eq!(
            records.read_every(),
            vec![Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(
                earlier.clone()
            ))]
        );
        assert_eq!(
            records.read("main").unwrap(),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(
                earlier.clone()
            ))
        );
        let failed_because = earlier.failed_because();
        assert!(
            failed_because.contains("an earlier runtime recorded it"),
            "{failed_because}"
        );
        assert!(
            failed_because.contains("run it again with `tatolab run` in /home/someone/hello"),
            "{failed_because}"
        );
        assert!(records.remove("main").unwrap());
        assert!(records.read_every().is_empty());
    }

    #[test]
    fn a_record_whose_stream_id_is_not_its_file_name_is_reported_unreadable_by_its_path() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let camera = a_record_of("a-camera", "main");
        records.write(&camera).unwrap();
        let renamed_copy = records.streams_directory().join("zz-copy.json");
        std::fs::copy(
            records.record_path_of(&camera.stream_id).unwrap(),
            &renamed_copy,
        )
        .unwrap();

        let every_record = records.read_every();

        assert_eq!(every_record.len(), 2, "{every_record:?}");
        assert_eq!(every_record[0], Ok(current(camera.clone())));
        let mismatched = every_record[1].as_ref().unwrap_err();
        assert_eq!(mismatched.path, renamed_copy);
        assert!(
            mismatched
                .reason
                .contains(&format!("`{}`", camera.stream_id)),
            "{mismatched}"
        );
        let refusal = records.read("zz-copy").unwrap_err();
        assert_eq!(refusal.path, renamed_copy);
    }

    #[test]
    fn a_record_whose_id_is_not_the_one_its_location_computes_to_is_reported_naming_both() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let mut record = a_record_of("camera-app", "preview");
        record.stream_id = "camera-app-preview-000000".to_string();
        std::fs::write(
            records
                .streams_directory()
                .join("camera-app-preview-000000.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        let refusal = records
            .read("camera-app-preview-000000")
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("`camera-app-preview-000000`"), "{refusal}");
        assert!(
            refusal.contains(&format!("`{}`", record.location.stream_id())),
            "{refusal}"
        );
    }

    #[test]
    fn a_record_carrying_a_field_this_runtime_does_not_read_is_reported_naming_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of("camera-app", "preview");
        let mut record_document = serde_json::to_value(&record).unwrap();
        record_document["stoped"] = serde_json::json!(true);
        std::fs::write(
            records.record_path_of(&record.stream_id).unwrap(),
            serde_json::to_vec(&record_document).unwrap(),
        )
        .unwrap();

        let refusal = records.read(&record.stream_id).unwrap_err().to_string();

        assert!(
            refusal.contains(&format!("{}.json", record.stream_id)),
            "{refusal}"
        );
        assert!(refusal.contains("stoped"), "{refusal}");
    }

    #[test]
    fn a_removed_record_is_gone_and_removing_it_again_reports_there_was_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let record = a_record_of("camera-app", "preview");
        records.write(&record).unwrap();

        assert!(records.remove(&record.stream_id).unwrap());
        assert_eq!(records.read(&record.stream_id).unwrap(), None);
        assert!(!records.remove(&record.stream_id).unwrap());
        assert!(records.read_every().is_empty());
    }

    #[test]
    fn a_name_that_is_not_a_stream_id_names_no_record() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();

        assert_eq!(records.record_path_of("../escape"), None);
        assert_eq!(records.read("Camera Preview").unwrap(), None);
        assert!(!records.remove("Camera Preview").unwrap());
    }

    #[test]
    fn a_streams_path_that_is_not_a_directory_is_refused_by_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let not_a_directory = state_directory.path().join("streams");
        std::fs::write(&not_a_directory, b"").unwrap();

        let refusal = StreamRecordsInTheStateDirectory::open(&not_a_directory)
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&not_a_directory.display().to_string()),
            "{refusal}"
        );
    }

    fn a_graph_exposing(exposed: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "stream": "main",
            "nodes": [
                {"name": "Camera", "type": "tatolab.stream:CameraSource"},
                {"name": "display", "type": "tatolab.stream:DisplaySink"}
            ],
            "links": [],
            "exposed": exposed,
        })
    }

    #[test]
    fn a_ruling_on_a_port_the_function_exposes_takes_its_entrys_place_before_the_load() {
        let graph = a_graph_exposing(serde_json::json!([
            {"node": "camera", "port": "video", "level": "private"},
            {"node": "display", "port": "frames", "level": "private"}
        ]));

        let split = the_owners_exposure_rulings_split_around_the_load(
            &graph,
            &[a_ruling("Camera", "Video", OutputPortExposureLevel::Public)],
        );

        assert_eq!(
            split.graph_with_the_rulings_that_hold_before_the_load["exposed"],
            serde_json::json!([
                {"node": "Camera", "port": "Video", "level": "public"},
                {"node": "display", "port": "frames", "level": "private"}
            ])
        );
        assert_eq!(
            split.graph_with_the_rulings_that_hold_before_the_load["nodes"], graph["nodes"],
            "nothing but `exposed` moves"
        );
        assert!(split.rulings_applied_once_loaded.is_empty());
    }

    #[test]
    fn a_ruling_opening_a_port_the_function_left_internal_waits_for_the_load() {
        let graph = a_graph_exposing(serde_json::json!([
            {"node": "camera", "port": "video", "level": "private"}
        ]));
        let mut graph_with_no_exposed_key = graph.clone();
        graph_with_no_exposed_key
            .as_object_mut()
            .unwrap()
            .remove("exposed");
        let rulings = [
            a_ruling("camera", "preview", OutputPortExposureLevel::Private),
            a_ruling("camera", "renamed-port", OutputPortExposureLevel::Public),
            a_ruling("microphone", "audio", OutputPortExposureLevel::Public),
        ];

        for graph in [graph, graph_with_no_exposed_key] {
            let split = the_owners_exposure_rulings_split_around_the_load(&graph, &rulings);

            assert_eq!(
                split.graph_with_the_rulings_that_hold_before_the_load, graph,
                "no port the graph cannot show exists is written into the graph that loads"
            );
            assert_eq!(split.rulings_applied_once_loaded, rulings);
        }
    }

    #[test]
    fn an_internal_ruling_removes_the_ports_entry_before_the_load_and_changes_nothing_else() {
        let graph = a_graph_exposing(serde_json::json!([
            {"node": "camera", "port": "video", "level": "public"}
        ]));

        let split = the_owners_exposure_rulings_split_around_the_load(
            &graph,
            &[
                a_ruling("camera", "video", OutputPortExposureLevel::Internal),
                a_ruling("camera", "preview", OutputPortExposureLevel::Internal),
                a_ruling("microphone", "audio", OutputPortExposureLevel::Internal),
            ],
        );

        assert_eq!(
            split.graph_with_the_rulings_that_hold_before_the_load,
            a_graph_exposing(serde_json::json!([]))
        );
        assert!(split.rulings_applied_once_loaded.is_empty());
    }

    #[test]
    fn a_later_ruling_on_a_port_replaces_the_earlier_one_whatever_its_spelling() {
        let mut record = a_record_of("camera-app", "preview");
        record.record_the_owners_exposure_ruling(a_ruling(
            "camera",
            "video",
            OutputPortExposureLevel::Public,
        ));
        record.record_the_owners_exposure_ruling(a_ruling(
            "camera",
            "preview",
            OutputPortExposureLevel::Private,
        ));
        record.record_the_owners_exposure_ruling(a_ruling(
            "Camera",
            "Video",
            OutputPortExposureLevel::Internal,
        ));

        assert_eq!(
            record.exposure_rulings,
            [
                a_ruling("camera", "preview", OutputPortExposureLevel::Private),
                a_ruling("Camera", "Video", OutputPortExposureLevel::Internal),
            ]
        );
        let split = the_owners_exposure_rulings_split_around_the_load(
            record.graph.as_ref().unwrap(),
            &record.exposure_rulings,
        );
        assert_eq!(
            split.graph_with_the_rulings_that_hold_before_the_load["exposed"],
            serde_json::json!([])
        );
        assert_eq!(
            split.rulings_applied_once_loaded,
            [a_ruling(
                "camera",
                "preview",
                OutputPortExposureLevel::Private
            )]
        );
    }
}
