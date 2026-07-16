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
use streamlib_pack::NormalBuildDepGraph;
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
    // Check 12 — transitive trunk-set -> engine walk (cargo metadata based;
    // layered on the direct manifest check 11 inside scan_all).
    let engine_chains = run_trunk_transitive_check(project_root)?;
    for chain in &engine_chains {
        eprintln!(
            "[{}] trunk crate `{}` transitively depends on `{}`: {}\n    {}",
            CHECK_TRUNK_NO_ENGINE_DEP,
            chain.trunk,
            TRUNK_ENGINE_CRATE_NAME,
            chain.display_chain(),
            TRUNK_NO_ENGINE_DEP_RATIONALE,
        );
    }
    let total_violations = report.violations.len() + engine_chains.len();
    if total_violations == 0 {
        println!(
            "check-boundaries: {} file(s) scanned, no violations",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "check-boundaries: {} violation(s) ({} grep + {} transitive trunk->engine chain(s)) across {} file(s) scanned — see docs/architecture/subprocess-rhi-parity.md",
            total_violations,
            report.violations.len(),
            engine_chains.len(),
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
    check_streamlib_engine_confined(project_root, &mut violations, &mut files_scanned)?;
    check_streamlib_top_level_shortcut(project_root, &mut violations, &mut files_scanned)?;
    check_packages_facade_runtime_dep(project_root, &mut violations, &mut files_scanned)?;
    check_packages_engine_reach(project_root, &mut violations, &mut files_scanned)?;
    check_examples_cdylib_facade_dep(project_root, &mut violations, &mut files_scanned)?;
    check_trunk_set_no_engine_dep(project_root, &mut violations, &mut files_scanned)?;
    Ok(CheckReport {
        violations,
        files_scanned,
    })
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
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters", "tools", "vendor", "examples", "packages"];

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
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
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
                .filter(|e| e.path().file_name().and_then(|x| x.to_str()) == Some("Cargo.toml"))
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

const ASH_RATIONALE: &str = "ash is fully replaced by vulkanalia (#252); reintroducing it splits the workspace's GPU API surface";

fn check_no_ash(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ash::") || trimmed.starts_with("extern crate ash;") {
                violations.push(Violation {
                    path: rel_to_root(&path, project_root).to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: trimmed
                        .split_whitespace()
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" "),
                    check: CHECK_NO_ASH,
                    rationale: ASH_RATIONALE,
                });
            }
        }
    }
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
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

const VULKANALIA_RATIONALE: &str = "raw vulkanalia must stay inside the RHI / consumer-rhi / adapter crates and a small set of documented-exception files";

const VULKANALIA_ALLOWLIST: &[AllowEntry] = &[
    // Core RHI host side — owns every privileged Vulkan primitive.
    AllowEntry {
        path: "runtime/streamlib-engine/src/vulkan/",
        kind: AllowKind::PathPrefix,
        rationale: "host RHI lives here",
    },
    // Consumer-side carve-out — DMA-BUF FD import + bind + map only.
    AllowEntry {
        path: "runtime/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-rhi is the import-side carve-out (#560)",
    },
    // Every surface adapter crate (vulkan, opengl, cpu-readback, ...) and
    // their dedicated test-helper crates.
    AllowEntry {
        path: "adapters/streamlib-adapter-",
        kind: AllowKind::PathPrefix,
        rationale: "adapter crates ride consumer-rhi for import + bind",
    },
    // Vulkan video codec layer at `runtime/streamlib-engine/src/vulkan/video/`
    // is covered by the engine-vulkan PathPrefix entry above. The codec
    // layer was folded from the former `libs/vulkan-video` sibling crate
    // into the engine; it sits above `vulkan/rhi/` and migrates toward
    // engine-RHI-only Vulkan access as the RHI grows codec primitives.
    //
    // Display processor lives in `@tatolab/display` (#674) — the carve-out
    // rewrote it on `streamlib::sdk::engine::host_rhi::VulkanPresentTarget`,
    // retiring the prior CLAUDE.md exception for raw vulkanalia in the
    // engine's `linux/processors/display.rs`. No allowlist entry needed.
    //
    // GpuContext is the wrapper layer between processors and the RHI;
    // touches a small set of Vulkan handles to wire pools.
    AllowEntry {
        path: "runtime/streamlib-engine/src/core/context/gpu_context.rs",
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
    // The vendored fork crates themselves — vendored fork source; it IS the
    // Vulkan surface the RHI rides. Anchored to the three EXACT dirs
    // (trailing slash) so a future vendor/tatolab-vulkanalia-extras/ crate
    // does not silently inherit the exemption.
    AllowEntry {
        path: "vendor/tatolab-vulkanalia/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-sys/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-vma/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
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
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
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
    // (`runtime/streamlib-engine/Cargo.toml` matches `runtime/streamlib-engine/`, but
    // `runtime/streamlib-runtime/Cargo.toml` does not).
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, VULKANALIA_CARGO_DEP_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            // Both the workspace-renamed dep key (`vulkanalia`) and a direct
            // dep on the vendored crate names (`tatolab-vulkanalia*`) count —
            // either grants raw Vulkan access.
            if dep_name == "vulkanalia" || dep_name.starts_with("tatolab-vulkanalia") {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no,
                    line_text: format!("[{}] {} = ...", section, dep_name),
                    matched_pattern: format!("{} dep in [{}]", dep_name, section),
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
        path: "runtime/streamlib-engine/",
        kind: AllowKind::PathPrefix,
        rationale: "host crate: src/vulkan/ owns the RHI; processors/display.rs and processors/camera.rs are documented exceptions",
    },
    AllowEntry {
        path: "runtime/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-side carve-out (#560)",
    },
    AllowEntry {
        path: "adapters/streamlib-adapter-",
        kind: AllowKind::PathPrefix,
        rationale: "adapter crates ride consumer-rhi",
    },
    // Codec layer's Cargo deps are inherited from the engine — codec
    // moved into `runtime/streamlib-engine/src/vulkan/video/`, sharing the
    // engine's `vulkanalia.workspace = true` line.
    //
    // Subprocess cdylibs are intentionally NOT allowlisted post-#572 —
    // their `Cargo.toml`s no longer declare `vulkanalia`, and any
    // reintroduction is a capability-boundary regression.
    //
    // Polyglot example/scenario binaries are intentionally NOT
    // allowlisted post-#583 — host-side readback rides
    // `VulkanTextureReadback` via the streamlib host RHI.
    //
    // The vendored fork crates declare vulkanalia sibling deps by
    // construction — vendored fork source; it IS the Vulkan surface.
    // Anchored to the three EXACT dirs (trailing slash) so a future
    // vendor/tatolab-vulkanalia-extras/ crate does not silently inherit
    // the exemption.
    AllowEntry {
        path: "vendor/tatolab-vulkanalia/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-sys/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-vma/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — it IS the Vulkan surface",
    },
];

// ---------------------------------------------------------------------------
// Check 3 — cdylibs and adapter crates depend on consumer-rhi, NOT streamlib
// ---------------------------------------------------------------------------

const CHECK_CDYLIB_DEPS: &str = "no-streamlib-in-runtime-deps";

const CDYLIB_DEP_RATIONALE: &str = "cdylibs and adapter crates must depend on streamlib-consumer-rhi (carve-out), not the full streamlib crate, so the FullAccess capability boundary is type-system enforced";

/// Crates whose runtime dep graph must not include `streamlib`. `streamlib`
/// is allowed only in `[dev-dependencies]` (or `[target.*.dev-dependencies]`).
const NO_STREAMLIB_RUNTIME_DEP: &[&str] = &[
    "sdk/streamlib-python-native/Cargo.toml",
    "sdk/streamlib-deno-native/Cargo.toml",
    "adapters/streamlib-adapter-vulkan/Cargo.toml",
    "adapters/streamlib-adapter-opengl/Cargo.toml",
    "adapters/streamlib-adapter-cpu-readback/Cargo.toml",
    "adapters/streamlib-adapter-skia/Cargo.toml",
    "adapters/streamlib-adapter-cuda/Cargo.toml",
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
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value =
            toml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
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

const PRIVILEGED_VK_RATIONALE: &str = "vkAllocateMemory / vkGetMemoryFdKHR / vkCreateComputePipelines are privileged primitives owned by the host RHI";

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
        path: "runtime/streamlib-engine/src/vulkan/",
        kind: AllowKind::PathPrefix,
        rationale: "host RHI owns privileged primitives",
    },
    // Consumer-rhi calls allocate_memory ONLY with VkImportMemoryFdInfoKHR
    // chained via push_next — that is the carve-out, not raw allocation.
    // The pattern can't be distinguished syntactically from raw allocation
    // without an AST walk; allowlist the crate and rely on code review +
    // docs/architecture/subprocess-rhi-parity.md to keep it honest.
    AllowEntry {
        path: "runtime/streamlib-consumer-rhi/",
        kind: AllowKind::PathPrefix,
        rationale: "consumer-rhi import-side carve-out chains ImportMemoryFdInfoKHR",
    },
    // Codec layer at `runtime/streamlib-engine/src/vulkan/video/` is
    // covered by the engine-vulkan PathPrefix entry above. Interior
    // re-plumbing onto engine RHI primitives is the Vulkan Video RHI
    // Coupling milestone's ongoing work.
    //
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
        path: "adapters/streamlib-adapter-vulkan-helpers/",
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
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
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

const VULKANALIA_FORK_RATIONALE: &str = "all vulkanalia / vulkanalia-sys / vulkanalia-vma deps must inherit from [workspace.dependencies] (the vendored tatolab-vulkanalia* crates in vendor/) — a direct version spec or a direct tatolab-vulkanalia* dep can silently pull crates.io upstream or bypass the workspace rename and lose the VMA 3.3.0 patch";

/// The vendored fork crates' own sibling deps (`tatolab-vulkanalia` →
/// `tatolab-vulkanalia-sys`, `tatolab-vulkanalia-vma` → `tatolab-vulkanalia`)
/// cannot be workspace-inherited — the workspace deps ARE these crates.
/// Anchored to the three EXACT dirs (trailing slash) so a future
/// `vendor/tatolab-vulkanalia-extras/` crate does not silently inherit the
/// exemption.
const VULKANALIA_FORK_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "vendor/tatolab-vulkanalia/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — sibling deps are path+version+registry by construction",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-sys/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — sibling deps are path+version+registry by construction",
    },
    AllowEntry {
        path: "vendor/tatolab-vulkanalia-vma/",
        kind: AllowKind::PathPrefix,
        rationale: "vendored vulkanalia fork source — sibling deps are path+version+registry by construction",
    },
];

