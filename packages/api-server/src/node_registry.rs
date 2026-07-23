// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! On-disk discovery registry for ApiServer-hosting runtimes.
//!
//! A runtime that hosts an [`crate::ApiServerProcessor`] writes one JSON entry
//! per runtime into `$XDG_RUNTIME_DIR/streamlib/nodes/<runtime_id>.json` when
//! its control port binds, and removes it on clean teardown. A CLI discovers
//! live control planes by scanning that directory. Entry existence is tied to
//! the control endpoint existing: a runtime without an ApiServer never appears.
//!
//! The file body is the wire contract between the writing runtime and any
//! reader (today the `streamlib nodes` command, in-process via this crate);
//! [`NODE_REGISTRY_SCHEMA_VERSION`] stamps it so a reader rejects an entry it
//! does not understand.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Schema version stamped into every [`NodeRegistryEntry`]. A reader skips an
/// entry whose `schema_version` it does not recognize.
pub const NODE_REGISTRY_SCHEMA_VERSION: u32 = 1;

/// One discovery entry: a running ApiServer-hosting runtime's control endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRegistryEntry {
    /// Wire-format version of this entry ([`NODE_REGISTRY_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The runtime's `RuntimeUniqueId`, verbatim.
    pub runtime_id: String,
    /// The control plane's reachable base URL (`http://127.0.0.1:<bound_port>`).
    pub control_url: String,
    /// OS process id hosting the control plane.
    pub pid: u32,
    /// Human hint for disambiguating nodes in a listing (process arg0 + cwd).
    pub hint: String,
}

impl NodeRegistryEntry {
    /// Build an entry for `runtime_id` reachable at `control_url`, stamping the
    /// current process id and a hint derived from this process's arg0 and cwd.
    pub fn for_current_process(runtime_id: String, control_url: String) -> Self {
        Self {
            schema_version: NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id,
            control_url,
            pid: std::process::id(),
            hint: current_process_hint(),
        }
    }
}

/// A named failure of a node-registry filesystem operation. No `()`-errors: each
/// variant carries the offending path and the underlying cause.
#[derive(Debug, thiserror::Error)]
pub enum NodeRegistryError {
    /// Creating the registry directory (`.../streamlib/nodes`) failed.
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
    /// Reading the registry directory or an entry file failed.
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
    /// understand. Only `read_entry`'s strict single-entry lookup raises this;
    /// `scan_entries` skips such an entry instead.
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

/// The directory holding node discovery entries.
///
/// `$XDG_RUNTIME_DIR/streamlib/nodes` when `XDG_RUNTIME_DIR` is set and
/// non-empty; otherwise a fallback under the system temp dir
/// (`<temp>/streamlib/nodes`), per the XDG Base Directory spec's
/// replacement-directory guidance (a warning is emitted on the fallback). The
/// writing runtime and any reader resolve this identically within one user
/// session, so discovery agrees on both paths.
#[tracing::instrument]
pub fn registry_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("streamlib").join("nodes"),
        _ => {
            tracing::warn!(
                "XDG_RUNTIME_DIR is unset; node registry falling back to the system temp dir"
            );
            std::env::temp_dir().join("streamlib").join("nodes")
        }
    }
}

/// Write (create or replace) the discovery entry for `entry.runtime_id`,
/// creating the registry directory if needed. Returns the entry's path.
#[tracing::instrument(skip(entry), fields(runtime_id = %entry.runtime_id, control_url = %entry.control_url))]
pub fn write_entry(entry: &NodeRegistryEntry) -> Result<PathBuf, NodeRegistryError> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).map_err(|source| NodeRegistryError::RegistryDirCreate {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(entry_file_name(&entry.runtime_id));
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

/// Remove the discovery entry for `runtime_id`. A missing entry is not an error
/// (idempotent teardown).
#[tracing::instrument]
pub fn remove_entry(runtime_id: &str) -> Result<(), NodeRegistryError> {
    let path = registry_dir().join(entry_file_name(runtime_id));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(NodeRegistryError::EntryRemove { path, source }),
    }
}

/// Read the single discovery entry for `runtime_id`, or `None` if no entry
/// exists. A present-but-corrupt or version-mismatched entry is an error — this
/// is the strict single-entry lookup a `--node <runtime_id>` resolve uses.
#[tracing::instrument]
pub fn read_entry(runtime_id: &str) -> Result<Option<NodeRegistryEntry>, NodeRegistryError> {
    let path = registry_dir().join(entry_file_name(runtime_id));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(NodeRegistryError::EntryRead { path, source }),
    };
    let entry: NodeRegistryEntry = serde_json::from_slice(&bytes)
        .map_err(|source| NodeRegistryError::EntryDecode {
            path: path.clone(),
            source,
        })?;
    if entry.schema_version != NODE_REGISTRY_SCHEMA_VERSION {
        return Err(NodeRegistryError::EntrySchemaVersionMismatch {
            path,
            found: entry.schema_version,
            expected: NODE_REGISTRY_SCHEMA_VERSION,
        });
    }
    Ok(Some(entry))
}

