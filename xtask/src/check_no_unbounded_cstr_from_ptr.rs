// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans the unbounded-lifetime spelling `CStr::from_ptr(<owner>.as_ptr())` in
//! the Vulkan RHI trees.
//!
//! `CStr::from_ptr` returns an *unbounded* lifetime: the borrow checker cannot
//! tie the resulting `&CStr` to the buffer the pointer came from, so a name
//! read out of an enumerated `VkExtensionProperties` / `VkLayerProperties` /
//! `VkPhysicalDeviceProperties` buffer keeps compiling after that buffer is
//! dropped. Two device bring-up paths shipped exactly that use-after-free
//! (#1846). Every Vulkan inline name array is a `vk::StringArray`, whose
//! `as_cstr(&self) -> &CStr` performs the same read while borrowing from
//! `&self` — the lifetime the compiler can then check.
//!
//! `.as_ptr()` inside the argument is the discriminator. A bare pointer
//! argument — `CStr::from_ptr(extension_names_ptr)` over storage an external
//! API owns — has no Rust owner to borrow from and is not flagged.
//!
//! Cheap substring scan (no `syn`/compile). Whole-line comments are skipped, and
//! a per-line `streamlib:allow-unbounded-cstr-from-ptr` pragma is the escape
//! hatch.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Workspace-relative roots that own every `vulkanalia` call.
const SCAN_ROOTS: &[&str] = &[
    "runtime/streamlib-engine/src/vulkan",
    "runtime/streamlib-consumer-rhi/src",
];

const UNBOUNDED_CSTR_CONSTRUCTOR: &str = "CStr::from_ptr(";

/// Presence of this in the argument means a Rust value owns the storage, so a
/// borrowing accessor exists and the unbounded lifetime is unnecessary.
const OWNED_STORAGE_POINTER_ACCESSOR: &str = ".as_ptr()";

/// The borrowing accessor every `vk::StringArray` offers.
const BORROWING_ACCESSOR: &str = "StringArray::as_cstr";

/// Per-line escape hatch for a deliberate, reviewed unbounded borrow.
const ALLOW_LINE_PRAGMA: &str = "streamlib:allow-unbounded-cstr-from-ptr";

#[derive(Debug, PartialEq, Eq)]
pub struct UnboundedCStrFromPtrViolation {
    pub file: PathBuf,
    pub line: usize,
    pub call_text: String,
}

