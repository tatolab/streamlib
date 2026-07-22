// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared package-artifact assembly.
//!
//! One routine — [`assemble_artifact`] — turns a package source
//! directory into a *complete* loadable artifact, per language:
//!
//! - **Rust** — `cargo build [--release] -p <crate>` → prebuilt cdylib
//!   at `lib/<triple>/`, plus the crate source (`Cargo.toml` + `src/` …)
//!   so a host on another triple can rebuild ("sdist + one-triple wheel").
//! - **Python** — the full source tree (every `.py` + data / assets /
//!   models + `pyproject.toml` + `uv.lock`). No wheel is built: the engine
//!   runs a Python processor from its source dir, so only its dependencies
//!   are installed at load time, and shipping identical source in dev and
//!   in the artifact removes the editable-vs-wheel packaging skew.
//! - **Deno** — the full authored source tree (every `.ts` + `deno.json`
//!   + `.npmrc` + assets), staged verbatim at the package root. Like
//!   Python, nothing is relocated: the staged layout is a faithful
//!   mirror of what the developer wrote, so relative resolution
//!   (sibling `streamlib.yaml`, `./_generated_/…`, asset paths) holds
//!   identically in dev and in the artifact.
//! - **always** — `streamlib.yaml` + `schemas/`.
//!
//! The same assembly emits to either of two [`AssembleTarget`]s: a
//! compressed `.slpkg` (what `streamlib pack` ships) or an extracted
//! staged directory (what `streamlib-build-orchestrator` materializes
//! into the package cache at runtime). Both shapes are byte-identical
//! per file — a runtime-built staged dir is exactly what extracting the
//! corresponding `.slpkg` would produce.
//!
//! This crate intentionally does NOT depend on `streamlib-engine` or the
//! `streamlib` SDK — it sits on the lean schema/idents/cargo-build
//! crates so both the CLI and the runtime orchestrator can call it
//! without a dependency cycle.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use streamlib_idents::{DependencySpec, Manifest};
use streamlib_processor_schema::ProcessorLanguage;

pub use streamlib_cargo_build::CargoProfile;

pub mod catalog;
pub mod dependency_reconcile;
pub mod static_registry;

// The `streamlib link` marker schema + discovery moved to `streamlib-idents`
// (its natural home alongside the manifest/lockfile types) so the engine module
// loader — which deps `streamlib-idents` but not `streamlib-pack` — can reach
// it. `streamlib-pack` still uses it for the pack/publish "refuse while linked"
// guard.
use streamlib_idents::link_marker;

/// One member of the engine release closure: a publishable workspace library
/// crate, with its version and manifest directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCrate {
    pub name: String,
    pub version: String,
    /// Directory holding the crate's `Cargo.toml` (its manifest dir).
    pub manifest_dir: PathBuf,
}

/// The engine **release closure** — every publishable `streamlib*` /
/// `vulkan-jpeg` library crate in the workspace, in dependency (topological)
/// publish order (a crate always precedes its dependents).
///
/// This is the single, only definition of "the set of crates a release
/// publishes." There is deliberately no "SDK-subset vs. all-libs" switch: the
/// closure is the full linkable set by construction, so the easy-to-skip
/// libraries (`streamlib-plugin-sdk`, `vulkan-jpeg`) are members by definition
/// — the foot-gun a human-remembered "publish everything" flag used to hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseClosure {
    pub crates: Vec<ReleaseCrate>,
}

impl ReleaseClosure {
    /// The closure member names in topological publish order.
    pub fn names(&self) -> Vec<&str> {
        self.crates.iter().map(|c| c.name.as_str()).collect()
    }
}

/// Whether a crate name belongs to the streamlib release closure *by name*:
/// every workspace library crate named `streamlib*` (SDK, engine, macros,
/// plugin ABI, adapters, consumer-rhi, …), `vulkan-jpeg`, plus the vendored
/// `tatolab-vulkanalia*` fork crates. The full closure predicate also
/// requires a library target and a publishable `publish` setting — see
/// [`compute_release_closure`].
pub fn is_linkable_crate_name(name: &str) -> bool {
    name.starts_with("streamlib") || name == "vulkan-jpeg" || name.starts_with("tatolab-vulkanalia")
}

/// The library-target kinds a publishable crate must expose. A crate with
/// only a `bin` target (the CLI / runtime binaries) is excluded.
const RELEASE_CLOSURE_LIB_KINDS: &[&str] =
    &["lib", "rlib", "cdylib", "proc-macro", "dylib", "staticlib"];

fn json_has_library_target(pkg: &serde_json::Value) -> bool {
    pkg.get("targets")
        .and_then(|t| t.as_array())
        .is_some_and(|targets| {
            targets.iter().any(|t| {
                t.get("kind")
                    .and_then(|k| k.as_array())
                    .is_some_and(|kinds| {
                        kinds
                            .iter()
                            .filter_map(|k| k.as_str())
                            .any(|k| RELEASE_CLOSURE_LIB_KINDS.contains(&k))
                    })
            })
        })
}

/// `publish == []` in cargo metadata means `publish = false` — cargo refuses
/// to publish it, so it's excluded from the closure.
fn json_is_publishable(pkg: &serde_json::Value) -> bool {
    !pkg.get("publish")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.is_empty())
}

/// True iff a `deps[]` entry from `cargo metadata`'s resolve graph is a normal
/// or build edge (`kind` `null` or `"build"`) — i.e. it participates in the
/// publish / link closure. A dev-only edge returns `false`.
///
/// This is the single definition of a closure-participating edge, shared by
/// [`NormalBuildDepGraph`] and every consumer that walks the resolved graph.
/// Dropping dev-only edges is load-bearing: a crate's conformance test may pull
/// a heavy dependency through `[dev-dependencies]`, and counting that edge as a
/// real link would false-report the dependency as linked into the crate.
pub fn resolve_dep_is_normal_or_build(dep: &serde_json::Value) -> bool {
    dep.get("dep_kinds")
        .and_then(|k| k.as_array())
        .map(|kinds| {
            kinds
                .iter()
                .any(|k| matches!(k.get("kind").and_then(|v| v.as_str()), None | Some("build")))
        })
        .unwrap_or(true)
}

/// A `cargo metadata` dependency graph reduced to the edges that participate in
/// the publish / link closure: normal + build edges only, keyed by package id.
///
/// Dev-only edges are dropped via [`resolve_dep_is_normal_or_build`]. Sharing
/// this one construction between the release-closure DFS and the xtask
/// trunk-set → engine transitive boundary walk keeps the dev-edge-dropping
/// `dep_kinds` filter defined exactly once — reinventing it risks one walker
/// counting a dev-dependency as a real link edge and the other not.
#[derive(Debug, Clone)]
pub struct NormalBuildDepGraph {
    deps_by_id: std::collections::HashMap<String, Vec<String>>,
    name_by_id: std::collections::HashMap<String, String>,
    workspace_member_ids: std::collections::HashSet<String>,
}

impl NormalBuildDepGraph {
    /// Build the graph from a parsed `cargo metadata --format-version 1`
    /// document. Errors if the document carries no `resolve` graph (produced
    /// with `--no-deps`), since the dependency edges are then absent.
    pub fn from_metadata(metadata: &serde_json::Value) -> Result<Self> {
        let workspace_member_ids: std::collections::HashSet<String> = metadata
            .get("workspace_members")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let empty = Vec::new();
        let packages = metadata
            .get("packages")
            .and_then(|p| p.as_array())
            .unwrap_or(&empty);
        let mut name_by_id = std::collections::HashMap::new();
        for pkg in packages {
            if let (Some(id), Some(name)) = (
                pkg.get("id").and_then(|v| v.as_str()),
                pkg.get("name").and_then(|v| v.as_str()),
            ) {
                name_by_id.insert(id.to_string(), name.to_string());
            }
        }

        let resolve_nodes = metadata
            .get("resolve")
            .and_then(|r| r.get("nodes"))
            .and_then(|n| n.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!("cargo metadata has no resolve graph (ran with --no-deps?)")
            })?;
        let mut deps_by_id = std::collections::HashMap::new();
        for node in resolve_nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let mut deps = Vec::new();
            if let Some(dep_arr) = node.get("deps").and_then(|d| d.as_array()) {
                for dep in dep_arr {
                    if resolve_dep_is_normal_or_build(dep)
                        && let Some(pkg) = dep.get("pkg").and_then(|v| v.as_str())
                    {
                        deps.push(pkg.to_string());
                    }
                }
            }
            deps_by_id.insert(id.to_string(), deps);
        }

        Ok(Self {
            deps_by_id,
            name_by_id,
            workspace_member_ids,
        })
    }

    /// The package name for `id`, if the metadata carried it.
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.name_by_id.get(id).map(|s| s.as_str())
    }

    /// True iff `id` is a workspace member (from `workspace_members`).
    pub fn is_workspace_member(&self, id: &str) -> bool {
        self.workspace_member_ids.contains(id)
    }

    /// The normal + build dependency ids of `id` (dev-only edges already
    /// dropped). Empty for an unknown id.
    pub fn normal_build_deps(&self, id: &str) -> &[String] {
        self.deps_by_id.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Every workspace-member package id.
    pub fn workspace_member_ids(&self) -> impl Iterator<Item = &str> {
        self.workspace_member_ids.iter().map(|s| s.as_str())
    }

    /// Every package id whose name equals `name`.
    pub fn ids_named<'graph>(&'graph self, name: &str) -> Vec<&'graph str> {
        self.name_by_id
            .iter()
            .filter(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id.as_str())
            .collect()
    }
}

/// Compute the engine release closure from a live `cargo metadata` run at
/// `workspace_root`. The predicate — workspace member, [`is_linkable_crate_name`],
/// a library target, and a publishable `publish` setting — is the *only*
/// definition of the closure; the topological ordering is derived from the
/// resolved dependency graph so it stays correct as the graph shifts.
pub fn compute_release_closure(workspace_root: &Path) -> Result<ReleaseClosure> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let output = Command::new("cargo")
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
    let md: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata JSON")?;

    // Normal + build dependency graph — the shared `dep_kinds`-filtered
    // construction (dev-only edges dropped). Same machinery the xtask trunk-set
    // boundary walk rides, so both agree on which edges participate in the
    // link closure.
    let graph = NormalBuildDepGraph::from_metadata(&md)?;

    let empty = Vec::new();
    let packages = md
        .get("packages")
        .and_then(|p| p.as_array())
        .unwrap_or(&empty);
    // id → package json, for version / manifest_path / library-target / publish
    // lookups the graph doesn't carry.
    let mut pkg_by_id: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for pkg in packages {
        if let Some(id) = pkg.get("id").and_then(|v| v.as_str()) {
            pkg_by_id.insert(id, pkg);
        }
    }
    // An id is "internal" (walked + publishable) when it's a workspace member
    // that satisfies the full closure predicate.
    let is_internal = |id: &str| -> bool {
        if !graph.is_workspace_member(id) {
            return false;
        }
        let Some(pkg) = pkg_by_id.get(id) else {
            return false;
        };
        graph.name_of(id).is_some_and(is_linkable_crate_name)
            && json_has_library_target(pkg)
            && json_is_publishable(pkg)
    };

    // Post-order DFS over internal deps ⇒ topological publish order.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut order: Vec<&str> = Vec::new();
    // Iterative DFS to avoid recursion-depth concerns on large graphs.
    let mut roots: Vec<&str> = graph
        .workspace_member_ids()
        .filter(|id| is_internal(id))
        .collect();
    roots.sort_by_key(|id| graph.name_of(id).unwrap_or_default().to_string());
    for root in roots {
        let mut stack: Vec<(&str, bool)> = vec![(root, false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                if !order.contains(&id) {
                    order.push(id);
                }
                continue;
            }
            if seen.contains(id) {
                continue;
            }
            seen.insert(id);
            stack.push((id, true));
            for dep in graph.normal_build_deps(id) {
                let dep = dep.as_str();
                if is_internal(dep) && !seen.contains(dep) {
                    stack.push((dep, false));
                }
            }
        }
    }

    let crates = order
        .into_iter()
        .map(|id| {
            let pkg = pkg_by_id[id];
            let name = graph.name_of(id).unwrap_or_default().to_string();
            let version = pkg
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let manifest_dir = pkg
                .get("manifest_path")
                .and_then(|v| v.as_str())
                .and_then(|m| Path::new(m).parent().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            ReleaseCrate {
                name,
                version,
                manifest_dir,
            }
        })
        .collect();
    Ok(ReleaseClosure { crates })
}

