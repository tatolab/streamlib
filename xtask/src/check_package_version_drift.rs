// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing that every publishable package's crate version matches
//! its `.slpkg` semver.
//!
//! A package's version lives in `streamlib.yaml` (`package.version`) — the
//! single source of truth. Its `Cargo.toml` `[package].version` (the crate
//! version the registry resolves) must equal it, so a stale in-tree crate
//! version can never reach the registry. `streamlib pack` stamps the artifact
//! copy at pack time; this lint keeps the *in-tree* `Cargo.toml` honest so the
//! bump workflow is "edit streamlib.yaml, run `--fix`" — never hand-edit
//! `Cargo.toml`.
//!
//! Skipped by construction:
//! - Packages with no `Cargo.toml` (schema-only, e.g. `@tatolab/escalate`).
//! - `Cargo.toml`s that inherit `version.workspace = true` — the version comes
//!   from the workspace root, not the crate, so there's nothing to drift
//!   against in-tree (the pack-time stamp handles the artifact copy).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Parent directory holding the publishable packages.
const PACKAGES_DIR: &str = "packages";

/// A package whose in-tree `Cargo.toml` crate version disagrees with its
/// `streamlib.yaml` package version.
#[derive(Debug, PartialEq, Eq)]
pub struct VersionDrift {
    pub package: String,
    pub cargo_path: PathBuf,
    pub cargo_version: String,
    pub manifest_version: String,
}

pub fn run(workspace_root: &Path, fix: bool) -> Result<()> {
    let drifts = scan(workspace_root)?;

    if fix {
        for d in &drifts {
            apply_fix(d)?;
            println!(
                "✓ fixed {}: {} → {}",
                d.package, d.cargo_version, d.manifest_version
            );
        }
        if drifts.is_empty() {
            println!("✓ check-package-version-drift --fix: nothing to fix");
        } else {
            println!("✓ check-package-version-drift --fix: {} package(s) synced", drifts.len());
        }
        return Ok(());
    }

    if drifts.is_empty() {
        println!("✓ check-package-version-drift: every package Cargo.toml matches its streamlib.yaml version");
        return Ok(());
    }

    eprintln!("✗ check-package-version-drift: {} package(s) drift", drifts.len());
    for d in &drifts {
        eprintln!(
            "  {}: Cargo.toml `[package].version` = {} but streamlib.yaml `package.version` = {} — run `cargo xtask check-package-version-drift --fix`",
            d.package, d.cargo_version, d.manifest_version
        );
    }
    anyhow::bail!("check-package-version-drift failed");
}