fn check_vulkanalia_uses_workspace_fork(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_cargo_toml(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, VULKANALIA_FORK_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, dep_value, line_no) in
            iter_dep_entries_with_values(&parsed, &content)
        {
            // Match the dep-table KEY and the value's `package =` rename —
            // `foo = { package = "tatolab-vulkanalia", path = "…" }` grants
            // the same raw Vulkan surface under an arbitrary key.
            let package_rename = dep_value
                .get("package")
                .and_then(|v| v.as_str())
                .filter(|pkg| is_vulkanalia_dep(pkg));
            if !is_vulkanalia_dep(&dep_name) && package_rename.is_none() {
                continue;
            }
            if dep_is_workspace_inherited(&dep_value) {
                continue;
            }
            let display_name = match package_rename {
                Some(pkg) if !is_vulkanalia_dep(&dep_name) => {
                    format!("{dep_name} (package = \"{pkg}\")")
                }
                _ => dep_name.clone(),
            };
            violations.push(Violation {
                path: rel.to_path_buf(),
                line_no,
                line_text: format!(
                    "[{}] {} = ... (not `workspace = true`)",
                    section, display_name
                ),
                matched_pattern: format!(
                    "{} bypasses workspace fork in [{}]",
                    display_name, section
                ),
                check: CHECK_VULKANALIA_FORK,
                rationale: VULKANALIA_FORK_RATIONALE,
            });
        }
    }
    Ok(())
}

fn is_vulkanalia_dep(name: &str) -> bool {
    // The workspace-renamed keys AND the vendored crate names — a direct
    // `tatolab-vulkanalia* = ...` dep bypasses the workspace rename just as
    // surely as a direct `vulkanalia = "0.35"` bypassed the fork.
    name == "vulkanalia"
        || name == "vulkanalia-sys"
        || name == "vulkanalia-vma"
        || name.starts_with("tatolab-vulkanalia")
}

// ---------------------------------------------------------------------------
// Check 6 — `streamlib_engine` direct imports confined to the engine + SDK
// ---------------------------------------------------------------------------
//
// The SDK / engine split (#731) makes `streamlib` the universal facade for
// all consumer code — apps, examples, domain packages, adapter crates,
// adapter helpers, engine tooling. The only places that legitimately import
// `streamlib_engine::*` directly are:
//
// 1. The engine itself (`runtime/streamlib-engine/`) — its own bins, tests,
//    benches link to its lib by the engine's published Cargo name.
// 2. The SDK's facade source (`sdk/streamlib-sdk/src/lib.rs`) — the one
//    file that pub-uses items from engine to expose them through
//    `streamlib::*`.
//
// Anywhere else, `use streamlib_engine` or `streamlib_engine::PATH`
// references mean someone is bypassing the SDK boundary visually — even if
// the Cargo dep graph is correct, a future reader can't tell whether the
// SDK is doing anything. Engine-bridge access (host_rhi, Host*Ext, etc.)
// goes through `streamlib::sdk::engine::*` instead.

const CHECK_STREAMLIB_ENGINE: &str = "streamlib-engine-only-in-sdk-or-engine";

const STREAMLIB_ENGINE_RATIONALE: &str = "direct streamlib_engine::* imports must stay in the engine itself or in sdk/streamlib-sdk/src/lib.rs (the SDK facade); consumer code routes through streamlib::* (with engine extensions via streamlib::sdk::engine::*)";

const STREAMLIB_ENGINE_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "runtime/streamlib-engine/",
        kind: AllowKind::PathPrefix,
        rationale: "engine itself — its own bins/tests/benches link to its lib by name",
    },
    AllowEntry {
        path: "sdk/streamlib-sdk/src/lib.rs",
        kind: AllowKind::ExactFile,
        rationale: "SDK facade — pub uses items from engine to expose via streamlib::*",
    },
    AllowEntry {
        path: "tools/streamlib-build-orchestrator/",
        kind: AllowKind::PathPrefix,
        rationale: "default BuildOrchestrator impl — implements the engine's BuildOrchestrator trait and calls engine package-cache APIs (get_cached_package_dir); it is engine-tier infrastructure, not consumer code, and CANNOT route through the streamlib::* SDK facade because the SDK depends on it (the auto-build feature) — that would be a dependency cycle. Direct streamlib_engine::* is unavoidable here",
    },
];

// ---------------------------------------------------------------------------
// Check 7 — `streamlib::*` top-level shortcuts forbidden
// ---------------------------------------------------------------------------
//
// All consumer code must route through the SDK's three-tier path system:
//
//   - `streamlib::sdk::*`            (default — public SDK API)
//   - `streamlib::sdk::engine::*`    (curated engine-bridge surface)
//   - `streamlib::engine_internal::*`(direct passthrough — rare; signals
//                                     "I'm reaching past the curated boundary")
//
// Top-level `streamlib::Foo` shortcuts (e.g., `use streamlib::StreamRuntime`)
// are forbidden because they hide which boundary tier the consumer is in.
// Path segments are the documentation: a future reader scans the import
// and immediately knows which tier is being used.
//
// The engine itself uses `extern crate self as streamlib;` and references
// items through that alias for proc-macro path resolution. Those uses
// are exempt — the engine's lib.rs and its own internal modules legitimately
// reach internal items through `streamlib::*`.

const CHECK_TOP_LEVEL_SHORTCUT: &str = "streamlib-top-level-shortcut-forbidden";

const TOP_LEVEL_SHORTCUT_RATIONALE: &str = "use streamlib::sdk::* (or sdk::engine::* / engine_internal::* for engine-side access); top-level streamlib::Foo shortcuts hide the boundary tier";

const TOP_LEVEL_SHORTCUT_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "runtime/streamlib-engine/",
        kind: AllowKind::PathPrefix,
        rationale: "engine itself — `extern crate self as streamlib;` aliases the engine; internal modules reach items through this alias for proc-macro path resolution",
    },
    AllowEntry {
        path: "sdk/streamlib-sdk/src/lib.rs",
        kind: AllowKind::ExactFile,
        rationale: "SDK facade — defines the streamlib::sdk::* / engine_internal::* tree",
    },
    AllowEntry {
        path: "sdk/streamlib-macros/",
        kind: AllowKind::PathPrefix,
        rationale: "proc-macro emit-paths for downstream consumers; not consumer code",
    },
];

// Path-prefix allowlist for the FIRST segment after `streamlib::`. A
// `streamlib::FOO` reference is OK only if FOO is one of these segments.
const TOP_LEVEL_ALLOWED_SEGMENTS: &[&str] = &["sdk", "engine_internal"];

