// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one definition of "this package is pinned to the machine that wrote it".
//!
//! A path-flavored `patch:` override or a Cargo path dependency names a
//! directory that exists only on the authoring machine. `add` and `install`
//! take finalized artifacts; `link` is the local-development path. So a package
//! carrying either is refused wherever it is materialized, whatever the
//! container it arrived in.
//!
//! This lives beside [`crate::app_modules`] rather than in the publish tooling
//! because the defect belongs to the materialization primitive: folder, zip,
//! tar.gz, and the runtime module loader all funnel through the same staging
//! path, and a producer-side-only guard is bypassable by construction once a
//! plain directory installs without ever going through publish.

use std::path::{Path, PathBuf};

use crate::manifest::{DependencySpec, Manifest};

/// Why a package cannot be materialized into an app's `streamlib_modules/`.
#[derive(Debug, thiserror::Error)]
pub enum PathArtifactError {
    /// A package file could not be read.
    #[error("read {path}: {detail}")]
    Unreadable { path: PathBuf, detail: String },

    /// A package file did not parse.
    #[error("parse {path}: {detail}")]
    Unparseable { path: PathBuf, detail: String },
}

/// Every path artifact `package_dir` carries, each rendered for a diagnostic.
/// Empty means the package is standalone.
///
/// The one predicate every consumer is built from — `xtask install-packages`
/// uses it to decide what it will not compile, the publish path to decide what
/// it will not ship, and `app_modules` to decide what will not install. Two
/// spellings would let one of those drift into a gap.
///
/// Callers render their own diagnostic: the subject worth naming differs
/// (the source a user typed, versus the package a lockfile pinned), and the
/// directory actually scanned is an ephemeral staging path that would mean
/// nothing to a reader.
pub fn non_distributable_path_offenders(
    package_dir: &Path,
) -> Result<Vec<String>, PathArtifactError> {
    let mut offenders = path_patch_offenders(package_dir)?;
    offenders.extend(cargo_path_dependency_offenders(package_dir)?);
    Ok(offenders)
}

/// Path-flavored `patch:` overrides the package manifest declares.
pub fn path_patch_offenders(package_dir: &Path) -> Result<Vec<String>, PathArtifactError> {
    let manifest_path = package_dir.join(Manifest::FILE_NAME);
    if !manifest_path.is_file() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&manifest_path).map_err(|e| PathArtifactError::Unreadable {
            path: manifest_path.clone(),
            detail: e.to_string(),
        })?;
    let manifest: Manifest =
        serde_yaml::from_str(&body).map_err(|e| PathArtifactError::Unparseable {
            path: manifest_path,
            detail: e.to_string(),
        })?;

    Ok(manifest
        .patch
        .iter()
        .filter_map(|(dependency_ref, spec)| match spec {
            DependencySpec::Path(path_dependency) => Some(format!(
                "`{dependency_ref}` → `{}`",
                path_dependency.path.display()
            )),
            _ => None,
        })
        .collect())
}

/// Path dependencies the package's `Cargo.toml` declares.
pub fn cargo_path_dependency_offenders(
    package_dir: &Path,
) -> Result<Vec<String>, PathArtifactError> {
    let cargo_toml_path = package_dir.join("Cargo.toml");
    if !cargo_toml_path.is_file() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&cargo_toml_path).map_err(|e| PathArtifactError::Unreadable {
            path: cargo_toml_path.clone(),
            detail: e.to_string(),
        })?;
    let document: toml::Value =
        toml::from_str(&body).map_err(|e| PathArtifactError::Unparseable {
            path: cargo_toml_path,
            detail: e.to_string(),
        })?;
    Ok(cargo_path_dependency_names(&document))
}

/// Every dependency in a Cargo manifest declared with a `path` key, across
/// `[dependencies]`, `[build-dependencies]`, `[dev-dependencies]`, and their
/// `[target.<cfg>.*]` counterparts.
fn cargo_path_dependency_names(document: &toml::Value) -> Vec<String> {
    fn scan_dependency_table(table: &toml::value::Table, out: &mut Vec<String>) {
        for (name, spec) in table {
            if let toml::Value::Table(spec_table) = spec
                && spec_table.contains_key("path")
            {
                out.push(name.clone());
            }
        }
    }
    fn scan_dependency_sections(root: &toml::value::Table, out: &mut Vec<String>) {
        for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
            if let Some(toml::Value::Table(table)) = root.get(key) {
                scan_dependency_table(table, out);
            }
        }
    }

    let mut out = Vec::new();
    if let toml::Value::Table(root) = document {
        scan_dependency_sections(root, &mut out);
        if let Some(toml::Value::Table(targets)) = root.get("target") {
            for target_table in targets.values() {
                if let toml::Value::Table(table) = target_table {
                    scan_dependency_sections(table, &mut out);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
