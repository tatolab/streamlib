// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The streams a runtime records in its state directory: one record per
//! stream, named by its id and written whole or not at all, changed only
//! through one read-edit-write at a time, and the stream actions queued on it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};
use streamlib_runtime_client_contract::owner_only_file_written_whole::write_an_owner_only_file_whole;

use super::{StreamEnvironment, StreamListingState, StreamLocation};
use crate::core::graph::is_in_exposed_name_cast_form;
use crate::core::{Error, Result};

/// The schema version of a stream record this runtime writes and reads.
pub const STREAM_RECORD_SCHEMA_VERSION: u32 = 9;

/// The extension every stream record's file name carries.
const STREAM_RECORD_FILE_EXTENSION: &str = "json";

/// One stream, as its runtime re-loads it from its graph document: its id and location, the
/// interpreter its processors start in, whether its project is closed or its playback stopped,
/// and whether it has failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRecord {
    /// [`STREAM_RECORD_SCHEMA_VERSION`] for a record this runtime wrote.
    pub schema_version: u32,
    /// The stream id it was first recorded under, which its file is named after.
    pub stream_id: String,
    /// Its project and stream asset, and where the project sat at its last load.
    pub location: StreamLocation,
    /// The project's venv interpreter, as it was spelled at the load.
    pub interpreter: PathBuf,
    /// Whether its playback is stopped, so a restart holds it loaded and stopped.
    pub stopped: bool,
    /// Whether its project is closed, so a restart leaves it unloaded; a closed stream is
    /// stopped too.
    pub closed: bool,
    /// Whether its playback is paused, so a restart brings it back paused; a stopped stream
    /// is never paused.
    #[serde(default)]
    pub paused: bool,
    /// Why the stream is failed, so the runtime's start skips it until
    /// an `open_project` or `reload_project` retries it; `None` when it is not failed.
    pub failed_because: Option<String>,
    /// How many of the runtime's crashes in a row, the last included, the
    /// stream was implicated in.
    pub runtime_crashes_in_a_row_implicating_it: u32,
    /// The stop and remove actions queued on the stream and not yet
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
    /// Records the stream's project closed.
    Close,
    /// Forgets the record.
    Remove,
}

impl StreamRecord {
    /// A record of the stream at `location` under `stream_id`, running from
    /// `stream_environment`, neither stopped nor failed, and implicated in no crash.
    pub fn of_a_running_stream(
        stream_id: String,
        location: StreamLocation,
        stream_environment: &StreamEnvironment,
    ) -> Self {
        Self {
            schema_version: STREAM_RECORD_SCHEMA_VERSION,
            stream_id,
            location,
            interpreter: stream_environment.interpreter.clone(),
            stopped: false,
            closed: false,
            paused: false,
            failed_because: None,
            runtime_crashes_in_a_row_implicating_it: 0,
            pending_stream_actions: Vec::new(),
        }
    }

    /// Whether the stream is failed.
    pub fn is_failed(&self) -> bool {
        self.failed_because.is_some()
    }

    /// Whether the stream, failed for crashing the runtime, crashed it again held from its
    /// document with none of its node code read, so a start loads none of it.
    pub fn crashed_the_runtime_even_held_from_its_document(&self) -> bool {
        self.is_failed_for_crashing_the_runtime()
            && self.runtime_crashes_in_a_row_implicating_it
                > super::RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM
    }

    /// Whether the stream is failed for crashing the runtime, so a start reads none of its
    /// node code.
    pub fn is_failed_for_crashing_the_runtime(&self) -> bool {
        self.is_failed()
            && self.runtime_crashes_in_a_row_implicating_it
                >= super::RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM
    }

    /// The state the stream is listed in while it is not loaded: failed
    /// before closed, closed before stopped, and running otherwise.
    pub fn listing_state(&self) -> StreamListingState {
        if self.is_failed() {
            StreamListingState::Failed
        } else if self.closed {
            StreamListingState::Closed
        } else if self.stopped {
            StreamListingState::Stopped
        } else {
            StreamListingState::Running
        }
    }

    /// Record the stream open and playing, not failed, its crash count reset;
    /// `false` when it already was.
    pub fn mark_running_resetting_its_crash_count(&mut self) -> bool {
        let changed = self.stopped
            || self.closed
            || self.paused
            || self.is_failed()
            || self.runtime_crashes_in_a_row_implicating_it != 0;
        self.stopped = false;
        self.closed = false;
        self.paused = false;
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

    /// Record the stream's project open with its playback stopped.
    pub fn mark_open_with_its_playback_stopped(&mut self) {
        self.stopped = true;
        self.closed = false;
        self.paused = false;
    }

    /// Record the stream's project closed, its playback stopped.
    pub fn mark_closed(&mut self) {
        self.stopped = true;
        self.closed = true;
        self.paused = false;
    }

    /// Whether a restart brings the stream back paused: its playback paused, and neither
    /// stopped, closed nor failed.
    pub fn comes_back_paused_at_a_restart(&self) -> bool {
        self.paused && !self.stopped && !self.closed && !self.is_failed()
    }

    /// Whether the stream is stopped once its pending stops have run.
    pub fn is_stopped_once_its_pending_stream_actions_run(&self) -> bool {
        self.stopped
            || self.pending_stream_actions.iter().any(|pending| {
                matches!(
                    pending.kind,
                    PendingStreamActionKind::Stop | PendingStreamActionKind::Close
                )
            })
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
    /// disk: the pending actions and, unless this record holds the stream stopped, its pause,
    /// neither of which a load changes.
    pub fn keep_the_attributes_of(&mut self, current: &StreamRecord) {
        self.paused = current.paused && !self.stopped;
        self.pending_stream_actions = current.pending_stream_actions.clone();
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
                    "run it again with `tatolab open --play` in {}",
                    project_directory.display()
                )
            }
            None => "run it again with `tatolab open --play` in its project".to_string(),
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
    /// name, rulings and pending actions — kept from the record on disk, and
    /// answer it as written.
    pub fn write_the_record_a_load_made(&self, mut record: StreamRecord) -> Result<StreamRecord> {
        let stream_id = record.stream_id.clone();
        self.update(&stream_id, |current| {
            if let Some(current) = current.as_ref() {
                record.keep_the_attributes_of(current);
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
        Some(version) if version == current_version => {}
        Some(earlier_version) if earlier_version < current_version => {
            return Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(
                StreamRecordOfAnEarlierRuntime {
                    stream_id: record_file_stem,
                    schema_version: earlier_version,
                    project_directory: record_document
                        .pointer("/location/project_directory")
                        .or_else(|| record_document.get("project_directory"))
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
    Ok(StreamRecordAsRead::Current(record))
}

#[cfg(test)]
mod tests;