fn check_streamlib_top_level_shortcut(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    // Hand-rolled scanner — match `streamlib::FOO` where FOO starts at
    // a word boundary and is an identifier.
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, TOP_LEVEL_SHORTCUT_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue; // skip comments (incl. `///` and `//!`)
            }
            // Walk every occurrence of "streamlib::" in the line, check the
            // segment that follows.
            let bytes = line.as_bytes();
            let mut i = 0;
            while let Some(found) = line[i..].find("streamlib::") {
                let start = i + found;
                // Boundary check: char before must NOT be ident/_ (avoid
                // matching `streamlib_engine::...`, `streamlib_consumer_rhi::...` etc.)
                if start > 0 {
                    let prev = bytes[start - 1] as char;
                    if prev.is_ascii_alphanumeric() || prev == '_' {
                        i = start + "streamlib::".len();
                        continue;
                    }
                }
                let after = start + "streamlib::".len();
                // Read the segment ident
                let mut end = after;
                while end < bytes.len() {
                    let c = bytes[end] as char;
                    if c.is_ascii_alphanumeric() || c == '_' {
                        end += 1;
                    } else {
                        break;
                    }
                }
                if end > after {
                    let segment = &line[after..end];
                    if !TOP_LEVEL_ALLOWED_SEGMENTS.contains(&segment) {
                        violations.push(Violation {
                            path: rel.to_path_buf(),
                            line_no: line_no + 1,
                            line_text: line.to_string(),
                            matched_pattern: format!("streamlib::{} (top-level)", segment),
                            check: CHECK_TOP_LEVEL_SHORTCUT,
                            rationale: TOP_LEVEL_SHORTCUT_RATIONALE,
                        });
                    }
                }
                i = end;
            }
        }
    }
    Ok(())
}

fn check_streamlib_engine_confined(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        *files_scanned += 1;
        let rel = rel_to_root(&path, project_root);
        if matches_allow(rel, STREAMLIB_ENGINE_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (line_no, line) in content.lines().enumerate() {
            // Skip comment-only lines (doc-comment intra-doc links are not
            // type-checked but still encourage future authors to reach for
            // the engine path; flag them too).
            if !line.contains("streamlib_engine") {
                continue;
            }
            // Trim leading whitespace to detect the line-shape.
            let trimmed = line.trim_start();
            // We flag any non-comment line that mentions `streamlib_engine`.
            // Comment lines (//, ///, //!) we skip — doc-link rewriting is
            // handled by docs sweep and not boundary-relevant.
            if trimmed.starts_with("//") {
                continue;
            }
            violations.push(Violation {
                path: rel.to_path_buf(),
                line_no: line_no + 1,
                line_text: line.to_string(),
                matched_pattern: "streamlib_engine".to_string(),
                check: CHECK_STREAMLIB_ENGINE,
                rationale: STREAMLIB_ENGINE_RATIONALE,
            });
        }
    }
    Ok(())
}

fn dep_is_workspace_inherited(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|t| t.get("workspace"))
        .and_then(|w| w.as_bool())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Check 8 — `packages/*` crates must not link the full `streamlib` facade
// ---------------------------------------------------------------------------
//
// A distributable `.slpkg` is built engine-free against the plugin-authoring
// SDK; carrying the full `streamlib` facade as a runtime dep pulls the
// FullAccess engine surface into a crate that ships as a source-only package.
// The facade is host-by-design only for the `api-server` (a host application
// package) and `test-fixtures` (host-side `cargo test`) packages; every other
// facade linker is the shrinking conversion backlog and drops off this
// allowlist as its package converts to the engine-free authoring SDK.
//
// This is a per-entry-rationale ratchet seeded to the exact current violating
// set: any NEW `packages/*` crate that adds a non-dev `streamlib` dep fails.
// Mirrors check 3 (`iter_dep_entries` + `section_is_dev_only`), but as a
// tree-wide ratchet over `packages/*` rather than a fixed crate list.

const CHECK_PACKAGES_FACADE_DEP: &str = "packages-no-facade-runtime-dep";

const PACKAGES_FACADE_DEP_RATIONALE: &str = "a packages/* crate must not carry the full `streamlib` facade as a runtime dep — a distributable .slpkg builds engine-free against the plugin-authoring SDK; the facade is host-by-design only for api-server + test-fixtures. Move it to [dev-dependencies] or convert the package to the engine-free authoring SDK";

/// The `packages/*` crates that still link the `streamlib` facade as a
/// non-dev runtime dep (green baseline). `api-server` + `test-fixtures` are
/// permanent (host-by-design); the remaining entries are the shrinking
/// conversion backlog, each gated on a new engine-free primitive — remove each
/// entry as its package converts to the engine-free plugin-authoring SDK.
const PACKAGES_FACADE_DEP_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "packages/api-server/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "permanent, host-by-design: api-server is a host application package that hosts processors and legitimately links the full facade",
    },
    AllowEntry {
        path: "packages/test-fixtures/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "permanent, host-by-design: test-fixtures run host-side under cargo test and legitimately link the full facade",
    },
    // Shrinking conversion backlog — each drops off as its package converts
    // to the engine-free plugin-authoring SDK. The remaining entries are
    // gated on a new engine-free primitive (present target, exportable
    // timelines / surface-store registration, hardware encode/decode) that
    // the package names the raw host device without.
    AllowEntry {
        path: "packages/camera/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade linker (shrinking backlog)",
    },
    AllowEntry {
        path: "packages/h264/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade linker (shrinking backlog)",
    },
    AllowEntry {
        path: "packages/h265/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade linker (shrinking backlog)",
    },
];

