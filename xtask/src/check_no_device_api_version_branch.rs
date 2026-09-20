// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans reading a physical device's reported `apiVersion` field.
//!
//! The requested *instance* API version is the engine's floor for promoted entry
//! points: `vk::make_version(1, 4, 0)` at instance creation is what makes
//! `cmd_pipeline_barrier2`, `cmd_begin_rendering`, `queue_submit2` and
//! `wait_semaphores` resolve. A device's reported `apiVersion` is not a
//! capability report and must never decide which entry points to call —
//! MoltenVK clamps it to whatever the instance requested, so it answers 1.0.323
//! to an instance that asked for 1.0 and 1.4.323 to one that asked for 1.4, on
//! the same hardware. Code that probes it reads its own request back and calls
//! that a capability.
//!
//! Test code is gated too, and deliberately: a test asserting a floor on the
//! reported version passes everywhere for the wrong reason, and teaches the
//! model this gate exists to unteach.
//!
//! Cheap substring scan (no `syn`/compile). The builder setter
//! `.api_version(...)` sets the request and is always allowed; a per-line
//! `streamlib:allow-device-api-version-read` pragma is the escape hatch for a
//! read that only reports.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Workspace trees that hold Vulkan code this gate owns.
const SCAN_ROOTS: &[&str] = &["runtime", "adapters", "sdk"];

/// The field read this gate bans. A following `(` makes it the
/// `VkApplicationInfo` builder setter instead, which is the request, not a probe.
const BANNED_DEVICE_API_VERSION_FIELD_READ: &str = ".api_version";

/// Per-line escape hatch for a read that only reports the value.
const ALLOW_LINE_PRAGMA: &str = "streamlib:allow-device-api-version-read";

/// Files whose source text spells the banned pattern without reading a device —
/// this gate's own constants and fixtures.
const SCAN_EXEMPT_FILES: &[&str] = &["xtask/src/check_no_device_api_version_branch.rs"];

/// One banned read of a device's reported `apiVersion`.
#[derive(Debug)]
pub struct DeviceApiVersionReadViolation {
    pub path: PathBuf,
    pub line: usize,
    pub line_text: String,
}

/// What one gate run read and found.
#[derive(Debug, Default)]
pub struct DeviceApiVersionScanReport {
    pub violations: Vec<DeviceApiVersionReadViolation>,
    pub files_scanned: usize,
    pub files_scanned_per_scan_root: Vec<(&'static str, usize)>,
}

/// Run the gate over the workspace.
pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-no-device-api-version-branch",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "a device `apiVersion` probe deciding which entry points resolve",
    )?;
    crate::ensure_every_source_walking_gate_scan_root_contributed(
        "check-no-device-api-version-branch",
        &report.files_scanned_per_scan_root,
    )?;

    if report.violations.is_empty() {
        println!(
            "✓ check-no-device-api-version-branch: {} file(s) scanned across {:?}, no device \
             `apiVersion` read",
            report.files_scanned, SCAN_ROOTS,
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-no-device-api-version-branch: {} violation(s)",
        report.violations.len()
    );
    for violation in &report.violations {
        eprintln!(
            "  {}:{}\n      {}",
            violation.path.display(),
            violation.line,
            violation.line_text.trim(),
        );
    }
    eprintln!(
        "\nA device's reported `apiVersion` is not a capability report — MoltenVK clamps it to \
         whatever the instance requested, so it answers back the engine's own \
         `vk::make_version(1, 4, 0)` and a probe of it proves nothing. The requested instance \
         version is the floor for promoted entry points; assert on that constant instead. A read \
         that only reports the value takes the `{ALLOW_LINE_PRAGMA}` pragma."
    );
    anyhow::bail!(
        "check-no-device-api-version-branch: {} device `apiVersion` read(s)",
        report.violations.len()
    );
}

/// Scan every git-tracked Rust file under the scan roots.
pub fn scan(workspace_root: &Path) -> Result<DeviceApiVersionScanReport> {
    let tracked = crate::tracked_files_under_scan_roots(
        workspace_root,
        SCAN_ROOTS,
        "check-no-device-api-version-branch",
    )?;
    scan_files(workspace_root, &tracked)
}

/// Scan the given workspace-relative paths.
pub fn scan_files(
    workspace_root: &Path,
    relative_paths: &[PathBuf],
) -> Result<DeviceApiVersionScanReport> {
    let mut report = DeviceApiVersionScanReport {
        files_scanned_per_scan_root: SCAN_ROOTS.iter().map(|root| (*root, 0)).collect(),
        ..Default::default()
    };

    for relative_path in relative_paths {
        if relative_path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        if SCAN_EXEMPT_FILES
            .iter()
            .any(|exempt| relative_path == Path::new(exempt))
        {
            continue;
        }

        let body = fs::read_to_string(workspace_root.join(relative_path))
            .with_context(|| format!("failed to read {}", relative_path.display()))?;

        report.files_scanned += 1;
        let scan_root_of_this_file = SCAN_ROOTS
            .iter()
            .find(|root| relative_path.starts_with(root));
        if let Some(root) = scan_root_of_this_file {
            for (counted_root, count) in report.files_scanned_per_scan_root.iter_mut() {
                if counted_root == root {
                    *count += 1;
                }
            }
        }

        for (line_index, line_text) in body.lines().enumerate() {
            if crate::source_call_site_scan::is_a_whole_line_comment(line_text)
                || line_text.contains(ALLOW_LINE_PRAGMA)
            {
                continue;
            }
            if !line_reads_a_device_api_version_field(line_text) {
                continue;
            }
            report.violations.push(DeviceApiVersionReadViolation {
                path: relative_path.clone(),
                line: line_index + 1,
                line_text: line_text.to_string(),
            });
        }
    }

    Ok(report)
}

