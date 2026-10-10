// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The kept streams a runtime holds in its state directory: one record per
//! stream, written whole or not at all, and the owner's exposure rulings each
//! record carries, split around the stream's load.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

use super::StreamEnvironment;
use crate::core::graph::{OutputPortExposureLevel, cast_exposed_name_to_url_safe};
use crate::core::{Error, Result};

/// The schema version of a kept-stream record this runtime writes and reads.
pub const KEPT_STREAM_RECORD_SCHEMA_VERSION: u32 = 1;

/// The mode a kept-stream record is written at: its owner's alone.
pub const KEPT_STREAM_RECORD_FILE_MODE: u32 = 0o600;

/// The extension every kept-stream record's file name carries.
const KEPT_STREAM_RECORD_FILE_EXTENSION: &str = "json";

/// One kept stream, as its runtime re-loads it: the graph compiled at its
/// load, the environment its interpreters start in, whether it was stopped,
/// and the owner's exposure rulings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptStreamRecord {
    /// [`KEPT_STREAM_RECORD_SCHEMA_VERSION`] for a record this runtime wrote.
    pub schema_version: u32,
    /// The stream's URL-safe cast name, which its file is named after.
    pub stream_name: String,
    /// The directory the stream's own modules import from.
    pub project_directory: PathBuf,
    /// The project's venv interpreter, as it was spelled at the load.
    pub interpreter: PathBuf,
    /// The stream function as the load named it, `None` for the sole
    /// `@stream` in the project's `stream.py`.
    pub stream_function: Option<String>,
    /// The graph compiled at the load, before any ruling was applied.
    pub graph: serde_json::Value,
    /// Whether the owner stopped the stream, so a restart leaves it unloaded.
    pub stopped: bool,
    /// The owner's exposure rulings, each overriding the level the stream
    /// function gave its port.
    pub exposure_rulings: Vec<OwnerExposureRuling>,
}

impl KeptStreamRecord {
    /// A record of the stream `stream_name` running from `stream_environment`,
    /// not stopped and carrying no ruling.
    pub fn of_a_running_stream(
        stream_name: impl Into<String>,
        stream_environment: &StreamEnvironment,
        stream_function: Option<String>,
        graph: serde_json::Value,
    ) -> Self {
        Self {
            schema_version: KEPT_STREAM_RECORD_SCHEMA_VERSION,
            stream_name: stream_name.into(),
            project_directory: stream_environment.project_directory.clone(),
            interpreter: stream_environment.interpreter.clone(),
            stream_function,
            graph,
            stopped: false,
            exposure_rulings: Vec::new(),
        }
    }

    /// The environment the stream's interpreters start in.
    pub fn stream_environment(&self) -> StreamEnvironment {
        StreamEnvironment {
            project_directory: self.project_directory.clone(),
            interpreter: self.interpreter.clone(),
        }
    }

    /// The owner's rulings split around the recorded graph's load, as
    /// [`the_owners_exposure_rulings_split_around_the_load`] splits them.
    pub fn the_owners_exposure_rulings_split_around_its_load(
        &self,
    ) -> OwnerExposureRulingsSplitAroundTheLoad {
        the_owners_exposure_rulings_split_around_the_load(&self.graph, &self.exposure_rulings)
    }

    /// Record `ruling`, replacing an earlier ruling on the same port.
    pub fn record_the_owners_exposure_ruling(&mut self, ruling: OwnerExposureRuling) {
        self.exposure_rulings
            .retain(|recorded| !recorded.names_the_same_port_as(&ruling));
        self.exposure_rulings.push(ruling);
    }
}

/// The owner's decision on how far one output port of a kept stream may be
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
        match (self.cast_node_and_port(), other.cast_node_and_port()) {
            (Some(this_port), Some(other_port)) => this_port == other_port,
            _ => self.node == other.node && self.port == other.port,
        }
    }

    fn cast_node_and_port(&self) -> Option<(String, String)> {
        Some((
            cast_exposed_name_to_url_safe(&self.node).ok()?.into_owned(),
            cast_exposed_name_to_url_safe(&self.port).ok()?.into_owned(),
        ))
    }
}

