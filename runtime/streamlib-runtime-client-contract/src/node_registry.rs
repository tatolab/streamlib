// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! On-disk discovery registry for runtimes serving a local API.
//!
//! A host serving a runtime's local API writes one JSON entry per runtime into
//! `<runtime directory>/nodes/<runtime_id>.json` once its local API socket is
//! served, and removes it when it stops serving. The runtime directory
//! is the one the engine resolved and checked as the runtime started
//! ([`crate::streamlib_runtime_directory::StreamlibRuntimeDirectory::node_registry_directory`]).
//! A CLI discovers live control planes by scanning that directory. Entry existence
//! is tied to the control endpoint existing: a runtime without a local API never appears.
//!
//! The file body is the wire contract between the writing runtime and any
//! reader — the native `tatolab nodes` reads it through this module;
//! [`NODE_REGISTRY_SCHEMA_VERSION`] stamps it so a reader rejects an entry it
//! does not understand.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

/// What every entry file's name ends with.
const ENTRY_FILE_NAME_SUFFIX: &str = ".json";

/// Schema version stamped into every [`NodeRegistryEntry`]. A reader skips an
/// entry whose `schema_version` it does not recognize.
pub const NODE_REGISTRY_SCHEMA_VERSION: u32 = 3;

/// One discovery entry: a running runtime's local API endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRegistryEntry {
    /// Wire-format version of this entry ([`NODE_REGISTRY_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The runtime's `RuntimeUniqueId`, verbatim.
    pub runtime_id: String,
    /// The name the runtime's tap channels carry — stable across runs of one
    /// app, and what `--node` resolves.
    pub runtime_name: String,
    /// The Unix socket the runtime's local API is served on, openable only by its user.
    pub local_api_socket_path: PathBuf,
    /// OS process id hosting the control plane.
    pub pid: u32,
    /// Human hint for disambiguating nodes in a listing (process arg0 + cwd);
    /// an entry that carries none reads as empty.
    #[serde(default)]
    pub hint: String,
}

impl NodeRegistryEntry {
    /// Build an entry for the runtime named `runtime_name` reachable at
    /// `local_api_socket_path`, stamping the current process id and a hint
    /// derived from this process's arg0 and cwd.
    pub fn for_current_process(
        runtime_id: String,
        runtime_name: &str,
        local_api_socket_path: PathBuf,
    ) -> Self {
        Self {
            schema_version: NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id,
            runtime_name: runtime_name.to_string(),
            local_api_socket_path,
            pid: std::process::id(),
            hint: current_process_hint(),
        }
    }
}

/// One entry [`scan_entries`] read, with the file it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedNodeRegistryEntry {
    /// The entry file the scan read.
    pub entry_file_path: PathBuf,
    /// The entry that file holds.
    pub node_registry_entry: NodeRegistryEntry,
}

