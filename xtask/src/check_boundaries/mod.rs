// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Boundary CI check over Cargo manifests and the dependency graph — Layer 6 of the regression-prevention defense for
//! the Vulkan RHI capability split.
//!
//! Cargo-manifest and dependency-graph checks, each over the workspace,
//! allowlists driven by data: no `ash` or out-of-place `vulkanalia` Cargo dep,
//! cdylibs and adapter crates on `streamlib-consumer-rhi` rather than the full
//! `streamlib` crate, every `vulkanalia*` dep inherited from the workspace
//! fork, no full facade in a package, and no engine reachable from the trunk
//! crates or the engine-free clients. The source-pattern halves (imports,
//! privileged Vulkan calls, top-level shortcuts, engine reach from packages)
//! are ast-grep rules in `ast-grep-rules/rules/`.
//!
//! The manifest checks are sub-second on a clean runner and need no Cargo
//! build. Allowlists are explicit and carry per-entry rationale
//! so future contributors can decide whether to extend the allowlist or
//! refactor the offending file.

use crate::normal_build_dep_graph::NormalBuildDepGraph;
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
    let dependency_graph = NormalBuildDepGraph::from_metadata(
        &crate::run_cargo_metadata_resolve_document(project_root)?,
    )?;
    // `find_trunk_engine_chains` — transitive trunk-set -> engine walk (cargo metadata based;
    // layered on the direct manifest `check_trunk_set_no_engine_dep` inside scan_all).
    let engine_chains = find_trunk_engine_chains(&dependency_graph);
    for chain in &engine_chains {
        eprintln!(
            "[{}] trunk crate `{}` transitively depends on `{}`: {}\n    {}",
            CHECK_TRUNK_NO_ENGINE_DEP,
            chain.root_crate,
            TRUNK_ENGINE_CRATE_NAME,
            chain.display_chain(),
            TRUNK_NO_ENGINE_DEP_RATIONALE,
        );
    }
    // `find_engine_free_client_engine_chains` — the engine-free clients' closure holds no engine-carrying crate.
    let client_engine_chains = find_engine_free_client_engine_chains(&dependency_graph)?;
    for chain in &client_engine_chains {
        tracing::error!(
            "[{}] `{}` links `{}`: {}\n    {}",
            CHECK_CLIENT_LINKS_NO_ENGINE,
            chain.root_crate,
            chain.chain.last().map(String::as_str).unwrap_or_default(),
            chain.display_chain(),
            CLIENT_LINKS_NO_ENGINE_RATIONALE,
        );
    }
    let total_violations =
        report.violations.len() + engine_chains.len() + client_engine_chains.len();
    if total_violations == 0 {
        println!(
            "check-boundaries: {} file(s) scanned, no violations",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "check-boundaries: {} violation(s) ({} manifest + {} transitive trunk->engine chain(s) + {} engine-free client->engine chain(s)) across {} file(s) scanned",
            total_violations,
            report.violations.len(),
            engine_chains.len(),
            client_engine_chains.len(),
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
    check_vulkanalia_uses_workspace_fork(project_root, &mut violations, &mut files_scanned)?;
    check_packages_facade_runtime_dep(project_root, &mut violations, &mut files_scanned)?;
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
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Workspace traversal
// ---------------------------------------------------------------------------

/// Top-level directories whose `Cargo.toml` manifests are checked. Skips
/// `target/`, `.git/`, vendored node_modules, etc. by construction. `xtask`
/// is excluded: it is build tooling with no Vulkan deps.
const SCAN_ROOTS: &[&str] = &[
    "runtime", "sdk", "adapters", "vendor", "examples", "packages",
];

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
// no `ash`
// ---------------------------------------------------------------------------

const CHECK_NO_ASH: &str = "no-ash";

const ASH_RATIONALE: &str = "ash is fully replaced by vulkanalia (#252); reintroducing it splits the workspace's GPU API surface";

fn check_no_ash(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
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
// `vulkanalia` Cargo dep confined to the RHI crates
// ---------------------------------------------------------------------------

const CHECK_VULKANALIA: &str = "vulkanalia-only-in-rhi";

const VULKANALIA_RATIONALE: &str = "raw vulkanalia must stay inside the RHI / consumer-rhi / adapter crates and a small set of documented-exception files";

fn check_vulkanalia_confined(
    project_root: &Path,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    // The allowlist is keyed on crate roots. Matching is against the full
    // Cargo.toml file path so trailing-slash directory boundaries hit
    // (`runtime/streamlib-engine/Cargo.toml` matches `runtime/streamlib-engine/`, but
    // `sdk/streamlib-error/Cargo.toml` does not).
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
/// crate qualifies if at least one file under it is listed in the `ignores:` of
/// `ast-grep-rules/rules/no-vulkanalia-outside-the-rhi.yml`; declaring the
/// dep elsewhere is a regression.
const VULKANALIA_CARGO_DEP_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "runtime/streamlib-engine/",
        kind: AllowKind::PathPrefix,
        rationale: "host crate: src/vulkan/ owns the RHI",
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
    AllowEntry {
        path: "runtime/streamlib-vulkan-video/",
        kind: AllowKind::PathPrefix,
        rationale: "Vulkan Video codec tier of the RHI",
    },
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
// cdylibs and adapter crates depend on consumer-rhi, NOT streamlib
// ---------------------------------------------------------------------------

const CHECK_CDYLIB_DEPS: &str = "no-streamlib-in-runtime-deps";

const CDYLIB_DEP_RATIONALE: &str = "cdylibs and adapter crates must depend on streamlib-consumer-rhi (carve-out), not the full streamlib crate, so the FullAccess capability boundary is type-system enforced";

/// Crates whose runtime dep graph must not include `streamlib`. `streamlib`
/// is allowed only in `[dev-dependencies]` (or `[target.*.dev-dependencies]`).
const NO_STREAMLIB_RUNTIME_DEP: &[&str] = &[
    // These sit directly beneath every adapter core below, so a `streamlib`
    // dep in any would reach all of their runtime graphs at once.
    "adapters/streamlib-surface-adapter/Cargo.toml",
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
// every member crate's vulkanalia* dep is workspace-inherited
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

fn dep_is_workspace_inherited(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|t| t.get("workspace"))
        .and_then(|w| w.as_bool())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// `packages/*` crates must not link the full `streamlib` facade
// ---------------------------------------------------------------------------
//
// A distributable package is built engine-free against the authoring
// SDK; carrying the full `streamlib` facade as a runtime dep pulls the
// FullAccess engine surface into a crate that ships as a source-only package.
// The facade is host-by-design only for `test-fixtures` (host-side `cargo
// test`); every other facade linker is the shrinking conversion backlog and
// drops off this allowlist as its package converts to the engine-free
// authoring SDK.
//
// This is a per-entry-rationale ratchet seeded to the exact current violating
// set: any NEW `packages/*` crate that adds a non-dev `streamlib` dep fails.
// Mirrors `check_cdylib_and_adapter_runtime_deps` (`iter_dep_entries` + `section_is_dev_only`), but as a
// tree-wide ratchet over `packages/*` rather than a fixed crate list.

const CHECK_PACKAGES_FACADE_DEP: &str = "packages-no-facade-runtime-dep";

const PACKAGES_FACADE_DEP_RATIONALE: &str = "a packages/* crate must not carry the full `streamlib` facade as a runtime dep — a distributable package builds engine-free against the authoring SDK; the facade is host-by-design only for test-fixtures. Move it to [dev-dependencies] or convert the package to the engine-free authoring SDK";

/// The `packages/*` crates that link the `streamlib` facade as a non-dev
/// runtime dep. `test-fixtures` is the only permitted linker (host-by-design);
/// every distributable package builds engine-free against the plugin-authoring
/// SDK. A newly-carved package is NOT added here — it converts to the
/// engine-free SDK instead.
const PACKAGES_FACADE_DEP_ALLOWLIST: &[AllowEntry] = &[AllowEntry {
    path: "packages/test-fixtures/Cargo.toml",
    kind: AllowKind::ExactFile,
    rationale: "permanent, host-by-design: test-fixtures run host-side under cargo test and legitimately link the full facade",
}];

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
// the engine-free TRUNK SET must never Cargo-dep the engine
// ---------------------------------------------------------------------------
//
// MEMBERSHIP RULE for the trunk root set: roots = every crate packages are
// mandated or expected to link directly and consume by version. The
// engine-free trunk roots — `streamlib-macros` (sdk/streamlib-macros) and
// `streamlib-consumer-rhi` (runtime/streamlib-consumer-rhi) — are the
// authoring substrate consumers link against. A non-dev `[dependencies]`
// entry that resolves to the engine crate (`streamlib-engine` at
// runtime/streamlib-engine) in either statically pulls the FullAccess engine
// surface into that substrate.
//
// The list is PRINCIPLED, not enumerated ad hoc: a crate earns root status by
// being something packages dep DIRECTLY and consume by version. The small
// utility crates (`streamlib-error`, `streamlib-processor-schema`) are
// deliberately NOT listed — they are covered
// TRANSITIVELY through the roots (`find_trunk_engine_chains` walks the closure), so a listing
// would be redundant. `streamlib-consumer-rhi` earns root status on its own
// because `check_cdylib_and_adapter_runtime_deps` makes it a first-class part of the boundary contract:
// adapter crates dep it directly (in place of the full facade), so consumers
// link it by version — an engine dep in it would propagate the FullAccess
// surface to external consumers.
//
// `[dev-dependencies]` are EXEMPT — a trunk crate's conformance tests may
// legitimately pull the engine to exercise the host backing. Reuses the same
// `iter_dep_entries_with_values` + `section_is_dev_only` machinery as
// `check_packages_facade_runtime_dep`, and resolves `package = "..."` alias keys (an entry
// `foo = { package = "streamlib-engine", ... }` is caught by its resolved
// package name, not the section key) — closing the exact alias evasion the
// facade check leaves as a known low.

const CHECK_TRUNK_NO_ENGINE_DEP: &str = "trunk-set-no-engine-cargo-dep";

const TRUNK_NO_ENGINE_DEP_RATIONALE: &str = "PERMANENT trunk ban: an engine-free trunk root (streamlib-macros / streamlib-consumer-rhi) must never carry `streamlib-engine` as a non-dev Cargo dep. MEMBERSHIP RULE: roots = every crate packages are mandated or expected to link directly and consume by version — the small utility crates (streamlib-error / streamlib-processor-schema) are covered transitively through the roots, and consumer-rhi earns root status because adapter crates must dep it in place of the facade, which makes it a first-class part of the boundary contract. External packages consume these roots by version from the registry, so a published root that pulled the engine would propagate the FullAccess engine surface to every external consumer invisibly. This ban has no shrinking allowlist; [dev-dependencies] are exempt (conformance tests may pull the engine)";

/// The engine crate's Cargo package name (lib name is `streamlib_engine`; the
/// Cargo dependency key / `package =` rename resolves to this hyphenated form).
const TRUNK_ENGINE_CRATE_NAME: &str = "streamlib-engine";

/// The engine-free trunk roots whose non-dev dep graph must never resolve to
/// `streamlib-engine`. A fixed list (mirrors `check_cdylib_and_adapter_runtime_deps`'s
/// `NO_STREAMLIB_RUNTIME_DEP`) — this is a permanent invariant, not a
/// shrinking ratchet. Roots = every crate packages are mandated or expected to
/// link directly and consume by version; the small utility crates
/// (streamlib-error / streamlib-processor-schema) are
/// covered transitively through these roots (`find_trunk_engine_chains`), and consumer-rhi is a
/// root because `check_cdylib_and_adapter_runtime_deps` makes it a first-class part of the boundary contract.
const TRUNK_NO_ENGINE_DEP: &[&str] = &[
    "sdk/streamlib-macros/Cargo.toml",
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
            // this is enforcement, not discovery (mirrors `check_cdylib_and_adapter_runtime_deps`).
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
// the trunk set must never TRANSITIVELY reach the engine
// ---------------------------------------------------------------------------
//
// `check_trunk_set_no_engine_dep` is the DIRECT manifest scan: it
// catches a `streamlib-engine` dep written straight into a trunk crate's
// `Cargo.toml`, with precise file:line reporting. But a trunk crate can reach
// the engine INDIRECTLY — `streamlib-macros` -> some workspace crate ->
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
// `--filter-platform` is deliberately NOT passed: a trunk crate's
// `streamlib-consumer-rhi` dep is linux-target and must stay covered.
//
// On a violation the offending CHAIN is printed
// (`<trunk> -> … -> streamlib-engine`) so the intermediate edge is obvious.
// This has NO allowlist and never shrinks — same permanence as `check_trunk_set_no_engine_dep`.

/// The engine-free trunk root names whose transitive normal + build
/// closure must never reach [`TRUNK_ENGINE_CRATE_NAME`]. Mirrors `check_trunk_set_no_engine_dep`'s
/// `TRUNK_NO_ENGINE_DEP` (which keys on manifest paths); here we key on package
/// names because the walk resolves through `cargo metadata`. Roots = every
/// crate packages are mandated or expected to link directly and consume by
/// version; consumer-rhi is a root because `check_cdylib_and_adapter_runtime_deps` makes it a first-class part
/// of the boundary contract (packages dep it directly).
const TRUNK_CRATE_NAMES: &[&str] = &["streamlib-macros", "streamlib-consumer-rhi"];

/// A root crate's normal + build dependency chain to a crate its closure must
/// never hold, as package names from the root to that crate inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyChainToAForbiddenCrate {
    /// The crate the chain starts at.
    pub root_crate: String,
    /// Package names from the root crate to the forbidden crate, inclusive and
    /// in traversal order.
    pub chain: Vec<String>,
}

impl DependencyChainToAForbiddenCrate {
    /// Render the chain as `<root> -> … -> <forbidden crate>`.
    pub fn display_chain(&self) -> String {
        self.chain.join(" -> ")
    }
}

/// Walk the transitive normal + build closure of each trunk crate over
/// WORKSPACE MEMBERS ONLY and return every chain that reaches
/// `streamlib-engine`. Pure over the parsed graph so it unit-tests against a
/// synthetic `cargo metadata` fixture without the live tree.
pub fn find_trunk_engine_chains(
    graph: &NormalBuildDepGraph,
) -> Vec<DependencyChainToAForbiddenCrate> {
    find_dependency_chains_to_forbidden_crates(graph, TRUNK_CRATE_NAMES, &[TRUNK_ENGINE_CRATE_NAME])
}

/// For each workspace crate named in `root_crate_names`, the shortest chain over
/// workspace-member normal + build edges to any crate named in
/// `forbidden_crate_names`, when one exists.
fn find_dependency_chains_to_forbidden_crates(
    graph: &NormalBuildDepGraph,
    root_crate_names: &[&str],
    forbidden_crate_names: &[&str],
) -> Vec<DependencyChainToAForbiddenCrate> {
    let mut chains = Vec::new();
    for &root_crate_name in root_crate_names {
        for root_id in graph.ids_named(root_crate_name) {
            // A root resolves to a workspace member; skip any same-named
            // external package (cannot reach an in-tree crate anyway).
            if !graph.is_workspace_member(root_id) {
                continue;
            }
            if let Some(chain_ids) =
                shortest_member_chain_to_a_forbidden_crate(graph, root_id, forbidden_crate_names)
            {
                chains.push(DependencyChainToAForbiddenCrate {
                    root_crate: root_crate_name.to_string(),
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
/// edges, returning the shortest id path (root..=forbidden crate) that reaches
/// a crate named in `forbidden_crate_names`, or `None` if none is reachable.
fn shortest_member_chain_to_a_forbidden_crate<'graph>(
    graph: &'graph NormalBuildDepGraph,
    root_id: &'graph str,
    forbidden_crate_names: &[&str],
) -> Option<Vec<&'graph str>> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut predecessor: HashMap<&str, &str> = HashMap::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    visited.insert(root_id);
    queue.push_back(root_id);
    while let Some(id) = queue.pop_front() {
        if id != root_id
            && graph
                .name_of(id)
                .is_some_and(|name| forbidden_crate_names.contains(&name))
        {
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
            // depend on an in-tree crate, so it cannot lie on the chain. Every
            // forbidden crate is a member, so this never prunes the target.
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

// ---------------------------------------------------------------------------
// `tatolab` and the runtime client contract link no engine
// ---------------------------------------------------------------------------
//
// `tatolab` reaches a running runtime only through the files it leaves on disk
// and its local API socket, both spelled by the runtime client contract, so
// neither crate's normal + build closure may hold a crate that carries the
// engine. Same walk as `find_trunk_engine_chains`, over the same `cargo metadata` graph.

const CHECK_CLIENT_LINKS_NO_ENGINE: &str = "client-crates-link-no-engine";

const CLIENT_LINKS_NO_ENGINE_RATIONALE: &str = "`tatolab` and streamlib-runtime-client-contract reach a running runtime only through its files and its local API socket, so their normal + build closure must never hold streamlib-engine, the `streamlib` facade, streamlib-api-server or streamlib-python-wheel. Move what the client needs into streamlib-runtime-client-contract (or another engine-free crate) rather than depending on one of those. [dev-dependencies] are exempt";

/// The crates whose normal + build closure must never hold a crate named in
/// [`ENGINE_CARRYING_CRATE_NAMES`].
const ENGINE_FREE_CLIENT_CRATE_NAMES: &[&str] =
    &["tatolab-cli", "streamlib-runtime-client-contract"];

/// The engine, and the workspace crates that carry it into whatever links them.
const ENGINE_CARRYING_CRATE_NAMES: &[&str] = &[
    "streamlib-engine",
    "streamlib",
    "streamlib-api-server",
    "streamlib-python-wheel",
];

/// Every engine-free client crate → engine-carrying crate chain in `graph`.
///
/// Refuses a graph missing any crate either list names: a renamed crate would
/// otherwise make the walk pass without reading what it guards.
pub fn find_engine_free_client_engine_chains(
    graph: &NormalBuildDepGraph,
) -> Result<Vec<DependencyChainToAForbiddenCrate>> {
    for &named_crate in ENGINE_FREE_CLIENT_CRATE_NAMES
        .iter()
        .chain(ENGINE_CARRYING_CRATE_NAMES)
    {
        anyhow::ensure!(
            graph
                .ids_named(named_crate)
                .into_iter()
                .any(|id| graph.is_workspace_member(id)),
            "[{CHECK_CLIENT_LINKS_NO_ENGINE}] no workspace crate is named `{named_crate}` — \
             the crate was renamed or removed, so update the gate's lists in \
             xtask/src/check_boundaries/mod.rs rather than let it pass without reading it"
        );
    }
    Ok(find_dependency_chains_to_forbidden_crates(
        graph,
        ENGINE_FREE_CLIENT_CRATE_NAMES,
        ENGINE_CARRYING_CRATE_NAMES,
    ))
}

/// True iff `rel` (a workspace-relative path) begins with `prefix`
/// (`/`-separated, e.g. `"packages/"`).
fn rel_starts_with(rel: &Path, prefix: &str) -> bool {
    rel.to_string_lossy().replace('\\', "/").starts_with(prefix)
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

#[cfg(test)]
mod dependency_graph_walk_tests;
#[cfg(test)]
mod manifest_check_tests;