/// The owner's rulings on one stream, split around its load: the graph the
/// stream loads from, and the rulings its load applies once it has the nodes'
/// ports.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerExposureRulingsSplitAroundTheLoad {
    /// The graph with every ruling applied that makes a port internal or rules
    /// on a port the stream function exposes, so a restriction holds before
    /// the stream admits any reader.
    pub graph_with_the_rulings_that_hold_before_the_load: serde_json::Value,
    /// The `private` and `public` rulings on ports the stream function left
    /// internal, which the graph cannot show exist; each is applied to the
    /// loaded stream, and skipped when its port is not there.
    pub rulings_applied_once_loaded: Vec<OwnerExposureRuling>,
}

/// Split `rulings` around the load of `graph_json`: an `internal` ruling, and
/// one on a port the function's own `exposed` names, is applied to the graph;
/// a ruling that opens a port the function left internal waits for the load.
pub fn the_owners_exposure_rulings_split_around_the_load(
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
    graph_json
        .get("exposed")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|exposed| {
            exposed
                .iter()
                .any(|entry| the_exposed_entry_names_the_port_of(entry, ruling))
        })
}

/// Whether one entry of a graph's `exposed` names the port `ruling` rules on,
/// the names compared cast.
fn the_exposed_entry_names_the_port_of(
    exposed_entry: &serde_json::Value,
    ruling: &OwnerExposureRuling,
) -> bool {
    let named = |key: &str| {
        exposed_entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    ruling.names_the_same_port_as(&OwnerExposureRuling {
        node: named("node"),
        port: named("port"),
        level: ruling.level,
    })
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
        let first_entry_of_the_port = exposed
            .iter()
            .position(|entry| the_exposed_entry_names_the_port_of(entry, ruling));
        exposed.retain(|entry| !the_exposed_entry_names_the_port_of(entry, ruling));
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

/// A kept-stream record that could not be read, by the path it sits at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptStreamRecordReadFailure {
    /// The record's file.
    pub path: PathBuf,
    /// Why it could not be read.
    pub reason: String,
}

impl std::fmt::Display for KeptStreamRecordReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the kept-stream record {} cannot be read: {}",
            self.path.display(),
            self.reason
        )
    }
}

/// The kept-stream records in a state directory's `streams/`, one
/// `<stream>.json` per kept stream.
#[derive(Debug, Clone)]
pub struct KeptStreamRecordsInTheStateDirectory {
    kept_streams_directory: PathBuf,
}

impl KeptStreamRecordsInTheStateDirectory {
    /// The records in `kept_streams_directory`, created owner-only when it is
    /// absent; a path there that is not a directory is refused by name.
    pub fn open(kept_streams_directory: &Path) -> Result<Self> {
        create_directory_and_its_missing_parents_at_mode(
            kept_streams_directory,
            OWNER_ONLY_DIRECTORY_MODE,
        )
        .map_err(|cannot_create| {
            Error::Configuration(format!(
                "the kept-streams directory {} cannot be opened: {cannot_create}",
                kept_streams_directory.display()
            ))
        })?;
        Ok(Self {
            kept_streams_directory: kept_streams_directory.to_path_buf(),
        })
    }

    /// The directory the records sit in.
    pub fn kept_streams_directory(&self) -> &Path {
        &self.kept_streams_directory
    }

    /// The file the record of `stream_name` sits at: `<cast name>.json`.
    pub fn record_path_of(&self, stream_name: &str) -> Result<PathBuf> {
        let stream_cast = cast_exposed_name_to_url_safe(stream_name)?;
        Ok(self
            .kept_streams_directory
            .join(format!("{stream_cast}.{KEPT_STREAM_RECORD_FILE_EXTENSION}")))
    }