/// Which child-process stream a build-log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackStream {
    Stdout,
    Stderr,
}

/// Sink for build diagnostics emitted during assembly. The CLI forwards
/// to stdout/`tracing`; the runtime orchestrator adapts it to the
/// engine's `BuildEventSink` so logs flow to a daemon / UI. The unit
/// type is a no-op sink for callers that don't care.
pub trait PackEventSink: Send + Sync {
    /// A per-language build step began (`"rust"` / `"python"`).
    fn started(&self, _language: &str) {}
    /// One line of build-tool output.
    fn line(&self, _stream: PackStream, _line: &str) {}
    /// A per-language build step finished.
    fn finished(&self, _language: &str) {}
}

impl PackEventSink for () {}

/// Where [`assemble_artifact`] writes the assembled package.
#[derive(Debug, Clone)]
pub enum AssembleTarget {
    /// Write a compressed `.slpkg` zip at this path (the distribution
    /// artifact `streamlib pack` ships).
    Slpkg(PathBuf),
    /// Materialize the extracted package layout into this directory (the
    /// shape an extracted `.slpkg` / a GitHub install lands in). The
    /// directory is assumed to already exist and be empty; the caller
    /// owns the build-to-temp + atomic-rename dance.
    StagedDir(PathBuf),
}

/// How `dependencies` / `patch` `path:` entries in the manifest are
/// treated when the manifest is written into the artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDepPolicy {
    /// Reject path-flavor `patch:` entries (publishing semantics — paths
    /// are relative to the dev's source tree and don't generalize to a
    /// distributed `.slpkg`). Used by `streamlib pack`.
    RejectPathPatches,
    /// Rewrite relative `path:` deps/patches to absolute, anchored at the
    /// original source dir. Used when staging into the cache: the package
    /// is relocated out of its source tree, so a `path: ../core` would
    /// otherwise dangle. Keeps the transitive-dep walk resolving each dep
    /// to its real source.
    RewriteRelativeToAbsolute,
}

/// Knobs for [`assemble_artifact`].
#[derive(Debug, Clone)]
pub struct AssembleOptions {
    /// Skip auto-build: require `lib/<triple>/` (Rust) and
    /// `python/wheels/` (Python) to be pre-populated. Mirrors
    /// `streamlib pack --no-build`.
    pub no_build: bool,
    /// Cargo profile for the Rust cdylib build.
    pub profile: CargoProfile,
    /// Manifest `path:` handling.
    pub path_deps: PathDepPolicy,
}

/// Summary of what [`assemble_artifact`] produced.
#[derive(Debug, Clone)]
pub struct AssembleOutcome {
    pub package_name: String,
    pub package_version: String,
    pub schemas: usize,
    pub processors: usize,
    pub python_wheels: usize,
    /// Whether a compiler / wheel-builder actually ran (vs. everything
    /// pre-built or no-build).
    pub rebuilt: bool,
}

/// Assemble a complete package artifact from `pkg_dir` into `target`.
pub fn assemble_artifact(
    pkg_dir: &Path,
    target: &AssembleTarget,
    opts: &AssembleOptions,
    sink: &dyn PackEventSink,
) -> Result<AssembleOutcome> {
    assemble_artifact_with_cargo_config(pkg_dir, target, opts, sink, &[], None)
}

/// [`assemble_artifact`], plus the two `streamlib link` toolchain overrides the
/// build orchestrator threads when a link is active:
///
/// - `cargo_config_files` — extra cargo `--config <file>` TOML files (the
///   consumer's `streamlib link`-emitted `.cargo/config.toml`) so a staged
///   package cdylib resolves its `streamlib*` **crate** deps from the linked
///   checkout (the `[patch."<index>"]` block) instead of the registry.
/// - `link_checkout` — the linked checkout path, threaded to the package's
///   `build.rs` via [`streamlib_idents::LINK_CHECKOUT_ENV`] so its **schema**
///   deps resolve from `<checkout>/packages/<name>` too — completing the
///   zero-registry dev loop (host + plugin + schemas from one source tree).
///
/// Empty slice + `None` ⇒ identical to [`assemble_artifact`].
pub fn assemble_artifact_with_cargo_config(
    pkg_dir: &Path,
    target: &AssembleTarget,
    opts: &AssembleOptions,
    sink: &dyn PackEventSink,
    cargo_config_files: &[PathBuf],
    link_checkout: Option<&Path>,
) -> Result<AssembleOutcome> {
    let config = streamlib_cargo_build::read_minimal_project_config(pkg_dir)
        .context("Failed to read streamlib.yaml")?
        .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", pkg_dir.display()))?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;
    let pkg_name = package.name.as_str().to_string();
    let pkg_version = package.version.to_string();

    // A package is valid when it owns at least one schema OR one
    // processor (schema-only packages like `@tatolab/core` are
    // first-class).
    let schema_files = collect_schema_files(pkg_dir)?;
    if config.processors.is_empty() && schema_files.is_empty() {
        anyhow::bail!(
            "streamlib.yaml at {} declares no processors AND no schemas. \
             A publishable package must own at least one of either.",
            pkg_dir.display()
        );
    }

    // Distribution artifacts are standalone: a published `.slpkg` resolves
    // every dependency from the registry, never a path. Refuse to ship one
    // that still carries a path-flavored Cargo dep or a streamlib.yaml path
    // `patch:` (dev-only monorepo affordances). The orchestrator's
    // `StagedDir` materialization is exempt — it builds in place under the
    // `RewriteRelativeToAbsolute` policy, which is the dev-resolution path.
    if matches!(target, AssembleTarget::Slpkg(_)) {
        ensure_no_path_artifacts(pkg_dir)?;
        // Same rationale as the path check: a distributable `.slpkg` must not
        // be assembled from a tree whose dependency resolution is redirected
        // by an active `streamlib link`. `StagedDir` stays exempt so
        // orchestrator load-time builds keep working while linked.
        link_marker::ensure_no_active_link_for_pack(pkg_dir)?;
        // The `@session/…` org is reserved for in-app / session-local
        // processors registered live at runtime; it is deliberately NOT
        // publishable. Refuse to build a distribution `.slpkg` that squats it
        // so a session-local name can never masquerade as an installable
        // package on the registry. `StagedDir` stays exempt (dev-resolution).
        if package.org.is_reserved_for_session() {
            anyhow::bail!(
                "streamlib.yaml at {} declares the reserved `@{}/…` org, which is \
                 reserved for in-app / session-local processors and cannot be built \
                 into a distributable package. Publish under your own org instead.",
                pkg_dir.display(),
                streamlib_idents::SESSION_ORG,
            );
        }
        // The `#[processor(...)]` / `@processor(...)` declarations in code are the
        // source of truth for the `processors:` section (#1411). Derive the
        // processor set from code and refuse to build a distributable `.slpkg`
        // whose committed `processors:` disagrees. `StagedDir` (runtime
        // orchestrator load-time build) is exempt — it assembles an already-
        // published, drift-validated artifact.
        enforce_processor_manifest_matches_code(pkg_dir, &config.processors)?;
        // Reconcile the hand-declared `dependencies:` against the dependency
        // set derived from the package's schema/port references. An undeclared
        // reference is a hard error; an unreferenced, non-`runtime` dependency
        // is warned as prunable dead weight. `StagedDir` is exempt for the same
        // reason as the processor drift gate — it assembles an already-
        // reconciled, published artifact.
        enforce_declared_dependencies_match_code(pkg_dir, &config.processors)?;
    }

    // (archive_path, source_path) pairs for every file EXCEPT the
    // manifest, which is handled separately (its bytes may be rewritten).
    let mut files: Vec<(String, PathBuf)> = Vec::new();

    // pyproject.toml / deno.json (per-language manifests).
    let pyproject = pkg_dir.join("pyproject.toml");
    if pyproject.exists() {
        files.push(("pyproject.toml".to_string(), pyproject.clone()));
    }
    let deno_json = pkg_dir.join("deno.json");
    if deno_json.exists() {
        files.push(("deno.json".to_string(), deno_json));
    }

    // Schemas (declared or every `schemas/*.yaml`).
    for schema_rel in &schema_files {
        let abs = pkg_dir.join(schema_rel);
        if !abs.exists() {
            anyhow::bail!(
                "Schema file declared in streamlib.yaml not found: {}",
                abs.display()
            );
        }
        files.push((schema_rel.to_string_lossy().replace('\\', "/"), abs));
    }

    // Entrypoint resolution is the runtime's job, not the packer's.
    //
    // A processor's `entrypoint` (`module:Class` for Python, `file.ts:export`
    // for Deno) is resolved at load time by the language's own import system —
    // Python via `importlib.import_module` (the PyPA entry-point object-reference
    // algorithm), Deno via its module loader. Reimplementing that resolution
    // here as a build-time path-stat is lossy and gap-prone: a dotted Python
    // module path (`pkg.module`) maps to `pkg/module.py` OR
    // `pkg/module/__init__.py` OR a PEP 420 namespace-package directory, and can
    // also be provided via a zip / `.pth` / editable layout — none of which a
    // naive `"{module}.py"` check resolves (it looks for the literal file
    // `pkg.module.py`). So we do NOT validate or relocate entrypoints here: the
    // FULL authored source tree (every entrypoint + helper module + asset) ships
    // verbatim via `collect_source_tree` below, and a genuinely-bad entrypoint
    // surfaces at load with a precise `importlib` / loader error instead of a
    // guessed "entrypoint file not found" at pack time.

    let mut rebuilt = false;

    // Rust cdylib.
    let has_rust = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust));
    // A source-only `.slpkg` (the distribution artifact `streamlib pkg build`
    // / `publish` ships) carries NO prebuilt cdylib and NO local compilation —
    // the consumer builds it from the bundled source on their own host
    // (`streamlib add` / `Strategy::Registry`, AlwaysBuild), resolving every dep
    // from the registry. Only the runtime orchestrator's `StagedDir` target
    // compiles the cdylib here, because that materialization IS the host build.
    if has_rust && matches!(target, AssembleTarget::StagedDir(_)) {
        let host_triple = streamlib_cargo_build::host_target_triple();
        let dylib_ext = streamlib_cargo_build::host_dylib_extension();
        let triple_dir = pkg_dir.join("lib").join(host_triple);
        let prebuilt = streamlib_cargo_build::collect_host_dylibs_in_lib(&triple_dir, dylib_ext)?;

        if !prebuilt.is_empty() {
            for path in prebuilt {
                let filename = dylib_filename(&path)?;
                files.push((format!("lib/{host_triple}/{filename}"), path));
            }
        } else if opts.no_build {
            let cargo_hint = streamlib_cargo_build::read_cargo_package_name(pkg_dir)
                .map(|name| format!("cargo build --release -p {name}"))
                .unwrap_or_else(|_| "cargo build --release -p <name>".to_string());
            anyhow::bail!(
                "Package at {} declares Rust runtime processors but {} contains no \
                 host-OS dylib (`*.{}`) for triple `{}` and `--no-build` was specified. \
                 Either run `{}` to populate lib/{}/ first, or omit `--no-build` to let \
                 assembly invoke cargo automatically.",
                pkg_dir.display(),
                triple_dir.display(),
                dylib_ext,
                host_triple,
                cargo_hint,
                host_triple,
            );
        } else {
            ensure_tool("cargo", "install the Rust toolchain — https://rustup.rs")?;
            let cargo_name =
                streamlib_cargo_build::read_cargo_package_name(pkg_dir).with_context(|| {
                    format!(
                        "Package at {} declares Rust runtime processors but the Cargo \
                         crate name to build could not be determined",
                        pkg_dir.display()
                    )
                })?;
            sink.started("rust");
            let built = cargo_build_streaming(
                pkg_dir,
                &cargo_name,
                dylib_ext,
                opts.profile,
                sink,
                cargo_config_files,
                link_checkout,
            )?;
            sink.finished("rust");
            rebuilt = true;
            let filename = dylib_filename(&built)?;
            files.push((format!("lib/{host_triple}/{filename}"), built));
        }
    }

    // Python: distribute as SOURCE — no wheel.
    //
    // The engine runs a Python processor from its source dir
    // (`PYTHONPATH = <staged package dir>`), not from a pip-installed
    // copy, so a wheel would only ever install the package's
    // *dependencies* — and rebuilding it on every `.py` edit busts the
    // dependency venv (the deps reinstall) for zero benefit. Instead we
    // ship the FULL source tree (every `.py` + data / assets / models +
    // `pyproject.toml` + `uv.lock`); the install side caches the
    // dependency venv by the dependency closure (`pyproject` contents)
    // and runs the source directly. Because dev and the `.slpkg` carry
    // the identical source, there is no dev/distribution packaging skew.
    //
    // A package that ships a pre-built `python/wheels/*.whl` keeps it
    // (the full-tree copy includes it, and the install side honours a
    // pre-built wheel) — but nothing is built here.
    let has_python = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Python));
    let mut python_wheels = 0usize;
    if has_python {
        python_wheels = collect_wheels_in_dir(&pkg_dir.join("python").join("wheels"))?.len();
    }

    let has_deno = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::TypeScript));

    // Bundle the source tree when the package ships code that's run or
    // built FROM source:
    //   - Python → the engine runs it from source (see above).
    //   - Deno   → the engine runs the `.ts` from source; the whole
    //     authored tree travels at its authored paths (entrypoints,
    //     helper modules, `deno.json`, `.npmrc`, assets) so the staged
    //     package is a faithful mirror of what the developer wrote.
    //     `_generated_` is excluded (a per-consumer codegen artifact,
    //     regenerated at stage time — same as Python's `_generated_`).
    //   - Rust   → so a host on a different triple (or one given a
    //     source-only box) can `cargo build` the cdylib itself. The
    //     prebuilt cdylib for the packing host is already in `files`
    //     (lib/<triple>/), and `collect_source_tree` excludes `lib/`, so
    //     the two don't collide — the box becomes "sdist + one-triple
    //     wheel". A package whose Cargo deps are path/workspace-only only
    //     builds where those resolve (same constraint crates.io has); it
    //     relies on the bundled prebuilt for its own triple.
    if has_python || has_rust || has_deno {
        collect_source_tree(pkg_dir, &mut files)?;
    }

    // Manifest bytes (possibly rewritten).
    let manifest_bytes = manifest_bytes_for(pkg_dir, opts.path_deps)?;

    // Derive the crate version from the `.slpkg` semver. A Rust package's
    // `Cargo.toml` `[package].version` is stamped to match
    // `streamlib.yaml`'s `package.version` so a stale in-tree crate version
    // can never reach the registry via the artifact. `None` when there's
    // nothing to stamp (no `Cargo.toml`, no `[package].version`).
    let stamped_cargo_toml = if has_rust {
        stamped_cargo_toml_bytes(pkg_dir, &pkg_version)?
    } else {
        None
    };

    // Emit.
    match target {
        AssembleTarget::Slpkg(zip_path) => emit_slpkg(
            zip_path,
            &files,
            &manifest_bytes,
            stamped_cargo_toml.as_deref(),
        )?,
        AssembleTarget::StagedDir(dir) => emit_staged_dir(
            pkg_dir,
            dir,
            &files,
            &manifest_bytes,
            stamped_cargo_toml.as_deref(),
        )?,
    }

    Ok(AssembleOutcome {
        package_name: pkg_name,
        package_version: pkg_version,
        schemas: schema_files.len(),
        processors: config.processors.len(),
        python_wheels,
        rebuilt,
    })
}

