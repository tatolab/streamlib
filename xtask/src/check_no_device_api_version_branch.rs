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
//!
//! Two passes: dotted reads (`props.api_version`) per line, and `api_version`
//! bound out of a `PhysicalDeviceProperties { … }` destructuring pattern, which
//! carries no dot at all.
//!
//! The floor it accepts, stated so nobody mistakes it for a parser: only
//! whole-line comments are skipped, so `.api_version` in a trailing comment, a
//! `/* */` span or a string literal is flagged and takes the pragma; and a
//! pattern destructured through a type alias rather than the
//! `PhysicalDeviceProperties` name is not seen.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Workspace trees this gate owns. `xtask` is in so this gate's own
/// [`SCAN_EXEMPT_FILES`] entry is reachable rather than dead.
const SCAN_ROOTS: &[&str] = &["runtime", "adapters", "sdk", "xtask"];

/// The field read this gate bans. A following `(` makes it the
/// `VkApplicationInfo` builder setter instead, which is the request, not a probe.
const BANNED_DEVICE_API_VERSION_FIELD_READ: &str = ".api_version";

/// The field name a destructuring pattern binds, with no dot to find it by.
const BOUND_DEVICE_API_VERSION_FIELD: &str = "api_version";

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

        for line in lines_binding_api_version_in_a_device_properties_pattern(&body) {
            let line_text = body.lines().nth(line - 1).unwrap_or_default();
            if line_text.contains(ALLOW_LINE_PRAGMA) {
                continue;
            }
            report.violations.push(DeviceApiVersionReadViolation {
                path: relative_path.clone(),
                line,
                line_text: line_text.to_string(),
            });
        }
    }

    Ok(report)
}

/// Lines binding `api_version` out of a `VkPhysicalDeviceProperties`
/// destructuring pattern.
///
/// `let vk::PhysicalDeviceProperties { api_version, .. } = properties;` reads the
/// device's report with no dot anywhere, so the dotted scan above cannot see it
/// and the binding is free to decide an entry point on the next line. Walks the
/// braces of each `PhysicalDeviceProperties { … }` so a pattern split across
/// lines is caught too. A field *initialiser* — `api_version:` — builds a
/// properties value rather than reading one, and is left alone.
fn lines_binding_api_version_in_a_device_properties_pattern(body: &str) -> Vec<usize> {
    const DEVICE_PROPERTIES_TYPE: &str = "PhysicalDeviceProperties";
    let mut lines = Vec::new();

    for (type_start, matched) in body.match_indices(DEVICE_PROPERTIES_TYPE) {
        let after_type = type_start + matched.len();
        // Only a brace the type name itself opens is a pattern or a literal.
        // `fn probe(properties: vk::PhysicalDeviceProperties) -> bool {` also has
        // a `{` after the name — the function body — and taking that one would
        // make every bare `api_version` in the body a violation.
        let Some(brace_offset) =
            body[after_type..].find(|character: char| !character.is_whitespace())
        else {
            continue;
        };
        if body[after_type..].as_bytes()[brace_offset] != b'{' {
            continue;
        }
        let pattern_start = after_type + brace_offset;
        let Some(pattern_end) = matching_close_brace(body, pattern_start) else {
            continue;
        };

        let pattern = &body[pattern_start..pattern_end];
        for (field_start, field) in pattern.match_indices(BOUND_DEVICE_API_VERSION_FIELD) {
            let character_before = pattern[..field_start].chars().next_back();
            let is_a_dotted_read_or_longer_identifier = character_before.is_some_and(|character| {
                character == '.' || character.is_alphanumeric() || character == '_'
            });
            let character_after = pattern[field_start + field.len()..]
                .chars()
                .find(|character| !character.is_whitespace());
            // `api_version:` initialises a field; `api_version,` / `api_version }`
            // binds one.
            let is_a_field_initialiser = character_after == Some(':');
            if is_a_dotted_read_or_longer_identifier || is_a_field_initialiser {
                continue;
            }
            let absolute = pattern_start + field_start;
            lines.push(body[..absolute].matches('\n').count() + 1);
        }
    }

    lines
}

/// The offset of the `}` closing the `{` at `open_brace`.
fn matching_close_brace(body: &str, open_brace: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, character) in body[open_brace..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open_brace + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `line` reads an `api_version` field rather than calling the builder
/// setter of the same name.
fn line_reads_a_device_api_version_field(line: &str) -> bool {
    line.match_indices(BANNED_DEVICE_API_VERSION_FIELD_READ)
        .any(|(match_start, matched)| {
            // `(` makes it the builder setter; an identifier character makes it
            // `.api_version_something`, a different field.
            let character_after = line[match_start + matched.len()..].chars().next();
            !character_after.is_some_and(|character| {
                character == '(' || character.is_alphanumeric() || character == '_'
            })
        })
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
    fn rejects_api_version_bound_out_of_a_device_properties_pattern() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let vk::PhysicalDeviceProperties { api_version, .. } = properties;\n",
        );
        assert_eq!(
            violations.len(),
            1,
            "a destructuring binding reads the report with no dot to find it by: {violations:?}"
        );
    }

    /// rustfmt splits a wide pattern across lines; the binding is the same read.
    #[test]
    fn rejects_a_device_properties_pattern_split_across_lines() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let vk::PhysicalDeviceProperties {\n    device_name,\n    api_version,\n    ..\n\
             } = properties;\n",
        );
        assert_eq!(
            violations.len(),
            1,
            "a multi-line pattern binds just the same: {violations:?}"
        );
        assert_eq!(
            violations[0].line, 3,
            "the report must point at the binding, not the pattern's first line"
        );
    }

    /// Building a properties value is not reading a device's report.
    #[test]
    fn accepts_a_device_properties_struct_literal_initialising_the_field() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let properties = vk::PhysicalDeviceProperties {\n    \
             api_version: REQUESTED_VULKAN_INSTANCE_API_VERSION,\n    ..Default::default()\n};\n",
        );
        assert!(
            violations.is_empty(),
            "a field initialiser constructs, it does not probe: {violations:?}"
        );
    }

    /// The pattern pass must not fire on every `api_version` in the file — only
    /// on one inside a `PhysicalDeviceProperties { … }` span.
    #[test]
    fn accepts_an_unrelated_binding_of_the_same_name_outside_a_properties_pattern() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let SomeOtherThing { api_version, .. } = thing;\n\
             struct Request { api_version: u32 }\n",
        );
        assert!(
            violations.is_empty(),
            "only a device-properties pattern is a device read: {violations:?}"
        );
    }

    /// A signature naming the type also has a `{` after it — the function body.
    /// Taking that brace would make every bare `api_version` in the body a
    /// violation, including an unrelated local.
    #[test]
    fn accepts_a_local_named_the_same_inside_a_function_typed_by_device_properties() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "fn describe(properties: vk::PhysicalDeviceProperties) -> u32 {\n    \
             let api_version = REQUESTED_VULKAN_INSTANCE_API_VERSION;\n    api_version\n}\n",
        );
        assert!(
            violations.is_empty(),
            "only a brace the type name itself opens is a pattern: {violations:?}"
        );
    }

    #[test]
    fn accepts_a_bound_field_carrying_the_pragma() {
        let violations = scan_one(
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "let vk::PhysicalDeviceProperties { api_version, .. } = properties; \
             // streamlib:allow-device-api-version-read\n",
        );
        assert!(
            violations.is_empty(),
            "the pragma exempts a reporting binding too: {violations:?}"
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
        assert!(
            violations.is_empty(),
            "only Rust is scanned: {violations:?}"
        );
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