    /// Write `record` whole or not at all: to a temporary file beside it at
    /// [`KEPT_STREAM_RECORD_FILE_MODE`], synced, then renamed over the record.
    pub fn write(&self, record: &KeptStreamRecord) -> Result<()> {
        let record_path = self.record_path_of(&record.stream_name)?;
        let refuse = |what_failed: String| {
            Error::Runtime(format!(
                "the kept-stream record of `{}` was not written to {}: {what_failed}",
                record.stream_name,
                record_path.display()
            ))
        };
        let mut record_json = serde_json::to_vec_pretty(record)
            .map_err(|cannot_serialize| refuse(cannot_serialize.to_string()))?;
        record_json.push(b'\n');

        let temporary_path = self.a_temporary_path_beside(&record_path);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEPT_STREAM_RECORD_FILE_MODE)
            .open(&temporary_path)
            .and_then(|mut temporary_file| {
                temporary_file.write_all(&record_json)?;
                temporary_file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temporary_path, &record_path));
        if let Err(write_failure) = written {
            let _ = std::fs::remove_file(&temporary_path);
            return Err(refuse(write_failure.to_string()));
        }
        // The rename reaches the disk with the directory's own sync.
        if let Ok(directory) = std::fs::File::open(&self.kept_streams_directory) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    /// The record of `stream_name`, `None` when there is none; a record that
    /// cannot be read is refused naming its path.
    pub fn read(&self, stream_name: &str) -> Result<Option<KeptStreamRecord>> {
        let record_path = self.record_path_of(stream_name)?;
        match std::fs::read(&record_path) {
            Ok(record_bytes) => read_a_kept_stream_record(&record_path, &record_bytes)
                .map(Some)
                .map_err(|failure| Error::Runtime(failure.to_string())),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(not_read) => Err(Error::Runtime(
                KeptStreamRecordReadFailure {
                    path: record_path,
                    reason: not_read.to_string(),
                }
                .to_string(),
            )),
        }
    }