fn check_packages_facade_runtime_dep(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_cargo_toml(project_root) {
        let rel = rel_to_root(&path, project_root);
        if !rel_starts_with(rel, "packages/") {
            continue;
        }
        *files_scanned += 1;
        if matches_allow(rel, PACKAGES_FACADE_DEP_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            if dep_name == "streamlib" && !section_is_dev_only(&section) {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no,
                    line_text: format!("[{}] streamlib = ...", section),
                    matched_pattern: format!("streamlib facade dep in [{}]", section),
                    check: CHECK_PACKAGES_FACADE_DEP,
                    rationale: PACKAGES_FACADE_DEP_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 9 — `packages/*` source must not reach the engine bridge / host device
// ---------------------------------------------------------------------------
//
// A separately-built source-only `.slpkg` can have a binary that diverges from
// the host's even at a matched version. GPU code that grabs the raw transited
// host device (`host_vulkan_device_arc`) and hand-rolls RHI on it corrupts the
// driver in that scenario — package GPU code must go through the cdylib-safe
// FullAccess primitives (see docs/learnings/slpkg-raw-device-rhi-construction.md).
// The engine-bridge path `streamlib::sdk::engine::*` is the curated surface that
// exposes those raw handles; reaching it from a package is the same boundary
// crossing.
//
// Grep-shaped (comment lines skipped): the engine-bridge path substring and the
// bare `host_vulkan_device_arc` identifier. Seeded to the current reachers (see
// PACKAGES_ENGINE_REACH_ALLOWLIST below); `test-fixtures` is permanent
// (host-side, exercises the bridge by design), the rest shrink as conversions
// land.
//
// A package's TOP-LEVEL `tests/` and `benches/` dirs are EXEMPT: an
// engine-backed integration test / benchmark legitimately reaches the bridge,
// and check 8 already blesses the `streamlib` dev-dep those targets link
// against. Only the cargo target dir directly under the package root is
// exempt — a `src/tests/` helper dir stays covered. `src/` stays strict
// EVERYWHERE, including `#[cfg(test)]` mods — for a tooling reason, NOT because
// the reach ships: the pack / load build is `cargo build -p <crate>`, never
// `--tests` (tools/streamlib-pack/src/lib.rs), so the `test` cfg is OFF and a
// `#[cfg(test)]` reach is compiled out — it does NOT ship in the cdylib. It
// stays flagged because this grep is line-based and cannot reliably scope a
// reach to a `#[cfg(test)]` mod; engine-backed tests belong in the top-level
// `tests/` target (blessed by check 8). An in-`src` hit is told to move the
// engine-backed test to `tests/`.

const CHECK_PACKAGES_ENGINE_REACH: &str = "packages-no-engine-bridge-reach";

const PACKAGES_ENGINE_REACH_RATIONALE: &str = "packages/* source must not reach the engine bridge (`streamlib::sdk::engine::*`) or grab the raw transited host device via `host_vulkan_device_arc` — a separately-built source-only .slpkg whose GPU code hand-rolls RHI on the host device corrupts the driver (docs/learnings/slpkg-raw-device-rhi-construction.md); package GPU code goes through the cdylib-safe FullAccess primitives";

/// Engine-bridge module path — the curated surface that hands packages raw
/// engine primitives. A substring match is enough; it is unambiguous.
const ENGINE_BRIDGE_PATH: &str = "streamlib::sdk::engine::";

/// Accessor that returns the raw transited host `VulkanDevice`. Matched as a
/// bare identifier (word boundaries) so a longer lookalike does not trip it.
const HOST_DEVICE_ARC_IDENT: &str = "host_vulkan_device_arc";

/// The `packages/*` dirs whose source currently reaches the engine bridge or
/// the host device (green baseline). `test-fixtures` is permanent (host-side);
/// the rest are the shrinking conversion backlog.
const PACKAGES_ENGINE_REACH_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "packages/test-fixtures/",
        kind: AllowKind::PathPrefix,
        rationale: "permanent: test-fixtures run host-side and exercise the engine bridge directly by design",
    },
    // Shrinking conversion backlog.
    AllowEntry {
        path: "packages/camera/",
        kind: AllowKind::PathPrefix,
        rationale: "pre-conversion engine-bridge reacher (shrinking backlog)",
    },
    AllowEntry {
        path: "packages/h264/",
        kind: AllowKind::PathPrefix,
        rationale: "pre-conversion engine-bridge reacher (shrinking backlog)",
    },
    AllowEntry {
        path: "packages/h265/",
        kind: AllowKind::PathPrefix,
        rationale: "pre-conversion engine-bridge reacher (shrinking backlog)",
    },
];

fn check_packages_engine_reach(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_rs(project_root) {
        let rel = rel_to_root(&path, project_root);
        if !rel_starts_with(rel, "packages/") {
            continue;
        }
        // A package's TOP-LEVEL tests/ or benches/ dir is exempt — engine-backed
        // integration tests / benchmarks belong there (check 8 blesses the
        // dev-dep). `src/` stays strict everywhere, including a `src/tests/`
        // helper dir and `#[cfg(test)]` mods.
        if package_top_level_test_or_bench_dir(rel) {
            continue;
        }
        *files_scanned += 1;
        if matches_allow(rel, PACKAGES_ENGINE_REACH_ALLOWLIST) {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let matched = if line.contains(ENGINE_BRIDGE_PATH) {
                Some(ENGINE_BRIDGE_PATH)
            } else if line_has_bare_ident(line, HOST_DEVICE_ARC_IDENT) {
                Some(HOST_DEVICE_ARC_IDENT)
            } else {
                None
            };
            if let Some(pattern) = matched {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: pattern.to_string(),
                    check: CHECK_PACKAGES_ENGINE_REACH,
                    rationale: PACKAGES_ENGINE_REACH_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 10 — `examples/*` cdylib plugins must not link the full `streamlib`
//            facade
// ---------------------------------------------------------------------------
//
// An `examples/*` crate that ships as a cdylib is a plugin: it is built
// independently at load time and rides the engine-free plugin-authoring SDK
// (`streamlib-plugin-sdk`) plus `streamlib-consumer-rhi`, never the FullAccess
// `streamlib` facade. Only cdylib-shipping example crates are gated — an
// example *app* (a plain bin / rlib) links the facade by design (apps are code
// calling the runtime's add-module API). Cdylib detection reuses
// `check_cdylib_reach::cargo_toml_has_cdylib` — one crate-type detector.
//
// Seeded to the 3 current offenders; `camera-plugin-sdk-compute/plugin` is left
// un-allowlisted as live proof the rule passes for a correctly-authored cdylib
// example (it links `streamlib-plugin-sdk`, never the facade).

const CHECK_EXAMPLES_CDYLIB_FACADE_DEP: &str = "examples-cdylib-no-facade-dep";

const EXAMPLES_CDYLIB_FACADE_DEP_RATIONALE: &str = "an examples/* cdylib plugin must not link the full `streamlib` facade — a cdylib plugin is built independently at load time and rides streamlib-plugin-sdk / streamlib-consumer-rhi, never the FullAccess facade. Move it to [dev-dependencies] or author against the plugin SDK";

/// The 3 `examples/*` cdylib crates that currently link the `streamlib` facade
/// as a non-dev runtime dep (green baseline). Each is the shrinking conversion
/// backlog. `camera-plugin-sdk-compute/plugin` is deliberately absent — it
/// links `streamlib-plugin-sdk` (never the facade) and proves the rule passes.
const EXAMPLES_CDYLIB_FACADE_DEP_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "examples/camera-rust-plugin/plugin/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade-linking cdylib example (shrinking backlog)",
    },
    AllowEntry {
        path: "examples/camera-python-display/effects/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade-linking cdylib example (shrinking backlog)",
    },
    AllowEntry {
        path: "examples/polyglot-manual-source/plugin/Cargo.toml",
        kind: AllowKind::ExactFile,
        rationale: "pre-conversion facade-linking cdylib example (shrinking backlog)",
    },
];

fn check_examples_cdylib_facade_dep(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for path in walk_cargo_toml(project_root) {
        let rel = rel_to_root(&path, project_root);
        if !rel_starts_with(rel, "examples/") {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        // Only cdylib-shipping example crates are gated — reuse the single
        // crate-type detector rather than a divergent second parser.
        if !crate::check_cdylib_reach::cargo_toml_has_cdylib(&content) {
            continue;
        }
        *files_scanned += 1;
        if matches_allow(rel, EXAMPLES_CDYLIB_FACADE_DEP_ALLOWLIST) {
            continue;
        }
        let parsed: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (section, dep_name, line_no) in iter_dep_entries(&parsed, &content) {
            if dep_name == "streamlib" && !section_is_dev_only(&section) {
                violations.push(Violation {
                    path: rel.to_path_buf(),
                    line_no,
                    line_text: format!("[{}] streamlib = ...", section),
                    matched_pattern: format!("streamlib facade dep in [{}]", section),
                    check: CHECK_EXAMPLES_CDYLIB_FACADE_DEP,
                    rationale: EXAMPLES_CDYLIB_FACADE_DEP_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 11 — the engine-free TRUNK SET must never Cargo-dep the engine
// ---------------------------------------------------------------------------
//
// MEMBERSHIP RULE for the trunk root set: roots = every crate packages are
// mandated or expected to link directly and consume by version. The four
// engine-free trunk roots — `streamlib-plugin-sdk` (sdk/streamlib-plugin-sdk),
// `streamlib-macros` (sdk/streamlib-macros), `streamlib-plugin-abi`
// (runtime/streamlib-plugin-abi), and `streamlib-consumer-rhi`
// (runtime/streamlib-consumer-rhi) — are the authoring substrate every
// distributable `.slpkg` links against. A non-dev `[dependencies]` entry that
// resolves to the engine crate (`streamlib-engine` at runtime/streamlib-engine)
// in ANY of them statically pulls the FullAccess engine surface into that
// substrate.
//
// The list is PRINCIPLED, not enumerated ad hoc: a crate earns root status by
// being something packages dep DIRECTLY and consume by version. The small
// utility crates (`streamlib-error`, `streamlib-processor-schema`,
// `streamlib-idents`) are deliberately NOT listed — they are covered
// TRANSITIVELY through the roots (check 12 walks the closure), so a listing
// would be redundant. `streamlib-consumer-rhi` earns root status on its own
// because check 3 makes it a first-class part of the boundary contract:
// cdylibs and adapter crates dep it directly (in place of the full facade), so
// packages link it by version just like the plugin-authoring SDK — an engine
// dep in it would propagate the FullAccess surface to external consumers
// exactly as one in plugin-sdk would.
//
// This is the PERMANENT invariant that replaces the dropped (dead-path)
// "plugin/* -> libs/" exit criterion: it is the enforcement that SURVIVES the
// packages -> streamlib-packages split. External packages consume plugin-sdk
// by VERSION from the registry, so a published plugin-sdk that pulled the
// engine would propagate the engine to every external consumer invisibly —
// with no in-tree crate left to catch it. Unlike the transitional `packages/*`
// leaves ratchet (checks 8 & 9, which carry a shrinking per-package
// allowlist), this trunk ban has NO allowlist and never shrinks.
//
// `[dev-dependencies]` are EXEMPT — a trunk crate's conformance tests may
// legitimately pull the engine to exercise the host backing. Reuses the same
// `iter_dep_entries_with_values` + `section_is_dev_only` machinery as checks
// 8-10, and resolves `package = "..."` alias keys (an entry
// `foo = { package = "streamlib-engine", ... }` is caught by its resolved
// package name, not the section key) — closing the exact alias evasion the
// facade check leaves as a known low.

const CHECK_TRUNK_NO_ENGINE_DEP: &str = "trunk-set-no-engine-cargo-dep";

const TRUNK_NO_ENGINE_DEP_RATIONALE: &str = "PERMANENT trunk ban (survives the packages -> streamlib-packages split): an engine-free trunk root (streamlib-plugin-sdk / streamlib-macros / streamlib-plugin-abi / streamlib-consumer-rhi) must never carry `streamlib-engine` as a non-dev Cargo dep. MEMBERSHIP RULE: roots = every crate packages are mandated or expected to link directly and consume by version — the small utility crates (streamlib-error / streamlib-processor-schema / streamlib-idents) are covered transitively through the roots, and consumer-rhi earns root status because check 3 makes it a first-class part of the boundary contract (cdylibs and adapter crates dep it directly). External packages consume these roots by version from the registry, so a published root that pulled the engine would propagate the FullAccess engine surface to every external consumer invisibly. Unlike the transitional packages/* leaves ratchet, this ban has no shrinking allowlist; [dev-dependencies] are exempt (conformance tests may pull the engine)";

/// The engine crate's Cargo package name (lib name is `streamlib_engine`; the
/// Cargo dependency key / `package =` rename resolves to this hyphenated form).
const TRUNK_ENGINE_CRATE_NAME: &str = "streamlib-engine";

/// The four engine-free trunk roots whose non-dev dep graph must never
/// resolve to `streamlib-engine`. A fixed list (mirrors check 3's
/// `NO_STREAMLIB_RUNTIME_DEP`) — this is a permanent invariant, not a
/// shrinking ratchet. Roots = every crate packages are mandated or expected to
/// link directly and consume by version; the small utility crates
/// (streamlib-error / streamlib-processor-schema / streamlib-idents) are
/// covered transitively through these roots (check 12), and consumer-rhi is a
/// root because check 3 makes it a first-class part of the boundary contract.
const TRUNK_NO_ENGINE_DEP: &[&str] = &[
    "sdk/streamlib-plugin-sdk/Cargo.toml",
    "sdk/streamlib-macros/Cargo.toml",
    "runtime/streamlib-plugin-abi/Cargo.toml",
    "runtime/streamlib-consumer-rhi/Cargo.toml",
];

fn check_trunk_set_no_engine_dep(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for rel_path in TRUNK_NO_ENGINE_DEP {
        let path = project_root.join(rel_path);
        if !path.exists() {
            // Allowlisted crate may have been renamed/deleted; skip silently —
            // this is enforcement, not discovery (mirrors check 3).
            continue;
        }
        *files_scanned += 1;
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: toml::Value =
            toml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
        for (section, dep_name, dep_value, line_no) in
            iter_dep_entries_with_values(&parsed, &content)
        {
            // [dev-dependencies] (and target.*.dev-dependencies) are exempt —
            // conformance tests may legitimately pull the engine.
            if section_is_dev_only(&section) {
                continue;
            }
            // Resolve the `package = "..."` alias key: `foo = { package =
            // "streamlib-engine", ... }` must be caught by its RESOLVED package
            // name, not the section key. Fall back to the key when absent.
            let resolved = dep_value
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(dep_name.as_str());
            if resolved == TRUNK_ENGINE_CRATE_NAME {
                let display_name = if resolved != dep_name {
                    format!("{dep_name} (package = \"{resolved}\")")
                } else {
                    dep_name.clone()
                };
                violations.push(Violation {
                    path: PathBuf::from(rel_path),
                    line_no,
                    line_text: format!("[{}] {} = ...", section, display_name),
                    matched_pattern: format!(
                        "streamlib-engine dep in [{}] (as {})",
                        section, display_name
                    ),
                    check: CHECK_TRUNK_NO_ENGINE_DEP,
                    rationale: TRUNK_NO_ENGINE_DEP_RATIONALE,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check 12 — the trunk set must never TRANSITIVELY reach the engine
// ---------------------------------------------------------------------------
//
// Check 11 (`check_trunk_set_no_engine_dep`) is the DIRECT manifest scan: it
// catches a `streamlib-engine` dep written straight into a trunk crate's
// `Cargo.toml`, with precise file:line reporting. But a trunk crate can reach
// the engine INDIRECTLY — `streamlib-plugin-sdk` -> some workspace crate ->
// `streamlib-engine` — and no single manifest shows that chain. This check
// layers the transitive closure on top: it resolves each trunk crate's
// normal + build dependency graph via `cargo metadata` and flags
// `streamlib-engine` appearing ANYWHERE in that closure.
//
// It reuses `streamlib-pack`'s `NormalBuildDepGraph` — the SAME
// `dep_kinds`-filtered adjacency construction the release-closure DFS rides —
// so the dev-only-edge drop has exactly one definition. That drop is
// load-bearing: `[dev-dependencies]` are exempt (a trunk crate's conformance
// tests may pull the engine), and a second hand-rolled walker could miss it.
//
// The walk stays within WORKSPACE MEMBERS — an external registry crate cannot
// depend on the in-tree engine, so it cannot lie on a trunk -> engine chain,
// and `streamlib-engine` is itself a member so the target is never pruned.
// `--filter-platform` is deliberately NOT passed: plugin-sdk's
// `streamlib-consumer-rhi` dep is linux-target and must stay covered.
//
// On a violation the offending CHAIN is printed
// (`<trunk> -> … -> streamlib-engine`) so the intermediate edge is obvious.
// This has NO allowlist and never shrinks — same permanence as check 11.

/// The four engine-free trunk root names whose transitive normal + build
/// closure must never reach [`TRUNK_ENGINE_CRATE_NAME`]. Mirrors check 11's
/// `TRUNK_NO_ENGINE_DEP` (which keys on manifest paths); here we key on package
/// names because the walk resolves through `cargo metadata`. Roots = every
/// crate packages are mandated or expected to link directly and consume by
/// version; consumer-rhi is a root because check 3 makes it a first-class part
/// of the boundary contract (packages dep it directly).
const TRUNK_CRATE_NAMES: &[&str] = &[
    "streamlib-plugin-sdk",
    "streamlib-macros",
    "streamlib-plugin-abi",
    "streamlib-consumer-rhi",
];

/// A discovered trunk-crate → `streamlib-engine` dependency chain, as package
/// names from the trunk crate to the engine inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrunkEngineChain {
    /// The trunk crate the chain starts at.
    pub trunk: String,
    /// Package names from the trunk crate to `streamlib-engine`, inclusive and
    /// in traversal order.
    pub chain: Vec<String>,
}

impl TrunkEngineChain {
    /// Render the chain as `<trunk> -> … -> streamlib-engine`.
    pub fn display_chain(&self) -> String {
        self.chain.join(" -> ")
    }
}

/// Walk the transitive normal + build closure of each trunk crate over
/// WORKSPACE MEMBERS ONLY and return every chain that reaches
/// `streamlib-engine`. Pure over the parsed graph so it unit-tests against a
/// synthetic `cargo metadata` fixture without the live tree.
pub fn find_trunk_engine_chains(graph: &NormalBuildDepGraph) -> Vec<TrunkEngineChain> {
    let mut chains = Vec::new();
    for &trunk_name in TRUNK_CRATE_NAMES {
        for root_id in graph.ids_named(trunk_name) {
            // A trunk crate resolves to a workspace member; skip any same-named
            // external package (cannot reach the in-tree engine anyway).
            if !graph.is_workspace_member(root_id) {
                continue;
            }
            if let Some(chain_ids) = shortest_member_chain_to_engine(graph, root_id) {
                chains.push(TrunkEngineChain {
                    trunk: trunk_name.to_string(),
                    chain: chain_ids
                        .iter()
                        .map(|id| graph.name_of(id).unwrap_or(id).to_string())
                        .collect(),
                });
            }
        }
    }
    chains
}

/// Breadth-first search from `root_id` over workspace-member normal + build
/// edges, returning the shortest id path (root..=engine) that reaches
/// `streamlib-engine`, or `None` if the engine is unreachable.
fn shortest_member_chain_to_engine<'graph>(
    graph: &'graph NormalBuildDepGraph,
    root_id: &'graph str,
) -> Option<Vec<&'graph str>> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut predecessor: HashMap<&str, &str> = HashMap::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    visited.insert(root_id);
    queue.push_back(root_id);
    while let Some(id) = queue.pop_front() {
        if id != root_id && graph.name_of(id) == Some(TRUNK_ENGINE_CRATE_NAME) {
            // Reconstruct the path root..=id via the predecessor map.
            let mut path = vec![id];
            let mut cursor = id;
            while let Some(&prev) = predecessor.get(cursor) {
                path.push(prev);
                cursor = prev;
            }
            path.reverse();
            return Some(path);
        }
        for dep in graph.normal_build_deps(id) {
            let dep = dep.as_str();
            // Traverse INTO workspace members only — an external crate cannot
            // depend on the in-tree engine, so it cannot lie on the chain. The
            // engine is a member, so this never prunes the target.
            if !graph.is_workspace_member(dep) {
                continue;
            }
            if visited.insert(dep) {
                predecessor.insert(dep, id);
                queue.push_back(dep);
            }
        }
    }
    None
}

/// Run `cargo metadata` at `project_root` and return every trunk-set → engine
/// transitive chain. Layered on the direct manifest [`check_trunk_set_no_engine_dep`]:
/// that check catches a direct engine dep with precise file:line; this catches
/// an engine reached through an intermediate workspace crate.
fn run_trunk_transitive_check(project_root: &Path) -> Result<Vec<TrunkEngineChain>> {
    let manifest_path = project_root.join("Cargo.toml");
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .with_context(|| format!("running cargo metadata at {}", manifest_path.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "cargo metadata failed at {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata JSON")?;
    let graph = NormalBuildDepGraph::from_metadata(&metadata)?;
    Ok(find_trunk_engine_chains(&graph))
}

/// True iff `rel` (a workspace-relative path) begins with `prefix`
/// (`/`-separated, e.g. `"packages/"`).
fn rel_starts_with(rel: &Path, prefix: &str) -> bool {
    rel.to_string_lossy().replace('\\', "/").starts_with(prefix)
}

/// True iff `rel` sits under a package's TOP-LEVEL `tests/` or `benches/` cargo
/// target dir — i.e. its path components are `packages / <pkg> / (tests|benches)
/// / …`. A deeper `src/tests/` helper dir does NOT match: only the target dir
/// directly under the package root is a cargo test / bench target, so only it
/// gets the engine-reach exemption.
fn package_top_level_test_or_bench_dir(rel: &Path) -> bool {
    let comps: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    comps.len() >= 3 && comps[0] == "packages" && (comps[2] == "tests" || comps[2] == "benches")
}

/// True iff `line` contains `ident` as a bare identifier — the characters
/// immediately before and after the match are not identifier characters, so a
/// longer lookalike (`host_vulkan_device_arc_cached`) does not trip it.
fn line_has_bare_ident(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(off) = line[from..].find(ident) {
        let start = from + off;
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_char(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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
fn iter_dep_entries(toml_value: &toml::Value, raw_text: &str) -> Vec<(String, String, usize)> {
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
                out.push((
                    section_name.to_string(),
                    dep_name.clone(),
                    value.clone(),
                    line,
                ));
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
        write_fixture(dir.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
        dir
    }

    // ----- Check 1: no ash -----

    #[test]
    fn rejects_use_ash_in_rust_file() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/src/lib.rs",
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
            "runtime/streamlib-engine/src/lib.rs",
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
            "runtime/streamlib-engine/Cargo.toml",
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
            "runtime/streamlib-engine/src/lib.rs",
            "use ahash::AHashMap;\nlet h: Hash = todo!();\n",
        );
        // Cargo.toml with ahash dep.
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/Cargo.toml",
            "[package]\nname = \"streamlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nahash = \"0.8\"\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let no_ash: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_NO_ASH)
            .collect();
        assert!(
            no_ash.is_empty(),
            "ahash should not match ash: {:?}",
            no_ash
        );
    }

    // ----- Check 2: vulkanalia confined -----

    #[test]
    fn rejects_use_vulkanalia_outside_allowlist() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/src/core/some_unrelated.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_use_vulkanalia_in_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/src/vulkan/rhi/example.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA)
            .collect();
        assert!(vk.is_empty(), "vulkanalia in RHI should pass: {:?}", vk);
    }

    #[test]
    fn allows_use_vulkanalia_in_consumer_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-consumer-rhi/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA)
            .collect();
        assert!(
            vk.is_empty(),
            "vulkanalia in consumer-rhi should pass: {:?}",
            vk
        );
    }

    #[test]
    fn allows_use_vulkanalia_in_adapter_crate() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA)
            .collect();
        assert!(
            vk.is_empty(),
            "vulkanalia in adapter crate should pass: {:?}",
            vk
        );
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_outside_allowlist() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-runtime/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_vulkanalia_cargo_dep_in_adapter_crate() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.20"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA)
            .collect();
        assert!(
            vk.is_empty(),
            "vulkanalia dep in adapter crate should pass: {:?}",
            vk
        );
    }

    // ----- Check 5: vulkanalia uses workspace fork -----

    #[test]
    fn rejects_direct_vulkanalia_version_in_member() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_vulkanalia_vma_version_in_member() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/test-member/Cargo.toml",
            r#"[package]
name = "test-member"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia-vma = "0.4"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK),
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
            "adapters/streamlib-adapter-opengl/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation for git dep, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_tatolab_vulkanalia_dep_in_member() {
        let dir = empty_workspace();
        // A direct dep on the vendored crate NAME bypasses the workspace
        // rename exactly like a direct `vulkanalia = "0.35"` bypassed the
        // fork — check 5 must flag it.
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
tatolab-vulkanalia = "0.35"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK),
            "expected workspace-fork violation for direct tatolab-vulkanalia dep, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_vendored_fork_sibling_deps_and_sources() {
        let dir = empty_workspace();
        // The vendored fork crates' own sibling deps cannot be
        // workspace-inherited, and their sources ARE the Vulkan surface —
        // both checks 2 and 5 exempt vendor/tatolab-vulkanalia*.
        write_fixture(
            dir.path(),
            "vendor/tatolab-vulkanalia-vma/Cargo.toml",
            r#"[package]
name = "tatolab-vulkanalia-vma"
version = "0.9.0"
edition = "2021"

[dependencies]
vulkanalia = { package = "tatolab-vulkanalia", version = "0.35", path = "../tatolab-vulkanalia", registry = "tatolab", default-features = false }
"#,
        );
        write_fixture(
            dir.path(),
            "vendor/tatolab-vulkanalia-vma/src/lib.rs",
            "use vulkanalia::vk::*;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA || v.check == CHECK_VULKANALIA_FORK)
            .collect();
        assert!(
            hits.is_empty(),
            "vendored fork dirs should be exempt from checks 2 and 5: {:?}",
            hits
        );
    }

    #[test]
    fn vendored_dir_lookalike_does_not_inherit_exemptions() {
        let dir = empty_workspace();
        // The vendored-dir exemptions are anchored to the three EXACT dirs;
        // a hypothetical vendor/tatolab-vulkanalia-extras/ crate must NOT
        // silently inherit them in either check 2 or check 5.
        write_fixture(
            dir.path(),
            "vendor/tatolab-vulkanalia-extras/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        write_fixture(
            dir.path(),
            "vendor/tatolab-vulkanalia-extras/Cargo.toml",
            r#"[package]
name = "tatolab-vulkanalia-extras"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { package = "tatolab-vulkanalia", version = "0.35", path = "../tatolab-vulkanalia", registry = "tatolab" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check == CHECK_VULKANALIA
                && v.path
                    .to_string_lossy()
                    .contains("tatolab-vulkanalia-extras")),
            "lookalike dir must trip check 2, got {:?}",
            report.violations,
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK
                    && v.path
                        .to_string_lossy()
                        .contains("tatolab-vulkanalia-extras")),
            "lookalike dir must trip check 5, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_package_rename_bypass_in_member() {
        let dir = empty_workspace();
        // `foo = { package = "tatolab-vulkanalia", … }` grants the same raw
        // Vulkan surface under an arbitrary dep key — check 5 must match the
        // value's `package` field, not just the dep-table key.
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
foo = { package = "tatolab-vulkanalia", path = "../tatolab-vulkanalia", version = "0.35" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA_FORK
                    && v.matched_pattern
                        .contains("package = \"tatolab-vulkanalia\"")),
            "expected workspace-fork violation for the package= rename bypass, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_tatolab_vulkanalia_dep_outside_rhi_confinement() {
        let dir = empty_workspace();
        // Check 2's Cargo dep scan must also catch the vendored crate name —
        // a non-RHI crate gaining raw Vulkan via `tatolab-vulkanalia*`.
        write_fixture(
            dir.path(),
            "runtime/streamlib-runtime/Cargo.toml",
            r#"[package]
name = "streamlib-runtime"
version = "0.1.0"
edition = "2021"

[dependencies]
tatolab-vulkanalia-vma = "0.9"
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation for direct tatolab-vulkanalia-vma dep, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_workspace_inline_vulkanalia_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let fork: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA_FORK)
            .collect();
        assert!(
            fork.is_empty(),
            "{{ workspace = true }} should pass: {:?}",
            fork
        );
    }

    #[test]
    fn allows_workspace_dotted_vulkanalia_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia.workspace = true
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let fork: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA_FORK)
            .collect();
        assert!(fork.is_empty(), "dotted-key form should pass: {:?}", fork);
    }

    // The camera-python-display effects crate is NOT allowlisted — the
    // kernel wrappers ride VulkanGraphicsKernel::offscreen_render plus
    // RhiCommandRecorder and contain no direct vulkanalia. The general
    // "non-allowlisted path rejects vulkanalia" regression locks cover
    // this; no example-specific lock is needed.

    #[test]
    fn allows_use_vulkanalia_in_tests() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-opengl/tests/common.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let vk: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_VULKANALIA)
            .collect();
        assert!(vk.is_empty(), "vulkanalia in tests/ should pass: {:?}", vk);
    }

    // ----- Check 3: cdylib + adapter runtime deps -----

    #[test]
    fn rejects_streamlib_in_cdylib_runtime_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-python-native/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_CDYLIB_DEPS),
            "expected cdylib runtime-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_streamlib_in_adapter_runtime_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_CDYLIB_DEPS),
            "expected adapter target runtime-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_streamlib_in_cdylib_dev_dep() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-cpu-readback/Cargo.toml",
            r#"[package]
name = "streamlib-adapter-cpu-readback"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dev-dependencies]
streamlib = { path = "../streamlib" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let cdy: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_CDYLIB_DEPS)
            .collect();
        assert!(
            cdy.is_empty(),
            "streamlib in dev-deps should pass: {:?}",
            cdy
        );
    }

    // ----- Check 4: privileged vk calls -----

    #[test]
    fn rejects_allocate_memory_outside_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/src/core/some_other.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_create_compute_pipelines_outside_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "adapters/streamlib-adapter-vulkan/src/state.rs",
            "fn f() { unsafe { dev.create_compute_pipelines(cache, &[info], None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation in adapter crate, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_privileged_call_in_rhi() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs",
            "fn f() { unsafe { self.device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let pv: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PRIVILEGED_VK)
            .collect();
        assert!(
            pv.is_empty(),
            "privileged call in RHI should pass: {:?}",
            pv
        );
    }

    #[test]
    fn allows_privileged_call_in_consumer_rhi_for_carve_out() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "runtime/streamlib-consumer-rhi/src/consumer_vulkan_device.rs",
            "fn f() { unsafe { self.device.allocate_memory(&alloc_info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let pv: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PRIVILEGED_VK)
            .collect();
        assert!(
            pv.is_empty(),
            "consumer-rhi import-side carve-out should pass: {:?}",
            pv
        );
    }

    // ----- Cdylib tightening (#572) -----
    //
    // Post-#550/#553, neither cdylib needs raw `vulkanalia` access. These
    // tests prove the path-prefix exemption is gone — any reintroduction
    // of `use vulkanalia`, a `vulkanalia` Cargo dep, or a privileged-vk
    // call inside `sdk/streamlib-{python,deno}-native/` trips a
    // violation rather than slipping through.

    #[test]
    fn rejects_use_vulkanalia_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-python-native/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_use_vulkanalia_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-deno-native/src/lib.rs",
            "use vulkanalia::vk;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia confinement violation in deno-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-python-native/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo-dep violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_vulkanalia_cargo_dep_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-deno-native/Cargo.toml",
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
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_VULKANALIA),
            "expected vulkanalia Cargo-dep violation in deno-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_allocate_memory_in_python_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-python-native/src/some_module.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PRIVILEGED_VK),
            "expected privileged-vk violation in python-native cdylib, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_allocate_memory_in_deno_native_cdylib() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "sdk/streamlib-deno-native/src/some_module.rs",
            "fn f() { unsafe { device.allocate_memory(&info, None).unwrap(); } }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PRIVILEGED_VK),
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
            "runtime/streamlib-engine/src/lib.rs",
            "// use ash::vk; — kept for historical reference only\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let no_ash: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_NO_ASH)
            .collect();
        assert!(
            no_ash.is_empty(),
            "commented import should not match: {:?}",
            no_ash
        );
    }

    // ----- Check 8: packages/* facade-dep ban -----

    #[test]
    fn rejects_facade_dep_in_new_package() {
        let dir = empty_workspace();
        // A newly-carved package that is NOT on the seeded baseline must trip
        // the ratchet the moment it links the full facade.
        write_fixture(
            dir.path(),
            "packages/newly-carved/Cargo.toml",
            r#"[package]
name = "streamlib-newly-carved"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { version = "0.6.0" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PACKAGES_FACADE_DEP),
            "expected packages facade-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_facade_dep_in_allowlisted_package() {
        let dir = empty_workspace();
        // packages/camera is on the seeded green baseline — its facade dep must
        // NOT trip the ratchet.
        write_fixture(
            dir.path(),
            "packages/camera/Cargo.toml",
            r#"[package]
name = "streamlib-camera"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { version = "0.6.0" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_FACADE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "allowlisted package facade dep should pass: {:?}",
            hits
        );
    }

    #[test]
    fn allows_facade_dev_dep_in_new_package() {
        let dir = empty_workspace();
        // The escape hatch: move the facade to [dev-dependencies] and a
        // non-allowlisted package passes (dev-only is exempt, per check 3).
        write_fixture(
            dir.path(),
            "packages/newly-carved/Cargo.toml",
            r#"[package]
name = "streamlib-newly-carved"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
streamlib = { version = "0.6.0" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_FACADE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "facade dep in dev-dependencies should pass: {:?}",
            hits
        );
    }

    // ----- Check 9: packages/* engine-bridge / host-device reach ban -----

    #[test]
    fn rejects_engine_bridge_reach_in_new_package() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/lib.rs",
            "use streamlib::sdk::engine::HostSurfaceStoreExt;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PACKAGES_ENGINE_REACH
                    && v.matched_pattern == "streamlib::sdk::engine::"),
            "expected engine-bridge reach violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_host_vulkan_device_arc_reach_in_new_package() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/gpu.rs",
            "fn f() { let host_device = full.host_vulkan_device_arc().unwrap(); }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PACKAGES_ENGINE_REACH
                    && v.matched_pattern == "host_vulkan_device_arc"),
            "expected host-device reach violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_engine_reach_in_allowlisted_package() {
        let dir = empty_workspace();
        // packages/h264 is on the seeded baseline — both reach forms pass.
        write_fixture(
            dir.path(),
            "packages/h264/src/linux/encoder.rs",
            "use streamlib::sdk::engine::host_rhi::VulkanDevice;\nfn f() { let d = full.host_vulkan_device_arc().unwrap(); }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_ENGINE_REACH)
            .collect();
        assert!(
            hits.is_empty(),
            "allowlisted package engine reach should pass: {:?}",
            hits
        );
    }

    #[test]
    fn skips_commented_engine_reach_in_package() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/lib.rs",
            "// host_vulkan_device_arc() and streamlib::sdk::engine:: are off-limits here\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_ENGINE_REACH)
            .collect();
        assert!(
            hits.is_empty(),
            "commented engine reach should not match: {:?}",
            hits
        );
    }

    #[test]
    fn ignores_host_vulkan_device_arc_lookalike_in_package() {
        let dir = empty_workspace();
        // A longer identifier that merely contains the banned name as a
        // substring must NOT trip the bare-identifier match.
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/lib.rs",
            "fn f() { let x = obj.host_vulkan_device_arc_cached(); }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_ENGINE_REACH)
            .collect();
        assert!(
            hits.is_empty(),
            "lookalike identifier should not match: {:?}",
            hits
        );
    }

    // ----- Check 10: examples/* cdylib facade-dep ban -----

    #[test]
    fn rejects_facade_dep_in_new_cdylib_example() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "examples/newly-added/plugin/Cargo.toml",
            r#"[package]
