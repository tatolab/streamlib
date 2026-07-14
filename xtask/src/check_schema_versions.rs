// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing the package-as-publication-unit rule from milestone 10.
//!
//! Schema YAMLs declare `type` (and content fields); they do NOT declare a
//! top-level `version`. Versioning lives at the package level
//! (`streamlib.yaml`'s `package.version`) — bumping any type bumps the
//! whole package. This lint catches anyone re-introducing a top-level
//! `version` key in a schema YAML.
//!
//! See `docs/architecture/schema-identity-and-packaging.md`.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Directory globs to walk for schema YAMLs. New layout (`packages/*/schemas`)
/// is included alongside the legacy `libs/*/schemas` so the lint stays
/// effective during the milestone-10 migration.
pub const SCHEMA_DIR_PARENTS: &[&str] = &["runtime", "sdk", "adapters", "tools", "vendor", "packages", "examples"];

/// Files that look like schema YAMLs but are NOT (e.g. `streamlib.yaml`,
/// `Cargo.toml.orig`). The lint runs only on files matching `*.yaml` /
/// `*.yml` in a directory whose name is exactly `schemas/`.
const SCHEMA_DIR_NAME: &str = "schemas";

#[derive(Debug, PartialEq, Eq)]
pub enum LintViolation {
    TopLevelVersion { file: PathBuf },
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = lint_workspace(workspace_root)?;

    if violations.is_empty() {
        println!("✓ check-schema-versions: no schema YAMLs declare a top-level `version` key");
        return Ok(());
    }

    eprintln!("✗ check-schema-versions: {} violation(s)", violations.len());
    for v in &violations {
        match v {
            LintViolation::TopLevelVersion { file } => {
                eprintln!(
                    "  {}: schema YAMLs must not declare a top-level `version` key (publication-unit lives in streamlib.yaml; see docs/architecture/schema-identity-and-packaging.md)",
                    file.display()
                );
            }
        }
    }
    anyhow::bail!("check-schema-versions failed");
}

pub fn lint_workspace(workspace_root: &Path) -> Result<Vec<LintViolation>> {
    let mut violations = Vec::new();
    for parent in SCHEMA_DIR_PARENTS {
        let parent_path = workspace_root.join(parent);
        if !parent_path.exists() {
            continue;
        }
        for entry in WalkDir::new(&parent_path).follow_links(false) {
            let entry = entry.with_context(|| format!("walking {}", parent_path.display()))?;
            let path = entry.path();
            if !is_schema_yaml(path) {
                continue;
            }
            if has_top_level_version(path)? {
                violations.push(LintViolation::TopLevelVersion {
                    file: path.to_path_buf(),
                });
            }
        }
    }
    Ok(violations)
}

fn is_schema_yaml(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let ext = path.extension().and_then(|e| e.to_str());
    if !matches!(ext, Some("yaml") | Some("yml")) {
        return false;
    }
    // Must live under a directory literally named `schemas`.
    path.ancestors()
        .any(|a| a.file_name().and_then(|n| n.to_str()) == Some(SCHEMA_DIR_NAME))
}

fn has_top_level_version(path: &Path) -> Result<bool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    // Empty file is fine.
    if content.trim().is_empty() {
        return Ok(false);
    }

    let value: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("parsing {} as YAML", path.display()))?;

    let mapping = match value {
        serde_yaml::Value::Mapping(m) => m,
        // A non-mapping schema (anchor list, sequence, etc.) has no
        // top-level keys at all — pass.
        _ => return Ok(false),
    };

    Ok(mapping.contains_key(serde_yaml::Value::String("version".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn passes_on_canonical_jtd_metadata_block() {
        // The current schema shape (metadata.version), which the lint must
        // accept until the milestone-10 sweep migrates schemas off it.
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "runtime/foo/schemas/com.tatolab.videoframe.yaml",
            "metadata:\n  name: com.tatolab.videoframe\n  version: 1.0.0\nproperties:\n  width:\n    type: uint32\n",
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert!(
            violations.is_empty(),
            "metadata.version is grandfathered until later issues sweep, but {:?}",
            violations
        );
    }

    #[test]
    fn rejects_top_level_version_key() {
        let dir = TempDir::new().unwrap();
        let bad = write_fixture(
            dir.path(),
            "runtime/foo/schemas/com.tatolab.videoframe.yaml",
            "type: VideoFrame\nversion: 1.0.0\nproperties:\n  width:\n    type: uint32\n",
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert_eq!(violations.len(), 1);
        match &violations[0] {
            LintViolation::TopLevelVersion { file } => assert_eq!(file, &bad),
        }
    }

    #[test]
    fn ignores_non_schemas_directory() {
        // A YAML that lives outside a `schemas/` directory is not a schema —
        // even if it has a top-level version. (e.g. `streamlib.yaml`,
        // `release.yml`, `.github/workflows/*.yml`.)
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "runtime/foo/streamlib.yaml",
            "package:\n  org: tatolab\n  name: foo\n  version: 1.0.0\n",
        );
        write_fixture(
            dir.path(),
            ".github/workflows/release.yml",
            "name: Release\non:\n  push: {}\n",
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert!(violations.is_empty(), "{:?}", violations);
    }

    #[test]
    fn ignores_non_yaml_files() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "runtime/foo/schemas/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n",
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert!(violations.is_empty(), "{:?}", violations);
    }

    #[test]
    fn handles_yml_extension() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "runtime/foo/schemas/anything.yml",
            "type: VideoFrame\nversion: 1.0.0\n",
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn empty_file_is_a_pass() {
        let dir = TempDir::new().unwrap();
        write_fixture(dir.path(), "runtime/foo/schemas/empty.yaml", "");
        let violations = lint_workspace(dir.path()).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn invalid_yaml_is_an_error_not_a_silent_pass() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "runtime/foo/schemas/broken.yaml",
            ":::: not yaml ::::",
        );
        let res = lint_workspace(dir.path());
        assert!(res.is_err(), "broken YAML must surface, not silently pass");
    }

    #[test]
    fn current_workspace_passes() {
        // Smoke test: run the lint against the actual workspace. Locks in
        // that no current schema declares a top-level `version` key. If
        // this fails, either a new schema was added with the wrong shape
        // or the migration sweep already happened (in which case the
        // grandfather test above also needs revisiting).
        let workspace = workspace_root().expect("workspace root");
        let violations = lint_workspace(&workspace).unwrap();
        assert!(
            violations.is_empty(),
            "current workspace has top-level version keys: {:?}",
            violations
        );
    }

    fn workspace_root() -> Result<PathBuf> {
        let manifest = env!("CARGO_MANIFEST_DIR"); // .../streamlib/xtask
        Ok(PathBuf::from(manifest)
            .parent()
            .ok_or_else(|| anyhow::anyhow!("xtask has no parent"))?
            .to_path_buf())
    }
}
