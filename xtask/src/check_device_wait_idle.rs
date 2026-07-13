// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans raw `vkDeviceWaitIdle` (`device_wait_idle()`) in the engine outside the
//! one mutex-guarded helper `HostVulkanDevice::wait_idle`.
//!
//! `vkDeviceWaitIdle` is externally synchronized over the `VkDevice` **and every
//! queue it owns** — a raw call that doesn't hold the per-queue mutexes races
//! concurrent `vkQueueSubmit2` / `vkQueuePresentKHR` during multi-processor
//! setup and crashes the NVIDIA driver (the validation layer reports
//! `UNASSIGNED-Threading-Info: vkDeviceWaitIdle(): Couldn't find VkQueue
//! Object`). `HostVulkanDevice::wait_idle` (in `vulkan_device.rs`) acquires all
//! five per-queue mutexes + the device mutex first; every consumer must route
//! through it.
//!
//! Cheap substring scan (no `syn`/compile). Test files and inline `#[cfg(test)]`
//! modules are exempt; the helper's own file is the single allowed raw site; a
//! per-line `streamlib:allow-raw-device-wait-idle` pragma is the escape hatch.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// The raw method invocation this lint bans outside the helper.
const BANNED: &str = ".device_wait_idle(";

/// Path (engine-`src`-relative, forward-slash) of the single file allowed to
/// call it raw — `HostVulkanDevice::wait_idle` is the guarded gateway.
const ALLOW_FILE: &str = "vulkan/rhi/vulkan_device.rs";

/// Per-line escape hatch for a deliberate, reviewed raw call.
const ALLOW_LINE_PRAGMA: &str = "streamlib:allow-raw-device-wait-idle";

#[derive(Debug)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
}

pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub files_scanned: usize,
}

pub fn run(project_root: &Path) -> Result<()> {
    let report = scan(project_root)?;
    for v in &report.violations {
        eprintln!(
            "{}:{}: raw `device_wait_idle()` bypasses HostVulkanDevice::wait_idle \
             (which holds the per-queue mutexes the Vulkan spec requires). Route \
             through the helper.\n    {}",
            v.path.display(),
            v.line_no,
            v.line_text.trim_end(),
        );
    }
    if report.violations.is_empty() {
        println!(
            "check-device-wait-idle: {} engine file(s) scanned, no raw \
             device_wait_idle outside HostVulkanDevice::wait_idle",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "check-device-wait-idle: {} raw device_wait_idle call(s) bypass \
             HostVulkanDevice::wait_idle",
            report.violations.len(),
        ))
    }
}

pub fn scan(project_root: &Path) -> Result<CheckReport> {
    let mut violations = Vec::new();
    let mut files_scanned = 0usize;
    let src = project_root.join("libs/streamlib-engine/src");
    if !src.exists() {
        return Ok(CheckReport {
            violations,
            files_scanned,
        });
    }
    for entry in WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let rel = path.strip_prefix(&src).unwrap_or(path);
        if rel.to_string_lossy().replace('\\', "/") == ALLOW_FILE {
            continue;
        }
        files_scanned += 1;
        scan_file(path, &mut violations)?;
    }
    Ok(CheckReport {
        violations,
        files_scanned,
    })
}

fn scan_file(path: &Path, violations: &mut Vec<Violation>) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if is_test_file(path, &content) {
        return Ok(());
    }
    let mut test_mod_depth: i32 = 0;
    let mut brace_balance_at_test_mod: i32 = 0;
    let mut running_balance: i32 = 0;
    let mut pending_test_mod = false;
    for line in content.lines() {
        // Track entry/exit of an inline `#[cfg(test)] mod … { … }` so raw waits
        // in unit-test helpers don't trip the lint.
        if line.contains("#[cfg(test)]") {
            pending_test_mod = true;
        }
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        if pending_test_mod && line.contains("mod ") && opens > 0 {
            test_mod_depth += 1;
            brace_balance_at_test_mod = running_balance;
            pending_test_mod = false;
        }
        running_balance += opens - closes;
        if test_mod_depth > 0 && running_balance <= brace_balance_at_test_mod {
            test_mod_depth = 0;
        }
        let _ = (opens, closes);
    }

    // Second pass for reporting (the brace pass above only gates whole files
    // via inline test mods; for line reporting we re-scan and skip lines that
    // fall inside a tracked test module).
    let mut depth: i32 = 0;
    let mut balance: i32 = 0;
    let mut anchor: i32 = 0;
    let mut pending = false;
    for (idx, line) in content.lines().enumerate() {
        if line.contains("#[cfg(test)]") {
            pending = true;
        }
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        let entering = pending && line.contains("mod ") && opens > 0;
        if entering {
            depth += 1;
            anchor = balance;
            pending = false;
        }
        let in_test = depth > 0;
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("//");
        if !in_test && !is_comment && !line.contains(ALLOW_LINE_PRAGMA) && line.contains(BANNED) {
            violations.push(Violation {
                path: path.to_path_buf(),
                line_no: idx + 1,
                line_text: line.to_string(),
            });
        }
        balance += opens - closes;
        if depth > 0 && balance <= anchor {
            depth = 0;
        }
    }
    Ok(())
}