/// Scan every discovery entry, skipping (with a warning) any unreadable,
/// undecodable, or version-mismatched file so one corrupt entry never breaks a
/// listing. A missing registry directory yields an empty list. Only a failure
/// to read the directory itself is a hard error.
#[tracing::instrument]
pub fn scan_entries() -> Result<Vec<NodeRegistryEntry>, NodeRegistryError> {
    let dir = registry_dir();
    let read_dir = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(NodeRegistryError::EntryRead { path: dir, source }),
    };

    let mut entries = Vec::new();
    for dir_entry in read_dir {
        let dir_entry = dir_entry.map_err(|source| NodeRegistryError::EntryRead {
            path: dir.clone(),
            source,
        })?;
        let path = dir_entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping unreadable node registry entry");
                continue;
            }
        };
        let entry: NodeRegistryEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping undecodable node registry entry");
                continue;
            }
        };
        if entry.schema_version != NODE_REGISTRY_SCHEMA_VERSION {
            tracing::warn!(
                path = %path.display(),
                schema_version = entry.schema_version,
                "skipping node registry entry with unrecognized schema_version"
            );
            continue;
        }
        entries.push(entry);
    }
    Ok(entries)
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
    format!("{sanitized}.json")
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
    //! Registry write / scan / read / remove / prune-shape and the
    //! `schema_version` round-trip. Each test swaps `XDG_RUNTIME_DIR` for a
    //! fresh tempdir (guarded `#[serial]` like the runtime's env-swap tests, so
    //! no concurrent test reads the mutated env) and asserts entries land under
    //! it.

    use serial_test::serial;

    use super::*;

    /// Point `XDG_RUNTIME_DIR` at a fresh tempdir for the closure's duration,
    /// restoring the prior value after.
    fn with_isolated_xdg_runtime_dir<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: tests are serialized via #[serial]; no concurrent env mutation.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        }
        let result = f(tmp.path());
        unsafe {
            match prev {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        result
    }

    fn sample_entry(runtime_id: &str, port: u16) -> NodeRegistryEntry {
        NodeRegistryEntry {
            schema_version: NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id: runtime_id.to_string(),
            control_url: format!("http://127.0.0.1:{port}"),
            pid: 4242,
            hint: "streamlib (/tmp/example)".to_string(),
        }
    }

    #[test]
    #[serial]
    fn write_then_scan_round_trips_the_entry_under_xdg_runtime_dir() {
        with_isolated_xdg_runtime_dir(|xdg| {
            let entry = sample_entry("Rnode-alpha", 8080);
            let path = write_entry(&entry).expect("write");
            assert!(
                path.starts_with(xdg),
                "entry {} must land under XDG_RUNTIME_DIR {}",
                path.display(),
                xdg.display()
            );

            let scanned = scan_entries().expect("scan");
            assert_eq!(scanned, vec![entry]);
        });
    }

    #[test]
    #[serial]
    fn schema_version_survives_a_serde_round_trip() {
        let entry = sample_entry("Rnode-beta", 9090);
        let json = serde_json::to_string(&entry).expect("encode");
        assert!(
            json.contains("\"schema_version\""),
            "schema_version must be present in the wire form: {json}"
        );
        let decoded: NodeRegistryEntry = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.schema_version, NODE_REGISTRY_SCHEMA_VERSION);
        assert_eq!(decoded, entry);
    }

    #[test]
    #[serial]
    fn remove_entry_deletes_the_file_and_is_idempotent() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            let entry = sample_entry("Rnode-gamma", 7000);
            write_entry(&entry).expect("write");
            assert_eq!(scan_entries().expect("scan").len(), 1);

            remove_entry(&entry.runtime_id).expect("remove");
            assert!(scan_entries().expect("scan").is_empty());

            remove_entry(&entry.runtime_id).expect("second remove is a no-op");
        });
    }

    #[test]
    #[serial]
    fn read_entry_returns_none_for_a_missing_runtime_and_the_entry_when_present() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            assert!(read_entry("Rnobody").expect("read missing").is_none());
            let entry = sample_entry("Rnode-delta", 6001);
            write_entry(&entry).expect("write");
            assert_eq!(read_entry(&entry.runtime_id).expect("read"), Some(entry));
        });
    }

    #[test]
    #[serial]
    fn scan_skips_a_corrupt_entry_and_still_returns_the_valid_ones() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            let good = sample_entry("Rgood", 5000);
            write_entry(&good).expect("write good");
            let corrupt_path = registry_dir().join("Rcorrupt.json");
            std::fs::write(&corrupt_path, b"not json").expect("write corrupt");

            let scanned = scan_entries().expect("scan tolerates corruption");
            assert_eq!(scanned, vec![good]);
        });
    }

    #[test]
    #[serial]
    fn scan_skips_an_entry_with_an_unrecognized_schema_version() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            let mut future = sample_entry("Rfuture", 5100);
            future.schema_version = NODE_REGISTRY_SCHEMA_VERSION + 1;
            write_entry(&future).expect("write future");
            assert!(
                scan_entries().expect("scan").is_empty(),
                "an unrecognized schema_version must be skipped"
            );
        });
    }

    #[test]
    #[serial]
    fn read_entry_rejects_an_entry_with_an_unrecognized_schema_version() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            let mut future = sample_entry("Rfuture-read", 5200);
            future.schema_version = NODE_REGISTRY_SCHEMA_VERSION + 1;
            write_entry(&future).expect("write future");
            let error = read_entry(&future.runtime_id)
                .expect_err("a version-mismatched entry must be a hard error, not Ok(Some(_))");
            assert!(
                matches!(error, NodeRegistryError::EntrySchemaVersionMismatch { .. }),
                "expected a schema-version-mismatch error; got: {error}"
            );
        });
    }

    #[test]
    #[serial]
    fn scan_on_a_missing_registry_directory_is_empty_not_an_error() {
        with_isolated_xdg_runtime_dir(|_xdg| {
            assert!(scan_entries().expect("scan of absent dir").is_empty());
        });
    }

    #[test]
    fn entry_file_name_neutralizes_path_separators() {
        assert_eq!(entry_file_name("Rplain"), "Rplain.json");
        assert_eq!(entry_file_name("../escape"), ".._escape.json");
        assert_eq!(entry_file_name("a/b"), "a_b.json");
    }
}
