// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Boundary-grep CI check — Layer 6 of the regression-prevention defense for
//! the Vulkan RHI capability split (see `docs/architecture/subprocess-rhi-parity.md`).
//!
//! Four checks, each over the workspace, allowlists driven by data:
//!
//! 1. **No `ash`** — fully replaced by `vulkanalia` (per #252). Any new `ash`
//!    import or Cargo dep is a regression.
//! 2. **`use vulkanalia` confined to RHI / consumer-rhi / adapter / codec
//!    crates** — anyone reaching for raw `vulkanalia` outside those allowlisted
//!    paths is breaking the RHI boundary.
//! 3. **Cdylibs and adapter crates depend on `streamlib-consumer-rhi`, NOT
//!    the full `streamlib` crate** in runtime deps — `streamlib` may appear
//!    only under `[dev-dependencies]` (or `[target.*.dev-dependencies]`).
//!    The `FullAccess` capability boundary is type-system enforced by the
//!    cdylib's dep graph excluding `streamlib`.
//! 4. **Raw privileged Vulkan calls outside the RHI** — `vkAllocateMemory`,
//!    `vkGetMemoryFdKHR`, `vkCreateComputePipelines` etc. The privileged set
//!    lives in the RHI, period.
//!
//! The check is grep-shaped on purpose: sub-second on a clean runner, no
//! Cargo build needed. Allowlists are explicit and carry per-entry rationale
//! so future contributors can decide whether to extend the allowlist or
//! refactor the offending file.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
    pub matched_pattern: String,
    pub check: &'static str,
    pub rationale: &'static str,
}

#[derive(Debug)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub files_scanned: usize,
}

pub fn run(project_root: &Path) -> Result<()> {
    let report = scan_all(project_root)?;
    for v in &report.violations {
        eprintln!(
            "{}:{}: [{}] {} — {}\n    {}",
            v.path.display(),
            v.line_no,
            v.check,
            v.matched_pattern,
            v.rationale,
            v.line_text.trim_end(),
        );
    }
    if report.violations.is_empty() {
        println!(
            "check-boundaries: {} file(s) scanned, no violations",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "check-boundaries: {} violation(s) across {} file(s) scanned — see docs/architecture/subprocess-rhi-parity.md",
            report.violations.len(),
            report.files_scanned,
        ))
    }
}

pub fn scan_all(project_root: &Path) -> Result<CheckReport> {
    let mut violations = Vec::new();
    let mut files_scanned = 0usize;
    check_no_ash(project_root, &mut violations, &mut files_scanned)?;
    check_vulkanalia_confined(project_root, &mut violations, &mut files_scanned)?;
    check_cdylib_and_adapter_runtime_deps(project_root, &mut violations, &mut files_scanned)?;
    check_privileged_vk_calls(project_root, &mut violations, &mut files_scanned)?;
    check_vulkanalia_uses_workspace_fork(project_root, &mut violations, &mut files_scanned)?;
    Ok(CheckReport { violations, files_scanned })
}

// ---------------------------------------------------------------------------
// Allowlist machinery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum AllowKind {
    /// `path` is a directory prefix (relative to workspace root, `/`-separated).
    /// Anything beneath this prefix is allowed.
    PathPrefix,
    /// `path` is an exact file (relative to workspace root).
    ExactFile,
    /// Any path component equals `path` (e.g. "tests" matches `**/tests/**`).
    PathSegment,
}

#[derive(Debug, Clone, Copy)]
pub struct AllowEntry {
    pub path: &'static str,
    pub kind: AllowKind,
    pub rationale: &'static str,
}

