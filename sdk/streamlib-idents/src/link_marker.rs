// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared `streamlib link` marker schema (`.streamlib/link.json`) + discovery.
//!
//! The link marker is a manifest-adjacent record: it lives here in
//! `streamlib-idents` (alongside [`Manifest`] / [`Lockfile`]) so every layer
//! that must reason about an active whole-tree link can reach it without a
//! heavier dependency — the CLI that writes it, the packer that refuses to
//! distribute while it's present, the build orchestrator that redirects staged
//! toolchains at the linked checkout, and the engine module loader that
//! resolves `@org/name` modules from the linked checkout's packages tree.
//!
//! [`Manifest`]: crate::Manifest
//! [`Lockfile`]: crate::Lockfile

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Consumer-root-relative directory holding all link state.
pub const LINK_STATE_DIR: &str = ".streamlib";
/// Manifest recording the link transaction (checkout + every touched file).
pub const LINK_MANIFEST_FILE: &str = "link.json";
/// Directory under [`LINK_STATE_DIR`] mirroring pre-edit backups by relative path.
pub const LINK_BACKUP_DIR: &str = "link-backup";

/// Failure modes of link-marker discovery and the pack/publish guard.
#[derive(Debug, thiserror::Error)]
pub enum LinkMarkerError {
    /// The link manifest exists but cannot be parsed — never silently ignored.
    #[error("link state at `{path}` is corrupt: {detail}")]
    CorruptLinkState { path: PathBuf, detail: String },

    /// A filesystem operation on the marker failed.
    #[error("filesystem error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A distributable pack/publish was attempted while a link is active.
    #[error(
        "this package cannot be packed or published while a streamlib link is active (marker: \
         {marker}). Local link overrides are dev-only and must not leak into a distributed \
         artifact — run `streamlib unlink` first"
    )]
    PackRefusedWhileLinked { marker: PathBuf },
}

/// Transaction state of a recorded link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkTransactionState {
    /// Manifest persisted; edits may be partially applied. Recoverable via `streamlib unlink`.
    Applying,
    /// Every edit and backup landed; the link is fully established.
    Active,
}

/// One manifest file the link plan covers, recorded for byte-clean teardown.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinkedManifestFile {
    /// Path relative to the consumer root.
    pub path: PathBuf,
    /// Whether the file existed before linking. `false` ⇒ unlink deletes it.
    pub existed_before: bool,
    /// Hex SHA-256 of the pre-edit content (empty when `!existed_before`).
    pub pre_edit_sha256: String,
    /// Hex SHA-256 of the planned post-edit content.
    pub post_edit_sha256: String,
}

/// Persisted record of a `streamlib link` transaction.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinkManifest {
    /// Canonicalized path of the linked streamlib checkout.
    pub checkout: PathBuf,
    /// Resolved absolute path of the checkout's Python SDK (uv source target).
    pub python_sdk_path: PathBuf,
    /// Resolved absolute path of the checkout's Deno SDK entrypoint module.
    pub deno_sdk_entrypoint_path: PathBuf,
    /// RFC-3339 timestamp of when the link was established.
    pub linked_at: String,
    /// Number of cargo crates redirected to the checkout.
    pub linked_crate_count: usize,
    /// Transaction state — `applying` until every edit lands, then `active`.
    pub state: LinkTransactionState,
    /// Every manifest file the link plan covers (in apply order).
    pub files: Vec<LinkedManifestFile>,
}

/// Walk up from `start` to the filesystem root looking for a link marker
/// (`.streamlib/link.json`) in ANY state. Returns the marker path when found.
#[tracing::instrument(skip_all, fields(start = %start.display()))]
pub fn find_active_link_marker(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let marker = d.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE);
        if marker.is_file() {
            return Some(marker);
        }
        dir = d.parent();
    }
    None
}