/// Fast-fail preflight: confirm a build tool is on `PATH`.
fn ensure_tool(tool: &str, hint: &str) -> Result<()> {
    let ok = Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        anyhow::bail!("build tool `{tool}` not found on PATH: {hint}")
    }
}

fn dylib_filename(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("dylib path has no filename: {}", path.display()))?
        .to_string_lossy()
        .into_owned())
}

/// Derive the processor set from a package's code and fail the build if the
/// committed `processors:` section disagrees. The Rust half is a `syn` source
/// scan (in-process, no compile); the Python / Deno halves shell out to the
/// SDK's import-and-enumerate extractor CLIs. Reachability is resolved for the
/// **host** target — the triple `pkg build` produces a cdylib for — so a
/// cross-platform arm the host doesn't compile never counts as drift.
///
/// A schema-only package (no Rust crate root, no `pyproject.toml`, no
/// `deno.json`) derives an empty set and must therefore carry no `processors:`;
/// a package that hosts code carries exactly the processors it declares.
fn enforce_processor_manifest_matches_code(
    pkg_dir: &Path,
    committed: &[streamlib_processor_schema::ProcessorSchema],
) -> Result<()> {
    let target = streamlib_processor_extract::ModuleReachabilityTarget::for_host();
    let extractor = streamlib_processor_extract::SystemSubprocessProcessorExtractor;
    let derived =
        streamlib_processor_extract::derive_package_processor_surfaces(pkg_dir, &target, &extractor)
            .map_err(|e| anyhow::anyhow!("deriving the processor set from code: {e}"))?;
    // Compare only the languages actually derived. A language whose extractor
    // runtime was unavailable (Python/Deno on a host without it) is skipped, and
    // its committed processors are excluded rather than falsely flagged as drift.
    let committed_in_scope =
        streamlib_processor_extract::filter_committed_to_languages(committed, &derived.derived_languages);
    streamlib_processor_extract::check_processor_manifest_drift(
        pkg_dir,
        &committed_in_scope,
        &derived.surfaces,
    )
    .map_err(|report| anyhow::anyhow!("{report}"))
}

/// Reconcile the hand-declared `dependencies:` against the dependency set
/// derived from the package's schema/port references (see
/// [`crate::dependency_reconcile`]). A referenced-but-undeclared package is a
/// hard error carrying a `streamlib add` fix-it; a declared-but-unreferenced
/// package that is not marked `runtime: true` is warned as prunable dead
/// weight. Called only for the [`AssembleTarget::Slpkg`] target.
///
/// Pruning is intentionally non-destructive: the shipped manifest stays
/// byte-identical to the one an orchestrator `StagedDir` build produces, so the
/// author is told to remove the dead dependency (or mark it `runtime: true`)
/// rather than the build silently rewriting the manifest under them.
fn enforce_declared_dependencies_match_code(
    pkg_dir: &Path,
    processors: &[streamlib_processor_schema::ProcessorSchema],
) -> Result<()> {
    let manifest = Manifest::load(pkg_dir)
        .with_context(|| format!("read {} for dependency reconcile", pkg_dir.display()))?;
    let reconciliation =
        crate::dependency_reconcile::reconcile_package_dependencies(&manifest, processors);

    if !reconciliation.undeclared.is_empty() {
        let fix_its = reconciliation
            .undeclared
            .iter()
            .map(|pkg| format!("  streamlib add {pkg}@<version>"))
            .collect::<Vec<_>>()
            .join("\n");
        let refs = reconciliation
            .undeclared
            .iter()
            .map(|pkg| pkg.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "{} references schema(s) from undeclared dependenc(ies) [{}] — a package's \
             `dependencies:` must declare every package its code references. Declare each:\n{}",
            pkg_dir.join(Manifest::FILE_NAME).display(),
            refs,
            fix_its,
        );
    }

    for pkg in &reconciliation.pruned {
        tracing::warn!(
            package = %pkg,
            manifest = %pkg_dir.join(Manifest::FILE_NAME).display(),
            "declared dependency is referenced by no schema/port — remove it, or mark it \
             `runtime: true` if the package composes it at runtime without importing its types"
        );
    }
    Ok(())
}

/// Enforce the standalone, registry-only contract for a published `.slpkg`:
/// fail if the package carries anything path-flavored — a `path = …` Cargo
/// dependency or a streamlib.yaml `patch:` entry. Both are dev-only monorepo
/// affordances; a distributed source package must resolve every artifact from
/// the registry, so a stray path would ship and break the consumer's off-tree
/// build. Called only for the `Slpkg` target (`pkg build` / `pkg publish`).
fn ensure_no_path_artifacts(pkg_dir: &Path) -> Result<()> {
    // Both halves flow through the SAME helpers the whole-tree emit's skip
    // predicate ([`non_distributable_path_offenders`]) is built from, so the
    // rejection set and the skip set are identical by construction — neither
    // half can drift into a "skip misses what reject catches" gap.
    let patch_offenders = path_patch_offenders(pkg_dir)?;
    if !patch_offenders.is_empty() {
        anyhow::bail!(
            "{} carries path `patch:` override(s) for [{}] — a published package \
             must be standalone (registry-only). Remove the `patch:` block; each \
             dependency resolves from the registry by the version in `dependencies:`.",
            pkg_dir.join(Manifest::FILE_NAME).display(),
            patch_offenders.join(", "),
        );
    }

    let cargo_offenders = cargo_path_dep_offenders(pkg_dir)?;
    if !cargo_offenders.is_empty() {
        anyhow::bail!(
            "{} declares path dependenc(ies) [{}] — a published package must be \
             standalone (registry-only). Replace each with \
             `{{ version = \"…\", registry = \"tatolab\" }}` so the crate resolves \
             from the registry.",
            pkg_dir.join("Cargo.toml").display(),
            cargo_offenders.join(", "),
        );
    }
    Ok(())
}

/// Names of dependency-table `path` deps in `<pkg_dir>/Cargo.toml` — the same
/// scan [`ensure_no_path_artifacts`] rejects on. Empty when the Cargo.toml is
/// absent or carries only registry-resolved deps. Reads + parses the Cargo.toml
/// then defers to [`cargo_path_dep_names`], so a `[[bin]].path` / `[lib].path`
/// TARGET path never counts — only dependency tables are scanned.
fn cargo_path_dep_offenders(pkg_dir: &Path) -> Result<Vec<String>> {
    let cargo_path = pkg_dir.join("Cargo.toml");
    if !cargo_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("read {}", cargo_path.display()))?;
    let doc: toml::Value =
        toml::from_str(&body).with_context(|| format!("parse {}", cargo_path.display()))?;
    Ok(cargo_path_dep_names(&doc))
}

/// Names of dependencies carrying a `path` key across every dependency table
/// in a parsed `Cargo.toml` — `[dependencies]`, `[build-dependencies]`,
/// `[dev-dependencies]`, and their `[target.<cfg>.…]` variants.
fn cargo_path_dep_names(doc: &toml::Value) -> Vec<String> {
    fn scan_dep_table(table: &toml::value::Table, out: &mut Vec<String>) {
        for (name, spec) in table {
            if let toml::Value::Table(t) = spec {
                if t.contains_key("path") {
                    out.push(name.clone());
                }
            }
        }
    }
    fn scan_section(root: &toml::value::Table, out: &mut Vec<String>) {
        for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
            if let Some(toml::Value::Table(t)) = root.get(key) {
                scan_dep_table(t, out);
            }
        }
    }
    let mut out = Vec::new();
    if let toml::Value::Table(root) = doc {
        scan_section(root, &mut out);
        if let Some(toml::Value::Table(targets)) = root.get("target") {
            for (_cfg, tbl) in targets.iter() {
                if let toml::Value::Table(t) = tbl {
                    scan_section(t, &mut out);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether a directory-entry name is a build artifact / dev-only file
/// that must NEVER ship as package source — VCS, language caches, build
/// outputs, and (critically) developer-local virtual environments. A
/// `.venv` left in a Python package dir during dev is the canonical trap:
/// it's huge, machine-specific, and full of symlinks, and shipping it
/// both bloats the artifact and breaks a plain file copy.
///
/// Shared by [`collect_source_tree`] and the orchestrator's source
/// fingerprint so "what counts as source" has one definition.
///
/// These directory names are **reserved**: a package must not use
/// `target` / `lib` / `venv` / `.venv` / `node_modules` / `__pycache__`
/// / `_generated_` (etc.) as its own source directories, because they're
/// stripped from the shipped source. This matches the ignore conventions
/// of cargo / pip / npm and is an accepted packaging constraint.
///
/// `_generated_` is the JTD-codegen wire vocabulary (Python
/// `<pkg>/_generated_/`): a build artifact regenerated per-consumer at
/// install time from the package's schemas, never shipped as source.
///
/// `Cargo.lock` is stripped too: a streamlib package is a cdylib *library*,
/// and a library's lockfile is neither published nor honored by a downstream
/// build. Shipping it is actively harmful in the registry model — the lock
/// pins transitive deps (incl. the streamlib SDK) by exact version+checksum,
/// so an in-place republish of any pinned version makes the lock's checksum
/// stale and `cargo build` aborts with "checksum changed between lock files".
/// The consumer re-resolves from the registry by the manifest's version reqs;
/// the lock is pure byproduct (already gitignored).
pub fn is_non_source_artifact(name: &std::ffi::OsStr) -> bool {
    match name.to_str() {
        Some(
            "target"
            | "lib"
            | ".git"
            | "node_modules"
            | "__pycache__"
            | "_generated_"
            | ".streamlib-build.json"
            | ".venv"
            | "venv"
            | "Cargo.lock"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".tox"
            | ".DS_Store",
        ) => true,
        Some(s) => s.ends_with(".slpkg") || s.ends_with(".egg-info") || s.ends_with(".pyc"),
        None => false,
    }
}

/// Recursively collect a package's source files (relative archive path,
/// absolute source path), excluding build artifacts / VCS / caches /
/// dev venvs (see [`is_non_source_artifact`]) and symlinks (a source
/// package's content is its real files, not machine-specific links).
/// Used to ship a Python package as SOURCE: every `.py` + data / asset /
/// model file travels, so what's importable matches the artifact exactly.
fn collect_source_tree(pkg_dir: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if is_non_source_artifact(&entry.file_name()) {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                // Skip symlinks: a distributed source package shouldn't
                // depend on machine-specific links, and `std::fs::copy`
                // would follow (and choke on) a broken / dir target.
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                walk(&path, root, out)?;
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, path));
            }
        }
        Ok(())
    }
    walk(pkg_dir, pkg_dir, files)
}