fn matches_allow(rel_path: &Path, allow: &[AllowEntry]) -> bool {
    let rel_str = rel_path.to_string_lossy().replace('\\', "/");
    for entry in allow {
        match entry.kind {
            AllowKind::PathPrefix => {
                if rel_str.starts_with(entry.path) {
                    return true;
                }
            }
            AllowKind::ExactFile => {
                if rel_str == entry.path {
                    return true;
                }
            }
            AllowKind::PathSegment => {
                for component in rel_path.components() {
                    if let std::path::Component::Normal(seg) = component {
                        if seg.to_string_lossy() == entry.path {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Workspace traversal
// ---------------------------------------------------------------------------

/// Top-level directories scanned by the `.rs` and `Cargo.toml` checks. Skips
/// `target/`, `.git/`, vendored node_modules, etc. by construction. `xtask`
/// is intentionally excluded — it is build tooling with no Vulkan deps, and
/// the fixture-test strings in this very file would otherwise self-flag.
const SCAN_ROOTS: &[&str] = &["libs", "examples"];

fn walk_rs(project_root: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    SCAN_ROOTS
        .iter()
        .map(move |r| project_root.join(r))
        .filter(|p| p.exists())
        .flat_map(|root| {
            WalkDir::new(root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter(|e| {
                    e.path().extension().and_then(|x| x.to_str()) == Some("rs")
                })
                .map(|e| e.into_path())
                .collect::<Vec<_>>()
        })
}

fn walk_cargo_toml(project_root: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    SCAN_ROOTS
        .iter()
        .map(move |r| project_root.join(r))
        .filter(|p| p.exists())
        .flat_map(|root| {
            WalkDir::new(root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter(|e| {
                    e.path().file_name().and_then(|x| x.to_str()) == Some("Cargo.toml")
                })
                .map(|e| e.into_path())
                .collect::<Vec<_>>()
        })
        .chain(std::iter::once(project_root.join("Cargo.toml")).filter(|p| p.exists()))
}

fn rel_to_root<'a>(path: &'a Path, project_root: &Path) -> &'a Path {
    path.strip_prefix(project_root).unwrap_or(path)
}

// ---------------------------------------------------------------------------
// Check 1 — no `ash`
// ---------------------------------------------------------------------------

const CHECK_NO_ASH: &str = "no-ash";

const ASH_RATIONALE: &str =
    "ash is fully replaced by vulkanalia (#252); reintroducing it splits the workspace's GPU API surface";

fn check_no_ash(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ash::") || trimmed.starts_with("extern crate ash;") {
                violations.push(Violation {
                    path: rel_to_root(&path, project_root).to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: trimmed.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
                    check: CHECK_NO_ASH,
                    rationale: ASH_RATIONALE,
                });
            }
        }
    }
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            if dep_name == "ash" {
                violations.push(Violation {
                    path: rel_to_root(&path, project_root).to_path_buf(),
                    line_no,
                    line_text: format!("[{}] ash = ...", section),
                    matched_pattern: format!("ash dep in [{}]", section),
                    check: CHECK_NO_ASH,
                    rationale: ASH_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 2 — `use vulkanalia` confined to RHI / consumer-rhi / adapter / codec
// ---------------------------------------------------------------------------

const CHECK_VULKANALIA: &str = "vulkanalia-only-in-rhi";

const VULKANALIA_RATIONALE: &str =
    "raw vulkanalia must stay inside the RHI / consumer-rhi / adapter crates and a small set of documented-exception files";

const VULKANALIA_ALLOWLIST: &[AllowEntry] = &[
    // Core RHI host side — owns every privileged Vulkan primitive.
    AllowEntry {
        path: "libs/streamlib/src/vulkan/",
        kind: AllowKind::PathPrefix,
        rationale: "host RHI lives here",
    },
    // Consumer-side carve-out — DMA-BUF FD import + bind + map only.
    AllowEntry {
        path: "libs/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-rhi is the import-side carve-out (#560)",
    },
    // Every surface adapter crate (vulkan, opengl, cpu-readback, ...) and
    // their dedicated test-helper crates.
    AllowEntry {
        path: "libs/streamlib-adapter-",
        kind: AllowKind::PathPrefix,
        rationale: "adapter crates ride consumer-rhi for import + bind",
    },
    // Vulkan video codec — sibling of the RHI; predates the boundary
    // and implements vkVideo extensions directly. Refactor-to-RHI is
    // tracked separately under the Vulkan Video RHI Coupling milestone.
    AllowEntry {
        path: "libs/vulkan-video/",
        kind: AllowKind::PathPrefix,
        rationale: "codec layer; refactor-to-RHI tracked under Vulkan Video RHI Coupling milestone",
    },
    // Display processor — CLAUDE.md-documented exception: needs raw
    // vulkanalia for the swapchain and rendering pipeline (mirrors how
    // Metal rendering is platform-specific on macOS).
    AllowEntry {
        path: "libs/streamlib/src/linux/processors/display.rs",
        kind: AllowKind::ExactFile,
        rationale: "platform display: swapchain + rendering pipeline (CLAUDE.md exception)",
    },
    // Camera processor — historical use of cmd_pipeline_barrier per
    // docs/learnings/vulkanalia-empty-slice-cast.md.
    AllowEntry {
        path: "libs/streamlib/src/linux/processors/camera.rs",
        kind: AllowKind::ExactFile,
        rationale: "cmd_pipeline_barrier for layout transitions (vulkanalia-empty-slice-cast learning)",
    },
    // GpuContext is the wrapper layer between processors and the RHI;
    // touches a small set of Vulkan handles to wire pools.
    AllowEntry {
        path: "libs/streamlib/src/core/context/gpu_context.rs",
        kind: AllowKind::ExactFile,
        rationale: "RHI wrapper layer; bridges processors to the RHI",
    },
    // Subprocess cdylibs are NOT allowlisted — post-#550/#553/#572
    // they no longer use `vulkanalia` directly; consumer-side device
    // construction lives in `streamlib-consumer-rhi`, layout enums
    // come typed from the cpu-readback adapter, and any reintroduction
    // of `use vulkanalia` or a `vulkanalia` Cargo dep in the cdylibs
    // is a regression of the FullAccess capability boundary.
    //
    // Polyglot example/scenario binaries are NOT allowlisted post-#583
    // — every host-side per-frame readback rides the host RHI's
    // `VulkanTextureReadback` primitive; any reintroduction of raw
    // `vulkanalia` in those crates means an example bypassed the RHI.
    //
    // Test code in any crate is allowed to use vulkanalia directly to
    // bring up real devices for end-to-end validation.
    AllowEntry {
        path: "tests",
        kind: AllowKind::PathSegment,
        rationale: "tests bring up real Vulkan devices for end-to-end validation",
    },
];

fn check_vulkanalia_confined(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, VULKANALIA_ALLOWLIST) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            // Only flag actual `use vulkanalia` import statements; not
            // doc comments or strings that happen to mention the crate.
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.starts_with("use vulkanalia::") || trimmed == "use vulkanalia;" {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: "use vulkanalia".to_string(),
                    check: CHECK_VULKANALIA,
                    rationale: VULKANALIA_RATIONALE,
                });
            }
        }
    }
    // Defense in depth: a non-allowlisted crate could add `vulkanalia` to
    // its Cargo.toml without a `use` statement (qualified paths,
    // re-exports, build.rs only) and slip past the import-statement scan.
    // The Cargo.toml dep scan catches that. The allowlist is keyed on
    // crate roots (one entry per crate that owns at least one
    // vulkanalia-allowlisted source file). Matching is against the full
    // Cargo.toml file path so trailing-slash directory boundaries hit
    // (`libs/streamlib/Cargo.toml` matches `libs/streamlib/`, but
    // `libs/streamlib-runtime/Cargo.toml` does not).
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, VULKANALIA_CARGO_DEP_ALLOWLIST) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            if dep_name == "vulkanalia" {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no,
                    line_text: format!("[{}] vulkanalia = ...", section),
                    matched_pattern: format!("vulkanalia dep in [{}]", section),
                    check: CHECK_VULKANALIA,
                    rationale: VULKANALIA_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

/// Crate roots permitted to declare `vulkanalia` as a Cargo dependency. A
/// crate qualifies if at least one file under it appears in the
/// `.rs`-level vulkanalia allowlist (`VULKANALIA_ALLOWLIST`); declaring
/// the dep elsewhere is a regression.
const VULKANALIA_CARGO_DEP_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "libs/streamlib/",
        kind: AllowKind::PathPrefix,
        rationale: "host crate: src/vulkan/ owns the RHI; processors/display.rs and processors/camera.rs are documented exceptions",
    },
    AllowEntry {
        path: "libs/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-side carve-out (#560)",
    },
    AllowEntry {
        path: "libs/streamlib-adapter-",
        kind: AllowKind::PathPrefix,
        rationale: "adapter crates ride consumer-rhi",
    },
    AllowEntry {
        path: "libs/vulkan-video/",
        kind: AllowKind::PathPrefix,
        rationale: "codec layer; refactor-to-RHI tracked under Vulkan Video RHI Coupling milestone",
    },
    // Subprocess cdylibs are intentionally NOT allowlisted post-#572 —
    // their `Cargo.toml`s no longer declare `vulkanalia`, and any
    // reintroduction is a capability-boundary regression.
    //
    // Polyglot example/scenario binaries are intentionally NOT
    // allowlisted post-#583 — host-side readback rides
    // `VulkanTextureReadback` via the streamlib host RHI.
];

// ---------------------------------------------------------------------------
// Check 3 — cdylibs and adapter crates depend on consumer-rhi, NOT streamlib
// ---------------------------------------------------------------------------

const CHECK_CDYLIB_DEPS: &str = "no-streamlib-in-runtime-deps";

const CDYLIB_DEP_RATIONALE: &str = "cdylibs and adapter crates must depend on streamlib-consumer-rhi (carve-out), not the full streamlib crate, so the FullAccess capability boundary is type-system enforced";

/// Crates whose runtime dep graph must not include `streamlib`. `streamlib`
/// is allowed only in `[dev-dependencies]` (or `[target.*.dev-dependencies]`).
const NO_STREAMLIB_RUNTIME_DEP: &[&str] = &[
    "libs/streamlib-python-native/Cargo.toml",
    "libs/streamlib-deno-native/Cargo.toml",
    "libs/streamlib-adapter-vulkan/Cargo.toml",
    "libs/streamlib-adapter-opengl/Cargo.toml",
    "libs/streamlib-adapter-cpu-readback/Cargo.toml",
    "libs/streamlib-adapter-skia/Cargo.toml",
    "libs/streamlib-adapter-cuda/Cargo.toml",
];

fn check_cdylib_and_adapter_runtime_deps(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for rel_path in NO_STREAMLIB_RUNTIME_DEP {
        let path = project_root.join(rel_path);
        if !path.exists() {
            // Allowlisted crate may have been renamed/deleted; skip
            // silently — this is enforcement, not discovery.
            continue;
        }
        *files_scanned += 1;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = toml::from_str(&content)
            .with_context(|| format!("parse {}", path.display()))?;
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            if dep_name == "streamlib" && !section_is_dev_only(&section) {
                violations.push(Violation {
                    path: PathBuf::from(rel_path),
                    line_no,
                    line_text: format!("[{}] streamlib = ...", section),
                    matched_pattern: format!("streamlib in [{}]", section),
                    check: CHECK_CDYLIB_DEPS,
                    rationale: CDYLIB_DEP_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

fn section_is_dev_only(section: &str) -> bool {
    section == "dev-dependencies"
        || section.starts_with("target.") && section.ends_with(".dev-dependencies")
}

// ---------------------------------------------------------------------------
// Check 4 — raw privileged Vulkan calls outside the RHI
// ---------------------------------------------------------------------------

const CHECK_PRIVILEGED_VK: &str = "no-privileged-vk-outside-rhi";

const PRIVILEGED_VK_RATIONALE: &str =
    "vkAllocateMemory / vkGetMemoryFdKHR / vkCreateComputePipelines are privileged primitives owned by the host RHI";

/// Privileged vulkanalia method names (snake_case form). A bare `\.<name>\(`
/// match is enough — these are unambiguous when called on a Device handle.
const PRIVILEGED_METHODS: &[&str] = &[
    "allocate_memory",
    "get_memory_fd_khr",
    "create_compute_pipelines",
];

const PRIVILEGED_VK_ALLOWLIST: &[AllowEntry] = &[
    // Host RHI — defines and owns the privileged calls.
    AllowEntry {
        path: "libs/streamlib/src/vulkan/",
        kind: AllowKind::PathPrefix,
        rationale: "host RHI owns privileged primitives",
    },
    // Consumer-rhi calls allocate_memory ONLY with VkImportMemoryFdInfoKHR
    // chained via push_next — that is the carve-out, not raw allocation.
    // The pattern can't be distinguished syntactically from raw allocation
    // without an AST walk; allowlist the crate and rely on code review +
    // docs/architecture/subprocess-rhi-parity.md to keep it honest.
    AllowEntry {
        path: "libs/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-rhi import-side carve-out chains ImportMemoryFdInfoKHR",
    },
    // Codec layer — predates the RHI boundary; refactor tracked under
    // Vulkan Video RHI Coupling milestone.
    AllowEntry {
        path: "libs/vulkan-video/",
        kind: AllowKind::PathPrefix,
        rationale: "codec layer; refactor-to-RHI tracked under Vulkan Video RHI Coupling milestone",
    },
    // Camera processor compiles a compute pipeline locally (NV12 → BGRA).
    // Tracked separately for migration to VulkanComputeKernel.
    AllowEntry {
        path: "libs/streamlib/src/linux/processors/camera.rs",
        kind: AllowKind::ExactFile,
        rationale: "compute pipeline for NV12→BGRA; migration to VulkanComputeKernel tracked separately",
    },
    // Subprocess cdylibs are NOT allowlisted post-#572 — their entire
    // privileged-vk surface lives in `streamlib-consumer-rhi`'s
    // import-side carve-out and `streamlib-adapter-*`. A privileged
    // call appearing inside a cdylib means a regression of the
    // FullAccess capability boundary.
    //
    // Polyglot example/scenario binaries are NOT allowlisted post-#583
    // — host-side readback rides `VulkanTextureReadback` via the
    // streamlib host RHI; raw privileged-vk inside them indicates the
    // example bypassed the RHI.
    //
    // Tests bring up real devices for end-to-end validation.
    AllowEntry {
        path: "tests",
        kind: AllowKind::PathSegment,
        rationale: "tests bring up real Vulkan devices for end-to-end validation",
    },
    // Adapter test-helper bin lives outside the test/ tree but is
    // test-only by purpose.
    AllowEntry {
        path: "libs/streamlib-adapter-vulkan-helpers/",
        kind: AllowKind::PathPrefix,
        rationale: "test-helper crate isolated so streamlib doesn't leak into adapter-vulkan runtime deps",
    },
];

fn check_privileged_vk_calls(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, PRIVILEGED_VK_ALLOWLIST) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for method in PRIVILEGED_METHODS {
                let needle = format!(".{}(", method);
                if line.contains(&needle) {
                    violations.push(Violation {
                        path: rel.to_path_buf(),
                        line_no: idx + 1,
                        line_text: line.to_string(),
                        matched_pattern: format!(".{}(", method),
                        check: CHECK_PRIVILEGED_VK,
                        rationale: PRIVILEGED_VK_RATIONALE,
                    });
                    break;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 5 — every member crate's vulkanalia* dep is workspace-inherited
// ---------------------------------------------------------------------------

const CHECK_VULKANALIA_FORK: &str = "vulkanalia-uses-workspace-fork";

const VULKANALIA_FORK_RATIONALE: &str = "all vulkanalia / vulkanalia-sys / vulkanalia-vma deps must inherit from [workspace.dependencies] (the tatolab fork) — direct version specifications can silently pull crates.io upstream and lose the VMA 3.3.0 patch";

fn check_vulkanalia_uses_workspace_fork(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, dep_value, line_no) in iter_dep_entries_with_values(&parsed, &content) {
            if !is_vulkanalia_dep(&dep_name) {
                continue;
            }
            if dep_is_workspace_inherited(&dep_value) {
                continue;
            }
            violations.push(Violation {
                path: rel.to_path_buf(),
                line_no,
                line_text: format!("[{}] {} = ... (not `workspace = true`)", section, dep_name),
                matched_pattern: format!("{} bypasses workspace fork in [{}]", dep_name, section),
                check: CHECK_VULKANALIA_FORK,
                rationale: VULKANALIA_FORK_RATIONALE,
            });
        }
    }
    Ok(())
}

fn is_vulkanalia_dep(name: &str) -> bool {
    name == "vulkanalia" || name == "vulkanalia-sys" || name == "vulkanalia-vma"
}

fn dep_is_workspace_inherited(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|t| t.get("workspace"))
        .and_then(|w| w.as_bool())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Cargo.toml dep iteration
// ---------------------------------------------------------------------------

/// Yields `(section, dep_name, line_no)` for every dependency entry across
/// all dependency-shaped sections in a Cargo.toml. Sections covered:
/// - `[dependencies]`
/// - `[dev-dependencies]`
/// - `[build-dependencies]`
/// - `[target.<cfg>.dependencies]`
/// - `[target.<cfg>.dev-dependencies]`
/// - `[target.<cfg>.build-dependencies]`
fn iter_dep_entries(
    toml_value: &toml::Value,
    raw_text: &str,
) -> Vec<(String, String, usize)> {
    iter_dep_entries_with_values(toml_value, raw_text)
        .into_iter()
        .map(|(section, name, _value, line)| (section, name, line))
        .collect()
}

/// Same as [`iter_dep_entries`] but also includes the dep value (as a
/// `toml::Value`), for checks that need to inspect whether a dep is
/// `workspace = true`, has a `git = "..."` URL, etc.
fn iter_dep_entries_with_values(
    toml_value: &toml::Value,
    raw_text: &str,
) -> Vec<(String, String, toml::Value, usize)> {
    let mut out = Vec::new();
    let Some(table) = toml_value.as_table() else {
        return out;
    };
    for section_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = table.get(section_name).and_then(|v| v.as_table()) {
            for (dep_name, value) in deps {
                let line = find_dep_line(raw_text, section_name, dep_name);
                out.push((section_name.to_string(), dep_name.clone(), value.clone(), line));
            }
        }
    }
    if let Some(target) = table.get("target").and_then(|v| v.as_table()) {
        for (cfg_key, cfg_val) in target {
            let Some(cfg_tbl) = cfg_val.as_table() else {
                continue;
            };
            for sub in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(deps) = cfg_tbl.get(sub).and_then(|v| v.as_table()) {
                    let qualified = format!("target.{}.{}", cfg_key, sub);
                    for (dep_name, value) in deps {
                        let line = find_dep_line(raw_text, &qualified, dep_name);
                        out.push((qualified.clone(), dep_name.clone(), value.clone(), line));
                    }
                }
            }
        }
    }
    out
}

/// Best-effort: find the line number where `<dep_name> =` appears under the
/// header for `section`. Falls back to 0 if not found (still counts as a
/// violation, just with no precise location).
fn find_dep_line(raw_text: &str, section: &str, dep_name: &str) -> usize {
    let header = format!("[{}]", section);
    let mut in_section = false;
    let dep_prefix_eq = format!("{} =", dep_name);
    let dep_prefix_space_eq = format!("{}  =", dep_name);
    let dep_prefix_dot = format!("{}.", dep_name);
    for (idx, line) in raw_text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            continue;
        }
        if in_section {
            let lt = line.trim_start();
            if lt.starts_with(&dep_prefix_eq)
                || lt.starts_with(&dep_prefix_space_eq)
                || lt.starts_with(&dep_prefix_dot)
            {
                return idx + 1;
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    fn empty_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Top-level Cargo.toml so walk_cargo_toml has something to find at
        // root if we choose to add deps there.
        write_fixture(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = []\n",
        );
        dir
    }

    // ----- Check 1: no ash -----

    #[test]
    fn rejects_use_ash_in_rust_file() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/lib.rs",
            "use ash::vk;\nfn main() {}\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_NO_ASH),
            "expected no-ash violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_extern_crate_ash() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/lib.rs",
            "extern crate ash;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(report.violations.iter().any(|v| v.check == CHECK_NO_ASH));
    }

    #[test]
    fn rejects_ash_in_cargo_toml_runtime_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/Cargo.toml",
            "[package]\nname = \"streamlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nash = \"0.38\"\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_NO_ASH),
            "expected ash Cargo dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn ignores_ahash_lookalike() {
        let dir = empty_workspace();
        // Lookalike substring should NOT trip the check.
        write_fixture(
            dir.path(),
            "libs/streamlib/src/lib.rs",
            "use ahash::AHashMap;\nlet h: Hash = todo!();\n",
        );
        // Cargo.toml with ahash dep.
        write_fixture(
            dir.path(),
            "libs/streamlib/Cargo.toml",
            "[package]\nname = \"streamlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nahash = \"0.8\"\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let no_ash: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_NO_ASH).collect();
        assert!(no_ash.is_empty(), "ahash should not match ash: {:?}", no_ash);
    }

    // ----- Check 2: vulkanalia confined -----

    #[test]
    fn rejects_use_vulkanalia_outside_allowlist() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/core/some_unrelated.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_use_vulkanalia_in_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/vulkan/rhi/example.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA).collect();
        assert!(vk.is_empty(), "vulkanalia in RHI should pass: {:?}", vk);
    }

    #[test]
    fn allows_use_vulkanalia_in_consumer_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-consumer-rhi/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA).collect();
        assert!(vk.is_empty(), "vulkanalia in consumer-rhi should pass: {:?}", vk);
    }

    #[test]
    fn allows_use_vulkanalia_in_adapter_crate() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA).collect();
        assert!(vk.is_empty(), "vulkanalia in adapter crate should pass: {:?}", vk);
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_outside_allowlist() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-runtime/Cargo.toml",
            r#"[package]