/// Load and parse the link manifest at `marker_path`. Corruption is a loud
/// typed error, never a silent `None`.
#[tracing::instrument(skip_all, fields(marker = %marker_path.display()))]
pub fn load_link_manifest(marker_path: &Path) -> Result<LinkManifest, LinkMarkerError> {
    let body = std::fs::read_to_string(marker_path).map_err(|e| LinkMarkerError::Io {
        path: marker_path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&body).map_err(|e| LinkMarkerError::CorruptLinkState {
        path: marker_path.to_path_buf(),
        detail: e.to_string(),
    })
}

/// Find (upward walk from `start`) and load the link manifest, if any.
pub fn find_and_load_active_link(
    start: &Path,
) -> Result<Option<(PathBuf, LinkManifest)>, LinkMarkerError> {
    match find_active_link_marker(start) {
        Some(marker) => {
            let manifest = load_link_manifest(&marker)?;
            Ok(Some((marker, manifest)))
        }
        None => Ok(None),
    }
}

/// Refuse a distributable pack/publish while a link marker exists (any state)
/// anywhere above `package_dir`.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn ensure_no_active_link_for_pack(package_dir: &Path) -> Result<(), LinkMarkerError> {
    if let Some(marker) = find_active_link_marker(package_dir) {
        return Err(LinkMarkerError::PackRefusedWhileLinked { marker });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_marker(root: &Path, body: &str) -> PathBuf {
        let dir = root.join(LINK_STATE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(LINK_MANIFEST_FILE);
        std::fs::write(&marker, body).unwrap();
        marker
    }

    fn valid_manifest_json() -> String {
        r#"{
  "checkout": "/opt/streamlib",
  "python_sdk_path": "/opt/streamlib/sdk/streamlib-python",
  "deno_sdk_entrypoint_path": "/opt/streamlib/sdk/streamlib-deno/mod.ts",
  "linked_at": "2026-01-01T00:00:00Z",
  "linked_crate_count": 3,
  "state": "active",
  "files": []
}"#
        .to_string()
    }

    #[test]
    fn corrupt_link_json_is_a_loud_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = write_marker(tmp.path(), "{ not json at all");
        let err = load_link_manifest(&marker).unwrap_err();
        assert!(
            matches!(err, LinkMarkerError::CorruptLinkState { .. }),
            "got {err:?}"
        );
        // And the find-and-load composite surfaces it too (no silent None).
        let err = find_and_load_active_link(tmp.path()).unwrap_err();
        assert!(
            matches!(err, LinkMarkerError::CorruptLinkState { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn upward_walk_finds_marker_in_parent_and_pack_guard_trips_in_any_state() {
        let tmp = tempfile::tempdir().unwrap();
        write_marker(tmp.path(), &valid_manifest_json());
        let nested = tmp.path().join("packages").join("thing");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(find_active_link_marker(&nested).is_some());
        assert!(matches!(
            ensure_no_active_link_for_pack(&nested),
            Err(LinkMarkerError::PackRefusedWhileLinked { .. })
        ));

        // `applying` state trips the guard identically (marker presence gates).
        write_marker(
            tmp.path(),
            &valid_manifest_json().replace("\"active\"", "\"applying\""),
        );
        assert!(matches!(
            ensure_no_active_link_for_pack(&nested),
            Err(LinkMarkerError::PackRefusedWhileLinked { .. })
        ));
    }

    #[test]
    fn manifest_round_trips_through_serde() {
        let manifest = LinkManifest {
            checkout: PathBuf::from("/opt/streamlib"),
            python_sdk_path: PathBuf::from("/opt/streamlib/sdk/streamlib-python"),
            deno_sdk_entrypoint_path: PathBuf::from("/opt/streamlib/sdk/streamlib-deno/mod.ts"),
            linked_at: "t".into(),
            linked_crate_count: 2,
            state: LinkTransactionState::Applying,
            files: vec![LinkedManifestFile {
                path: PathBuf::from(".cargo/config.toml"),
                existed_before: true,
                pre_edit_sha256: "aa".into(),
                post_edit_sha256: "bb".into(),
            }],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: LinkManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state, LinkTransactionState::Applying);
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.python_sdk_path, manifest.python_sdk_path);
    }
}