    /// Every record in the directory, ordered by file name; each that cannot
    /// be read — malformed, or of another schema version — is reported by its
    /// path and the rest are still read.
    pub fn read_every(
        &self,
    ) -> Vec<std::result::Result<KeptStreamRecord, KeptStreamRecordReadFailure>> {
        let directory_entries = match std::fs::read_dir(&self.kept_streams_directory) {
            Ok(directory_entries) => directory_entries,
            Err(not_listed) => {
                return vec![Err(KeptStreamRecordReadFailure {
                    path: self.kept_streams_directory.clone(),
                    reason: format!("the directory cannot be listed: {not_listed}"),
                })];
            }
        };
        let mut record_paths: Vec<PathBuf> = directory_entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_a_kept_stream_record_file_name(path))
            .collect();
        record_paths.sort();
        record_paths
            .into_iter()
            .map(|record_path| match std::fs::read(&record_path) {
                Ok(record_bytes) => read_a_kept_stream_record(&record_path, &record_bytes),
                Err(not_read) => Err(KeptStreamRecordReadFailure {
                    path: record_path,
                    reason: not_read.to_string(),
                }),
            })
            .collect()
    }

    /// Remove the record of `stream_name`; `false` when there was none.
    pub fn remove(&self, stream_name: &str) -> Result<bool> {
        let record_path = self.record_path_of(stream_name)?;
        match std::fs::remove_file(&record_path) {
            Ok(()) => {
                if let Ok(directory) = std::fs::File::open(&self.kept_streams_directory) {
                    let _ = directory.sync_all();
                }
                Ok(true)
            }
            Err(not_removed) if not_removed.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(not_removed) => Err(Error::Runtime(format!(
                "the kept-stream record {} was not removed: {not_removed}",
                record_path.display()
            ))),
        }
    }

    /// A path in the records' directory no other write uses, hidden from
    /// [`Self::read_every`].
    fn a_temporary_path_beside(&self, record_path: &Path) -> PathBuf {
        static TEMPORARY_RECORD_COUNTER: AtomicU64 = AtomicU64::new(0);
        let record_file_name = record_path
            .file_name()
            .map(|file_name| file_name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.kept_streams_directory.join(format!(
            ".{record_file_name}.{}.{}.tmp",
            std::process::id(),
            TEMPORARY_RECORD_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

/// Whether `path` names a record: `<name>.json`, not a hidden temporary.
fn is_a_kept_stream_record_file_name(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .is_some_and(|file_name| file_name.to_string_lossy().starts_with('.'));
    !hidden
        && path
            .extension()
            .is_some_and(|extension| extension == KEPT_STREAM_RECORD_FILE_EXTENSION)
}

/// Read one record from `record_bytes`, refusing a record of another schema
/// version by that version before its fields are read.
fn read_a_kept_stream_record(
    record_path: &Path,
    record_bytes: &[u8],
) -> std::result::Result<KeptStreamRecord, KeptStreamRecordReadFailure> {
    let failure = |reason: String| KeptStreamRecordReadFailure {
        path: record_path.to_path_buf(),
        reason,
    };
    let record_document: serde_json::Value = serde_json::from_slice(record_bytes)
        .map_err(|not_json| failure(format!("it is not JSON: {not_json}")))?;
    match record_document
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) if version == u64::from(KEPT_STREAM_RECORD_SCHEMA_VERSION) => {}
        Some(other_version) => {
            return Err(failure(format!(
                "it is a record of schema version {other_version}, and this runtime reads \
                 version {KEPT_STREAM_RECORD_SCHEMA_VERSION}"
            )));
        }
        None => {
            return Err(failure(
                "it carries no `schema_version`, so it is not a kept-stream record".to_string(),
            ));
        }
    }
    KeptStreamRecord::deserialize(&record_document)
        .map_err(|not_a_record| failure(format!("it is not a kept-stream record: {not_a_record}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn a_record_named(stream_name: &str) -> KeptStreamRecord {
        KeptStreamRecord::of_a_running_stream(
            stream_name,
            &StreamEnvironment {
                project_directory: PathBuf::from("/home/someone/camera-app"),
                interpreter: PathBuf::from("/home/someone/camera-app/.venv/bin/python"),
            },
            Some("stream.py:main".to_string()),
            serde_json::json!({
                "stream": stream_name,
                "nodes": [{"name": "camera", "type": "tatolab.stream:CameraSource", "config": {}}],
                "links": [],
                "exposed": [{"node": "camera", "port": "video", "level": "private"}],
            }),
        )
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
    fn a_written_record_reads_back_whole_from_a_file_named_after_its_stream() {
        let state_directory = tempfile::tempdir().unwrap();
        let records =
            KeptStreamRecordsInTheStateDirectory::open(&state_directory.path().join("streams"))
                .unwrap();
        let mut record = a_record_named("camera-preview");
        record.stopped = true;
        record.record_the_owners_exposure_ruling(a_ruling(
            "camera",
            "video",
            OutputPortExposureLevel::Internal,
        ));

        records.write(&record).unwrap();

        assert_eq!(
            records.read("camera-preview").unwrap(),
            Some(record.clone())
        );
        assert_eq!(records.read_every(), vec![Ok(record)]);
        assert_eq!(
            file_names_in(records.kept_streams_directory()),
            ["camera-preview.json"]
        );
    }

    #[test]
    fn the_streams_directory_is_owner_only_and_each_record_is_its_owners_alone() {
        let state_directory = tempfile::tempdir().unwrap();
        let kept_streams_directory = state_directory.path().join("streams");
        let records = KeptStreamRecordsInTheStateDirectory::open(&kept_streams_directory).unwrap();

        records.write(&a_record_named("camera-preview")).unwrap();

        let mode_of = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_of(&kept_streams_directory), 0o700);
        assert_eq!(
            mode_of(&kept_streams_directory.join("camera-preview.json")),
            0o600
        );
    }

    #[test]
    fn a_rewrite_replaces_the_record_whole_and_leaves_no_temporary_file() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = KeptStreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let first = a_record_named("camera-preview");
        records.write(&first).unwrap();

        let mut stopped = first.clone();
        stopped.stopped = true;
        records.write(&stopped).unwrap();

        assert_eq!(records.read("camera-preview").unwrap(), Some(stopped));
        assert_eq!(
            file_names_in(records.kept_streams_directory()),
            ["camera-preview.json"]
        );
    }

    #[test]
    fn every_record_is_read_and_each_unreadable_one_is_reported_by_its_path() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = KeptStreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let directory = records.kept_streams_directory().to_path_buf();
        records.write(&a_record_named("a-camera")).unwrap();
        records.write(&a_record_named("d-microphone")).unwrap();
        std::fs::write(directory.join("b-malformed.json"), b"{ not json").unwrap();
        let mut later_schema = serde_json::to_value(a_record_named("c-later")).unwrap();
        later_schema["schema_version"] = serde_json::json!(2);
        later_schema["failed"] = serde_json::json!(true);
        std::fs::write(
            directory.join("c-later.json"),
            serde_json::to_vec(&later_schema).unwrap(),
        )
        .unwrap();
        std::fs::write(directory.join("notes.txt"), b"not a record").unwrap();
        std::fs::write(directory.join(".e-half.json.1.0.tmp"), b"{").unwrap();

        let every_record = records.read_every();

        assert_eq!(every_record.len(), 4, "{every_record:?}");
        assert_eq!(every_record[0], Ok(a_record_named("a-camera")));
        let malformed = every_record[1].as_ref().unwrap_err();
        assert_eq!(malformed.path, directory.join("b-malformed.json"));
        assert!(malformed.reason.contains("not JSON"), "{malformed}");
        let later = every_record[2].as_ref().unwrap_err();
        assert_eq!(later.path, directory.join("c-later.json"));
        assert!(later.reason.contains("schema version 2"), "{later}");
        assert_eq!(every_record[3], Ok(a_record_named("d-microphone")));
    }

    #[test]
    fn a_record_carrying_a_field_this_runtime_does_not_read_is_reported_naming_it() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = KeptStreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        let mut record = serde_json::to_value(a_record_named("camera-preview")).unwrap();
        record["stoped"] = serde_json::json!(true);
        std::fs::write(
            records.kept_streams_directory().join("camera-preview.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        let refusal = records.read("camera-preview").unwrap_err().to_string();

        assert!(refusal.contains("camera-preview.json"), "{refusal}");
        assert!(refusal.contains("stoped"), "{refusal}");
    }

    #[test]
    fn a_removed_record_is_gone_and_removing_it_again_reports_there_was_none() {
        let state_directory = tempfile::tempdir().unwrap();
        let records = KeptStreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        records.write(&a_record_named("camera-preview")).unwrap();

        assert!(records.remove("camera-preview").unwrap());
        assert_eq!(records.read("camera-preview").unwrap(), None);
        assert!(!records.remove("camera-preview").unwrap());
        assert!(records.read_every().is_empty());
    }

    #[test]
    fn a_kept_streams_path_that_is_not_a_directory_is_refused_by_name() {
        let state_directory = tempfile::tempdir().unwrap();
        let not_a_directory = state_directory.path().join("streams");
        std::fs::write(&not_a_directory, b"").unwrap();

        let refusal = KeptStreamRecordsInTheStateDirectory::open(&not_a_directory)
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
        let mut record = a_record_named("camera-preview");
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
        let split = record.the_owners_exposure_rulings_split_around_its_load();
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