name = "streamlib-runtime"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.20"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_vulkanalia_cargo_dep_in_adapter_crate() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.20"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA).collect();
        assert!(vk.is_empty(), "vulkanalia dep in adapter crate should pass: {:?}", vk);
    }

    // ----- Check 5: vulkanalia uses workspace fork -----

    #[test]
    fn rejects_direct_vulkanalia_version_in_member() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.35"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_vulkanalia_vma_version_in_member() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/vulkan-video/Cargo.toml",
            r#"[package]
name = "vulkan-video"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia-vma = "0.4"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation for vulkanalia-vma, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_vulkanalia_git_in_member() {
        let dir = empty_workspace();
        // Even a git URL in a member crate is a violation: it bypasses the
        // single source of truth at workspace level.
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-opengl/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-opengl"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { git = "https://github.com/KhronosGroup/Vulkan-Headers" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation for git dep, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_workspace_inline_vulkanalia_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let fork: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA_FORK).collect();
        assert!(fork.is_empty(), "{{ workspace = true }} should pass: {:?}", fork);
    }

    #[test]
    fn allows_workspace_dotted_vulkanalia_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia.workspace = true
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let fork: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA_FORK).collect();
        assert!(fork.is_empty(), "dotted-key form should pass: {:?}", fork);
    }

    #[test]
    fn allows_use_vulkanalia_in_tests() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-opengl/tests/common.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_VULKANALIA).collect();
        assert!(vk.is_empty(), "vulkanalia in tests/ should pass: {:?}", vk);
    }

    // ----- Check 3: cdylib + adapter runtime deps -----

    #[test]
    fn rejects_streamlib_in_cdylib_runtime_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-python-native/Cargo.toml",
            r#"[package]