#[derive(Debug, Default)]
pub struct UnboundedCStrFromPtrScanReport {
    pub violations: Vec<UnboundedCStrFromPtrViolation>,
    pub files_scanned: usize,
    pub files_scanned_per_root: Vec<(&'static str, usize)>,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;
    crate::ensure_source_walking_gate_read_source(
        "check-no-unbounded-cstr-from-ptr",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "an unbounded-lifetime CStr borrow re-enter the Vulkan RHI",
    )?;
    ensure_every_scan_root_contributed(&report)?;

    if report.violations.is_empty() {
        println!(
            "✓ check-no-unbounded-cstr-from-ptr: {} Vulkan RHI file(s) scanned, \
             no `CStr::from_ptr(<owner>.as_ptr())`",
            report.files_scanned,
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-no-unbounded-cstr-from-ptr: {} violation(s)",
        report.violations.len()
    );
    for violation in &report.violations {
        eprintln!(
            "  {}:{}: `{}` — `CStr::from_ptr` returns an unbounded lifetime, so the \
             borrow is not tied to the storage `.as_ptr()` came from and compiles \
             even once that storage is gone. Use the owner's borrowing accessor \
             (`{BORROWING_ACCESSOR}`) so the lifetime is checked. See issue #1846.",
            violation.file.display(),
            violation.line,
            violation.call_text,
        );
    }
    anyhow::bail!(
        "check-no-unbounded-cstr-from-ptr: {} unbounded CStr borrow(s) in the Vulkan RHI",
        report.violations.len()
    );
}

/// A renamed or moved root would leave the other one carrying the whole gate,
/// which reads identically to a clean tree.
fn ensure_every_scan_root_contributed(report: &UnboundedCStrFromPtrScanReport) -> Result<()> {
    for (root, files_scanned) in &report.files_scanned_per_root {
        anyhow::ensure!(
            *files_scanned > 0,
            "check-no-unbounded-cstr-from-ptr scanned 0 files under {root} — that scan \
             root moved out from under the gate"
        );
    }
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<UnboundedCStrFromPtrScanReport> {
    let mut report = UnboundedCStrFromPtrScanReport::default();
    for root in SCAN_ROOTS {
        let mut files_scanned_here = 0usize;
        for entry in WalkDir::new(workspace_root.join(root))
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let body = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            files_scanned_here += 1;
            for (line, call_text) in unbounded_cstr_from_ptr_calls(&body) {
                report.violations.push(UnboundedCStrFromPtrViolation {
                    file: path.to_path_buf(),
                    line,
                    call_text,
                });
            }
        }
        report.files_scanned += files_scanned_here;
        report
            .files_scanned_per_root
            .push((root, files_scanned_here));
    }
    Ok(report)
}

/// Every `CStr::from_ptr(…)` in `body` whose argument reaches through
/// `.as_ptr()`, as `(1-based line, whitespace-collapsed call text)`.
fn unbounded_cstr_from_ptr_calls(body: &str) -> Vec<(usize, String)> {
    let code = blank_out_exempt_lines(body);
    let mut calls = Vec::new();
    let mut search_from = 0usize;
    while let Some(offset) = code[search_from..].find(UNBOUNDED_CSTR_CONSTRUCTOR) {
        let call_start = search_from + offset;
        let open_paren = call_start + UNBOUNDED_CSTR_CONSTRUCTOR.len() - 1;
        let Some(close_paren) = matching_close_paren(&code, open_paren) else {
            break;
        };
        if code[open_paren + 1..close_paren].contains(OWNED_STORAGE_POINTER_ACCESSOR) {
            calls.push((
                code[..call_start].matches('\n').count() + 1,
                code[call_start..=close_paren]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
        }
        search_from = close_paren + 1;
    }
    calls
}

/// Blank the lines the gate does not read while keeping the line count, so a
/// reported line number still points at the source.
fn blank_out_exempt_lines(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") || line.contains(ALLOW_LINE_PRAGMA) {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn matching_close_paren(code: &str, open_paren: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in code.as_bytes().iter().enumerate().skip(open_paren) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Both roots get a file, so a fixture exercises the same shape the gate
    /// asserts on the real tree.
    fn scan_engine_vulkan_file(rel: &str, body: &str) -> UnboundedCStrFromPtrScanReport {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), &format!("{}/{rel}", SCAN_ROOTS[0]), body);
        write(
            tmp.path(),
            &format!("{}/lib.rs", SCAN_ROOTS[1]),
            "pub fn ok() {}\n",
        );
        scan(tmp.path()).unwrap()
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    #[test]
    fn flags_the_inline_name_array_shape_that_shipped_as_use_after_free() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_device.rs",
            "fn names(ext: &vk::ExtensionProperties) -> &CStr {\n    \
             unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) }\n}\n",
        );
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(
            report.violations[0].call_text,
            "CStr::from_ptr(ext.extension_name.as_ptr())"
        );
    }

    #[test]
    fn accepts_a_bare_pointer_owned_by_an_external_api() {
        let report = scan_engine_vulkan_file(
            "rhi/drm_modifier_probe.rs",
            "let exts = unsafe { CStr::from_ptr(exts_ptr) }.to_str().unwrap_or(\"\");\n",
        );
        assert!(
            report.violations.is_empty(),
            "an EGL-owned pointer has no Rust owner to borrow from: {:?}",
            report.violations
        );
    }

    #[test]
    fn accepts_the_borrowing_accessor() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_device.rs",
            "let name = device_props.device_name.as_cstr();\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn skips_a_doc_comment_that_names_the_banned_spelling() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_extension_names.rs",
            "//! `CStr::from_ptr(properties.extension_name.as_ptr())` was the #1846 bug.\n\
             /// Superseded by `CStr::from_ptr(x.as_ptr())`.\n\
             pub fn ok() {}\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn skips_a_commented_out_call() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_device.rs",
            "    // let name = CStr::from_ptr(ext.extension_name.as_ptr());\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn flags_a_call_rustfmt_split_across_lines() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_device.rs",
            "let name = unsafe {\n    CStr::from_ptr(\n        layer_properties.layer_name.as_ptr(),\n    )\n};\n",
        );
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(
            report.violations[0].call_text,
            "CStr::from_ptr( layer_properties.layer_name.as_ptr(), )"
        );
    }

    #[test]
    fn accepts_the_allow_line_pragma() {
        let report = scan_engine_vulkan_file(
            "rhi/vulkan_device.rs",
            "let n = CStr::from_ptr(a.as_ptr()); // streamlib:allow-unbounded-cstr-from-ptr\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn scans_the_consumer_rhi_root() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            &format!("{}/mod.rs", SCAN_ROOTS[0]),
            "pub fn ok() {}\n",
        );
        write(
            tmp.path(),
            &format!("{}/consumer_vulkan_device.rs", SCAN_ROOTS[1]),
            "let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) };\n",
        );
        let report = scan(tmp.path()).unwrap();
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
    }

    #[test]
    fn refuses_a_tree_where_one_scan_root_read_nothing() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            &format!("{}/mod.rs", SCAN_ROOTS[0]),
            "pub fn ok() {}\n",
        );
        let report = scan(tmp.path()).unwrap();
        let err = ensure_every_scan_root_contributed(&report).unwrap_err();
        assert!(err.to_string().contains(SCAN_ROOTS[1]), "got {err}");
    }
}
