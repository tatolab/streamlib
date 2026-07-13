// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing #402's atomic cutover off language-native metadata.
//!
//! `streamlib.yaml` is the single source of truth for a package's schemas,
//! dependencies, and identity. This lint catches anyone re-introducing the
//! pre-#402 metadata blocks:
//!
//! - `[package.metadata.streamlib]` in `Cargo.toml`
//! - `[tool.streamlib]` in `pyproject.toml`
//! - top-level `streamlib` key in `deno.json` / `deno.jsonc`
//!
//! See `docs/architecture/schema-identity-and-packaging.md` (Anti-pattern 4).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Workspace directories to scan. Build artifacts and vendored sources are
/// skipped explicitly below — adding more crates doesn't require updating
/// the list, only adding more skip prefixes when a new vendored tree shows
/// up.
const SCAN_PARENTS: &[&str] = &["libs", "packages", "examples", "xtask"];

/// Path components that should never be walked (build outputs, dependency
/// caches, generated bindings).
const SKIP_PATH_FRAGMENTS: &[&str] = &["/target/", "/_generated_/", "/node_modules/", "/.git/"];

#[derive(Debug, PartialEq, Eq)]
pub enum LintViolation {
    PackageMetadataStreamlib { file: PathBuf },
    ToolStreamlib { file: PathBuf },
    DenoStreamlibKey { file: PathBuf },
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = lint_workspace(workspace_root)?;

    if violations.is_empty() {
        println!("✓ check-no-streamlib-metadata: no pre-#402 metadata blocks detected");
        return Ok(());
    }

    eprintln!(
        "✗ check-no-streamlib-metadata: {} violation(s)",
        violations.len()
    );
    for v in &violations {
        match v {
            LintViolation::PackageMetadataStreamlib { file } => {
                eprintln!(
                    "  {}: `[package.metadata.streamlib]` re-introduced. Move schemas + dependencies to streamlib.yaml (see docs/architecture/schema-identity-and-packaging.md, anti-pattern 4)",
                    file.display()
                );
            }
            LintViolation::ToolStreamlib { file } => {
                eprintln!(
                    "  {}: `[tool.streamlib]` re-introduced. The Python pyproject must not carry a streamlib metadata block; use streamlib.yaml.",
                    file.display()
                );
            }
            LintViolation::DenoStreamlibKey { file } => {
                eprintln!(
                    "  {}: top-level `streamlib` key re-introduced. The Deno manifest must not carry streamlib metadata; use streamlib.yaml.",
                    file.display()
                );
            }
        }
    }
    anyhow::bail!("metadata lint failed: {} violation(s)", violations.len());
}

pub fn lint_workspace(workspace_root: &Path) -> Result<Vec<LintViolation>> {
    let mut violations = Vec::new();
    for parent in SCAN_PARENTS {
        let dir = workspace_root.join(parent);
        if !dir.exists() {
            continue;
        }
        scan_dir(&dir, &mut violations)?;
    }
    Ok(violations)
}

fn scan_dir(dir: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if SKIP_PATH_FRAGMENTS
            .iter()
            .any(|frag| path_str.contains(frag))
        {
            continue;
        }
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

        match file_name {
            "Cargo.toml" => check_cargo_toml(path, violations)?,
            "pyproject.toml" => check_pyproject_toml(path, violations)?,
            "deno.json" | "deno.jsonc" => check_deno_json(path, violations)?,
            _ => {}
        }
    }
    Ok(())
}

fn check_cargo_toml(path: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
    let body =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if body.lines().any(|line| {
        line.trim_start()
            .starts_with("[package.metadata.streamlib]")
    }) || body.lines().any(|line| {
        line.trim_start()
            .starts_with("[workspace.metadata.streamlib]")
    }) {
        violations.push(LintViolation::PackageMetadataStreamlib {
            file: path.to_path_buf(),
        });
    }
    Ok(())
}

fn check_pyproject_toml(path: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
    let body =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if body
        .lines()
        .any(|line| line.trim_start().starts_with("[tool.streamlib]"))
    {
        violations.push(LintViolation::ToolStreamlib {
            file: path.to_path_buf(),
        });
    }
    Ok(())
}

fn check_deno_json(path: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
    let body =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    // Strip line comments so deno.jsonc users aren't ambushed by a key
    // mention inside a comment.
    let stripped: String = body
        .lines()
        .filter_map(|line| {
            let l = line.trim_start();
            if l.starts_with("//") {
                None
            } else {
                Some(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Top-level `"streamlib":` key — match a quoted key followed by a colon
    // at indentation level <= 2 spaces (top-level in pretty-printed JSON).
    let has_top_level_streamlib_key = stripped.lines().any(|line| {
        let trimmed_left = line.trim_start();
        let leading = line.len() - trimmed_left.len();
        leading <= 2 && trimmed_left.starts_with("\"streamlib\":")
    });

    if has_top_level_streamlib_key {
        violations.push(LintViolation::DenoStreamlibKey {
            file: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn passes_on_clean_workspace() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        );
        write(
            tmp.path(),
            "libs/foo/pyproject.toml",
            "[project]\nname = \"foo\"\n",
        );
        write(
            tmp.path(),
            "libs/foo/deno.json",
            "{\n  \"name\": \"foo\"\n}\n",
        );

        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn fails_on_package_metadata_streamlib() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/Cargo.toml",
            "[package]\nname = \"foo\"\n\n[package.metadata.streamlib]\nschemas = []\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            LintViolation::PackageMetadataStreamlib { .. }
        ));
    }

    #[test]
    fn fails_on_workspace_metadata_streamlib() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.metadata.streamlib]\nschemas = []\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn fails_on_tool_streamlib() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/pyproject.toml",
            "[project]\nname = \"foo\"\n\n[tool.streamlib]\nschemas = []\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            LintViolation::ToolStreamlib { .. }
        ));
    }

    #[test]
    fn fails_on_deno_streamlib_key() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/deno.json",
            "{\n  \"name\": \"foo\",\n  \"streamlib\": { \"schemas\": [] }\n}\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            LintViolation::DenoStreamlibKey { .. }
        ));
    }

    #[test]
    fn deno_jsonc_comment_with_streamlib_keyword_does_not_trip() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/deno.jsonc",
            "{\n  // see streamlib.yaml for schemas\n  \"name\": \"foo\"\n}\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_target_and_generated_dirs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/target/release/Cargo.toml",
            "[package]\nname = \"foo\"\n[package.metadata.streamlib]\nschemas = []\n",
        );
        write(
            tmp.path(),
            "libs/foo/_generated_/Cargo.toml",
            "[package]\nname = \"foo\"\n[package.metadata.streamlib]\nschemas = []\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn streamlib_word_in_other_contexts_does_not_trip() {
        let tmp = TempDir::new().unwrap();
        // Crate named `streamlib-something` is fine.
        write(
            tmp.path(),
            "libs/foo/Cargo.toml",
            "[package]\nname = \"streamlib-foo\"\nversion = \"0.1.0\"\n",
        );
        // pyproject mentioning `streamlib` in description prose is fine.
        write(
            tmp.path(),
            "libs/foo/pyproject.toml",
            "[project]\nname = \"streamlib\"\ndescription = \"streamlib SDK\"\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }
}
