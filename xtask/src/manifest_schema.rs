// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSON Schema for `streamlib.yaml` — emit + check (#714).
//!
//! The schema is derived from [`StreamlibYaml`] in `streamlib-processor-schema`
//! and committed at `schemas/streamlib.schema.json`. Every `streamlib.yaml` in
//! the repo references it via the `# yaml-language-server: $schema=...` magic
//! comment, which `yaml-language-server` (Red Hat YAML extension, JetBrains,
//! nvim) reads to provide editor autocomplete and validation.
//!
//! Two commands:
//! - `emit-manifest-schema` regenerates the schema file from the Rust types.
//! - `check-manifest-schema` is the CI gate: regen + diff (drift from Rust),
//!   plus magic-comment + schema-validation checks across every committed
//!   `streamlib.yaml`.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use streamlib_processor_schema::StreamlibYaml;

const SCHEMA_RELPATH: &str = "schemas/streamlib.schema.json";
const MAGIC_COMMENT_PREFIX: &str = "# yaml-language-server: $schema=";
const MANIFEST_FILE_NAME: &str = "streamlib.yaml";

/// Generate the canonical JSON Schema document from [`StreamlibYaml`].
///
/// Pretty-printed with `preserve_order` so the output is stable across runs
/// (see the `serde_json` workspace feature).
pub fn generate_schema() -> Result<String> {
    let schema = schemars::schema_for!(StreamlibYaml);
    let mut json = serde_json::to_string_pretty(&schema)
        .context("serialise streamlib.yaml JSON Schema to string")?;
    json.push('\n');
    Ok(json)
}

/// Path to the committed schema artifact, relative to the workspace root.
pub fn committed_schema_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(SCHEMA_RELPATH)
}

/// `xtask emit-manifest-schema` — write the schema to disk.
pub fn emit(workspace_root: &Path) -> Result<()> {
    let schema = generate_schema()?;
    let path = committed_schema_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    std::fs::write(&path, schema).with_context(|| format!("write {}", path.display()))?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}

/// Walk every `streamlib.yaml` under the workspace, skipping `target/`.
pub fn find_streamlib_yamls(workspace_root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some("target") | Some("node_modules") | Some(".git")
            )
        })
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.file_name() == MANIFEST_FILE_NAME)
        .map(|entry| entry.into_path())
        .collect()
}

/// Pull the value of the `# yaml-language-server: $schema=...` magic comment,
/// if present in the file's leading comment block.
fn extract_schema_directive(yaml_text: &str) -> Option<&str> {
    yaml_text
        .lines()
        .take_while(|line| {
            let trimmed = line.trim_start();
            trimmed.is_empty() || trimmed.starts_with('#')
        })
        .find_map(|line| line.trim_start().strip_prefix(MAGIC_COMMENT_PREFIX))
        .map(str::trim)
}

#[derive(Debug)]
struct ManifestProblem {
    path: PathBuf,
    kind: ProblemKind,
}

#[derive(Debug)]
enum ProblemKind {
    MissingMagicComment,
    InvalidYaml(String),
    SchemaViolation(Vec<String>),
}