/// Scan `packages/*` for crate-version drift against `streamlib.yaml`.
pub fn scan(workspace_root: &Path) -> Result<Vec<VersionDrift>> {
    let packages_dir = workspace_root.join(PACKAGES_DIR);
    let mut drifts = Vec::new();
    if !packages_dir.is_dir() {
        return Ok(drifts);
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(&packages_dir)
        .with_context(|| format!("read {}", packages_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();

    for pkg_dir in entries {
        let cargo_path = pkg_dir.join("Cargo.toml");
        let manifest_path = pkg_dir.join("streamlib.yaml");
        // Schema-only packages carry no Cargo.toml — nothing to check.
        if !cargo_path.exists() || !manifest_path.exists() {
            continue;
        }
        // Workspace-inherited versions have no in-tree literal to drift.
        let Some(cargo_version) = cargo_literal_version(&cargo_path)? else {
            continue;
        };
        let Some(manifest_version) = manifest_package_version(&manifest_path)? else {
            continue;
        };
        if cargo_version != manifest_version {
            let package = pkg_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| pkg_dir.display().to_string());
            drifts.push(VersionDrift {
                package,
                cargo_path,
                cargo_version,
                manifest_version,
            });
        }
    }
    Ok(drifts)
}

/// The literal `[package].version` string in a `Cargo.toml`, or `None` when
/// the version is workspace-inherited (`version.workspace = true`) or absent.
fn cargo_literal_version(cargo_path: &Path) -> Result<Option<String>> {
    let body = fs::read_to_string(cargo_path)
        .with_context(|| format!("read {}", cargo_path.display()))?;
    let doc: toml::Value =
        toml::from_str(&body).with_context(|| format!("parse {}", cargo_path.display()))?;
    let version = doc
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str());
    Ok(version.map(|s| s.to_string()))
}

/// The `package.version` string in a `streamlib.yaml`, or `None` when the
/// manifest declares no `package.version`.
fn manifest_package_version(manifest_path: &Path) -> Result<Option<String>> {
    let body = fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&body)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let version = value.get("package").and_then(|p| p.get("version"));
    Ok(match version {
        Some(serde_yaml::Value::String(s)) => Some(s.clone()),
        Some(serde_yaml::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// Rewrite `[package].version` in the drifting `Cargo.toml` to the manifest
/// version, preserving formatting and comments via [`toml_edit`].
fn apply_fix(drift: &VersionDrift) -> Result<()> {
    let body = fs::read_to_string(&drift.cargo_path)
        .with_context(|| format!("read {}", drift.cargo_path.display()))?;
    let mut doc: toml_edit::DocumentMut = body
        .parse()
        .with_context(|| format!("parse {}", drift.cargo_path.display()))?;
    let package = doc
        .get_mut("package")
        .and_then(|p| p.as_table_mut())
        .ok_or_else(|| anyhow::anyhow!("{}: no [package] table", drift.cargo_path.display()))?;
    package["version"] = toml_edit::value(drift.manifest_version.clone());
    fs::write(&drift.cargo_path, doc.to_string())
        .with_context(|| format!("write {}", drift.cargo_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_pkg(root: &Path, name: &str, cargo: &str, manifest: &str) {
        let dir = root.join(PACKAGES_DIR).join(name);
        fs::create_dir_all(&dir).unwrap();
        if !cargo.is_empty() {
            fs::write(dir.join("Cargo.toml"), cargo).unwrap();
        }
        if !manifest.is_empty() {
            fs::write(dir.join("streamlib.yaml"), manifest).unwrap();
        }
    }

    const MANIFEST_1_0_0: &str =
        "package:\n  org: tatolab\n  name: foo\n  version: 1.0.0\n";

    #[test]
    fn detects_and_names_drift() {
        let root = TempDir::new().unwrap();
        write_pkg(
            root.path(),
            "foo",
            "[package]\nname = \"streamlib-foo\"\nversion = \"0.4.30\"\n",
            MANIFEST_1_0_0,
        );
        let drifts = scan(root.path()).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].package, "foo");
        assert_eq!(drifts[0].cargo_version, "0.4.30");
        assert_eq!(drifts[0].manifest_version, "1.0.0");
    }

    #[test]
    fn matching_versions_are_not_drift() {
        let root = TempDir::new().unwrap();
        write_pkg(
            root.path(),
            "foo",
            "[package]\nname = \"streamlib-foo\"\nversion = \"1.0.0\"\n",
            MANIFEST_1_0_0,
        );
        assert!(scan(root.path()).unwrap().is_empty());
    }

    #[test]
    fn prerelease_drift_is_detected_and_fixed() {
        let root = TempDir::new().unwrap();
        write_pkg(
            root.path(),
            "jpeg",
            "[package]\nname = \"streamlib-jpeg\"\nversion = \"0.4.35-dev.5\"\n",
            "package:\n  org: tatolab\n  name: jpeg\n  version: 1.0.7\n",
        );
        let drifts = scan(root.path()).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].manifest_version, "1.0.7");
        run(root.path(), true).unwrap();
        assert!(scan(root.path()).unwrap().is_empty(), "--fix must converge");
    }

    #[test]
    fn workspace_inherited_version_is_skipped() {
        let root = TempDir::new().unwrap();
        write_pkg(
            root.path(),
            "core",
            "[package]\nname = \"streamlib-core\"\nversion.workspace = true\n",
            MANIFEST_1_0_0,
        );
        assert!(
            scan(root.path()).unwrap().is_empty(),
            "workspace-inherited versions have no in-tree literal to drift"
        );
    }

    #[test]
    fn schema_only_package_without_cargo_toml_is_skipped() {
        let root = TempDir::new().unwrap();
        write_pkg(root.path(), "escalate", "", MANIFEST_1_0_0);
        assert!(scan(root.path()).unwrap().is_empty());
    }

    #[test]
    fn fix_converges_and_is_idempotent() {
        let root = TempDir::new().unwrap();
        write_pkg(
            root.path(),
            "foo",
            "# keep me\n[package]\nname = \"streamlib-foo\"\nversion = \"0.4.30\" # inline\nedition = \"2024\"\n\n[dependencies]\nserde = \"1.0\"\n",
            MANIFEST_1_0_0,
        );
        run(root.path(), true).unwrap();
        let cargo =
            fs::read_to_string(root.path().join(PACKAGES_DIR).join("foo").join("Cargo.toml"))
                .unwrap();
        assert!(cargo.contains("version = \"1.0.0\""), "got: {cargo}");
        assert!(cargo.contains("# keep me"), "comment preserved, got: {cargo}");
        assert!(cargo.contains("[dependencies]"), "unrelated tables preserved");
        assert!(scan(root.path()).unwrap().is_empty());
        // Second --fix is a no-op (idempotent).
        run(root.path(), true).unwrap();
        assert!(scan(root.path()).unwrap().is_empty());
    }

    #[test]
    fn current_workspace_has_no_drift() {
        // Smoke test: after the sweep, no real package drifts. Locks the
        // sweep in place — a future package with a stale crate version fails
        // here.
        let workspace = workspace_root();
        let drifts = scan(&workspace).unwrap();
        assert!(
            drifts.is_empty(),
            "current workspace has package version drift: {drifts:?}"
        );
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent")
            .to_path_buf()
    }
}