/// Run `cargo build` with `profile`, streaming stderr (human
/// diagnostics) to `sink` line-by-line while capturing the JSON artifact
/// stream to locate the produced cdylib. Cargo's own fingerprint
/// short-circuits when nothing changed — and catches out-of-package /
/// transitive changes a package-local check cannot.
/// Set `STREAMLIB_LINK_CHECKOUT` on `command` for a relocated `cargo build` —
/// the checkout path when a link is active, or EMPTY (the suppression sentinel
/// [`streamlib_idents::ResolverOptions::from_env_or_marker`] honors) when not.
/// BOTH arms call `command.env`, so the env is set UNCONDITIONALLY: that keeps
/// the orchestrator authoritative over link state, so the relocated `build.rs`
/// trusts this env and never re-derives the link from a stray
/// `.streamlib/link.json` up-tree of the staged crate dir. Reverting the `None`
/// arm to "leave the env unset" reintroduces the mixed-build hazard — locked by
/// `cargo_build_sets_link_checkout_env_unconditionally`.
fn apply_link_checkout_env(command: &mut Command, link_checkout: Option<&Path>) {
    let value: &std::ffi::OsStr = match link_checkout {
        Some(checkout) => checkout.as_os_str(),
        None => std::ffi::OsStr::new(""),
    };
    command.env(streamlib_idents::LINK_CHECKOUT_ENV, value);
}

fn cargo_build_streaming(
    pkg_dir: &Path,
    cargo_name: &str,
    dylib_ext: &str,
    profile: CargoProfile,
    sink: &dyn PackEventSink,
    cargo_config_files: &[PathBuf],
    link_checkout: Option<&Path>,
) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    if matches!(profile, CargoProfile::Release) {
        command.arg("--release");
    }
    // Merge extra cargo config TOML files (each a `cargo build --config <file>`).
    // The orchestrator uses this to inject the `streamlib link` `[patch]` block
    // so a linked package's cdylib builds against the checkout's crates. Cargo
    // merges these on top of the config it discovers by walking up from the
    // build dir, so the injected patch wins.
    for config_file in cargo_config_files {
        command.arg("--config").arg(config_file);
    }
    // Thread the active `streamlib link` checkout to the package's `build.rs`
    // schema-dep codegen, which reads it via `ResolverOptions::from_env_or_marker`
    // and resolves a dep present in `<checkout>/packages/<name>` from the
    // checkout — the schema-dep half of the zero-registry dev loop, mirroring the
    // cargo `[patch]` above for the crate half. `build.rs` runs as a child of
    // this `cargo build`, so an env var set here reaches it.
    //
    // The env is set UNCONDITIONALLY (see `apply_link_checkout_env`) so the
    // orchestrator stays authoritative over link state: this is a relocated
    // build (the package is staged into the streamlib-home cache, whose
    // `.streamlib` dir name collides with the link-state dir), so `build.rs`'s
    // marker walk-up from `CARGO_MANIFEST_DIR` could otherwise cross into a stray
    // `.streamlib/link.json` up-tree and silently redirect a locked /
    // distribution build to a dev checkout.
    apply_link_checkout_env(&mut command, link_checkout);
    command
        .arg("--message-format=json-render-diagnostics")
        .arg("-p")
        .arg(cargo_name)
        .current_dir(pkg_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn cargo build -p {cargo_name}"))?;

    let stderr = child.stderr.take();
    let stderr_thread = stderr.map(|err| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        (rx, handle)
    });

    let mut stdout_json = String::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some((rx, _)) = &stderr_thread {
                while let Ok(eline) = rx.try_recv() {
                    sink.line(PackStream::Stderr, &eline);
                }
            }
            stdout_json.push_str(&line);
            stdout_json.push('\n');
        }
    }
    if let Some((rx, handle)) = stderr_thread {
        let _ = handle.join();
        while let Ok(eline) = rx.recv() {
            sink.line(PackStream::Stderr, &eline);
        }
    }

    let status = child.wait().context("wait cargo build")?;
    if !status.success() {
        anyhow::bail!("cargo build -p {cargo_name} exited non-zero (see build log)");
    }

    streamlib_cargo_build::parse_cargo_artifact_for_cdylib(&stdout_json, cargo_name, dylib_ext)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cargo build -p {cargo_name} produced no host cdylib (`*.{dylib_ext}`); \
                 confirm the crate declares `crate-type = [\"cdylib\"]`"
            )
        })
}

/// Enumerate `*.whl` in `wheels_dir`. Empty when the dir is absent.
fn collect_wheels_in_dir(wheels_dir: &Path) -> Result<Vec<PathBuf>> {
    if !wheels_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(wheels_dir)
        .with_context(|| format!("read wheels dir: {}", wheels_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "whl") {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Discover the schema YAML files this package owns: explicit `schemas:`
/// in the manifest, else every `*.yaml`/`*.yml` under `schemas/`.
fn collect_schema_files(pkg_dir: &Path) -> Result<Vec<PathBuf>> {
    let manifest_path = pkg_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_yaml::from_str(&body)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    if let Some(declared) = manifest.schemas {
        let mut files: Vec<PathBuf> = declared
            .into_values()
            .filter_map(|entry| match entry {
                streamlib_idents::SchemaEntry::Local { file } => Some(file),
                streamlib_idents::SchemaEntry::External { .. } => None,
            })
            .collect();
        files.sort();
        return Ok(files);
    }

    let schemas_dir = pkg_dir.join("schemas");
    if !schemas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&schemas_dir)
        .with_context(|| format!("read schemas dir: {}", schemas_dir.display()))?
    {
        let path = entry?.path();
        if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yaml" | "yml")
        ) {
            files.push(path.strip_prefix(pkg_dir).unwrap_or(&path).to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

/// Compute the `streamlib.yaml` bytes to write into the artifact, per
/// the [`PathDepPolicy`].
fn manifest_bytes_for(pkg_dir: &Path, policy: PathDepPolicy) -> Result<Vec<u8>> {
    let manifest_path = pkg_dir.join("streamlib.yaml");
    match policy {
        PathDepPolicy::RejectPathPatches => {
            reject_path_patches(pkg_dir)?;
            std::fs::read(&manifest_path)
                .with_context(|| format!("read {}", manifest_path.display()))
        }
        PathDepPolicy::RewriteRelativeToAbsolute => rewrite_manifest_path_deps_absolute(pkg_dir),
    }
}

/// Names every path-flavor `patch:` entry in `<pkg_dir>/streamlib.yaml` —
/// dev-time overrides that don't generalize to a distributed artifact. An
/// empty result means the package is publishable through this gate; a
/// non-empty one lists each offender (`` `dep` → `path` ``). A missing
/// manifest is treated as no offenders.
///
/// The predicate the whole-tree static-registry emit skips on and the CLI
/// `.slpkg` gate rejects on share this one definition — the skip is the same
/// condition, sound by construction rather than a proxy. Filters exactly
/// [`DependencySpec::Path`], so git/registry `patch:` overrides never count.
pub(crate) fn path_patch_offenders(pkg_dir: &Path) -> Result<Vec<String>> {
    let manifest_path = pkg_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_yaml::from_str(&body)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest
        .patch
        .iter()
        .filter_map(|(dep_ref, spec)| match spec {
            DependencySpec::Path(p) => Some(format!("`{}` → `{}`", dep_ref, p.path.display())),
            _ => None,
        })
        .collect())
}

/// Every non-distributable path artifact in a package dir — the union of
/// `streamlib.yaml` path-`patch:` overrides ([`path_patch_offenders`]) and
/// Cargo.toml dependency-table `path` deps ([`cargo_path_dep_offenders`]).
///
/// [`ensure_no_path_artifacts`] rejects on these exact same two helpers for
/// the [`AssembleTarget::Slpkg`] target, so the whole-tree static-registry
/// emit's skip predicate keys on the same condition it would otherwise
/// hard-fail on: the skip set equals the rejection set, sound by construction
/// (one shared definition per half) rather than a proxy. TARGET paths
/// (`[[bin]].path` / `[lib].path`) are not dependency paths and never count.
pub(crate) fn non_distributable_path_offenders(pkg_dir: &Path) -> Result<Vec<String>> {
    let mut offenders = path_patch_offenders(pkg_dir)?;
    offenders.extend(cargo_path_dep_offenders(pkg_dir)?);
    Ok(offenders)
}

/// Reject path-flavor `patch:` entries (dev-time overrides that don't
/// generalize to a distributed artifact). Names every offender.
fn reject_path_patches(pkg_dir: &Path) -> Result<()> {
    let offenders = path_patch_offenders(pkg_dir)?;
    if offenders.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} carries path-flavor `patch:` entries which are dev-time overrides and not \
         publishable: {}. Remove them — or convert to a git/registry override — before packing.",
        pkg_dir.join(Manifest::FILE_NAME).display(),
        offenders.join(", "),
    );
}

/// Strip dev-time path-flavor `patch:` entries from a `streamlib.yaml`
/// body, returning the rewritten YAML. `dependencies:`, git/registry
/// `patch:` overrides, and every other manifest field pass through
/// unchanged.
///
/// This is the publish-side counterpart to
/// [`PathDepPolicy::RejectPathPatches`]. Where `streamlib pack` *rejects* a
/// path patch (a distributed source `.slpkg` must not carry a dev override),
/// `cargo publish` must *strip* it: the path patch is a legitimate dev
/// affordance inside the monorepo (it redirects a dep to local source for
/// instant edits), but the published manifest must be path-free so a
/// registry-cached consumer resolves the dep from the registry instead of a
/// dangling `../../packages/...` path. The schema-tier analog of cargo
/// stripping `path` from a `[dependencies]` path dep on publish.
///
/// Idempotent: a manifest with no path patches round-trips unchanged in
/// content (modulo serializer normalization).
pub fn strip_path_patches(manifest_yaml: &str) -> Result<String> {
    let mut manifest: streamlib_processor_schema::StreamlibYaml =
        serde_yaml::from_str(manifest_yaml).context("parse streamlib.yaml")?;
    manifest
        .patch
        .retain(|_dep_ref, spec| !matches!(spec, DependencySpec::Path(_)));
    serde_yaml::to_string(&manifest).context("serialize streamlib.yaml")
}

/// In-place [`strip_path_patches`] on `<dir>/streamlib.yaml`. Intended to run
/// against a scratch copy of a crate at `cargo publish` time (cargo bundles
/// `streamlib.yaml` verbatim, with no file-rewrite hook, so the strip happens
/// before publishing the staged copy). No-op when the file is absent.
pub fn strip_path_patches_in_dir(dir: &Path) -> Result<()> {
    let manifest_path = dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let stripped = strip_path_patches(&body)?;
    std::fs::write(&manifest_path, stripped)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    Ok(())
}

/// Serialize `streamlib.yaml` with every relative `path:` dep/patch
/// rewritten to absolute, anchored at `pkg_dir`. Registry / git entries
/// pass through unchanged.
fn rewrite_manifest_path_deps_absolute(pkg_dir: &Path) -> Result<Vec<u8>> {
    let yaml =
        std::fs::read_to_string(pkg_dir.join("streamlib.yaml")).context("read streamlib.yaml")?;
    let mut manifest: streamlib_processor_schema::StreamlibYaml =
        serde_yaml::from_str(&yaml).context("parse streamlib.yaml")?;

    let abs_pkg = std::fs::canonicalize(pkg_dir).unwrap_or_else(|_| pkg_dir.to_path_buf());
    let rewrite =
        |map: &mut std::collections::BTreeMap<streamlib_idents::PackageRef, DependencySpec>| {
            for spec in map.values_mut() {
                if let DependencySpec::Path(pd) = spec {
                    if pd.path.is_relative() {
                        let joined = abs_pkg.join(&pd.path);
                        pd.path = std::fs::canonicalize(&joined).unwrap_or(joined);
                    }
                }
            }
        };
    rewrite(&mut manifest.dependencies);
    rewrite(&mut manifest.patch);

    let out = serde_yaml::to_string(&manifest).context("serialize streamlib.yaml")?;
    Ok(out.into_bytes())
}

/// Rewrite `[package].version` in a `Cargo.toml` body to `version`,
/// format-preserving via [`toml_edit`] — comments and every other field pass
/// through unchanged. Handles the standard `[package]` table AND the inline
/// `package = { version = … }` form; replaces whatever `version` shape is
/// present (a literal string OR a `version.workspace = true` inheritance)
/// with the literal. Returns `Ok(None)` when there's no `[package]` table or
/// no `version` key.
pub fn rewrite_cargo_package_version(cargo_toml: &str, version: &str) -> Result<Option<String>> {
    let mut doc: toml_edit::DocumentMut = cargo_toml.parse().context("parse Cargo.toml")?;
    let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_like_mut()) else {
        return Ok(None);
    };
    if package.get("version").is_none() {
        return Ok(None);
    }
    package.insert("version", toml_edit::value(version));
    Ok(Some(doc.to_string()))
}

/// Compute the `Cargo.toml` bytes to ship in the artifact with
/// `[package].version` stamped to `package_version` (the `.slpkg` semver
/// from `streamlib.yaml`'s `package.version`) via
/// [`rewrite_cargo_package_version`].
///
/// Returns `Ok(None)` (ship the in-tree `Cargo.toml` verbatim) when there's
/// nothing to stamp: no `Cargo.toml`, no `[package]` table, or no `version`
/// key. A `version.workspace = true` inheritance is stamped to a literal —
/// defensive for artifact copies assembled without the defining workspace
/// root.
fn stamped_cargo_toml_bytes(pkg_dir: &Path, package_version: &str) -> Result<Option<Vec<u8>>> {
    let cargo_path = pkg_dir.join("Cargo.toml");
    if !cargo_path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("read {}", cargo_path.display()))?;
    let stamped = rewrite_cargo_package_version(&body, package_version)
        .with_context(|| format!("stamp version into {}", cargo_path.display()))?;
    Ok(stamped.map(String::into_bytes))
}