/// `xtask check-manifest-schema` — the CI gate.
///
/// Three assertions:
/// 1. The committed schema matches what the Rust types currently emit (drift
///    from Rust → fail; reminds the author to run `xtask emit-manifest-schema`).
/// 2. Every `streamlib.yaml` in the workspace carries the magic comment.
/// 3. Every `streamlib.yaml` validates against the schema.
pub fn check(workspace_root: &Path) -> Result<()> {
    let regenerated = generate_schema()?;
    let committed_path = committed_schema_path(workspace_root);
    let committed = std::fs::read_to_string(&committed_path).with_context(|| {
        format!(
            "read committed schema at {} — run `cargo xtask emit-manifest-schema` if missing",
            committed_path.display()
        )
    })?;
    if committed != regenerated {
        bail!(
            "{} is stale — run `cargo xtask emit-manifest-schema` to regenerate from the Rust source of truth",
            committed_path.display()
        );
    }

    let schema_value: serde_json::Value =
        serde_json::from_str(&regenerated).context("parse generated schema as JSON")?;
    let validator = jsonschema::validator_for(&schema_value)
        .map_err(|err| anyhow!("compile streamlib.yaml JSON Schema: {err}"))?;

    let yamls = find_streamlib_yamls(workspace_root);
    if yamls.is_empty() {
        bail!(
            "found no streamlib.yaml files under {} — guard against an accidental skip",
            workspace_root.display()
        );
    }

    let mut problems = Vec::new();
    for path in &yamls {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        if extract_schema_directive(&text).is_none() {
            problems.push(ManifestProblem {
                path: path.clone(),
                kind: ProblemKind::MissingMagicComment,
            });
            continue;
        }
        let yaml_value: serde_yaml::Value = match serde_yaml::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                problems.push(ManifestProblem {
                    path: path.clone(),
                    kind: ProblemKind::InvalidYaml(err.to_string()),
                });
                continue;
            }
        };
        let json_value: serde_json::Value =
            serde_json::to_value(&yaml_value).with_context(|| {
                format!("convert {} to JSON for validation", path.display())
            })?;
        let errors: Vec<String> = validator
            .iter_errors(&json_value)
            .map(|err| format!("{} at {}", err, err.instance_path))
            .collect();
        if !errors.is_empty() {
            problems.push(ManifestProblem {
                path: path.clone(),
                kind: ProblemKind::SchemaViolation(errors),
            });
        }
    }

    if problems.is_empty() {
        tracing::info!(
            "{} streamlib.yaml file(s) validated against {}",
            yamls.len(),
            committed_path.display()
        );
        return Ok(());
    }

    eprintln!(
        "found {} streamlib.yaml problem(s) (out of {} file(s) checked):",
        problems.len(),
        yamls.len()
    );
    for problem in &problems {
        let rel = problem.path.strip_prefix(workspace_root).unwrap_or(&problem.path);
        match &problem.kind {
            ProblemKind::MissingMagicComment => {
                eprintln!(
                    "  {}: missing `{}<relative-path>` header",
                    rel.display(),
                    MAGIC_COMMENT_PREFIX,
                );
            }
            ProblemKind::InvalidYaml(err) => {
                eprintln!("  {}: invalid YAML: {}", rel.display(), err);
            }
            ProblemKind::SchemaViolation(errors) => {
                eprintln!(
                    "  {}: {} schema violation(s):",
                    rel.display(),
                    errors.len()
                );
                for err in errors {
                    eprintln!("    - {}", err);
                }
            }
        }
    }
    bail!("streamlib.yaml validation failed — fix the files above and re-run");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_generates_and_round_trips_as_json() {
        let schema = generate_schema().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let title = parsed
            .get("title")
            .and_then(|t| t.as_str())
            .expect("schema carries a title");
        assert_eq!(title, "StreamlibYaml");
        assert!(parsed.get("properties").is_some());
    }

    #[test]
    fn schema_documents_top_level_keys() {
        let schema = generate_schema().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let properties = parsed.get("properties").unwrap().as_object().unwrap();
        for required_key in ["package", "dependencies", "schemas", "processors", "env"] {
            assert!(
                properties.contains_key(required_key),
                "schema missing top-level key `{}`",
                required_key
            );
        }
    }

    #[test]
    fn extract_schema_directive_finds_header() {
        let yaml = "# Copyright\n# yaml-language-server: $schema=../streamlib.schema.json\npackage:\n  org: tatolab\n";
        assert_eq!(
            extract_schema_directive(yaml),
            Some("../streamlib.schema.json"),
        );
    }

    #[test]
    fn extract_schema_directive_returns_none_when_absent() {
        let yaml = "package:\n  org: tatolab\n";
        assert_eq!(extract_schema_directive(yaml), None);
    }

    #[test]
    fn extract_schema_directive_skips_blank_leading_lines() {
        let yaml = "\n\n# yaml-language-server: $schema=foo.json\n";
        assert_eq!(extract_schema_directive(yaml), Some("foo.json"));
    }
}