name = "newly-added-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["rlib", "cdylib"]

[dependencies]
streamlib = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_EXAMPLES_CDYLIB_FACADE_DEP),
            "expected examples cdylib facade-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_facade_dep_in_allowlisted_cdylib_example() {
        let dir = empty_workspace();
        // examples/camera-rust-plugin/plugin is on the seeded baseline.
        write_fixture(
            dir.path(),
            "examples/camera-rust-plugin/plugin/Cargo.toml",
            r#"[package]
name = "camera-rust-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["rlib", "cdylib"]

[dependencies]
streamlib = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_EXAMPLES_CDYLIB_FACADE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "allowlisted cdylib example facade dep should pass: {:?}",
            hits
        );
    }

    #[test]
    fn ignores_facade_dep_in_non_cdylib_example() {
        let dir = empty_workspace();
        // An example *app* (bin/rlib, no cdylib) links the facade by design —
        // only cdylib plugins are gated. The cdylib detector gates the check.
        write_fixture(
            dir.path(),
            "examples/camera-display/Cargo.toml",
            r#"[package]
name = "camera-display"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_EXAMPLES_CDYLIB_FACADE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "non-cdylib example app should pass: {:?}",
            hits
        );
    }

    #[test]
    fn allows_cdylib_example_without_facade_dep() {
        let dir = empty_workspace();
        // The un-allowlisted proof: a correctly-authored cdylib example that
        // links the plugin SDK (never the facade) passes with no allowlist
        // entry — mirrors examples/camera-plugin-sdk-compute/plugin.
        write_fixture(
            dir.path(),
            "examples/camera-plugin-sdk-compute/plugin/Cargo.toml",
            r#"[package]
name = "camera-plugin-sdk-compute-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["rlib", "cdylib"]

[dependencies]
streamlib-plugin-sdk = { workspace = true }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_EXAMPLES_CDYLIB_FACADE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "cdylib example without facade dep should pass: {:?}",
            hits
        );
    }

    // ----- Check 11: trunk-set -> streamlib-engine Cargo-dep ban -----

    #[test]
    fn rejects_direct_engine_dep_in_trunk_crate() {
        let dir = empty_workspace();
        // A trunk crate that adds a DIRECT [dependencies] streamlib-engine dep
        // must trip the permanent ban — plugin-sdk is consumed by version from
        // the registry, so this would propagate the engine to every external
        // consumer invisibly.
        write_fixture(
            dir.path(),
            "sdk/streamlib-plugin-sdk/Cargo.toml",
            r#"[package]
name = "streamlib-plugin-sdk"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib-engine = { path = "../../runtime/streamlib-engine" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP),
            "expected trunk-set engine-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_aliased_engine_dep_in_trunk_crate() {
        let dir = empty_workspace();
        // An ALIASED dep key whose `package =` resolves to streamlib-engine
        // must be caught by its RESOLVED package name, not the section key —
        // locks the alias resolution that closes the facade-check evasion.
        write_fixture(
            dir.path(),
            "runtime/streamlib-plugin-abi/Cargo.toml",
            r#"[package]
name = "streamlib-plugin-abi"
version = "0.1.0"
edition = "2021"

[dependencies]
x = { package = "streamlib-engine", path = "../streamlib-engine" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report.violations.iter().any(|v| v.check
                == CHECK_TRUNK_NO_ENGINE_DEP
                && v.matched_pattern
                    .contains("package = \"streamlib-engine\"")),
            "expected trunk-set aliased engine-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_direct_engine_dep_in_consumer_rhi_trunk_crate() {
        let dir = empty_workspace();
        // consumer-rhi is the 4th trunk root (#1252 follow-up): cdylibs and
        // adapter crates dep it directly and consume it by version, so a
        // non-dev streamlib-engine dep in it would propagate the FullAccess
        // engine surface to every external consumer invisibly. Locks
        // consumer-rhi's root membership specifically — distinct from the
        // plugin-sdk / macros / plugin-abi cases above.
        write_fixture(
            dir.path(),
            "runtime/streamlib-consumer-rhi/Cargo.toml",
            r#"[package]
name = "streamlib-consumer-rhi"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib-engine = { path = "../streamlib-engine" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP),
            "expected consumer-rhi trunk-set engine-dep violation, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn allows_engine_dev_dep_in_trunk_crate() {
        let dir = empty_workspace();
        // [dev-dependencies] are exempt — a trunk crate's conformance tests may
        // legitimately pull the engine to exercise the host backing.
        write_fixture(
            dir.path(),
            "sdk/streamlib-macros/Cargo.toml",
            r#"[package]
name = "streamlib-macros"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
streamlib-engine = { path = "../../runtime/streamlib-engine" }
"#,
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP)
            .collect();
        assert!(
            hits.is_empty(),
            "engine dep in trunk [dev-dependencies] should pass: {:?}",
            hits
        );
    }

    // ----- Check 9 exemption: package top-level tests/ + benches/ dirs -----

    #[test]
    fn allows_engine_reach_in_package_top_level_tests_dir() {
        let dir = empty_workspace();
        // A NON-allowlisted package's TOP-LEVEL tests/ dir may reach the engine
        // bridge — engine-backed integration tests belong there (check 8
        // blesses the dev-dep). Locks the exemption.
        write_fixture(
            dir.path(),
            "packages/newly-carved/tests/integration.rs",
            "use streamlib::sdk::engine::HostSurfaceStoreExt;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_ENGINE_REACH)
            .collect();
        assert!(
            hits.is_empty(),
            "top-level tests/ engine reach should pass: {:?}",
            hits
        );
    }

    #[test]
    fn allows_engine_reach_in_package_top_level_benches_dir() {
        let dir = empty_workspace();
        write_fixture(
            dir.path(),
            "packages/newly-carved/benches/bench.rs",
            "fn f() { let d = full.host_vulkan_device_arc().unwrap(); }\n",
        );
        let report = scan_all(dir.path()).unwrap();
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.check == CHECK_PACKAGES_ENGINE_REACH)
            .collect();
        assert!(
            hits.is_empty(),
            "top-level benches/ engine reach should pass: {:?}",
            hits
        );
    }

    #[test]
    fn rejects_engine_reach_in_package_src_cfg_test_mod() {
        let dir = empty_workspace();
        // `src/` stays strict EVERYWHERE, including a `#[cfg(test)]` mod — not
        // because the reach ships (under `cargo build` the `test` cfg is OFF, so
        // it is compiled out) but because this line-based grep cannot reliably
        // scope a reach to a `#[cfg(test)]` mod; engine-backed tests belong in
        // `tests/` (blessed by check 8).
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    use streamlib::sdk::engine::HostSurfaceStoreExt;\n}\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PACKAGES_ENGINE_REACH),
            "src/ #[cfg(test)] engine reach must still fail, got {:?}",
            report.violations,
        );
    }

    #[test]
    fn rejects_engine_reach_in_package_src_tests_helper_dir() {
        let dir = empty_workspace();
        // A `src/tests/` helper dir is NOT a cargo test target — only the
        // package's TOP-LEVEL tests/ dir is exempt, so this deeper dir stays
        // covered. Locks the top-level-only distinction.
        write_fixture(
            dir.path(),
            "packages/newly-carved/src/tests/helpers.rs",
            "use streamlib::sdk::engine::HostSurfaceStoreExt;\n",
        );
        let report = scan_all(dir.path()).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.check == CHECK_PACKAGES_ENGINE_REACH),
            "src/tests/ helper dir must stay covered, got {:?}",
            report.violations,
        );
    }

    // ----- Check 12: transitive trunk-set -> engine walk (synthetic metadata) -----

    /// Build a synthetic `cargo metadata` document from workspace-member ids,
    /// `(id, name)` package pairs, and `(from_id, to_id, kind)` resolve edges
    /// where `kind` is `"normal"` (emitted as JSON `null`), `"build"`, or
    /// `"dev"`. Enough of the real shape for [`NormalBuildDepGraph::from_metadata`].
    fn synthetic_metadata(
        members: &[&str],
        names: &[(&str, &str)],
        edges: &[(&str, &str, &str)],
    ) -> serde_json::Value {
        use serde_json::json;
        let packages: Vec<_> = names
            .iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        let nodes: Vec<_> = names
            .iter()
            .map(|(id, _)| {
                let deps: Vec<_> = edges
                    .iter()
                    .filter(|(from, _, _)| from == id)
                    .map(|(_, to, kind)| {
                        let kind_val = if *kind == "normal" {
                            serde_json::Value::Null
                        } else {
                            json!(kind)
                        };
                        json!({ "pkg": to, "dep_kinds": [ { "kind": kind_val } ] })
                    })
                    .collect();
                json!({ "id": id, "deps": deps })
            })
            .collect();
        json!({
            "workspace_members": members,
            "packages": packages,
            "resolve": { "nodes": nodes },
        })
    }

    #[test]
    fn transitive_trunk_reaches_engine_through_intermediate_fails() {
        // plugin-sdk -> intermediate -> engine, all normal edges: the walk must
        // find the chain AND print it end to end.
        let md = synthetic_metadata(
            &["sdk", "mid", "eng"],
            &[
                ("sdk", "streamlib-plugin-sdk"),
                ("mid", "streamlib-intermediate"),
                ("eng", "streamlib-engine"),
            ],
            &[("sdk", "mid", "normal"), ("mid", "eng", "normal")],
        );
        let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
        let chains = find_trunk_engine_chains(&graph);
        assert_eq!(chains.len(), 1, "expected one chain, got {:?}", chains);
        assert_eq!(chains[0].trunk, "streamlib-plugin-sdk");
        assert_eq!(
            chains[0].display_chain(),
            "streamlib-plugin-sdk -> streamlib-intermediate -> streamlib-engine",
        );
    }

    #[test]
    fn transitive_trunk_reaches_engine_only_via_dev_edge_passes() {
        // plugin-sdk -> engine exists ONLY through a dev-only edge; the shared
        // `dep_kinds` filter drops it, so no chain is reported. Locks the
        // dev-dep exemption — without the filter this would false-red.
        let md = synthetic_metadata(
            &["sdk", "eng"],
            &[("sdk", "streamlib-plugin-sdk"), ("eng", "streamlib-engine")],
            &[("sdk", "eng", "dev")],
        );
        let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
        let chains = find_trunk_engine_chains(&graph);
        assert!(
            chains.is_empty(),
            "dev-only edge must not form a chain: {:?}",
            chains
        );
    }

    #[test]
    fn transitive_consumer_rhi_reaches_engine_through_intermediate_fails() {
        // consumer-rhi is the 4th trunk root (#1252 follow-up): its transitive
        // normal + build closure must also be walked. consumer-rhi -> mid ->
        // engine, all normal edges — the walk must find the chain and attribute
        // it to consumer-rhi. Locks consumer-rhi's root membership in the
        // transitive check specifically, distinct from the plugin-sdk case.
        let md = synthetic_metadata(
            &["crhi", "mid", "eng"],
            &[
                ("crhi", "streamlib-consumer-rhi"),
                ("mid", "streamlib-intermediate"),
                ("eng", "streamlib-engine"),
            ],
            &[("crhi", "mid", "normal"), ("mid", "eng", "normal")],
        );
        let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
        let chains = find_trunk_engine_chains(&graph);
        assert_eq!(chains.len(), 1, "expected one chain, got {:?}", chains);
        assert_eq!(chains[0].trunk, "streamlib-consumer-rhi");
        assert_eq!(
            chains[0].display_chain(),
            "streamlib-consumer-rhi -> streamlib-intermediate -> streamlib-engine",
        );
    }

    #[test]
    fn transitive_trunk_clean_graph_passes() {
        // plugin-sdk -> some other member, engine present but unreachable.
        let md = synthetic_metadata(
            &["sdk", "mid", "eng"],
            &[
                ("sdk", "streamlib-plugin-sdk"),
                ("mid", "streamlib-other"),
                ("eng", "streamlib-engine"),
            ],
            &[("sdk", "mid", "normal")],
        );
        let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
        let chains = find_trunk_engine_chains(&graph);
        assert!(
            chains.is_empty(),
            "clean graph must yield no chain: {:?}",
            chains
        );
    }
}