/// Whether `line` reads an `api_version` field rather than calling the builder
/// setter of the same name.
fn line_reads_a_device_api_version_field(line: &str) -> bool {
    let mut search_from = 0usize;
    while let Some(offset) = line[search_from..].find(BANNED_DEVICE_API_VERSION_FIELD_READ) {
        let match_start = search_from + offset;
        let match_end = match_start + BANNED_DEVICE_API_VERSION_FIELD_READ.len();
        // `.api_version_foo` is a different identifier, not this field.
        let next_character = line[match_end..].chars().next();
        let is_the_builder_setter = next_character == Some('(');
        let is_a_longer_identifier = next_character
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        if !is_the_builder_setter && !is_a_longer_identifier {
            return true;
        }
        search_from = match_end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scan_one(relative_path: &str, body: &str) -> Vec<DeviceApiVersionReadViolation> {
        let workspace = TempDir::new().unwrap();
        let path = workspace.path().join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        scan_files(workspace.path(), &[PathBuf::from(relative_path)])
            .unwrap()
            .violations
    }

    #[test]
    fn rejects_a_shift_of_the_reported_device_api_version() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let major = props.api_version >> 22;\n",
        );
        assert_eq!(
            violations.len(),
            1,
            "a device apiVersion read must be flagged: {violations:?}"
        );
    }

    #[test]
    fn rejects_an_entry_point_branch_on_the_reported_device_api_version() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_command_recorder.rs",
            "if device_properties.api_version >= vk::make_version(1, 3, 0) { \
             recorder.cmd_pipeline_barrier2(); }\n",
        );
        assert_eq!(
            violations.len(),
            1,
            "branching entry-point selection on the reported version must be flagged: \
             {violations:?}"
        );
    }

    #[test]
    fn accepts_the_application_info_builder_setter() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let app_info = vk::ApplicationInfo::builder()\n    \
             .api_version(REQUESTED_VULKAN_INSTANCE_API_VERSION)\n    .build();\n",
        );
        assert!(
            violations.is_empty(),
            "the request is set through this builder, not probed: {violations:?}"
        );
    }

    #[test]
    fn accepts_a_struct_field_declaration_and_its_literal_initialiser() {
        let violations = scan_one(
            "adapters/streamlib-adapter-vulkan/src/raw_handles.rs",
            "pub struct RawVulkanHandles { pub api_version: u32 }\n\
             fn snapshot() -> RawVulkanHandles { RawVulkanHandles { \
             api_version: vk::make_version(1, 4, 0) } }\n",
        );
        assert!(
            violations.is_empty(),
            "a field declaration and a literal initialiser carry the request: {violations:?}"
        );
    }

    #[test]
    fn accepts_a_longer_identifier_that_merely_starts_the_same() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let requested = info.api_version_requested_at_instance_creation;\n",
        );
        assert!(
            violations.is_empty(),
            "`.api_version_*` is a different field: {violations:?}"
        );
    }

    #[test]
    fn skips_a_comment_that_only_mentions_the_field() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "// MoltenVK clamps props.api_version to the instance request.\n\
             /// Never read `device.api_version` to select an entry point.\n",
        );
        assert!(
            violations.is_empty(),
            "a comment is not a read: {violations:?}"
        );
    }

    #[test]
    fn accepts_a_reporting_read_carrying_the_pragma() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "tracing::info!(reported = props.api_version); \
             // streamlib:allow-device-api-version-read\n",
        );
        assert!(
            violations.is_empty(),
            "the pragma exempts a reporting read: {violations:?}"
        );
    }

    #[test]
    fn gates_test_code_too() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "#[cfg(test)]\nmod tests {\n  #[test]\n  fn floor() { \
             assert!(props.api_version >= wanted); }\n}\n",
        );
        assert_eq!(
            violations.len(),
            1,
            "a test asserting a floor on the reported version teaches the wrong model: \
             {violations:?}"
        );
    }

    #[test]
    fn skips_a_file_that_is_not_rust() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/notes.md",
            "props.api_version is clamped by MoltenVK\n",
        );
        assert!(violations.is_empty(), "only Rust is scanned: {violations:?}");
    }

    #[test]
    fn the_engine_tree_holds_no_device_api_version_read() {
        let report = scan(&crate::workspace_root().unwrap()).unwrap();
        assert!(
            report.violations.is_empty(),
            "the tree must stay free of device apiVersion reads: {:?}",
            report.violations
        );
    }
}
