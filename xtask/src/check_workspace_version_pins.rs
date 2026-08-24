// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps every in-tree version pin equal to `[workspace.package] version`.
//!
//! Each workspace crate inherits its version (`version.workspace = true`), and
//! every sibling that path-depends on it also states a registry requirement —
//! `{ path = "../streamlib-error", version = "0.17.0" }` — so the closure still
//! resolves when it is published from a registry rather than from a checkout.
//! release-please bumps `[workspace.package] version` and nothing else: the
//! `simple` release type ships no cargo dependency-requirement updater. The
//! pins therefore sit still while the crates they name move.
//!
//! Within one minor line that is invisible, because `^0.17.0` matches `0.17.1`.
//! The next breaking bump is where it bites: `^0.17.0` excludes `0.18.0`, so
//! the release branch holds a workspace that cannot resolve at all and
//! `cargo update --workspace` fails with `failed to select a version for the
//! requirement`. That is not hypothetical — it held release 0.18.0 shut from
//! 2026-08-11, and with it every wheel the PEP 503 simple index would have
//! served.
//!
//! `cargo metadata --no-deps` does not catch it: it reads manifests without
//! resolving them, so a requirement that excludes its own sibling parses
//! clean. Only a real resolve fails, and the first real resolve happens on the
//! release branch — after the bump, inside the one job whose failure blocks the
//! tag. Gating the pins here instead turns a release-time wedge into a PR-time
//! failure with a one-command fix, and lets the release workflow call the same
//! code to move the pins in lockstep with the bump.
//!
//! Deliberately out of scope. A path dependency carrying no `version` at all is
//! left alone: it cannot be published, but it always resolves, so it is not
//! this gate's business. A pin onto a crate that states its own version —  the
//! vendored vulkanalia trees at `0.35.0` / `0.9.0` — is likewise untouched,
//! because the target does not inherit the workspace version. `vendor/` is
//! excluded from the walk outright, so no rewrite can reach sources the
//! licensing rules forbid reformatting.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{ensure_source_walking_gate_read_source, list_repository_files_under};

/// One dependency entry whose version requirement has drifted away from the
/// workspace version its target crate inherits.
#[derive(Debug, PartialEq, Eq)]
pub struct WorkspaceVersionPinDrift {
    pub manifest_repo_relative_path: String,
    pub line_number_one_based: usize,
    pub dependency_entry_name: String,
    pub pinned_version_requirement: String,
}

/// The result of reading one manifest: what drifted, and the same text with
/// every drifted pin moved onto the workspace version.
pub struct ManifestVersionPinScan {
    pub drifts: Vec<WorkspaceVersionPinDrift>,
    pub manifest_text_with_pins_synced: String,
}

/// `[workspace.package] version` from the workspace root manifest.
pub fn read_workspace_package_version(workspace_root: &Path) -> Result<String> {
    let root_manifest_path = workspace_root.join("Cargo.toml");
    let root_manifest_text = std::fs::read_to_string(&root_manifest_path)
        .with_context(|| format!("read {}", root_manifest_path.display()))?;
    let root_manifest_document: toml::Value = toml::from_str(&root_manifest_text)
        .with_context(|| format!("parse {}", root_manifest_path.display()))?;

    root_manifest_document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|workspace_package| workspace_package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| {
            format!(
                "{} states no [workspace.package] version",
                root_manifest_path.display()
            )
        })
}

/// Whether a manifest's own package version is inherited from the workspace,
/// in either spelling cargo accepts.
///
/// Whitespace is stripped before comparing so the two accepted forms are two
/// exact matches rather than a substring search that `version = "0.1.0" # not
/// workspace = true` could satisfy.
pub fn manifest_inherits_workspace_package_version(manifest_text: &str) -> bool {
    manifest_text.lines().any(|line| {
        let line_without_comment = line.split('#').next().unwrap_or("");
        let normalized: String = line_without_comment
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        normalized == "version.workspace=true" || normalized == "version={workspace=true}"
    })
}

