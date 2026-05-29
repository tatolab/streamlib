// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared package-artifact assembly.
//!
//! One routine — [`assemble_artifact`] — turns a package source
//! directory into a *complete* loadable artifact, per language:
//!
//! - **Rust** — `cargo build [--release] -p <crate>` → cdylib at
//!   `lib/<triple>/`.
//! - **Python** — `uv build --wheel` (or a pre-built wheel) →
//!   `python/wheels/*.whl`; the wheel carries every module + declared
//!   package-data, so the install side never needs the toolchain.
//! - **Deno** — entrypoint `.ts` under `deno/`.
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

    // Per-processor source: Python `.py` at root, Deno `.ts` under
    // `deno/`. Rust has no source to bundle (the cdylib is the artifact).
    for proc in &config.processors {
        if let Some(entrypoint) = &proc.entrypoint {
            let (source_file, archive_path) = match proc.runtime.language {
                ProcessorLanguage::Python => {
                    let module = entrypoint.split(':').next().unwrap_or(entrypoint);
                    let source = format!("{module}.py");
                    (source.clone(), source)
                }
                ProcessorLanguage::TypeScript => {
                    let source = entrypoint
                        .split(':')
                        .next()
                        .unwrap_or(entrypoint)
                        .to_string();
                    let archive = format!("deno/{source}");
                    (source, archive)
                }
                ProcessorLanguage::Rust => continue,
            };
            let source_path = pkg_dir.join(&source_file);
            if source_path.exists() {
                files.push((archive_path, source_path));
            } else {
                anyhow::bail!(
                    "Processor '{}' entrypoint file not found: {}",
                    proc.name,
                    source_path.display()
                );
            }
        }
    }

    let mut rebuilt = false;

    // Rust cdylib.
    let has_rust = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust));
    if has_rust {
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

    // Bundle the source tree when the package ships code that's run or
    // built FROM source:
    //   - Python → the engine runs it from source (see above).
    //   - Rust   → so a host on a different triple (or one given a
    //     source-only box) can `cargo build` the cdylib itself. The
    //     prebuilt cdylib for the packing host is already in `files`
    //     (lib/<triple>/), and `collect_source_tree` excludes `lib/`, so
    //     the two don't collide — the box becomes "sdist + one-triple
    //     wheel". A package whose Cargo deps are path/workspace-only only
    //     builds where those resolve (same constraint crates.io has); it
    //     relies on the bundled prebuilt for its own triple.
    if has_python || has_rust {
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

/// Whether a directory-entry name is a build artifact / dev-only file
/// that must NEVER ship as package source — VCS, language caches, build
/// outputs, and (critically) developer-local virtual environments. A
/// `.venv` left in a Python package dir during dev is the canonical trap:
/// it's huge, machine-specific, and full of symlinks, and shipping it
/// both bloats the artifact and breaks a plain file copy.
///
/// Shared by [`collect_source_tree`] and the orchestrator's source
/// fingerprint so "what counts as source" has one definition.
pub fn is_non_source_artifact(name: &std::ffi::OsStr) -> bool {
    match name.to_str() {
        Some(
            "target" | "lib" | ".git" | "node_modules" | "__pycache__"
            | ".streamlib-build.json" | ".venv" | "venv" | ".mypy_cache"
            | ".pytest_cache" | ".ruff_cache" | ".tox" | ".DS_Store",
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
    fn slpkg_rust_prebuilt_lib_bundles_triple_keyed_no_cargo() {
        // Pre-populated lib/<triple>/ → bundled as-is. No Cargo.toml, so a
        // stray cargo invocation would fail — passing proves cargo never ran.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let dylib = format!("librp.{}", host_dylib_extension());
        std::fs::write(triple_dir.join(&dylib), b"fake").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome =
            assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
                .unwrap();
        assert!(!outcome.rebuilt, "prebuilt lib must not trigger a build");
        let entries = zip_entries(&out);
        assert!(entries.contains(&format!("lib/{}/{}", host_target_triple(), dylib)));
    }

    #[test]
    fn slpkg_rust_bundles_source_alongside_prebuilt() {
        // The box is "sdist + one-triple wheel": a Rust package ships both
        // its prebuilt cdylib (for the packing host) AND its crate source,
        // so a host on a different triple can rebuild from the box. Revert
        // the source-bundle step and the .slpkg carries only the
        // host-specific binary — unusable on any other platform.
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
        std::fs::write(triple_dir.join(&dylib), b"prebuilt").unwrap();

        let out = dir.path().join("o.slpkg");
        let outcome =
            assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
                .unwrap();
        assert!(!outcome.rebuilt, "prebuilt present → no build");
        let entries = zip_entries(&out);
        assert!(
            entries.contains(&format!("lib/{}/{}", host_target_triple(), dylib)),
            "prebuilt cdylib must ship, got {entries:?}"
        );
        assert!(entries.contains(&"Cargo.toml".to_string()), "crate manifest must ship");
        assert!(entries.contains(&"src/lib.rs".to_string()), "crate source must ship");
    }

    #[test]
    fn slpkg_no_build_empty_lib_errors_actionably() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("lib").join(host_target_triple())).unwrap();
        let err = assemble_artifact(
            dir.path(),
            &AssembleTarget::Slpkg(dir.path().join("o.slpkg")),
            &slpkg_opts(true),
            &(),
        )
        .expect_err("no_build + empty lib must error");
        let msg = format!("{err}");
        assert!(msg.contains("--no-build") && msg.contains(host_target_triple()));
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

    #[test]
    fn slpkg_deno_source_lands_under_deno_subdir() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("t.ts"), b"export default class {}").unwrap();

        let out = dir.path().join("o.slpkg");
        assemble_artifact(dir.path(), &AssembleTarget::Slpkg(out.clone()), &slpkg_opts(false), &())
            .unwrap();
        let entries = zip_entries(&out);
        assert!(entries.contains(&"deno/t.ts".to_string()));
        assert!(!entries.contains(&"t.ts".to_string()), "must not duplicate at root");
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
        .expect_err("RejectPathPatches must reject a path-flavor patch");
        let msg = format!("{err}");
        assert!(msg.contains("@tatolab/core") && msg.contains("not publishable"));
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