/// A named failure of a node-registry filesystem operation. No `()`-errors: each
/// variant carries the offending path and the underlying cause.
#[derive(Debug, thiserror::Error)]
pub enum NodeRegistryError {
    /// Creating the registry directory (`<runtime directory>/nodes`) failed.
    #[error("failed to create node registry directory {path}: {source}")]
    RegistryDirCreate {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Serializing an entry to JSON failed.
    #[error("failed to encode node registry entry for {runtime_id}: {source}")]
    EntryEncode {
        runtime_id: String,
        source: serde_json::Error,
    },
    /// Writing an entry file failed.
    #[error("failed to write node registry entry {path}: {source}")]
    EntryWrite {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Reading the registry directory failed.
    #[error("failed to read node registry path {path}: {source}")]
    EntryRead {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Removing an entry file failed.
    #[error("failed to remove node registry entry {path}: {source}")]
    EntryRemove {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Decoding an entry file's JSON failed.
    #[error("failed to decode node registry entry {path}: {source}")]
    EntryDecode {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// An entry decoded but carries a `schema_version` this reader does not
    /// understand; `scan_entries` skips such an entry rather than failing.
    #[error(
        "node registry entry {path} has unrecognized schema_version {found} \
         (this reader understands {expected})"
    )]
    EntrySchemaVersionMismatch {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
}

/// Write (create or replace) the discovery entry for `entry.runtime_id` into
/// `registry_directory`, creating it if needed. Returns the entry's path.
#[tracing::instrument(skip(entry), fields(runtime_id = %entry.runtime_id, local_api_socket_path = %entry.local_api_socket_path.display()))]
pub fn write_entry(
    registry_directory: &Path,
    entry: &NodeRegistryEntry,
) -> Result<PathBuf, NodeRegistryError> {
    create_directory_and_its_missing_parents_at_mode(registry_directory, OWNER_ONLY_DIRECTORY_MODE)
        .map_err(|source| NodeRegistryError::RegistryDirCreate {
            path: registry_directory.to_path_buf(),
            source,
        })?;
    let path = registry_directory.join(entry_file_name(&entry.runtime_id));
    let json =
        serde_json::to_vec_pretty(entry).map_err(|source| NodeRegistryError::EntryEncode {
            runtime_id: entry.runtime_id.clone(),
            source,
        })?;
    std::fs::write(&path, json).map_err(|source| NodeRegistryError::EntryWrite {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Remove the discovery entry for `runtime_id` from `registry_directory`. A
/// missing entry is not an error (idempotent teardown).
#[tracing::instrument]
pub fn remove_entry(registry_directory: &Path, runtime_id: &str) -> Result<(), NodeRegistryError> {
    remove_scanned_entry_file(&registry_directory.join(entry_file_name(runtime_id)))
}

/// Decode an entry file's bytes, checking `schema_version` before the rest so an
/// entry of another version is refused by its version rather than by whichever
/// field it lacks.
///
/// Every field must be the exact JSON type the writing runtime emits — a JSON
/// object at the top, a whole number in `u32` for `schema_version` and `pid`,
/// a string for the rest — so a `null` name is refused rather than coerced into
/// one a listing would show and `--node` would resolve.
fn decode_entry_at_this_schema_version(
    path: &Path,
    bytes: &[u8],
) -> Result<NodeRegistryEntry, NodeRegistryError> {
    #[derive(Deserialize)]
    struct EntrySchemaVersionOnly {
        schema_version: u32,
    }

    let decode_failure = |source| NodeRegistryError::EntryDecode {
        path: path.to_path_buf(),
        source,
    };
    // Through a map, because serde also reads a struct from a JSON array.
    let entry_object = serde_json::Value::Object(
        serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
            .map_err(decode_failure)?,
    );
    let found = EntrySchemaVersionOnly::deserialize(&entry_object)
        .map_err(decode_failure)?
        .schema_version;
    if found != NODE_REGISTRY_SCHEMA_VERSION {
        return Err(NodeRegistryError::EntrySchemaVersionMismatch {
            path: path.to_path_buf(),
            found,
            expected: NODE_REGISTRY_SCHEMA_VERSION,
        });
    }
    NodeRegistryEntry::deserialize(entry_object).map_err(decode_failure)
}

/// Scan every discovery entry — every `*.json` file in `registry_directory`, in
/// file-name order — skipping (with a warning) any unreadable, undecodable, or
/// version-mismatched file so one corrupt entry never breaks a listing. A
/// missing registry directory yields an empty list. Only a failure to read the
/// directory itself is a hard error.
#[tracing::instrument]
pub fn scan_entries(
    registry_directory: &Path,
) -> Result<Vec<ScannedNodeRegistryEntry>, NodeRegistryError> {
    let read_dir = match std::fs::read_dir(registry_directory) {
        Ok(read_dir) => read_dir,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(NodeRegistryError::EntryRead {
                path: registry_directory.to_path_buf(),
                source,
            });
        }
    };

    let mut entry_file_paths = Vec::new();
    for dir_entry in read_dir {
        let dir_entry = dir_entry.map_err(|source| NodeRegistryError::EntryRead {
            path: registry_directory.to_path_buf(),
            source,
        })?;
        if dir_entry
            .file_name()
            .as_bytes()
            .ends_with(ENTRY_FILE_NAME_SUFFIX.as_bytes())
        {
            entry_file_paths.push(dir_entry.path());
        }
    }
    entry_file_paths.sort();

    let mut entries = Vec::new();
    for entry_file_path in entry_file_paths {
        let bytes = match std::fs::read(&entry_file_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(path = %entry_file_path.display(), %error, "skipping unreadable node registry entry");
                continue;
            }
        };
        match decode_entry_at_this_schema_version(&entry_file_path, &bytes) {
            Ok(node_registry_entry) => entries.push(ScannedNodeRegistryEntry {
                entry_file_path,
                node_registry_entry,
            }),
            Err(NodeRegistryError::EntrySchemaVersionMismatch { found, .. }) => {
                tracing::warn!(
                    path = %entry_file_path.display(),
                    schema_version = found,
                    "skipping node registry entry with unrecognized schema_version"
                );
            }
            Err(error) => {
                tracing::warn!(path = %entry_file_path.display(), %error, "skipping undecodable node registry entry");
            }
        }
    }
    Ok(entries)
}

/// Remove an entry file a scan read, for a reader pruning an entry whose runtime
/// is gone. A file already gone is not an error: another reader pruned it first.
#[tracing::instrument]
pub fn remove_scanned_entry_file(entry_file_path: &Path) -> Result<(), NodeRegistryError> {
    match std::fs::remove_file(entry_file_path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(NodeRegistryError::EntryRemove {
            path: entry_file_path.to_path_buf(),
            source,
        }),
    }
}

/// The on-disk filename for `runtime_id`: `<runtime_id>.json` with any character
/// outside `[A-Za-z0-9._-]` replaced by `_`, so a `STREAMLIB_RUNTIME_ID`
/// carrying path separators cannot escape the registry directory. `write_entry`
/// and `remove_entry` derive the name identically, so removal always targets the
/// file a prior write created.
fn entry_file_name(runtime_id: &str) -> String {
    let sanitized: String = runtime_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitized}{ENTRY_FILE_NAME_SUFFIX}")
}

/// A one-line hint for disambiguating nodes: the process's arg0 basename and
/// current working directory, when resolvable.
fn current_process_hint() -> String {
    let arg0 = std::env::args()
        .next()
        .map(|arg0| {
            std::path::Path::new(&arg0)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or(arg0)
        })
        .unwrap_or_default();
    let cwd = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.display().to_string())
        .unwrap_or_default();
    match (arg0.is_empty(), cwd.is_empty()) {
        (false, false) => format!("{arg0} ({cwd})"),
        (false, true) => arg0,
        (true, false) => cwd,
        (true, true) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    //! Registry write / scan / remove / prune-shape and the
    //! `schema_version` round-trip, each against its own tempdir registry.

    use super::*;

    /// Run `f` against a fresh registry directory inside a tempdir.
    fn with_isolated_registry_directory<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
        let runtime_directory = tempfile::tempdir().expect("tempdir");
        f(&runtime_directory.path().join("nodes"))
    }

    fn scanned_entries(registry_directory: &Path) -> Vec<NodeRegistryEntry> {
        scan_entries(registry_directory)
            .expect("scan")
            .into_iter()
            .map(|scanned| scanned.node_registry_entry)
            .collect()
    }

    fn sample_entry(runtime_id: &str) -> NodeRegistryEntry {
        NodeRegistryEntry {
            schema_version: NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id: runtime_id.to_string(),
            runtime_name: format!("rig-example-{runtime_id}"),
            local_api_socket_path: PathBuf::from(format!(
                "/tmp/streamlib-1000/local-api-{runtime_id}.sock"
            )),
            pid: 4242,
            hint: "streamlib (/tmp/example)".to_string(),
        }
    }

    #[test]
    fn write_then_scan_round_trips_the_entry_in_the_registry_directory() {
        with_isolated_registry_directory(|registry_directory| {
            let entry = sample_entry("Rnode-alpha");
            let path = write_entry(registry_directory, &entry).expect("write");
            assert!(
                path.starts_with(registry_directory),
                "entry {} must land in the registry directory {}",
                path.display(),
                registry_directory.display()
            );

            let scanned = scan_entries(registry_directory).expect("scan");
            assert_eq!(
                scanned,
                vec![ScannedNodeRegistryEntry {
                    entry_file_path: path,
                    node_registry_entry: entry
                }]
            );
        });
    }

    #[test]
    fn schema_version_survives_a_serde_round_trip() {
        let entry = sample_entry("Rnode-beta");
        let json = serde_json::to_string(&entry).expect("encode");
        assert!(
            json.contains("\"schema_version\""),
            "schema_version must be present in the wire form: {json}"
        );
        assert!(
            json.contains("\"runtime_name\""),
            "the runtime's name is part of the entry's wire form: {json}"
        );
        let decoded: NodeRegistryEntry = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.schema_version, NODE_REGISTRY_SCHEMA_VERSION);
        assert_eq!(decoded.runtime_name, entry.runtime_name);
        assert_eq!(decoded, entry);
    }

    #[test]
    fn remove_entry_deletes_the_file_and_is_idempotent() {
        with_isolated_registry_directory(|registry_directory| {
            let entry = sample_entry("Rnode-gamma");
            write_entry(registry_directory, &entry).expect("write");
            assert_eq!(scan_entries(registry_directory).expect("scan").len(), 1);

            remove_entry(registry_directory, &entry.runtime_id).expect("remove");
            assert!(scan_entries(registry_directory).expect("scan").is_empty());

            remove_entry(registry_directory, &entry.runtime_id).expect("second remove is a no-op");
        });
    }

    #[test]
    fn scan_skips_a_corrupt_entry_and_still_returns_the_valid_ones() {
        with_isolated_registry_directory(|registry_directory| {
            let good = sample_entry("Rgood");
            write_entry(registry_directory, &good).expect("write good");
            let corrupt_path = registry_directory.join("Rcorrupt.json");
            std::fs::write(&corrupt_path, b"not json").expect("write corrupt");

            assert_eq!(scanned_entries(registry_directory), vec![good]);
        });
    }

    #[test]
    fn scan_skips_an_entry_with_an_unrecognized_schema_version() {
        with_isolated_registry_directory(|registry_directory| {
            let mut future = sample_entry("Rfuture");
            future.schema_version = NODE_REGISTRY_SCHEMA_VERSION + 1;
            write_entry(registry_directory, &future).expect("write future");
            assert!(
                scan_entries(registry_directory).expect("scan").is_empty(),
                "an unrecognized schema_version must be skipped"
            );
        });
    }

    /// Entries are per run, so a schema-2 entry — one with no socket — has
    /// nothing to migrate: it is refused by its version, not by the field it lacks.
    #[test]
    fn a_schema_two_entry_is_refused_by_its_version() {
        with_isolated_registry_directory(|registry_directory| {
            std::fs::create_dir_all(registry_directory).unwrap();
            let schema_two_entry = serde_json::json!({
                "schema_version": 2,
                "runtime_id": "Rschema-two",
                "runtime_name": "rig-schema-two",
                "pid": 4242,
                "hint": "streamlib (/tmp/example)",
            });
            let schema_two_entry_path = registry_directory.join("Rschema-two.json");
            let schema_two_entry_bytes = serde_json::to_vec(&schema_two_entry).unwrap();
            std::fs::write(&schema_two_entry_path, &schema_two_entry_bytes).unwrap();

            let error = decode_entry_at_this_schema_version(
                &schema_two_entry_path,
                &schema_two_entry_bytes,
            )
            .expect_err("a schema-2 entry must be refused");
            assert!(
                matches!(
                    error,
                    NodeRegistryError::EntrySchemaVersionMismatch {
                        found: 2,
                        expected: 3,
                        ..
                    }
                ),
                "expected a refusal naming schema 2; got: {error}"
            );
            assert!(scan_entries(registry_directory).expect("scan").is_empty());
        });
    }

    /// The socket is the one way an entry says where to reach its runtime: the
    /// wire form names nothing a network client could dial.
    #[test]
    fn the_wire_form_carries_the_local_api_socket_path_and_nothing_else_to_dial() {
        let entry = sample_entry("Rnode-socket");
        let wire: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            wire["local_api_socket_path"],
            "/tmp/streamlib-1000/local-api-Rnode-socket.sock"
        );
        assert_eq!(wire["schema_version"], 3);
        let mut field_names: Vec<&str> = wire
            .as_object()
            .expect("an entry encodes as a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        field_names.sort_unstable();
        assert_eq!(
            field_names,
            [
                "hint",
                "local_api_socket_path",
                "pid",
                "runtime_id",
                "runtime_name",
                "schema_version"
            ]
        );
    }

    #[test]
    fn scan_on_a_missing_registry_directory_is_empty_not_an_error() {
        with_isolated_registry_directory(|registry_directory| {
            assert!(
                scan_entries(registry_directory)
                    .expect("scan of absent dir")
                    .is_empty()
            );
        });
    }

    #[test]
    fn entry_file_name_neutralizes_path_separators() {
        assert_eq!(entry_file_name("Rplain"), "Rplain.json");
        assert_eq!(entry_file_name("../escape"), ".._escape.json");
        assert_eq!(entry_file_name("a/b"), "a_b.json");
    }

    /// The fields of an entry as the runtime writes it, for a test to bend one of.
    fn sample_entry_json(runtime_id: &str) -> serde_json::Value {
        serde_json::to_value(sample_entry(runtime_id)).unwrap()
    }

    fn write_entry_file(registry_directory: &Path, file_name: &str, contents: &[u8]) -> PathBuf {
        std::fs::create_dir_all(registry_directory).unwrap();
        let entry_file_path = registry_directory.join(file_name);
        std::fs::write(&entry_file_path, contents).unwrap();
        entry_file_path
    }

    #[test]
    fn scan_skips_an_entry_whose_fields_are_the_wrong_json_type() {
        let wrong_shapes = [
            ("null-name", "runtime_name", serde_json::json!(null)),
            (
                "array-name",
                "runtime_name",
                serde_json::json!(["desk", "rig"]),
            ),
            (
                "object-id",
                "runtime_id",
                serde_json::json!({"nested": "object"}),
            ),
            (
                "fractional-version",
                "schema_version",
                serde_json::json!(2.9),
            ),
            ("float-version", "schema_version", serde_json::json!(3.0)),
            ("boolean-version", "schema_version", serde_json::json!(true)),
            ("boolean-pid", "pid", serde_json::json!(true)),
            ("string-pid", "pid", serde_json::json!("4242")),
            ("negative-pid", "pid", serde_json::json!(-1)),
            (
                "pid-beyond-u32",
                "pid",
                serde_json::json!(u64::from(u32::MAX) + 1),
            ),
            (
                "null-socket-path",
                "local_api_socket_path",
                serde_json::json!(null),
            ),
            ("null-hint", "hint", serde_json::json!(null)),
        ];
        for (wrong_shape, field_name, wrong_value) in wrong_shapes {
            with_isolated_registry_directory(|registry_directory| {
                let mut entry_json = sample_entry_json("Rmalformed");
                entry_json[field_name] = wrong_value;
                write_entry_file(
                    registry_directory,
                    "Rmalformed.json",
                    &serde_json::to_vec(&entry_json).unwrap(),
                );

                assert_eq!(scanned_entries(registry_directory), [], "{wrong_shape}");
            });
        }
    }

    #[test]
    fn scan_skips_an_entry_whose_top_level_is_a_json_array() {
        with_isolated_registry_directory(|registry_directory| {
            let entry = sample_entry("Rarray");
            let fields_in_declaration_order = serde_json::json!([
                entry.schema_version,
                entry.runtime_id,
                entry.runtime_name,
                entry.local_api_socket_path,
                entry.pid,
                entry.hint,
            ]);
            write_entry_file(
                registry_directory,
                "Rarray.json",
                &serde_json::to_vec(&fields_in_declaration_order).unwrap(),
            );

            assert_eq!(scanned_entries(registry_directory), []);
        });
    }

    #[test]
    fn an_entry_that_carries_no_hint_is_read_with_an_empty_one() {
        with_isolated_registry_directory(|registry_directory| {
            let mut entry_json = sample_entry_json("Rhintless");
            entry_json.as_object_mut().unwrap().remove("hint");
            write_entry_file(
                registry_directory,
                "Rhintless.json",
                &serde_json::to_vec(&entry_json).unwrap(),
            );

            let read = scanned_entries(registry_directory);
            assert_eq!(read.len(), 1);
            assert_eq!(read[0].hint, "");
        });
    }

    /// A released engine can write a key this reader has no field for, and an
    /// app pinned to that engine still runs.
    #[test]
    fn an_entry_carrying_a_key_this_reader_does_not_read_is_still_read() {
        with_isolated_registry_directory(|registry_directory| {
            let entry = sample_entry("Rcarries-an-unread-key");
            let mut entry_json = serde_json::to_value(&entry).unwrap();
            entry_json["a_key_this_reader_does_not_read"] = "any value".into();
            write_entry_file(
                registry_directory,
                "Rcarries-an-unread-key.json",
                &serde_json::to_vec(&entry_json).unwrap(),
            );

            assert_eq!(scanned_entries(registry_directory), [entry]);
        });
    }

    #[test]
    fn scan_reads_every_json_file_in_file_name_order_and_names_the_file_each_came_from() {
        with_isolated_registry_directory(|registry_directory| {
            for runtime_id in ["Rc", "Ra", "Rb"] {
                write_entry(registry_directory, &sample_entry(runtime_id)).unwrap();
            }
            let hidden_entry_file_path = write_entry_file(
                registry_directory,
                ".json",
                &serde_json::to_vec(&sample_entry("Rhidden")).unwrap(),
            );
            write_entry_file(
                registry_directory,
                "Rnot-an-entry.json.partial",
                &serde_json::to_vec(&sample_entry("Rpartial")).unwrap(),
            );

            let scanned = scan_entries(registry_directory).unwrap();

            assert_eq!(
                scanned
                    .iter()
                    .map(|scanned| scanned.node_registry_entry.runtime_id.as_str())
                    .collect::<Vec<_>>(),
                ["Rhidden", "Ra", "Rb", "Rc"]
            );
            assert_eq!(scanned[0].entry_file_path, hidden_entry_file_path);
            assert_eq!(
                scanned[1].entry_file_path,
                registry_directory.join("Ra.json")
            );
        });
    }

    #[test]
    fn removing_a_scanned_entry_file_deletes_it_and_a_file_already_gone_is_not_an_error() {
        with_isolated_registry_directory(|registry_directory| {
            let entry_file_path =
                write_entry(registry_directory, &sample_entry("Rpruned")).unwrap();

            remove_scanned_entry_file(&entry_file_path).expect("remove");
            assert!(!entry_file_path.exists());

            remove_scanned_entry_file(&entry_file_path)
                .expect("a file another reader already pruned is not an error");
        });
    }
}
