// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Workspace discovery — walks up from a starting path looking for the
//! nearest `streamlib.yaml` carrying a `workspace:` marker block. Mirrors
//! Cargo's workspace-discovery algorithm (a member crate finds its
//! workspace root by walking up to the first `Cargo.toml` with a
//! `[workspace]` block).
//!
//! Used by the runtime to consult workspace-level `[patch]` resolution
//! before falling back to the installed-package cache.
//!
//! See `docs/architecture/schema-identity-and-packaging.md` (when filed)
//! and the `Manifest` doc for the wire shape.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;

/// Outcome of walking up from a starting path looking for the nearest
/// workspace-flavor `streamlib.yaml`.
#[derive(Debug, Clone)]
pub struct DiscoveredWorkspace {
    /// Absolute path to the directory containing the workspace `streamlib.yaml`.
    pub root: PathBuf,
    /// Parsed manifest at the workspace root. The `workspace:` and `patch:`
    /// fields are the load-bearing ones for resolution.
    pub manifest: Manifest,
}

/// Walk up from `start` looking for a `streamlib.yaml` whose
/// `workspace:` block is set. Returns `None` when no workspace ancestor
/// exists (the runtime then falls back to installed-cache lookup for
/// canonical deps).
///
/// The walk skips manifest files that fail to parse — a non-workspace
/// `streamlib.yaml` between the start path and a real workspace root must
/// not block the walk. Errors at the workspace root itself surface to the
/// caller via `Err`.
pub fn discover_workspace(start: &Path) -> Option<DiscoveredWorkspace> {
    let mut cursor = start.to_path_buf();
    if cursor.is_relative() {
        if let Ok(canonical) = cursor.canonicalize() {
            cursor = canonical;
        }
    }
    loop {
        let candidate = cursor.join(Manifest::FILE_NAME);
        if candidate.exists() {
            // A non-workspace yaml must not stop the walk — it may be a
            // package or project manifest sitting between the start path
            // and the actual workspace root. Parse-failures are silently
            // skipped here for the same reason.
            if let Ok(manifest) = Manifest::load_file(&candidate) {
                if manifest.is_workspace_flavor() {
                    return Some(DiscoveredWorkspace {
                        root: cursor,
                        manifest,
                    });
                }
            }
        }
        cursor = cursor.parent()?.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::{Org, Package, PackageRef};
    use crate::manifest::DependencySpec;

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    #[test]
    fn discover_finds_workspace_at_starting_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(Manifest::FILE_NAME),
            r#"
workspace: {}
patch:
  "@tatolab/core":
    path: packages/core
"#,
        )
        .unwrap();

        let discovered = discover_workspace(tmp.path()).expect("workspace at start dir");
        assert_eq!(discovered.root, tmp.path().canonicalize().unwrap());
        assert!(discovered.manifest.is_workspace_flavor());
        assert!(discovered
            .manifest
            .patch
            .contains_key(&pkg_ref("tatolab", "core")));
    }

    #[test]
    fn discover_walks_up_through_non_workspace_manifests() {
        // /tmp/<id>/                  ← workspace root (workspace: {})
        // /tmp/<id>/middle/           ← package manifest (no workspace)
        // /tmp/<id>/middle/leaf/      ← project manifest (start dir)
        let tmp = tempfile::tempdir().unwrap();
        let workspace_dir = tmp.path();
        let middle_dir = workspace_dir.join("middle");
        let leaf_dir = middle_dir.join("leaf");
        std::fs::create_dir_all(&leaf_dir).unwrap();

        std::fs::write(
            workspace_dir.join(Manifest::FILE_NAME),
            "workspace: {}\npatch:\n  \"@tatolab/core\":\n    path: packages/core\n",
        )
        .unwrap();
        std::fs::write(
            middle_dir.join(Manifest::FILE_NAME),
            "package:\n  org: tatolab\n  name: middle\n  version: 1.0.0\n",
        )
        .unwrap();
        std::fs::write(
            leaf_dir.join(Manifest::FILE_NAME),
            r#"dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        )
        .unwrap();

        let discovered = discover_workspace(&leaf_dir).expect("workspace ancestor");
        assert_eq!(discovered.root, workspace_dir.canonicalize().unwrap());
        // The patch table from the workspace root, not the middle package.
        let core_patch = discovered.manifest.patch.get(&pkg_ref("tatolab", "core"));
        assert!(matches!(core_patch, Some(DependencySpec::Path(_))));
    }

    #[test]
    fn discover_returns_none_when_no_workspace_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(Manifest::FILE_NAME),
            "package:\n  org: tatolab\n  name: standalone\n  version: 1.0.0\n",
        )
        .unwrap();

        // No workspace ancestor anywhere up the tree.
        assert!(discover_workspace(&project).is_none());
    }

    #[test]
    fn discover_skips_unparseable_yamls() {
        // A broken yaml between the start dir and the workspace root must
        // not break the walk — the runtime should still find the
        // workspace and surface its patch table.
        let tmp = tempfile::tempdir().unwrap();
        let workspace_dir = tmp.path();
        let middle_dir = workspace_dir.join("middle");
        let leaf_dir = middle_dir.join("leaf");
        std::fs::create_dir_all(&leaf_dir).unwrap();

        std::fs::write(
            workspace_dir.join(Manifest::FILE_NAME),
            "workspace: {}\n",
        )
        .unwrap();
        // Intentionally broken yaml in the middle.
        std::fs::write(
            middle_dir.join(Manifest::FILE_NAME),
            "this: is: : not valid yaml\n  - garbage\n  bad indent\n",
        )
        .unwrap();
        std::fs::write(
            leaf_dir.join(Manifest::FILE_NAME),
            "package:\n  org: tatolab\n  name: leaf\n  version: 1.0.0\n",
        )
        .unwrap();

        let discovered = discover_workspace(&leaf_dir).expect("workspace ancestor despite broken middle");
        assert_eq!(discovered.root, workspace_dir.canonicalize().unwrap());
    }
}