/// File-level test exemption: a `#![cfg(test)]` / `#![cfg(all(test …))]` guard
/// or a `*_test.rs` / `*_tests.rs` name.
fn is_test_file(path: &Path, content: &str) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
            return true;
        }
    }
    content.contains("#![cfg(test") || content.contains("#![cfg(all(test")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn scan_one(rel: &str, content: &str) -> Vec<Violation> {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("libs/streamlib-engine/src");
        let path = src.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        scan(tmp.path()).unwrap().violations
    }

    #[test]
    fn rejects_raw_device_wait_idle_in_engine_code() {
        let v = scan_one(
            "vulkan/rhi/vulkan_compute_kernel.rs",
            "impl Drop for K { fn drop(&mut self) { unsafe { let _ = self.device.device_wait_idle(); } } }\n",
        );
        assert_eq!(v.len(), 1, "raw device_wait_idle must be flagged: {:?}", v);
    }

    #[test]
    fn accepts_the_helper_file() {
        // The helper's own file is the single allowed raw call site.
        let v = scan_one(
            "vulkan/rhi/vulkan_device.rs",
            "pub fn wait_idle(&self) { let _ = self.device.device_wait_idle(); }\n",
        );
        assert!(v.is_empty(), "vulkan_device.rs is exempt: {:?}", v);
    }

    #[test]
    fn accepts_routed_through_helper() {
        let v = scan_one(
            "vulkan/rhi/vulkan_graphics_kernel.rs",
            "impl Drop for K { fn drop(&mut self) { let _ = self.vulkan_device.wait_idle(); } }\n",
        );
        assert!(v.is_empty(), "wait_idle() helper call is fine: {:?}", v);
    }

    #[test]
    fn accepts_allow_line_pragma() {
        let v = scan_one(
            "vulkan/video/special.rs",
            "let _ = dev.device_wait_idle(); // streamlib:allow-raw-device-wait-idle\n",
        );
        assert!(v.is_empty(), "allow-line pragma exempts: {:?}", v);
    }

    #[test]
    fn skips_comment_line() {
        let v = scan_one(
            "vulkan/rhi/k.rs",
            "// historically this called self.device.device_wait_idle()\nfn f() {}\n",
        );
        assert!(v.is_empty(), "commented mention is not a call: {:?}", v);
    }

    #[test]
    fn skips_file_level_cfg_test() {
        let v = scan_one(
            "vulkan/rhi/repro_test.rs",
            "#![cfg(all(test, target_os = \"linux\"))]\nfn t() { let _ = d.device_wait_idle(); }\n",
        );
        assert!(v.is_empty(), "test-only file is exempt: {:?}", v);
    }

    #[test]
    fn skips_inline_cfg_test_module() {
        let v = scan_one(
            "vulkan/rhi/k.rs",
            "fn real() {}\n#[cfg(test)]\nmod tests {\n  fn t() { let _ = d.device_wait_idle(); }\n}\n",
        );
        assert!(v.is_empty(), "inline #[cfg(test)] mod is exempt: {:?}", v);
    }

    #[test]
    fn flags_real_code_even_with_a_test_module_present() {
        let v = scan_one(
            "vulkan/rhi/k.rs",
            "fn real() { let _ = self.device.device_wait_idle(); }\n#[cfg(test)]\nmod tests {\n  fn t() {}\n}\n",
        );
        assert_eq!(v.len(), 1, "non-test raw call must still flag: {:?}", v);
    }
}