/// Write the `.slpkg` zip: the manifest bytes as `streamlib.yaml`, the
/// version-stamped `Cargo.toml` (when present), then every collected file at
/// its archive path. Duplicate paths skipped — writing the stamped
/// `Cargo.toml` first means the verbatim copy from the source tree is deduped.
fn emit_slpkg(
    zip_path: &Path,
    files: &[(String, PathBuf)],
    manifest_bytes: &[u8],
    stamped_cargo_toml: Option<&[u8]>,
) -> Result<()> {
    use zip::ZipWriter;
    use zip::write::FileOptions;

    let file = File::create(zip_path).with_context(|| format!("create {}", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut seen = std::collections::HashSet::new();
    zip.start_file("streamlib.yaml", options)?;
    zip.write_all(manifest_bytes)?;
    seen.insert("streamlib.yaml".to_string());

    if let Some(bytes) = stamped_cargo_toml {
        zip.start_file("Cargo.toml", options)?;
        zip.write_all(bytes)?;
        seen.insert("Cargo.toml".to_string());
    }

    for (name, path) in files {
        if !seen.insert(name.clone()) {
            continue;
        }
        let mut contents = Vec::new();
        File::open(path)
            .with_context(|| format!("open {}", path.display()))?
            .read_to_end(&mut contents)?;
        zip.start_file(name, options)?;
        zip.write_all(&contents)?;
    }
    zip.finish()?;
    Ok(())
}

/// Whether two paths resolve to the same existing file. Both must canonicalize;
/// a not-yet-created destination is never "the same file" as its source, so the
/// normal detached copy/write proceeds.
fn is_same_existing_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Write the extracted layout into `dir`: the manifest bytes as
/// `streamlib.yaml`, the version-stamped `Cargo.toml` (when present), then
/// every collected file at its archive path (parents created). Duplicate
/// paths skipped — the stamped `Cargo.toml` is written first so the verbatim
/// source-tree copy is deduped.
///
/// In-place staging (`dir` IS the package source dir `src_pkg_dir`): the source
/// manifest / `Cargo.toml` / every source file already sit at their destination
/// path. Rewriting the manifest onto itself would mutate the user's source, and
/// `fs::copy(src, dest)` with `src == dest` truncates the file to zero — so any
/// write whose source and destination are the same file is skipped.
fn emit_staged_dir(
    src_pkg_dir: &Path,
    dir: &Path,
    files: &[(String, PathBuf)],
    manifest_bytes: &[u8],
    stamped_cargo_toml: Option<&[u8]>,
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    if !is_same_existing_file(&src_pkg_dir.join("streamlib.yaml"), &dir.join("streamlib.yaml")) {
        std::fs::write(dir.join("streamlib.yaml"), manifest_bytes)
            .context("write streamlib.yaml")?;
    }

    let mut seen = std::collections::HashSet::new();
    seen.insert("streamlib.yaml".to_string());
    if let Some(bytes) = stamped_cargo_toml {
        if !is_same_existing_file(&src_pkg_dir.join("Cargo.toml"), &dir.join("Cargo.toml")) {
            std::fs::write(dir.join("Cargo.toml"), bytes).context("write stamped Cargo.toml")?;
        }
        seen.insert("Cargo.toml".to_string());
    }
    for (name, src) in files {
        if !seen.insert(name.clone()) {
            continue;
        }
        let dest = dir.join(name);
        if is_same_existing_file(src, &dest) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(src, &dest)
            .with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_cargo_build::{host_dylib_extension, host_target_triple};
    use tempfile::tempdir;

    /// A relocated `cargo build` gets `STREAMLIB_LINK_CHECKOUT` set on its
    /// `Command` UNCONDITIONALLY — the checkout when a link is active, EMPTY
    /// (present, never absent) otherwise — so the orchestrator stays
    /// authoritative and the staged `build.rs` never re-derives the link from a
    /// stray marker up-tree. Inspecting the built `Command`'s env via
    /// `get_envs()` locks the CALL SITE, not just a value mapping: mentally-revert
    /// `apply_link_checkout_env`'s `None` arm to "leave the env unset" (the exact
    /// mixed-build regression) and the `Some(Some(""))` assertion below fails,
    /// because the env would be absent from `get_envs()` rather than present-empty.
    #[test]
    fn cargo_build_sets_link_checkout_env_unconditionally() {
        use std::ffi::OsStr;
        let key = OsStr::new(streamlib_idents::LINK_CHECKOUT_ENV);

        // No active link ⇒ env PRESENT and EMPTY (the suppression sentinel).
        let mut no_link = Command::new("true");
        apply_link_checkout_env(&mut no_link, None);
        let got = no_link.get_envs().find(|(k, _)| *k == key).map(|(_, v)| v);
        assert_eq!(
            got,
            Some(Some(OsStr::new(""))),
            "a non-linked relocated build must SET the EMPTY sentinel on the \
             command, not leave the env unset"
        );

        // Active link ⇒ the checkout path is threaded through as the override.
        let mut linked = Command::new("true");
        let checkout = Path::new("/opt/streamlib-checkout");
        apply_link_checkout_env(&mut linked, Some(checkout));
        let got = linked.get_envs().find(|(k, _)| *k == key).map(|(_, v)| v);
        assert_eq!(
            got,
            Some(Some(checkout.as_os_str())),
            "an active link threads the checkout path through as the override"
        );
    }

    #[test]
    fn staged_dir_onto_the_source_dir_never_truncates_source_files() {
        // In-place staging (StagedDir target == the package source dir): the
        // self-copy guard must skip `fs::copy(src, dest)` where src == dest,
        // which would otherwise truncate every source file to zero bytes, and
        // skip rewriting the source manifest onto itself. Mentally-revert the
        // `is_same_existing_file` guards and `schemas/t.yaml` is copied onto
        // itself → truncated to 0 bytes, failing the content assertion.
        let src = tempdir().unwrap();
        write_schemas_only_pkg(src.path());
        let schema = src.path().join("schemas").join("t.yaml");
        let original_schema = std::fs::read(&schema).unwrap();
        let original_manifest = std::fs::read(src.path().join("streamlib.yaml")).unwrap();
        assert!(!original_schema.is_empty(), "fixture schema must be non-empty");

        assemble_artifact(
            src.path(),
            &AssembleTarget::StagedDir(src.path().to_path_buf()),
            &slpkg_opts(false),
            &(),
        )
        .expect("in-place staging onto the source dir must succeed");

        assert_eq!(
            std::fs::read(&schema).unwrap(),
            original_schema,
            "an in-place stage must NOT truncate a source file onto itself"
        );
        assert_eq!(
            std::fs::read(src.path().join("streamlib.yaml")).unwrap(),
            original_manifest,
            "an in-place stage must leave the source manifest untouched"
        );
    }

    #[test]
    fn staged_dir_detached_still_copies_source_files_verbatim() {
        // The detached StagedDir path (dest != source) is unchanged by the
        // self-copy guard: every source file is copied verbatim into the slot.
        let src = tempdir().unwrap();
        write_schemas_only_pkg(src.path());
        let dest = tempdir().unwrap();

        assemble_artifact(
            src.path(),
            &AssembleTarget::StagedDir(dest.path().to_path_buf()),
            &slpkg_opts(false),
            &(),
        )
        .expect("detached staging must succeed");

        assert_eq!(
            std::fs::read(dest.path().join("schemas").join("t.yaml")).unwrap(),
            std::fs::read(src.path().join("schemas").join("t.yaml")).unwrap(),
            "detached staging must copy the schema file byte-for-byte"
        );
        assert!(dest.path().join("streamlib.yaml").is_file());
    }

    fn slpkg_opts(no_build: bool) -> AssembleOptions {
        AssembleOptions {
            no_build,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        }
    }

    fn write_schemas_only_pkg(dir: &Path) {
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: cfg-pkg\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();
    }

    /// `assemble_artifact` delegates to `assemble_artifact_with_cargo_config`
    /// with an empty slice — an unlinked build is byte-for-byte the same as
    /// before. And extra cargo-config files are harmlessly ignored for a
    /// non-Rust package (no `cargo build` runs). This locks that the new entry
    /// point doesn't change the no-Rust path. Mentally revert the delegation
    /// and this drifts.
    #[test]
    fn cargo_config_entry_point_matches_plain_assemble_for_non_rust_pkg() {
        let src = tempdir().unwrap();
        write_schemas_only_pkg(src.path());

        let plain_dir = tempdir().unwrap();
        assemble_artifact(
            src.path(),
            &AssembleTarget::StagedDir(plain_dir.path().to_path_buf()),
            &slpkg_opts(false),
            &(),
        )
        .expect("plain assemble must succeed");

        let with_cfg_dir = tempdir().unwrap();
        // A nonexistent cargo-config path would make cargo error IF it were
        // consulted — proving it is safely ignored for a non-Rust package.
        assemble_artifact_with_cargo_config(
            src.path(),
            &AssembleTarget::StagedDir(with_cfg_dir.path().to_path_buf()),
            &slpkg_opts(false),
            &(),
            &[PathBuf::from("/nonexistent/cargo-override.toml")],
            None,
        )
        .expect("with-cargo-config assemble must succeed (config ignored, no Rust)");

        // The staged manifest + schema are identical either way.
        for rel in ["streamlib.yaml", "schemas/t.yaml"] {
            assert_eq!(
                std::fs::read(plain_dir.path().join(rel)).unwrap(),
                std::fs::read(with_cfg_dir.path().join(rel)).unwrap(),
                "staged {rel} must match between the two entry points"
            );
        }
    }

    #[test]
    fn is_linkable_crate_name_covers_streamlib_and_vulkan_jpeg() {
        assert!(is_linkable_crate_name("streamlib-plugin-sdk"));
        assert!(is_linkable_crate_name("streamlib"));
        assert!(is_linkable_crate_name("vulkan-jpeg"));
        assert!(is_linkable_crate_name("tatolab-vulkanalia"));
        assert!(is_linkable_crate_name("tatolab-vulkanalia-sys"));
        assert!(is_linkable_crate_name("tatolab-vulkanalia-vma"));
        assert!(!is_linkable_crate_name("serde"));
        assert!(!is_linkable_crate_name("tokio"));
    }

    #[test]
    fn release_closure_includes_the_easy_to_skip_libs_by_definition() {
        // The whole point of the closure-by-definition model: the libraries a
        // human-remembered flag used to skip (streamlib-plugin-sdk,
        // vulkan-jpeg) are members by construction. Runs against the real
        // workspace so it cross-checks cargo metadata ground truth. Mentally
        // revert the predicate to the old SDK-subset roots and
        // streamlib-plugin-sdk / vulkan-jpeg drop out — the exact 0.4.36
        // foot-gun.
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root two levels above streamlib-pack")
            .to_path_buf();
        let closure = compute_release_closure(&workspace_root).expect("compute closure");
        let names: std::collections::HashSet<&str> = closure.names().into_iter().collect();

        for required in [
            "streamlib-plugin-sdk",
            "vulkan-jpeg",
            "streamlib-macros",
            "streamlib-plugin-abi",
            "streamlib-engine",
            "streamlib",
        ] {
            assert!(
                names.contains(required),
                "release closure must contain {required} by definition; got {names:?}"
            );
        }

        // Every member is a linkable name (predicate holds for the whole set).
        for c in &closure.crates {
            assert!(
                is_linkable_crate_name(&c.name),
                "non-linkable crate {} leaked into the closure",
                c.name
            );
        }

        // Never-published crates (publish = false) must be excluded — the
        // test-fixture packages are workspace members with library targets
        // and linkable names, so only the publishable filter keeps them out.
        // Mentally revert `json_is_publishable` and they leak in, and
        // publish-crates.sh would try (and fail) to publish them.
        assert!(
            !names.iter().any(|n| n.contains("test-fixtures")),
            "publish = false crates leaked into the closure: {names:?}"
        );

        // Topological order: a dependency precedes its dependents. The plugin
        // ABI is a low-level dep of the SDK facade, so it must come first.
        let pos = |name: &str| closure.names().iter().position(|n| *n == name);
        if let (Some(abi), Some(sdk)) = (pos("streamlib-plugin-abi"), pos("streamlib")) {
            assert!(
                abi < sdk,
                "topo order violated: streamlib-plugin-abi ({abi}) must precede streamlib ({sdk})"
            );
        }
    }

    fn zip_entries(slpkg: &Path) -> Vec<String> {
        let bytes = std::fs::read(slpkg).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    fn zip_file_contents(slpkg: &Path, name: &str) -> String {
        use std::io::Read as _;
        let bytes = std::fs::read(slpkg).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut f = zip.by_name(name).unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn slpkg_schemas_only_carries_yaml_and_schemas_no_lib() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: schemas-only\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        assert_eq!(outcome.schemas, 1);
        assert!(!outcome.rebuilt);
        let entries = zip_entries(&out);
        assert!(entries.contains(&"streamlib.yaml".to_string()));
        assert!(entries.contains(&"schemas/t.yaml".to_string()));
        assert!(
            !entries.iter().any(|e| e.starts_with("lib/")),
            "schemas-only must not carry lib/, got {entries:?}"
        );
    }

    /// Write a minimal Rust package that declares one `#[processor]` in code.
    /// `manifest_processors` is spliced verbatim as the `processors:` YAML so a
    /// test can make it agree with — or drift from — the code.
    fn write_rust_processor_pkg(dir: &Path, manifest_processors: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"cam\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            r#"#[processor("@tatolab/camera/Camera", execution = manual, output("video", "@tatolab/core/VideoFrame"))]
            pub struct Camera;
            "#,
        )
        .unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: cam\n  version: 0.1.0\n{manifest_processors}"
            ),
        )
        .unwrap();
    }

    /// A source-only `.slpkg` build assembles when the committed `processors:`
    /// matches the `#[processor(...)]` declarations in code. (The Slpkg target
    /// ships source only — no cdylib build — so the Rust source scan is the only
    /// derivation that runs; no live runtime is needed.)
    #[test]
    fn slpkg_build_succeeds_when_manifest_matches_code() {
        let dir = tempdir().unwrap();
        write_rust_processor_pkg(
            dir.path(),
            "processors:\n- name: Camera\n  runtime: rust\n  execution: manual\n  outputs:\n  - name: video\n    schema: VideoFrame\n",
        );
        let out = dir.path().join("o.slpkg");
        let outcome = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .expect("in-sync manifest must build");
        assert_eq!(outcome.processors, 1);
    }

    /// The truth-flip gate (#1411): a `.slpkg` build is a hard error when the
    /// committed `processors:` disagrees with code. Here the manifest omits the
    /// `Camera` the code declares. Mentally revert
    /// `enforce_processor_manifest_matches_code` and this build succeeds despite
    /// shipping a manifest that lies about the package's processors.
    #[test]
    fn slpkg_build_fails_on_processor_manifest_drift() {
        let dir = tempdir().unwrap();
        // Manifest lists a different processor name than code declares.
        write_rust_processor_pkg(
            dir.path(),
            "processors:\n- name: Stale\n  runtime: rust\n  execution: manual\n",
        );
        let out = dir.path().join("o.slpkg");
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out),
            &slpkg_opts(false),
            &(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Camera"), "drift error must name Camera: {msg}");
        assert!(
            msg.contains("source of truth"),
            "drift error must explain code is truth: {msg}"
        );
    }

    /// Write a minimal schema-owning package: one `Local` schema plus whatever
    /// extra manifest sections (`dependencies:` / `schemas:` imports) a test
    /// splices in. No processors — the dependency reconcile runs regardless.
    fn write_schema_pkg(dir: &Path, extra_manifest: &str) {
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::write(
            dir.join("schemas/local.yaml"),
            "metadata:\n  type: LocalT\n  expected_payload_bytes: 16\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: leaf\n  version: 0.1.0\n\
                 schemas:\n  LocalT:\n    file: schemas/local.yaml\n{extra_manifest}"
            ),
        )
        .unwrap();
    }

    /// A `.slpkg` build hard-errors when code references a schema from a
    /// dependency the manifest never declares, and the error carries the
    /// `streamlib add` fix-it. Mentally revert
    /// `enforce_declared_dependencies_match_code` and this build succeeds while
    /// shipping a manifest whose `dependencies:` lies about what it needs.
    #[test]
    fn slpkg_build_fails_on_undeclared_schema_dependency() {
        let dir = tempdir().unwrap();
        write_schema_pkg(
            dir.path(),
            "  Imported:\n    package: '@other/dep'\n",
        );
        let out = dir.path().join("o.slpkg");
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out),
            &slpkg_opts(false),
            &(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("@other/dep"), "must name the dep: {msg}");
        assert!(
            msg.contains("streamlib add @other/dep@<version>"),
            "must carry the streamlib add fix-it: {msg}"
        );
    }

    /// A declared dependency referenced by no schema still builds — pruning is a
    /// non-fatal advisory — and a `runtime: true` marker is the sanctioned way
    /// to keep a runtime-composition dependency that imports no schema types.
    #[test]
    fn slpkg_build_prunes_are_non_fatal_and_runtime_marker_is_honored() {
        for dep_block in [
            "dependencies:\n  '@tatolab/audio':\n    version: ^1.0.0\n",
            "dependencies:\n  '@tatolab/audio':\n    version: ^1.0.0\n    runtime: true\n",
        ] {
            let dir = tempdir().unwrap();
            write_schema_pkg(dir.path(), dep_block);
            let out = dir.path().join("o.slpkg");
            assemble_artifact(
                dir.path(),
                &AssembleTarget::Slpkg(out),
                &slpkg_opts(false),
                &(),
            )
            .expect("an unreferenced dep is pruned non-fatally, not a build error");
        }
    }

    #[test]
    fn slpkg_assembly_refuses_under_an_active_link_but_staged_dir_is_exempt() {
        // The load-bearing pack-seam guard: a distributable `.slpkg` must not
        // assemble while a `streamlib link` marker exists above the package
        // dir. StagedDir (orchestrator load-time build) stays exempt so
        // linked dev trees keep running pipelines.
        let root = tempdir().unwrap();
        let marker_dir = root.path().join(link_marker::LINK_STATE_DIR);
        std::fs::create_dir_all(&marker_dir).unwrap();
        std::fs::write(
            marker_dir.join(link_marker::LINK_MANIFEST_FILE),
            r#"{"checkout":"/opt/sl","python_sdk_path":"/opt/sl/sdk/streamlib-python","deno_sdk_entrypoint_path":"/opt/sl/sdk/streamlib-deno/mod.ts","linked_at":"t","linked_crate_count":1,"state":"active","files":[]}"#,
        )
        .unwrap();

        let pkg = root.path().join("pkg");
        std::fs::create_dir_all(pkg.join("schemas")).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: linked-pkg\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();

        // Slpkg target → typed refusal.
        let err = assemble_artifact(
            &pkg,
            &AssembleTarget::Slpkg(pkg.join("o.slpkg")),
            &slpkg_opts(false),
            &(),
        )
        .unwrap_err();
        assert!(
            err.downcast_ref::<link_marker::LinkMarkerError>()
                .is_some_and(|e| matches!(
                    e,
                    link_marker::LinkMarkerError::PackRefusedWhileLinked { .. }
                )),
            "expected PackRefusedWhileLinked, got {err:?}"
        );

        // StagedDir target → exempt, assembles fine while linked.
        let staged = tempdir().unwrap();
        assemble_artifact(
            &pkg,
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: false,
                profile: CargoProfile::Release,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .expect("StagedDir assembly must stay exempt while linked");
    }

    #[test]
    fn slpkg_assembly_rejects_reserved_session_org_but_staged_dir_is_exempt() {
        // The reserved-org publish guard: `@session/…` is reserved for in-app /
        // session-local processors registered live at runtime and is NOT
        // publishable, so a distributable `.slpkg` that declares it is refused.
        // StagedDir (orchestrator dev-resolution build) stays exempt. Mentally
        // revert the `is_reserved_for_session()` bail and the Slpkg arm below
        // assembles instead of erroring, letting a session name squat the
        // registry.
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: session\n  name: my-processor\n  version: 0.0.1\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();

        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(dir.path().join("o.slpkg")),
            &slpkg_opts(false),
            &(),
        )
        .expect_err("a package under the reserved @session org must be refused for build/publish");
        let msg = format!("{err}");
        assert!(
            msg.contains("session"),
            "the refusal must name the reserved session org, got: {msg}"
        );

        // StagedDir target → exempt, assembles fine (dev-resolution path).
        let staged = tempdir().unwrap();
        assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: false,
                profile: CargoProfile::Release,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .expect("StagedDir assembly must stay exempt from the session-org publish guard");
    }

    #[test]
    fn slpkg_rust_is_source_only_ignores_prebuilt_lib() {
        // Source-only contract: a distributed `.slpkg` carries NO prebuilt
        // cdylib — the consumer builds from source on their host. Even when a
        // `lib/<triple>/` is pre-populated, the `Slpkg` target must NOT bundle
        // it. Revert the `StagedDir`-only build gate and the host-specific
        // binary leaks into the distribution artifact.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            b"#[processor(\"@tatolab/rp/P\", execution = manual)]\npub struct P;\n",
        )
        .unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let dylib = format!("librp.{}", host_dylib_extension());
        std::fs::write(triple_dir.join(&dylib), b"prebuilt-should-be-ignored").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        assert!(!outcome.rebuilt, "source-only pack never compiles");
        let entries = zip_entries(&out);
        // Crate SOURCE ships so the consumer can build.
        assert!(
            entries.contains(&"Cargo.toml".to_string()),
            "crate manifest must ship"
        );
        assert!(
            entries.contains(&"src/lib.rs".to_string()),
            "crate source must ship"
        );
        // The prebuilt cdylib does NOT — source-only.
        assert!(
            !entries.iter().any(|e| e.starts_with("lib/")),
            "source-only .slpkg must not carry a prebuilt cdylib, got {entries:?}"
        );
    }

    #[test]
    fn slpkg_rejects_path_cargo_dependency() {
        // The no-path gate: a published package must be standalone
        // (registry-only). A `path = …` Cargo dep is refused so a
        // non-standalone package can never ship. Revert the gate and a
        // dangling `../foo` path would break the consumer's off-tree build.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            b"[package]\nname='rp'\nversion='0.1.0'\n[dependencies]\nfoo = { path = \"../foo\", version = \"1.0\" }\n",
        )
        .unwrap();
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(dir.path().join("o.slpkg")),
            &slpkg_opts(false),
            &(),
        )
        .expect_err("a path Cargo dependency must be refused for a published package");
        let msg = format!("{err}");
        assert!(
            msg.contains("foo") && msg.contains("path") && msg.contains("standalone"),
            "error must name the offending path dep and the standalone contract, got: {msg}"
        );
    }

    #[test]
    fn slpkg_rejects_path_patch() {
        // The no-path gate also refuses a streamlib.yaml path `patch:` — the
        // dev-only monorepo override must never ship in a distribution artifact.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\ndependencies:\n  \"@tatolab/core\": \"^1.0.0\"\npatch:\n  \"@tatolab/core\":\n    path: ../core\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(dir.path().join("o.slpkg")),
            &slpkg_opts(false),
            &(),
        )
        .expect_err("a path patch must be refused for a published package");
        let msg = format!("{err}");
        assert!(
            msg.contains("@tatolab/core") && msg.contains("patch") && msg.contains("standalone"),
            "error must name the offending path patch and the standalone contract, got: {msg}"
        );
    }

    #[test]
    fn slpkg_python_strips_generated_wire_vocabulary() {
        // `_generated_/` is the JTD-codegen wire vocabulary — a build artifact
        // regenerated per-consumer at install time, never shipped as source.
        // Revert the `is_non_source_artifact` entry and stale generated bindings
        // leak into the distribution, shadowing the consumer's regenerated set.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname='py'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.py"), b"# entrypoint").unwrap();
        std::fs::create_dir(dir.path().join("_generated_")).unwrap();
        std::fs::write(
            dir.path().join("_generated_/tatolab__py.py"),
            b"# generated",
        )
        .unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        let entries = zip_entries(&out);
        assert!(
            entries.contains(&"p.py".to_string()),
            "entrypoint module must ship"
        );
        assert!(
            !entries.iter().any(|e| e.contains("_generated_")),
            "generated wire vocabulary must be stripped, got {entries:?}"
        );
    }

    #[test]
    fn cargo_lock_is_stripped_from_collected_source() {
        // A streamlib package is a cdylib library; shipping its Cargo.lock
        // breaks the consumer's build when a pinned dep is republished (the
        // lock's checksum goes stale → "checksum changed between lock files").
        // Revert the is_non_source_artifact entry and the lock leaks into the
        // .slpkg, reproducing exactly that failure at materialize time.
        use std::ffi::OsStr;
        assert!(
            is_non_source_artifact(OsStr::new("Cargo.lock")),
            "Cargo.lock must be a non-source artifact"
        );

        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            b"[package]\nname=\"p\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// src").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), b"# stale lock\n").unwrap();

        let mut files = Vec::new();
        collect_source_tree(dir.path(), &mut files).unwrap();
        let names: Vec<&str> = files.iter().map(|(rel, _)| rel.as_str()).collect();
        assert!(
            names.contains(&"Cargo.toml"),
            "manifest must ship: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("lib.rs")),
            "src must ship: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("Cargo.lock")),
            "Cargo.lock must be stripped from shipped source: {names:?}"
        );
    }

    #[test]
    fn slpkg_python_ships_full_source_tree_not_entrypoint_subset() {
        // Regression lock for the lossy-staging bug: a Python package is
        // distributed as SOURCE — every `.py` (entrypoint AND helper
        // modules) plus data/assets travels, not a wheel and not just the
        // entrypoint. Mentally revert to entrypoint-only collection and
        // `helper.py` / `models/weights.bin` vanish from the artifact, so
        // the processor's `import helper` fails at runtime — exactly the
        // shape that broke camera-python-display.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname='py'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.py"), b"import helper").unwrap();
        std::fs::write(dir.path().join("helper.py"), b"# non-entrypoint module").unwrap();
        std::fs::create_dir(dir.path().join("models")).unwrap();
        std::fs::write(dir.path().join("models/weights.bin"), b"\x00\x01\x02").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        assert!(
            !outcome.rebuilt,
            "no wheel/compile runs for a source-only Python package"
        );
        let entries = zip_entries(&out);
        assert!(
            entries.contains(&"p.py".to_string()),
            "entrypoint module must ship"
        );
        assert!(
            entries.contains(&"helper.py".to_string()),
            "non-entrypoint module must ship"
        );
        assert!(
            entries.contains(&"models/weights.bin".to_string()),
            "data/assets must ship"
        );
        assert!(
            entries.contains(&"pyproject.toml".to_string()),
            "dep manifest must ship"
        );
        // No wheel is built — the source IS the distribution.
        assert!(
            !entries.iter().any(|e| e.ends_with(".whl")),
            "no wheel should be produced, got {entries:?}"
        );
    }

    #[test]
    fn nested_and_namespace_python_entrypoint_packs_without_path_stat() {
        // Regression lock for the build-time entrypoint-resolution bug: a PyPA
        // object-reference entrypoint (`module:Class`) is a dotted *module
        // path*, not a filename. `cuda_fisheye.processor` maps to
        // `cuda_fisheye/processor.py` — here a PEP 420 namespace package (no
        // `__init__.py`) — which the old `format!("{module}.py")` path-stat
        // mis-resolved to the literal `cuda_fisheye.processor.py` and aborted.
        // Assembly must NOT reimplement import resolution: it ships the full
        // tree and lets the runtime's `importlib` resolve the entrypoint.
        // Mentally restore the per-processor path-stat and this bails on a
        // valid layout — even a `replace('.', "/")`-plus-`__init__.py` check
        // would still reject this namespace-package case.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"cuda_fisheye.processor:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname='py'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("cuda_fisheye")).unwrap();
        std::fs::write(
            dir.path().join("cuda_fisheye/processor.py"),
            b"class P:\n    pass\n",
        )
        .unwrap();

        let out = dir.path().join("o.slpkg");
        // Must NOT bail on the dotted/nested entrypoint.
        assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        let entries = zip_entries(&out);
        assert!(
            entries.contains(&"cuda_fisheye/processor.py".to_string()),
            "nested entrypoint module must ship via the source tree, got {entries:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn slpkg_python_excludes_dev_venv_and_tolerates_symlinks() {
        // Regression lock: a Python package dir often carries a dev-local
        // `.venv/` (machine-specific, symlink-laden) and stray symlinks.
        // Assembly must NOT ship `.venv/` and must NOT choke copying a
        // symlink (a dangling one would make `std::fs::copy` error).
        // Mentally revert either the `.venv` exclude or the symlink skip
        // and this either ships a huge venv or fails to assemble.
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname='py'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.py"), b"# real source").unwrap();
        // Dev venv with a regular file and a symlink (mirrors `lib64 -> lib`).
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(venv.join("lib")).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), b"home = /usr").unwrap();
        symlink("lib", venv.join("lib64")).unwrap();
        // A dangling top-level symlink — the exact shape that broke a copy.
        symlink("does-not-exist", dir.path().join("dangling-link")).unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .expect("assembly must tolerate .venv + dangling symlinks");
        let entries = zip_entries(&out);
        assert!(
            entries.contains(&"p.py".to_string()),
            "real source must ship"
        );
        assert!(
            !entries.iter().any(|e| e.starts_with(".venv/")),
            "dev .venv must not ship, got {entries:?}"
        );
        assert!(
            !entries.iter().any(|e| e.contains("dangling")),
            "symlinks must be skipped, got {entries:?}"
        );
    }

    #[test]
    fn slpkg_python_with_prebuilt_wheel_still_carries_it() {
        // A package that pre-ships a wheel under python/wheels/ keeps it
        // (the full-source copy includes it); the install side may prefer
        // it. Nothing is BUILT either way.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.py"), b"# stub").unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("py-0.1.0-py3-none-any.whl"), b"PK\x03\x04").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        assert_eq!(outcome.python_wheels, 1);
        let entries = zip_entries(&out);
        assert!(entries.contains(&"python/wheels/py-0.1.0-py3-none-any.whl".to_string()));
        assert!(entries.contains(&"p.py".to_string()));
    }

    /// A Deno package stages as a faithful mirror of the authored layout:
    /// the entrypoint `.ts` sits at the package root (NOT relocated under
    /// `deno/`), and every other authored file — helper `.ts`, `deno.json`,
    /// `.npmrc`, and assets a package ships (future embedded movies / html /
    /// data) — travels at its authored path. This is the same source-tree
    /// bundling Python/Rust already get; nothing is moved. Reverting the
    /// `has_deno` gate would drop the asset/`.npmrc`/helper assertions.
    #[test]
    fn slpkg_deno_source_mirrors_authored_layout() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("t.ts"), b"export default class {}").unwrap();
        std::fs::write(dir.path().join("helper.ts"), b"export const x = 1;").unwrap();
        std::fs::write(dir.path().join("deno.json"), b"{\"imports\":{}}").unwrap();
        std::fs::write(dir.path().join(".npmrc"), b"@tatolab:registry=http://x/\n").unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/logo.bin"), b"\x00\x01\x02").unwrap();
        // `_generated_` is a codegen artifact regenerated per-consumer at
        // stage time — it must NOT ship as source.
        std::fs::create_dir(dir.path().join("_generated_")).unwrap();
        std::fs::write(dir.path().join("_generated_/stale.ts"), b"// stale").unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        let entries = zip_entries(&out);
        // Entrypoint at the authored path — NOT relocated under `deno/`.
        assert!(
            entries.contains(&"t.ts".to_string()),
            "entrypoint must stage at root, got {entries:?}"
        );
        assert!(
            !entries.contains(&"deno/t.ts".to_string()),
            "must not relocate under deno/"
        );
        // The whole authored tree travels at its authored paths.
        assert!(
            entries.contains(&"helper.ts".to_string()),
            "helper module must travel"
        );
        assert!(entries.contains(&"deno.json".to_string()));
        assert!(
            entries.contains(&".npmrc".to_string()),
            ".npmrc must travel so the package is self-contained"
        );
        assert!(
            entries.contains(&"assets/logo.bin".to_string()),
            "assets must travel at their authored path"
        );
        // Codegen artifact excluded.
        assert!(
            !entries.contains(&"_generated_/stale.ts".to_string()),
            "_generated_ must not ship as source"
        );
    }

    /// A staged Deno package keeps `streamlib.yaml` beside the entrypoint
    /// `.ts` at the staged root — which is what the `@processor` decorator's
    /// sibling-manifest lookup requires. This locks the layout the runtime
    /// SDK depends on; relocating the `.ts` would break the decorator.
    #[test]
    fn staged_deno_manifest_sits_beside_entrypoint() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("t.ts"), b"export default class {}").unwrap();

        let staged = tempdir().unwrap();
        assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: false,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .unwrap();
        // Decorator does `join(dirname(<t.ts>), "streamlib.yaml")` — both at root.
        assert!(
            staged.path().join("t.ts").is_file(),
            "entrypoint at staged root"
        );
        assert!(
            staged.path().join("streamlib.yaml").is_file(),
            "manifest beside entrypoint"
        );
    }

    #[test]
    fn slpkg_reject_path_patches_fails() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: foo\n  version: 1.0.0\nschemas:\n  T:\n    file: schemas/t.yaml\ndependencies:\n  \"@tatolab/core\": \"^1.0.0\"\npatch:\n  \"@tatolab/core\":\n    path: ../core\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(dir.path().join("o.slpkg")),
            &slpkg_opts(false),
            &(),
        )
        .expect_err("a path-flavor patch must be rejected for a published package");
        let msg = format!("{err}");
        // The no-path gate intercepts before the manifest-write policy, with
        // the standalone-contract message.
        assert!(msg.contains("@tatolab/core") && msg.contains("standalone"));
    }

    #[test]
    fn strip_path_patches_removes_path_patch_keeps_dependencies() {
        // Engine-shaped manifest: a registry dep + a dev path patch. The
        // strip must drop the patch entry but leave the dependency range,
        // schemas, package block, and everything else intact.
        let yaml = "package:\n  org: tatolab\n  name: engine\n  version: 0.4.30\ndependencies:\n  \"@tatolab/escalate\": \"^1.0.0\"\npatch:\n  \"@tatolab/escalate\":\n    path: ../../packages/escalate\nschemas:\n  EscalateRequest:\n    package: \"@tatolab/escalate\"\n";
        let stripped = strip_path_patches(yaml).unwrap();
        // No path patch survives.
        assert!(!stripped.contains("../../packages/escalate"));
        assert!(!stripped.contains("patch:") || !stripped.contains("path:"));
        // The dependency range + schema import are preserved.
        assert!(stripped.contains("@tatolab/escalate"));
        assert!(stripped.contains("^1.0.0"));
        // Re-parse to prove it's still a valid, path-free manifest.
        let manifest: streamlib_idents::Manifest = serde_yaml::from_str(&stripped).unwrap();
        assert!(manifest.patch.is_empty());
        assert_eq!(manifest.dependencies.len(), 1);
    }

    #[test]
    fn strip_path_patches_preserves_non_path_patches() {
        // A git-flavor patch override is NOT a dev path affordance — it must
        // survive the strip (only `path:` patches are dev-only).
        let yaml = "package:\n  org: tatolab\n  name: foo\n  version: 1.0.0\ndependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n  \"@tatolab/bar\": \"^2.0.0\"\npatch:\n  \"@tatolab/core\":\n    path: ../core\n  \"@tatolab/bar\":\n    git: https://example.com/bar\n    rev: abc123\n";
        let stripped = strip_path_patches(yaml).unwrap();
        let manifest: streamlib_idents::Manifest = serde_yaml::from_str(&stripped).unwrap();
        // The git patch survives; the path patch is gone.
        assert_eq!(manifest.patch.len(), 1);
        let (dep_ref, bar) = manifest.patch.iter().next().unwrap();
        assert_eq!(dep_ref.to_string(), "@tatolab/bar");
        assert!(matches!(bar, DependencySpec::Git(_)));
    }

    #[test]
    fn strip_path_patches_idempotent_when_no_path_patch() {
        // A manifest with no path patch round-trips through parse+serialize
        // (content equal modulo serializer normalization — re-stripping a
        // stripped manifest is a no-op on the dependency graph).
        let yaml = "package:\n  org: tatolab\n  name: leaf\n  version: 1.0.0\ndependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n";
        let once = strip_path_patches(yaml).unwrap();
        let twice = strip_path_patches(&once).unwrap();
        assert_eq!(once, twice);
        let manifest: streamlib_idents::Manifest = serde_yaml::from_str(&once).unwrap();
        assert!(manifest.patch.is_empty());
        assert_eq!(manifest.dependencies.len(), 1);
    }

    #[test]
    fn strip_path_patches_in_dir_rewrites_file() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: engine\n  version: 0.4.30\ndependencies:\n  \"@tatolab/escalate\": \"^1.0.0\"\npatch:\n  \"@tatolab/escalate\":\n    path: ../../packages/escalate\n",
        )
        .unwrap();
        strip_path_patches_in_dir(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join("streamlib.yaml")).unwrap();
        assert!(!body.contains("../../packages/escalate"));
        let manifest: streamlib_idents::Manifest = serde_yaml::from_str(&body).unwrap();
        assert!(manifest.patch.is_empty());
        assert_eq!(manifest.dependencies.len(), 1);
    }

    #[test]
    fn staged_dir_target_extracts_layout() {
        // The StagedDir target writes the extracted layout (what the
        // orchestrator stages into the package cache) — byte-identical
        // per file to the slpkg's contents.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: schemas-only\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();

        let staged = tempdir().unwrap();
        assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: false,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .unwrap();
        assert!(staged.path().join("streamlib.yaml").is_file());
        assert!(staged.path().join("schemas/t.yaml").is_file());
    }

    #[test]
    fn rewrite_path_deps_makes_relative_paths_absolute() {
        // A relative `path:` dep must become absolute in the staged
        // manifest (the package is relocated out of its source tree).
        // Mentally reverting the rewrite would leave `../core` dangling
        // when the dep walk resolves it from the cache slot.
        let workspace = tempdir().unwrap();
        let pkg = workspace.path().join("pkg");
        let core = workspace.path().join("core");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::create_dir_all(&core).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: foo\n  version: 1.0.0\ndependencies:\n  \"@tatolab/core\":\n    path: ../core\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        std::fs::create_dir(pkg.join("schemas")).unwrap();
        std::fs::write(
            pkg.join("schemas/t.yaml"),
            "metadata:\n  type: T\n  expected_payload_bytes: 16\n",
        )
        .unwrap();

        let staged = tempdir().unwrap();
        assemble_artifact(
            &pkg,
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: false,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .unwrap();
        let staged_yaml = std::fs::read_to_string(staged.path().join("streamlib.yaml")).unwrap();
        assert!(
            !staged_yaml.contains("../core"),
            "relative path must be rewritten, got: {staged_yaml}"
        );
        let core_abs = std::fs::canonicalize(&core).unwrap();
        assert!(
            staged_yaml.contains(core_abs.to_str().unwrap()),
            "manifest must carry the absolute core path, got: {staged_yaml}"
        );
    }

    // ---- Version-stamp: derive crate version from the .slpkg semver ----

    #[test]
    fn stamp_replaces_literal_crate_version_with_manifest_version() {
        // In-tree Cargo.toml pins a stale crate version; the stamp derives
        // the version from streamlib.yaml. Mentally revert the stamp and the
        // stale literal survives.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rp\"\nversion = \"0.4.30\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let bytes = stamped_cargo_toml_bytes(dir.path(), "1.1.3-dev.4")
            .unwrap()
            .expect("a literal [package].version must be stamped");
        let out = String::from_utf8(bytes).unwrap();
        assert!(out.contains("version = \"1.1.3-dev.4\""), "got: {out}");
        assert!(
            !out.contains("0.4.30"),
            "stale literal must be gone, got: {out}"
        );
        // Other fields + shape preserved.
        assert!(out.contains("name = \"rp\""));
        assert!(out.contains("edition = \"2024\""));
    }

    #[test]
    fn stamp_replaces_workspace_inherited_version_with_literal() {
        // `version.workspace = true` cannot resolve in a standalone artifact
        // build (no defining workspace root travels), so it must become a
        // literal. Revert this branch and a staged workspace-member package
        // fails to `cargo build`.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rp\"\nversion.workspace = true\nedition = \"2024\"\n",
        )
        .unwrap();
        let bytes = stamped_cargo_toml_bytes(dir.path(), "1.0.0")
            .unwrap()
            .expect("a workspace-inherited version must be stamped to a literal");
        let out = String::from_utf8(bytes).unwrap();
        assert!(out.contains("version = \"1.0.0\""), "got: {out}");
        assert!(
            !out.contains("workspace = true"),
            "inheritance must be gone, got: {out}"
        );
    }

    #[test]
    fn stamp_preserves_comments_and_unrelated_fields() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "# top comment\n[package]\nname = \"rp\"\nversion = \"0.1.0\" # inline\nedition = \"2024\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        let out = String::from_utf8(
            stamped_cargo_toml_bytes(dir.path(), "2.5.0")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            out.contains("# top comment"),
            "comment preserved, got: {out}"
        );
        assert!(out.contains("[dependencies]") && out.contains("serde = \"1.0\""));
        assert!(out.contains("version = \"2.5.0\""), "got: {out}");
    }

    #[test]
    fn stamp_is_noop_without_cargo_toml_or_version() {
        // No Cargo.toml → nothing to stamp.
        let dir = tempdir().unwrap();
        assert!(
            stamped_cargo_toml_bytes(dir.path(), "1.0.0")
                .unwrap()
                .is_none()
        );
        // A [package] with no version key → nothing to stamp (ship verbatim).
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"rp\"\n").unwrap();
        assert!(
            stamped_cargo_toml_bytes(dir.path(), "1.0.0")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn slpkg_stamps_crate_version_from_manifest_semver() {
        // Integration: the emitted `.slpkg` carries a `Cargo.toml` whose
        // `[package].version` equals streamlib.yaml's `package.version`,
        // regardless of the stale in-tree value. Revert the stamp step and
        // the verbatim copy would carry `0.4.30`.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 1.1.3-dev.4\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rp\"\nversion = \"0.4.30\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            b"#[processor(\"@tatolab/rp/P\", execution = manual)]\npub struct P;\n",
        )
        .unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(out.clone()),
            &slpkg_opts(false),
            &(),
        )
        .unwrap();
        let entries = zip_entries(&out);
        // Exactly one Cargo.toml (the stamped one; verbatim copy deduped).
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.as_str() == "Cargo.toml")
                .count(),
            1,
            "exactly one Cargo.toml must ship, got {entries:?}"
        );
        let cargo = zip_file_contents(&out, "Cargo.toml");
        assert!(cargo.contains("version = \"1.1.3-dev.4\""), "got: {cargo}");
        assert!(
            !cargo.contains("0.4.30"),
            "stale version must not reach the artifact, got: {cargo}"
        );
    }

    #[test]
    fn staged_dir_stamps_crate_version_from_manifest_semver() {
        // Same lock for the StagedDir target (orchestrator load-time build):
        // the stale in-tree crate version cannot reach the built artifact.
        // A prebuilt lib/<triple>/ is pre-populated so the rust path takes the
        // prebuilt branch and no cargo build runs in the test.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 1.1.3-dev.4\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rp\"\nversion = \"0.4.30\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// crate source").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(
            triple_dir.join(format!("librp.{}", host_dylib_extension())),
            b"prebuilt",
        )
        .unwrap();

        let staged = tempdir().unwrap();
        assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: true,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .unwrap();
        let cargo = std::fs::read_to_string(staged.path().join("Cargo.toml")).unwrap();
        assert!(cargo.contains("version = \"1.1.3-dev.4\""), "got: {cargo}");
        assert!(
            !cargo.contains("0.4.30"),
            "stale version must not reach the staged build, got: {cargo}"
        );
    }

    #[test]
    fn stamp_handles_inline_package_table() {
        // The inline `package = { … }` form is valid TOML that
        // `as_table_mut` silently misses (it's an inline table, not a
        // standard table) — the stamp must cover it via `as_table_like_mut`
        // or the stale version ships verbatim.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "package = { name = \"rp\", version = \"0.4.30\", edition = \"2024\" }\n",
        )
        .unwrap();
        let out = String::from_utf8(
            stamped_cargo_toml_bytes(dir.path(), "1.0.0")
                .unwrap()
                .expect("inline package table must be stamped"),
        )
        .unwrap();
        assert!(out.contains("\"1.0.0\""), "got: {out}");
        assert!(
            !out.contains("0.4.30"),
            "stale inline version must be gone, got: {out}"
        );
    }

    #[test]
    fn malformed_cargo_toml_is_a_typed_error_not_a_panic() {
        // StagedDir assembly reaches the stamp parse (prebuilt lib +
        // no_build skips the cargo invocation); garbage TOML must surface
        // as a typed error naming the file, never a panic.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 1.0.0\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), ":::: not toml ::::\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// crate source").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(
            triple_dir.join(format!("librp.{}", host_dylib_extension())),
            b"prebuilt",
        )
        .unwrap();

        let staged = tempdir().unwrap();
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: true,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .expect_err("malformed Cargo.toml must be a typed error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Cargo.toml") && msg.contains("parse"),
            "error must name the file and the parse failure, got: {msg}"
        );
    }

    #[test]
    fn staged_dir_stamps_workspace_inherited_version_to_literal() {
        // Inherited→literal asserted through an emitted artifact: a
        // `version.workspace = true` source stages with a literal derived
        // from streamlib.yaml. The artifact lacks the defining workspace
        // root, so the stamped literal is what makes `[package].version`
        // resolvable there at all.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 1.0.0\nprocessors:\n  - name: P\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rp\"\nversion.workspace = true\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// crate source").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(
            triple_dir.join(format!("librp.{}", host_dylib_extension())),
            b"prebuilt",
        )
        .unwrap();

        let staged = tempdir().unwrap();
        assemble_artifact(
            dir.path(),
            &AssembleTarget::StagedDir(staged.path().to_path_buf()),
            &AssembleOptions {
                no_build: true,
                profile: CargoProfile::Dev,
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &(),
        )
        .unwrap();
        let cargo = std::fs::read_to_string(staged.path().join("Cargo.toml")).unwrap();
        assert!(cargo.contains("version = \"1.0.0\""), "got: {cargo}");
        assert!(
            !cargo.contains("version.workspace"),
            "version inheritance must be replaced with the literal, got: {cargo}"
        );
    }
}