/// The dependency-entry name on a line stating an inline table
/// (`streamlib-error = { … }`), or `None` for any other line.
///
/// `[lib]`'s bare `path = "src/lib.rs"` and `[[test]]`'s bare `path =
/// "tests/…"` are not dependency entries and carry no requirement to keep in
/// step — this is what tells them apart from a dependency that happens to name
/// a path.
fn dependency_entry_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (entry_name, entry_value) = line.split_once('=')?;
    if !entry_value.trim_start().starts_with('{') {
        return None;
    }
    let entry_name = entry_name.trim().trim_matches('"');
    if entry_name.is_empty()
        || !entry_name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '-' || character == '_')
    {
        return None;
    }
    Some(entry_name)
}

/// The value of an inline `key = "…"` on one line, as `(value_start,
/// value_end, value)` byte offsets into `line`.
///
/// The key must appear as a whole token: `default-features` does not answer a
/// lookup for `features`, and a `version` inside a longer identifier is not a
/// version requirement.
fn inline_string_value_for_key(line: &str, key: &str) -> Option<(usize, usize, String)> {
    let mut search_start = 0usize;
    while let Some(key_offset) = line[search_start..].find(key) {
        let key_start = search_start + key_offset;
        let key_end = key_start + key.len();
        search_start = key_end;

        let preceded_by_word_character = line[..key_start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '"');
        if preceded_by_word_character {
            continue;
        }

        let mut remainder = line[key_end..].char_indices().peekable();
        let mut cursor = key_end;
        while let Some((_, character)) = remainder.peek() {
            if character.is_whitespace() {
                cursor += character.len_utf8();
                remainder.next();
            } else {
                break;
            }
        }
        if !line[cursor..].starts_with('=') {
            continue;
        }
        cursor += 1;
        while line[cursor..].starts_with(char::is_whitespace) {
            cursor += line[cursor..].chars().next().map_or(0, char::len_utf8);
        }
        if !line[cursor..].starts_with('"') {
            continue;
        }
        let value_start = cursor + 1;
        let value_end = value_start + line[value_start..].find('"')?;
        return Some((
            value_start,
            value_end,
            line[value_start..value_end].to_owned(),
        ));
    }
    None
}