name = "streamlib-python-native"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { path = "../streamlib" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_CDYLIB_DEPS),
            "expected cdylib runtime-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_streamlib_in_adapter_runtime_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dependencies]
streamlib = { path = "../streamlib" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_CDYLIB_DEPS),
            "expected adapter target runtime-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_streamlib_in_cdylib_dev_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-cpu-readback/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-cpu-readback"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dev-dependencies]
streamlib = { path = "../streamlib" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let cdy: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_CDYLIB_DEPS).collect();
        assert!(cdy.is_empty(), "streamlib in dev-deps should pass: {:?}", cdy);
    }

    // ----- Check 4: privileged vk calls -----

    #[test]
    fn rejects_allocate_memory_outside_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/core/some_other.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_create_compute_pipelines_outside_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-adapter-vulkan/src/state.rs",
            "fn f() { unsafe { dev.create_compute_pipelines(cache, &[info], None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation in adapter crate, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_privileged_call_in_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/vulkan/rhi/vulkan_device.rs",
            "fn f() { unsafe { self.device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let pv: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_PRIVILEGED_VK).collect();
        assert!(pv.is_empty(), "privileged call in RHI should pass: {:?}", pv);
    }

    #[test]
    fn allows_privileged_call_in_consumer_rhi_for_carve_out() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-consumer-rhi/src/consumer_vulkan_device.rs",
            "fn f() { unsafe { self.device.allocate_memory(&alloc_info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let pv: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_PRIVILEGED_VK).collect();
        assert!(pv.is_empty(), "consumer-rhi import-side carve-out should pass: {:?}", pv);
    }

    // ----- Cdylib tightening (#572) -----
    //
    // Post-#550/#553, neither cdylib needs raw `vulkanalia` access. These
    // tests prove the path-prefix exemption is gone — any reintroduction
    // of `use vulkanalia`, a `vulkanalia` Cargo dep, or a privileged-vk
    // call inside `libs/streamlib-{python,deno}-native/` trips a
    // violation rather than slipping through.

    #[test]
    fn rejects_use_vulkanalia_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-python-native/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_use_vulkanalia_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-deno-native/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation in deno-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-python-native/Cargo.toml",
            r#"[package]
name = "streamlib-python-native"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dependencies]
vulkanalia = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo-dep violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-deno-native/Cargo.toml",
            r#"[package]
name = "streamlib-deno-native"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dependencies]
vulkanalia = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo-dep violation in deno-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_allocate_memory_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-python-native/src/some_module.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_allocate_memory_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib-deno-native/src/some_module.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation in deno-native cdylib, got {:?}",
            report.violations,
        );
    }

    // ----- Comment-line skip -----

    #[test]
    fn skips_commented_use_ash() {
        // After `trim_start()` the line begins with `//`, so
        // `starts_with("use ash::")` is false and the check correctly
        // ignores commented-out imports. Locks the behavior so a future
        // refactor to `line.contains("use ash::")` would fail loudly.
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "libs/streamlib/src/lib.rs",
            "// use ash::vk; — kept for historical reference only\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let no_ash: Vec<_> = report.violations.iter().filter(|v| v.check == CHECK_NO_ASH).collect();
        assert!(no_ash.is_empty(), "commented import should not match: {:?}", no_ash);
    }
}
