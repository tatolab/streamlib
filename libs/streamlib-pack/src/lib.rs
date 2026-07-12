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

pub mod link_marker;

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
    // (`pkg install` / `Strategy::Registry`, AlwaysBuild), resolving every dep
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
            let built = cargo_build_streaming(pkg_dir, &cargo_name, dylib_ext, opts.profile, sink)?;
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

    // Emit.
    match target {
        AssembleTarget::Slpkg(zip_path) => emit_slpkg(zip_path, &files, &manifest_bytes)?,
        AssembleTarget::StagedDir(dir) => emit_staged_dir(dir, &files, &manifest_bytes)?,
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

/// Enforce the standalone, registry-only contract for a published `.slpkg`:
/// fail if the package carries anything path-flavored — a `path = …` Cargo
/// dependency or a streamlib.yaml `patch:` entry. Both are dev-only monorepo
/// affordances; a distributed source package must resolve every artifact from
/// the registry, so a stray path would ship and break the consumer's off-tree
/// build. Called only for the `Slpkg` target (`pkg build` / `pkg publish`).
fn ensure_no_path_artifacts(pkg_dir: &Path) -> Result<()> {
    let manifest_path = pkg_dir.join(Manifest::FILE_NAME);
    if manifest_path.exists() {
        let body = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: streamlib_processor_schema::StreamlibYaml =
            serde_yaml::from_str(&body).context("parse streamlib.yaml")?;
        let path_patches: Vec<String> = manifest
            .patch
            .iter()
            .filter(|(_, spec)| matches!(spec, DependencySpec::Path(_)))
            .map(|(dep, _)| dep.to_string())
            .collect();
        if !path_patches.is_empty() {
            anyhow::bail!(
                "{} carries path `patch:` override(s) for [{}] — a published package \
                 must be standalone (registry-only). Remove the `patch:` block; each \
                 dependency resolves from the registry by the version in `dependencies:`.",
                manifest_path.display(),
                path_patches.join(", "),
            );
        }
    }

    let cargo_path = pkg_dir.join("Cargo.toml");
    if cargo_path.exists() {
        let body = std::fs::read_to_string(&cargo_path)
            .with_context(|| format!("read {}", cargo_path.display()))?;
        let doc: toml::Value =
            toml::from_str(&body).with_context(|| format!("parse {}", cargo_path.display()))?;
        let offenders = cargo_path_dep_names(&doc);
        if !offenders.is_empty() {
            anyhow::bail!(
                "{} declares path dependenc(ies) [{}] — a published package must be \
                 standalone (registry-only). Replace each with \
                 `{{ version = \"…\", registry = \"gitea\" }}` so the crate resolves \
                 from the registry.",
                cargo_path.display(),
                offenders.join(", "),
            );
        }
    }
    Ok(())
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
            "target" | "lib" | ".git" | "node_modules" | "__pycache__"
            | "_generated_" | ".streamlib-build.json" | ".venv" | "venv"
            | "Cargo.lock"
            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache" | ".tox" | ".DS_Store",
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
fn cargo_build_streaming(
    pkg_dir: &Path,
    cargo_name: &str,
    dylib_ext: &str,
    profile: CargoProfile,
    sink: &dyn PackEventSink,
) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    if matches!(profile, CargoProfile::Release) {
        command.arg("--release");
    }
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
        if matches!(path.extension().and_then(|s| s.to_str()), Some("yaml" | "yml")) {
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
        PathDepPolicy::RewriteRelativeToAbsolute => {
            rewrite_manifest_path_deps_absolute(pkg_dir)
        }
    }
}

/// Reject path-flavor `patch:` entries (dev-time overrides that don't
/// generalize to a distributed artifact). Names every offender.
fn reject_path_patches(pkg_dir: &Path) -> Result<()> {
    let manifest_path = pkg_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_yaml::from_str(&body)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let offenders: Vec<String> = manifest
        .patch
        .iter()
        .filter_map(|(dep_ref, spec)| match spec {
            DependencySpec::Path(p) => Some(format!("`{}` → `{}`", dep_ref, p.path.display())),
            _ => None,
        })
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} carries path-flavor `patch:` entries which are dev-time overrides and not \
         publishable: {}. Remove them — or convert to a git/registry override — before packing.",
        manifest_path.display(),
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
    let yaml = std::fs::read_to_string(pkg_dir.join("streamlib.yaml"))
        .context("read streamlib.yaml")?;
    let mut manifest: streamlib_processor_schema::StreamlibYaml =
        serde_yaml::from_str(&yaml).context("parse streamlib.yaml")?;

    let abs_pkg = std::fs::canonicalize(pkg_dir).unwrap_or_else(|_| pkg_dir.to_path_buf());
    let rewrite = |map: &mut std::collections::BTreeMap<
        streamlib_idents::PackageRef,
        DependencySpec,
    >| {
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

/// Write the `.slpkg` zip: the manifest bytes as `streamlib.yaml`, then
/// every collected file at its archive path. Duplicate paths skipped.
fn emit_slpkg(zip_path: &Path, files: &[(String, PathBuf)], manifest_bytes: &[u8]) -> Result<()> {
    use zip::write::FileOptions;
    use zip::ZipWriter;

    let file = File::create(zip_path)
        .with_context(|| format!("create {}", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let options =
        FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut seen = std::collections::HashSet::new();
    zip.start_file("streamlib.yaml", options)?;
    zip.write_all(manifest_bytes)?;
    seen.insert("streamlib.yaml".to_string());

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

/// Write the extracted layout into `dir`: the manifest bytes as
/// `streamlib.yaml`, then every collected file at its archive path
/// (parents created). Duplicate paths skipped.
fn emit_staged_dir(dir: &Path, files: &[(String, PathBuf)], manifest_bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(dir.join("streamlib.yaml"), manifest_bytes).context("write streamlib.yaml")?;

    let mut seen = std::collections::HashSet::new();
    seen.insert("streamlib.yaml".to_string());
    for (name, src) in files {
        if !seen.insert(name.clone()) {
            continue;
        }
        let dest = dir.join(name);
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

    fn slpkg_opts(no_build: bool) -> AssembleOptions {
        AssembleOptions {
            no_build,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        }
    }

    fn zip_entries(slpkg: &Path) -> Vec<String> {
        let bytes = std::fs::read(slpkg).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
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
            "metadata:\n  type: T\n  max_payload_bytes: 16\n",
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
            r#"{"checkout":"/opt/sl","python_sdk_path":"/opt/sl/libs/streamlib-python","deno_sdk_entrypoint_path":"/opt/sl/libs/streamlib-deno/mod.ts","linked_at":"t","linked_crate_count":1,"state":"active","files":[]}"#,
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
            "metadata:\n  type: T\n  max_payload_bytes: 16\n",
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
                .is_some_and(|e| matches!(e, link_marker::LinkMarkerError::PackRefusedWhileLinked { .. })),
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
    fn slpkg_rust_is_source_only_ignores_prebuilt_lib() {
        // Source-only contract: a distributed `.slpkg` carries NO prebuilt
        // cdylib — the consumer builds from source on their host. Even when a
        // `lib/<triple>/` is pre-populated, the `Slpkg` target must NOT bundle
        // it. Revert the `StagedDir`-only build gate and the host-specific
        // binary leaks into the distribution artifact.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// crate source").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let dylib = format!("librp.{}", host_dylib_extension());
        std::fs::write(triple_dir.join(&dylib), b"prebuilt-should-be-ignored").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome =
            assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
                .unwrap();
        assert!(!outcome.rebuilt, "source-only pack never compiles");
        let entries = zip_entries(&out);
        // Crate SOURCE ships so the consumer can build.
        assert!(entries.contains(&"Cargo.toml".to_string()), "crate manifest must ship");
        assert!(entries.contains(&"src/lib.rs".to_string()), "crate source must ship");
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
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
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
            "metadata:\n  type: T\n  max_payload_bytes: 16\n",
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
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='py'\nversion='0.1.0'\n").unwrap();
        std::fs::write(dir.path().join("p.py"), b"# entrypoint").unwrap();
        std::fs::create_dir(dir.path().join("_generated_")).unwrap();
        std::fs::write(dir.path().join("_generated_/tatolab__py.py"), b"# generated").unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
            .unwrap();
        let entries = zip_entries(&out);
        assert!(entries.contains(&"p.py".to_string()), "entrypoint module must ship");
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
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"p\"\nversion=\"0.1.0\"\n")
            .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"// src").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), b"# stale lock\n").unwrap();

        let mut files = Vec::new();
        collect_source_tree(dir.path(), &mut files).unwrap();
        let names: Vec<&str> = files.iter().map(|(rel, _)| rel.as_str()).collect();
        assert!(names.contains(&"Cargo.toml"), "manifest must ship: {names:?}");
        assert!(names.iter().any(|n| n.contains("lib.rs")), "src must ship: {names:?}");
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
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='py'\nversion='0.1.0'\n").unwrap();
        std::fs::write(dir.path().join("p.py"), b"import helper").unwrap();
        std::fs::write(dir.path().join("helper.py"), b"# non-entrypoint module").unwrap();
        std::fs::create_dir(dir.path().join("models")).unwrap();
        std::fs::write(dir.path().join("models/weights.bin"), b"\x00\x01\x02").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome =
            assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
                .unwrap();
        assert!(!outcome.rebuilt, "no wheel/compile runs for a source-only Python package");
        let entries = zip_entries(&out);
        assert!(entries.contains(&"p.py".to_string()), "entrypoint module must ship");
        assert!(entries.contains(&"helper.py".to_string()), "non-entrypoint module must ship");
        assert!(entries.contains(&"models/weights.bin".to_string()), "data/assets must ship");
        assert!(entries.contains(&"pyproject.toml".to_string()), "dep manifest must ship");
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
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"cuda_fisheye.processor:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='py'\nversion='0.1.0'\n").unwrap();
        std::fs::create_dir(dir.path().join("cuda_fisheye")).unwrap();
        std::fs::write(dir.path().join("cuda_fisheye/processor.py"), b"class P:\n    pass\n").unwrap();

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
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='py'\nversion='0.1.0'\n").unwrap();
        std::fs::write(dir.path().join("p.py"), b"# real source").unwrap();
        // Dev venv with a regular file and a symlink (mirrors `lib64 -> lib`).
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(venv.join("lib")).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), b"home = /usr").unwrap();
        symlink("lib", venv.join("lib64")).unwrap();
        // A dangling top-level symlink — the exact shape that broke a copy.
        symlink("does-not-exist", dir.path().join("dangling-link")).unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
            .expect("assembly must tolerate .venv + dangling symlinks");
        let entries = zip_entries(&out);
        assert!(entries.contains(&"p.py".to_string()), "real source must ship");
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
            "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.py"), b"# stub").unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("py-0.1.0-py3-none-any.whl"), b"PK\x03\x04").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome =
            assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
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
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
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
        assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
            .unwrap();
        let entries = zip_entries(&out);
        // Entrypoint at the authored path — NOT relocated under `deno/`.
        assert!(entries.contains(&"t.ts".to_string()), "entrypoint must stage at root, got {entries:?}");
        assert!(!entries.contains(&"deno/t.ts".to_string()), "must not relocate under deno/");
        // The whole authored tree travels at its authored paths.
        assert!(entries.contains(&"helper.ts".to_string()), "helper module must travel");
        assert!(entries.contains(&"deno.json".to_string()));
        assert!(entries.contains(&".npmrc".to_string()), ".npmrc must travel so the package is self-contained");
        assert!(entries.contains(&"assets/logo.bin".to_string()), "assets must travel at their authored path");
        // Codegen artifact excluded.
        assert!(!entries.contains(&"_generated_/stale.ts".to_string()), "_generated_ must not ship as source");
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
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
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
        assert!(staged.path().join("t.ts").is_file(), "entrypoint at staged root");
        assert!(staged.path().join("streamlib.yaml").is_file(), "manifest beside entrypoint");
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
        std::fs::write(dir.path().join("schemas/t.yaml"), "metadata:\n  type: T\n  max_payload_bytes: 16\n").unwrap();
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
        std::fs::write(dir.path().join("schemas/t.yaml"), "metadata:\n  type: T\n  max_payload_bytes: 16\n").unwrap();

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
        std::fs::write(pkg.join("schemas/t.yaml"), "metadata:\n  type: T\n  max_payload_bytes: 16\n").unwrap();

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
}