/// Read one manifest's dependency entries, reporting every drifted pin and
/// returning the text with each of them moved onto the workspace version.
///
/// `target_inherits_workspace_version` is handed the raw `path = "…"` value so
/// the scan itself stays free of the filesystem.
pub fn scan_manifest_version_pins(
    manifest_repo_relative_path: &str,
    manifest_text: &str,
    workspace_package_version: &str,
    target_inherits_workspace_version: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<ManifestVersionPinScan> {
    let mut drifts = Vec::new();
    let mut manifest_text_with_pins_synced = String::with_capacity(manifest_text.len());

    for (line_index, raw_line) in manifest_text.split_inclusive('\n').enumerate() {
        let line_terminator_length = raw_line.len() - raw_line.trim_end_matches(['\n', '\r']).len();
        let line = &raw_line[..raw_line.len() - line_terminator_length];
        let mut line_with_pin_synced = line.to_owned();

        if let Some(entry_name) = dependency_entry_name(line)
            && let Some((_, _, dependency_path)) = inline_string_value_for_key(line, "path")
            && let Some((value_start, value_end, pinned_version_requirement)) =
                inline_string_value_for_key(line, "version")
            && pinned_version_requirement != workspace_package_version
            && target_inherits_workspace_version(&dependency_path)?
        {
            drifts.push(WorkspaceVersionPinDrift {
                manifest_repo_relative_path: manifest_repo_relative_path.to_owned(),
                line_number_one_based: line_index + 1,
                dependency_entry_name: entry_name.to_owned(),
                pinned_version_requirement,
            });
            line_with_pin_synced.replace_range(value_start..value_end, workspace_package_version);
        }

        manifest_text_with_pins_synced.push_str(&line_with_pin_synced);
        manifest_text_with_pins_synced
            .push_str(&raw_line[raw_line.len() - line_terminator_length..]);
    }

    Ok(ManifestVersionPinScan {
        drifts,
        manifest_text_with_pins_synced,
    })
}

/// Every tracked `Cargo.toml` outside `vendor/`.
fn list_scanned_manifest_repo_relative_paths(workspace_root: &Path) -> Result<Vec<String>> {
    Ok(list_repository_files_under(workspace_root, ".")?
        .into_iter()
        .filter(|repo_relative_path| {
            repo_relative_path == "Cargo.toml" || repo_relative_path.ends_with("/Cargo.toml")
        })
        // The vendored vulkanalia fork stays byte-identical to its upstream
        // rev; nothing in it may be rewritten, and none of its pins name a
        // workspace-versioned crate anyway.
        .filter(|repo_relative_path| !repo_relative_path.starts_with("vendor/"))
        .collect())
}

/// Answers "does this path dependency's target inherit the workspace version?",
/// memoized so a crate depended on by fifteen siblings is read once.
struct WorkspaceVersionInheritanceLookup {
    workspace_root: PathBuf,
    answer_by_target_manifest_path: HashMap<PathBuf, bool>,
}

impl WorkspaceVersionInheritanceLookup {
    fn new(workspace_root: &Path) -> Self {
        Self {
            workspace_root: workspace_root.to_path_buf(),
            answer_by_target_manifest_path: HashMap::new(),
        }
    }

    fn target_inherits_workspace_version(
        &mut self,
        manifest_repo_relative_path: &str,
        dependency_path: &str,
    ) -> Result<bool> {
        let manifest_directory = self
            .workspace_root
            .join(manifest_repo_relative_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.workspace_root.clone());
        let target_manifest_path = manifest_directory.join(dependency_path).join("Cargo.toml");

        if let Some(answer) = self
            .answer_by_target_manifest_path
            .get(&target_manifest_path)
        {
            return Ok(*answer);
        }
        // A path that does not resolve to a manifest is a broken dependency,
        // which cargo reports far better than this gate could. Not our failure
        // to raise.
        let answer = match std::fs::read_to_string(&target_manifest_path) {
            Ok(target_manifest_text) => {
                manifest_inherits_workspace_package_version(&target_manifest_text)
            }
            Err(_) => false,
        };
        self.answer_by_target_manifest_path
            .insert(target_manifest_path, answer);
        Ok(answer)
    }
}

/// Read every scanned manifest, returning each one's scan alongside its path.
fn scan_every_manifest(
    workspace_root: &Path,
    workspace_package_version: &str,
) -> Result<Vec<(String, ManifestVersionPinScan)>> {
    let manifest_repo_relative_paths = list_scanned_manifest_repo_relative_paths(workspace_root)?;

    ensure_source_walking_gate_read_source(
        "check-workspace-version-pins",
        "the repository's tracked Cargo.toml files",
        manifest_repo_relative_paths.len(),
        "a version requirement that excludes its own sibling crate reach the release branch",
    )?;

    let mut inheritance_lookup = WorkspaceVersionInheritanceLookup::new(workspace_root);
    let mut scans = Vec::new();

    for manifest_repo_relative_path in manifest_repo_relative_paths {
        let manifest_path = workspace_root.join(&manifest_repo_relative_path);
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;

        let scanned_manifest_path = manifest_repo_relative_path.clone();
        let scan = scan_manifest_version_pins(
            &manifest_repo_relative_path,
            &manifest_text,
            workspace_package_version,
            &mut |dependency_path| {
                inheritance_lookup
                    .target_inherits_workspace_version(&scanned_manifest_path, dependency_path)
            },
        )?;
        scans.push((manifest_repo_relative_path, scan));
    }

    Ok(scans)
}

/// The gate: fail when any in-tree pin has drifted from the workspace version.
pub fn run(workspace_root: &Path) -> Result<()> {
    let workspace_package_version = read_workspace_package_version(workspace_root)?;
    let scans = scan_every_manifest(workspace_root, &workspace_package_version)?;

    let drifts: Vec<&WorkspaceVersionPinDrift> = scans
        .iter()
        .flat_map(|(_, scan)| scan.drifts.iter())
        .collect();

    anyhow::ensure!(
        drifts.is_empty(),
        "{} in-tree version pin(s) have drifted from [workspace.package] version \
         {workspace_package_version} — the next breaking release bump would leave the \
         workspace unresolvable. Run `cargo xtask check-workspace-version-pins --fix`.\n{}",
        drifts.len(),
        drifts
            .iter()
            .map(|drift| format!(
                "  {}:{} — {} pinned at {}",
                drift.manifest_repo_relative_path,
                drift.line_number_one_based,
                drift.dependency_entry_name,
                drift.pinned_version_requirement,
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    tracing::info!(
        "every in-tree version pin states [workspace.package] version {workspace_package_version}"
    );
    Ok(())
}

/// Move every drifted pin onto the workspace version, returning the manifests
/// rewritten.
///
/// This is what the release workflow calls on the release-please branch, after
/// the bump and before `cargo update --workspace`.
pub fn rewrite_version_pins_to_workspace_version(workspace_root: &Path) -> Result<Vec<String>> {
    let workspace_package_version = read_workspace_package_version(workspace_root)?;
    let scans = scan_every_manifest(workspace_root, &workspace_package_version)?;

    let mut rewritten_manifest_repo_relative_paths = Vec::new();
    for (manifest_repo_relative_path, scan) in scans {
        if scan.drifts.is_empty() {
            continue;
        }
        let manifest_path = workspace_root.join(&manifest_repo_relative_path);
        std::fs::write(&manifest_path, &scan.manifest_text_with_pins_synced)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        tracing::info!(
            "{manifest_repo_relative_path}: {} pin(s) moved to {workspace_package_version}",
            scan.drifts.len()
        );
        rewritten_manifest_repo_relative_paths.push(manifest_repo_relative_path);
    }

    if rewritten_manifest_repo_relative_paths.is_empty() {
        tracing::info!("every in-tree version pin already states {workspace_package_version}");
    }
    Ok(rewritten_manifest_repo_relative_paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every path dependency in these tests targets a workspace-versioned
    /// crate unless its path says `vendor`, which stands in for the vendored
    /// trees that carry their own version.
    fn inheritance_by_path_convention(dependency_path: &str) -> Result<bool> {
        Ok(!dependency_path.contains("vendor"))
    }

    fn scan(manifest_text: &str) -> ManifestVersionPinScan {
        scan_manifest_version_pins(
            "sdk/streamlib-error/Cargo.toml",
            manifest_text,
            "0.18.0",
            &mut inheritance_by_path_convention,
        )
        .expect("the convention lookup never fails")
    }

    #[test]
    fn a_drifted_pin_is_reported_and_synced() {
        let scanned = scan(
            "[dependencies]\n\
             streamlib-processor-schema = { path = \"../streamlib-processor-schema\", version = \"0.17.0\" }\n",
        );

        assert_eq!(scanned.drifts.len(), 1);
        assert_eq!(scanned.drifts[0].line_number_one_based, 2);
        assert_eq!(
            scanned.drifts[0].dependency_entry_name,
            "streamlib-processor-schema"
        );
        assert_eq!(scanned.drifts[0].pinned_version_requirement, "0.17.0");
        assert!(
            scanned
                .manifest_text_with_pins_synced
                .contains("version = \"0.18.0\"")
        );
    }

    #[test]
    fn a_pin_already_on_the_workspace_version_is_left_alone() {
        let manifest_text =
            "streamlib-error = { path = \"../streamlib-error\", version = \"0.18.0\" }\n";
        let scanned = scan(manifest_text);

        assert!(scanned.drifts.is_empty());
        assert_eq!(scanned.manifest_text_with_pins_synced, manifest_text);
    }

    #[test]
    fn a_pin_onto_a_crate_carrying_its_own_version_is_left_alone() {
        let manifest_text = "vulkanalia = { package = \"tatolab-vulkanalia\", path = \"vendor/tatolab-vulkanalia\", version = \"0.35.0\" }\n";
        let scanned = scan(manifest_text);

        assert!(scanned.drifts.is_empty());
        assert_eq!(scanned.manifest_text_with_pins_synced, manifest_text);
    }

    #[test]
    fn a_path_dependency_stating_no_version_is_left_alone() {
        let manifest_text = "streamlib-error = { path = \"../streamlib-error\" }\n";
        let scanned = scan(manifest_text);

        assert!(scanned.drifts.is_empty());
        assert_eq!(scanned.manifest_text_with_pins_synced, manifest_text);
    }

    #[test]
    fn a_lib_or_test_target_path_is_not_a_dependency_entry() {
        let manifest_text =
            "[lib]\npath = \"src/lib.rs\"\n\n[[test]]\npath = \"tests/conformance.rs\"\n";
        let scanned = scan(manifest_text);

        assert!(scanned.drifts.is_empty());
        assert_eq!(scanned.manifest_text_with_pins_synced, manifest_text);
        assert_eq!(dependency_entry_name("path = \"src/lib.rs\""), None);
    }

    #[test]
    fn a_commented_out_dependency_is_not_scanned() {
        let manifest_text =
            "# streamlib-error = { path = \"../streamlib-error\", version = \"0.17.0\" }\n";
        let scanned = scan(manifest_text);

        assert!(scanned.drifts.is_empty());
        assert_eq!(scanned.manifest_text_with_pins_synced, manifest_text);
    }

    #[test]
    fn a_manifest_with_no_trailing_newline_keeps_its_shape() {
        let scanned =
            scan("streamlib-error = { path = \"../streamlib-error\", version = \"0.17.0\" }");

        assert_eq!(scanned.drifts.len(), 1);
        assert_eq!(
            scanned.manifest_text_with_pins_synced,
            "streamlib-error = { path = \"../streamlib-error\", version = \"0.18.0\" }"
        );
    }

    #[test]
    fn only_the_version_value_moves_on_a_dependency_line_naming_other_keys() {
        let scanned = scan(
            "streamlib = { path = \"../streamlib-sdk\", version = \"0.17.0\", default-features = false, features = [\"vulkan\"] }\n",
        );

        assert_eq!(scanned.drifts.len(), 1);
        assert_eq!(
            scanned.manifest_text_with_pins_synced,
            "streamlib = { path = \"../streamlib-sdk\", version = \"0.18.0\", default-features = false, features = [\"vulkan\"] }\n"
        );
    }

    #[test]
    fn both_inherited_version_spellings_are_recognised() {
        assert!(manifest_inherits_workspace_package_version(
            "[package]\nname = \"streamlib-error\"\nversion.workspace = true\n"
        ));
        assert!(manifest_inherits_workspace_package_version(
            "[package]\nversion = { workspace = true }\n"
        ));
    }

    #[test]
    fn a_crate_stating_its_own_version_does_not_read_as_inherited() {
        assert!(!manifest_inherits_workspace_package_version(
            "[package]\nname = \"tatolab-vulkanalia\"\nversion = \"0.35.0\"\n"
        ));
        assert!(!manifest_inherits_workspace_package_version(
            "[package]\nversion = \"0.1.0\" # workspace = true, but not really\n"
        ));
    }

    #[test]
    fn an_inline_key_lookup_matches_whole_tokens_only() {
        let line = "streamlib = { path = \"../streamlib-sdk\", default-features = false }";
        assert_eq!(inline_string_value_for_key(line, "features"), None);
        assert_eq!(
            inline_string_value_for_key(line, "path").map(|(_, _, value)| value),
            Some("../streamlib-sdk".to_owned())
        );
    }

    #[test]
    fn the_workspace_package_version_is_read_from_the_root_manifest() {
        let workspace_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            workspace_root.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.18.0\"\n",
        )
        .expect("write root manifest");

        assert_eq!(
            read_workspace_package_version(workspace_root.path()).expect("read version"),
            "0.18.0"
        );
    }
}
