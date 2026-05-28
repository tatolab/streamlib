// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The default polyglot [`BuildOrchestrator`] implementation.
//!
//! [`PolyglotBuildOrchestrator`] is the in-process builder the SDK wires
//! (behind the `auto-build` feature) so build-requiring module loads
//! ([`Strategy::Path`] / [`Strategy::Git`] with a non-`NeverBuild`
//! [`BuildPolicy`]) materialize from source at runtime. It is the one
//! shape every dev loop, CLI, and runtime-authoring host (AI agents
//! writing packages on the fly) uses; a frozen `.slpkg`-only deployment
//! simply doesn't wire it and is therefore compiler-free by
//! construction.
//!
//! Per-language dispatch (union — a package may host more than one):
//!
//! - **Rust** — `cargo build [--release] -p <crate>` produces the
//!   cdylib; staleness is delegated to cargo's own fingerprint (the
//!   build short-circuits when nothing changed — never an mtime check).
//! - **Python** — the source (`python/`, `pyproject.toml`, any
//!   pre-built `python/wheels/`) is staged; the subprocess runner
//!   installs from it.
//! - **Deno** — the `deno/` sources + `deno.json` are staged.
//! - **schemas-only** — just `streamlib.yaml` + `schemas/`.
//!
//! Output is staged into the streamlib build cache
//! (`<STREAMLIB_HOME>/build-cache/<profile>/<org>__<name>/`) via
//! build-to-temp + atomic rename, with a `.streamlib-build.json` sidecar
//! recording the plugin-ABI version, host triple, and profile. The
//! engine loads from the returned directory.
//!
//! [`BuildOrchestrator`]: streamlib_engine::core::runtime::BuildOrchestrator
//! [`Strategy::Path`]: streamlib_engine::core::runtime::Strategy::Path
//! [`Strategy::Git`]: streamlib_engine::core::runtime::Strategy::Git
//! [`BuildPolicy`]: streamlib_engine::core::runtime::BuildPolicy

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use streamlib_cargo_build as build;
use streamlib_engine::core::runtime::{
    BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest,
    BuildSource, BuildStream, StagedArtifact,
};
use streamlib_processor_schema::ProcessorLanguage;

/// Monotonic counter for unique per-build temp dir names (process-local;
/// combined with the PID to avoid cross-process collisions).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The default in-process polyglot builder. Construct with
/// [`PolyglotBuildOrchestrator::default`] (profile matched to the host
/// binary's compiled profile) or [`PolyglotBuildOrchestrator::with_profile`].
pub struct PolyglotBuildOrchestrator {
    profile: build::CargoProfile,
}

impl Default for PolyglotBuildOrchestrator {
    fn default() -> Self {
        Self {
            profile: host_profile(),
        }
    }
}

impl PolyglotBuildOrchestrator {
    /// Build packages with an explicit cargo profile (overriding the
    /// host-profile default). Used by `streamlib pack` / CI to force
    /// release builds regardless of how the host was compiled.
    pub fn with_profile(profile: build::CargoProfile) -> Self {
        Self { profile }
    }

    /// The profile this orchestrator builds packages with.
    pub fn profile(&self) -> build::CargoProfile {
        self.profile
    }
}

/// Match the host binary's compiled profile so a debug host loads debug
/// packages and a release host loads release packages — no mix. This is
/// a consistency choice, not an ABI requirement (`#[repr(C)]` makes
/// debug/release cdylibs cross-loadable); overridable via
/// [`PolyglotBuildOrchestrator::with_profile`].
fn host_profile() -> build::CargoProfile {
    if cfg!(debug_assertions) {
        build::CargoProfile::Dev
    } else {
        build::CargoProfile::Release
    }
}

impl BuildOrchestrator for PolyglotBuildOrchestrator {
    fn materialize(
        &self,
        request: &BuildRequest,
        sink: &dyn BuildEventSink,
    ) -> Result<StagedArtifact, BuildError> {
        let pkg_dir = match &request.source {
            BuildSource::PackageDir(dir) => dir.clone(),
            // The engine extracts `.slpkg` itself (pure filesystem) and
            // never routes it through the orchestrator; if one arrives,
            // it's a wiring bug.
            BuildSource::SlpkgArchive(p) => {
                return Err(BuildError::UnsupportedSource(format!(
                    "slpkg archive {} — extraction is the engine's job, not the builder's",
                    p.display()
                )))
            }
            // Remote fetch is a build-service concern (future streamlibd),
            // not the in-process builder's.
            BuildSource::Remote(url) => {
                return Err(BuildError::UnsupportedSource(format!(
                    "remote source {url} — the in-process orchestrator builds local \
                     sources only; a build-service orchestrator handles remotes"
                )))
            }
        };
        self.materialize_package_dir(request, &pkg_dir, sink)
    }
}

impl PolyglotBuildOrchestrator {
    fn materialize_package_dir(
        &self,
        request: &BuildRequest,
        pkg_dir: &Path,
        sink: &dyn BuildEventSink,
    ) -> Result<StagedArtifact, BuildError> {
        let package = &request.package;
        let pkg_label = package.to_string();

        let config = build::read_minimal_project_config(pkg_dir)
            .map_err(|e| other(&pkg_label, format!("reading streamlib.yaml: {e}")))?
            .ok_or_else(|| other(&pkg_label, "no streamlib.yaml at source dir".into()))?;

        let has_rust = build::has_rust_runtime_processors(&config);
        let triple = &request.host_triple;
        let cache_slot = self.cache_slot(package);

        // ---- staleness: the orchestrator's OWN language-agnostic input
        // fingerprint (NOT cargo's) — covers Rust src, python/, ts/,
        // schemas/, and the manifests. Works for any standalone package
        // repo; pure Python/Deno/schema packages never touch cargo.
        let inputs_hash = compute_inputs_hash(pkg_dir)
            .map_err(|e| other(&pkg_label, format!("hashing package inputs: {e}")))?;

        // IfStale + no Rust: if the staged artifact already matches
        // (inputs + ABI + triple + profile), it's up to date — skip.
        // Rust packages always fall through to `cargo build` below,
        // because a package-local hash can't see out-of-package /
        // transitive changes a Rust cdylib links (the #1072 case); cargo's
        // own fingerprint catches those and short-circuits cheaply.
        if matches!(request.policy, BuildPolicy::IfStale) && !has_rust {
            if let Some(prev) = read_sidecar(&cache_slot) {
                if prev.inputs_hash == inputs_hash
                    && prev.abi_version == streamlib_plugin_abi::STREAMLIB_ABI_VERSION
                    && prev.triple == *triple
                    && prev.profile == self.profile.label()
                    && cache_slot.join("streamlib.yaml").exists()
                {
                    tracing::debug!(
                        package = %pkg_label,
                        "inputs unchanged — staged artifact is up to date, skipping build"
                    );
                    return Ok(StagedArtifact {
                        staged_dir: cache_slot,
                        rebuilt: false,
                    });
                }
            }
        }

        // ---- per-language build (Rust is the only real compile today) ----
        let dylib_ext = build::host_dylib_extension();
        let built_cdylib: Option<PathBuf> = if has_rust {
            // Fast-fail before attempting the build if the toolchain is
            // absent (clear message rather than a raw spawn error).
            ensure_tool("cargo", "rust", "install the Rust toolchain — https://rustup.rs")?;
            sink.emit(BuildEvent::Started { language: "rust" });
            let cargo_name = build::read_cargo_package_name(pkg_dir)
                .map_err(|e| other(&pkg_label, format!("resolving cargo crate name: {e}")))?;
            let cdylib = self.cargo_build_streaming(pkg_dir, &cargo_name, dylib_ext, &pkg_label, sink)?;
            sink.emit(BuildEvent::Finished { language: "rust" });
            Some(cdylib)
        } else {
            None
        };
        if has_language(&config, ProcessorLanguage::Python) {
            sink.emit(BuildEvent::Started { language: "python" });
            // No compile step today — the subprocess runner installs from
            // the staged source / pre-built wheels.
            sink.emit(BuildEvent::Finished { language: "python" });
        }
        if has_language(&config, ProcessorLanguage::TypeScript) {
            sink.emit(BuildEvent::Started { language: "deno" });
            sink.emit(BuildEvent::Finished { language: "deno" });
        }

        // ---- stage into the build cache (temp + atomic rename) ----
        let temp_dir = stage_temp_dir(&cache_slot);
        // Clean any prior temp residue from an interrupted build.
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| other(&pkg_label, format!("create temp stage dir: {e}")))?;

        stage_into(pkg_dir, &temp_dir, built_cdylib.as_deref(), triple, dylib_ext)
            .map_err(|e| other(&pkg_label, format!("staging: {e}")))?;
        write_sidecar(&temp_dir, triple, self.profile, &inputs_hash)
            .map_err(|e| other(&pkg_label, format!("writing build sidecar: {e}")))?;

        atomic_swap(&temp_dir, &cache_slot)
            .map_err(|e| other(&pkg_label, format!("atomic stage swap: {e}")))?;

        tracing::info!(package = %pkg_label, staged = %cache_slot.display(), "materialized package");
        Ok(StagedArtifact {
            staged_dir: cache_slot,
            rebuilt: true,
        })
    }

    /// `<STREAMLIB_HOME>/build-cache/<profile>/<org>__<name>/`. One slot
    /// per package per profile; overwritten on each build. Separate from
    /// the `streamlib install` cache and from the host's cargo `target/`.
    fn cache_slot(&self, package: &streamlib_idents::PackageRef) -> PathBuf {
        build_cache_root()
            .join(self.profile.label())
            .join(build::staged_package_dir_name(
                package.org.as_str(),
                package.name.as_str(),
            ))
    }

    /// Run `cargo build` with the orchestrator's profile, streaming the
    /// build tool's output to `sink` line-by-line (so a daemon / UI sees
    /// progress) while capturing the JSON artifact stream to locate the
    /// produced cdylib. Cargo's own fingerprint short-circuits when
    /// nothing changed — this is the staleness oracle.
    fn cargo_build_streaming(
        &self,
        pkg_dir: &Path,
        cargo_name: &str,
        dylib_ext: &str,
        pkg_label: &str,
        sink: &dyn BuildEventSink,
    ) -> Result<PathBuf, BuildError> {
        let mut command = Command::new("cargo");
        command.arg("build");
        if matches!(self.profile, build::CargoProfile::Release) {
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
            .map_err(|e| build_failed(pkg_label, format!("spawn cargo: {e}")))?;

        // Stream stderr (human diagnostics) to the sink on a side thread.
        let stderr = child.stderr.take();
        let stderr_thread = stderr.map(|err| {
            // The sink is `&dyn` borrowed for this call; forward via a
            // channel so the reader thread doesn't capture the borrow.
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

        // Read stdout (JSON artifact stream) fully on this thread, while
        // draining stderr lines to the sink as they arrive.
        let mut stdout_json = String::new();
        if let Some(out) = child.stdout.take() {
            let reader = BufReader::new(out);
            for line in reader.lines().map_while(Result::ok) {
                // Drain any pending stderr lines first for ordering.
                if let Some((rx, _)) = &stderr_thread {
                    while let Ok(eline) = rx.try_recv() {
                        sink.emit(BuildEvent::Line {
                            stream: BuildStream::Stderr,
                            line: eline,
                        });
                    }
                }
                stdout_json.push_str(&line);
                stdout_json.push('\n');
            }
        }
        // Drain remaining stderr.
        if let Some((rx, handle)) = stderr_thread {
            let _ = handle.join();
            while let Ok(eline) = rx.recv() {
                sink.emit(BuildEvent::Line {
                    stream: BuildStream::Stderr,
                    line: eline,
                });
            }
        }

        let status = child
            .wait()
            .map_err(|e| build_failed(pkg_label, format!("wait cargo: {e}")))?;
        if !status.success() {
            return Err(build_failed(
                pkg_label,
                "cargo build exited non-zero (see build log)".into(),
            ));
        }

        build::parse_cargo_artifact_for_cdylib(&stdout_json, cargo_name, dylib_ext)
            .map_err(|e| build_failed(pkg_label, format!("parsing cargo artifacts: {e}")))?
            .ok_or_else(|| {
                build_failed(
                    pkg_label,
                    format!(
                        "cargo build produced no host cdylib (`*.{dylib_ext}`); confirm \
                         the crate declares `crate-type = [\"cdylib\"]`"
                    ),
                )
            })
    }
}

/// Whether the manifest declares at least one processor in `lang`.
fn has_language(config: &streamlib_processor_schema::ProjectConfigMinimal, lang: ProcessorLanguage) -> bool {
    config.processors.iter().any(|p| p.runtime.language == lang)
}

/// `<STREAMLIB_HOME>/build-cache`.
fn build_cache_root() -> PathBuf {
    streamlib_engine::core::get_streamlib_home().join("build-cache")
}

/// A unique sibling temp dir for build-to-temp + atomic rename.
fn stage_temp_dir(cache_slot: &Path) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = cache_slot
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "pkg".to_string());
    cache_slot
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".tmp-{name}-{pid}-{seq}"))
}

/// Copy the package's loadable content (manifest + schemas + per-language
/// artifacts) into `dest`. The cdylib, when built, lands under
/// `lib/<triple>/`.
fn stage_into(
    pkg_dir: &Path,
    dest: &Path,
    built_cdylib: Option<&Path>,
    triple: &str,
    _dylib_ext: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;

    // streamlib.yaml — rewritten so relative `path:` deps/patches point
    // at their ORIGINAL source (absolute), not at a sibling of the cache
    // slot. The package is relocated into the build cache, so a
    // `path: ../core` that resolved against `packages/<pkg>` would
    // otherwise break; rewriting to the absolute source path keeps the
    // engine's transitive-dep walk resolving each dep to its real source
    // (where the orchestrator can build it).
    stage_manifest_with_absolute_path_deps(pkg_dir, dest)?;

    // schemas/ (always, if present).
    copy_dir_if_exists(&pkg_dir.join("schemas"), &dest.join("schemas"))?;

    // Per-language source artifacts (present iff that language hosts processors).
    copy_file_if_exists(&pkg_dir.join("pyproject.toml"), &dest.join("pyproject.toml"))?;
    copy_dir_if_exists(&pkg_dir.join("python"), &dest.join("python"))?;
    copy_file_if_exists(&pkg_dir.join("deno.json"), &dest.join("deno.json"))?;
    copy_dir_if_exists(&pkg_dir.join("deno"), &dest.join("deno"))?;

    // Rust cdylib → lib/<triple>/.
    if let Some(cdylib) = built_cdylib {
        let triple_dir = dest.join("lib").join(triple);
        std::fs::create_dir_all(&triple_dir).with_context(|| "create lib/<triple>")?;
        let filename = cdylib
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("built cdylib has no filename"))?;
        std::fs::copy(cdylib, triple_dir.join(filename)).with_context(|| "copy cdylib")?;
    }
    Ok(())
}

/// Copy `streamlib.yaml` into `dest`, rewriting every relative `path:`
/// entry in `dependencies` / `patch` to an absolute path anchored at the
/// original `pkg_dir`. Registry / git entries pass through unchanged.
fn stage_manifest_with_absolute_path_deps(pkg_dir: &Path, dest: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    use streamlib_idents::DependencySpec;

    let yaml = std::fs::read_to_string(pkg_dir.join("streamlib.yaml"))
        .with_context(|| "read streamlib.yaml")?;
    let mut manifest: streamlib_processor_schema::StreamlibYaml =
        serde_yaml::from_str(&yaml).with_context(|| "parse streamlib.yaml")?;

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

    let out = serde_yaml::to_string(&manifest).with_context(|| "serialize streamlib.yaml")?;
    std::fs::write(dest.join("streamlib.yaml"), out).with_context(|| "write streamlib.yaml")?;
    Ok(())
}

/// Parsed `.streamlib-build.json` sidecar contents.
struct Sidecar {
    abi_version: u32,
    triple: String,
    profile: String,
    inputs_hash: String,
}

const SIDECAR_NAME: &str = ".streamlib-build.json";

/// Sidecar recording the staged artifact's toolchain context + the input
/// fingerprint it was built from. The fingerprint drives `IfStale`
/// staleness (language-agnostic — see [`compute_inputs_hash`]); the
/// abi/triple/profile are defense-in-depth atop the runtime
/// `PluginDeclaration.abi_version` handshake.
fn write_sidecar(
    dest: &Path,
    triple: &str,
    profile: build::CargoProfile,
    inputs_hash: &str,
) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "abi_version": streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
        "triple": triple,
        "profile": profile.label(),
        "inputs_hash": inputs_hash,
    }))?;
    std::fs::write(dest.join(SIDECAR_NAME), body)?;
    Ok(())
}

/// Read + parse the sidecar from a staged dir. `None` if absent or
/// unparseable (treated as "needs rebuild").
fn read_sidecar(staged_dir: &Path) -> Option<Sidecar> {
    let body = std::fs::read_to_string(staged_dir.join(SIDECAR_NAME)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(Sidecar {
        abi_version: u32::try_from(v.get("abi_version")?.as_u64()?).ok()?,
        triple: v.get("triple")?.as_str()?.to_string(),
        profile: v.get("profile")?.as_str()?.to_string(),
        inputs_hash: v.get("inputs_hash")?.as_str()?.to_string(),
    })
}

/// Language-agnostic content fingerprint of a package's build inputs:
/// every source file under `pkg_dir` (Rust `src/`, `python/`, `ts/`,
/// `schemas/`, manifests), excluding build artifacts (`target/`, staged
/// `lib/`), VCS, and caches. Paths are sorted so the digest is stable.
/// This is the orchestrator's OWN staleness oracle — it does not depend
/// on cargo, so it works for Python / Deno / schemas-only packages and
/// for any standalone package repo.
fn compute_inputs_hash(pkg_dir: &Path) -> anyhow::Result<String> {
    use std::hash::{Hash, Hasher};

    fn is_excluded(name: &std::ffi::OsStr) -> bool {
        matches!(
            name.to_str(),
            Some("target" | ".git" | "lib" | "node_modules" | "__pycache__" | ".streamlib-build.json")
        )
    }

    fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            if is_excluded(&file_name) {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_dir() {
                collect(&path, root, out)?;
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                out.push((rel, std::fs::read(&path)?));
            }
        }
        Ok(())
    }

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect(pkg_dir, pkg_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (rel, bytes) in &files {
        rel.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

/// Fast-fail preflight: confirm a build tool is on `PATH` before
/// attempting a build, so a missing toolchain surfaces as a clear
/// [`BuildError::ToolNotAvailable`] rather than a raw spawn error.
fn ensure_tool(tool: &str, language: &str, hint: &str) -> Result<(), BuildError> {
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
        Err(BuildError::ToolNotAvailable {
            tool: tool.to_string(),
            language: language.to_string(),
            hint: hint.to_string(),
        })
    }
}

/// Replace `final_path` with `temp` atomically (best-effort): remove any
/// existing slot, then rename. Rename is atomic within a filesystem; the
/// remove+rename window is guarded by the per-package nature of the slot
/// (concurrent builders each stage to a unique temp; last rename wins,
/// and each rename moves a fully-staged dir, so no torn reads).
fn atomic_swap(temp: &Path, final_path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| "create cache parent")?;
    }
    if final_path.exists() {
        std::fs::remove_dir_all(final_path).with_context(|| "remove stale cache slot")?;
    }
    std::fs::rename(temp, final_path).with_context(|| "rename temp into cache slot")?;
    Ok(())
}

fn copy_dir_if_exists(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if src.is_dir() {
        copy_dir_recursive(src, dst)?;
    }
    Ok(())
}

fn copy_file_if_exists(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if src.is_file() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn other(package: &str, detail: String) -> BuildError {
    BuildError::Other {
        package: package.to_string(),
        detail,
    }
}

fn build_failed(package: &str, detail: String) -> BuildError {
    BuildError::BuildFailed {
        tool: "cargo".to_string(),
        package: package.to_string(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use streamlib_engine::core::runtime::BuildPolicy;

    /// A `BuildEventSink` that discards events (the materialize path
    /// under test emits Started/Finished but we don't assert on them
    /// here).
    struct NullSink;
    impl BuildEventSink for NullSink {
        fn emit(&self, _event: BuildEvent) {}
    }

    /// Schemas-only materialize: no cargo, so this exercises the
    /// stage-into-cache + sidecar + atomic-swap path end-to-end without
    /// a toolchain. Reverting `stage_into`/`write_sidecar` would drop the
    /// manifest or sidecar and fail the assertions.
    #[test]
    #[serial]
    fn materializes_schemas_only_package_into_build_cache() {
        let home = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test; STREAMLIB_HOME read by build_cache_root.
        unsafe {
            std::env::set_var("STREAMLIB_HOME", home.path());
        }

        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: orch-schemas\n  version: \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir(src.path().join("schemas")).unwrap();
        std::fs::write(
            src.path().join("schemas/foo.yaml"),
            "metadata:\n  type: Foo\n",
        )
        .unwrap();

        let request = BuildRequest {
            package: streamlib_idents::PackageRef::new(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("orch-schemas").unwrap(),
            ),
            source: BuildSource::PackageDir(src.path().to_path_buf()),
            policy: BuildPolicy::IfStale,
            host_triple: build::host_target_triple().to_string(),
        };

        let orch = PolyglotBuildOrchestrator::default();
        let staged = orch.materialize(&request, &NullSink).expect("materialize");

        assert!(staged.staged_dir.join("streamlib.yaml").exists());
        assert!(staged.staged_dir.join("schemas/foo.yaml").exists());
        assert!(
            staged.staged_dir.join(".streamlib-build.json").exists(),
            "sidecar must record toolchain context"
        );
        assert!(
            !staged.staged_dir.join("lib").exists(),
            "schemas-only package must not create lib/"
        );
        let sidecar = std::fs::read_to_string(staged.staged_dir.join(".streamlib-build.json")).unwrap();
        assert!(sidecar.contains("abi_version"));
        assert!(sidecar.contains(build::host_target_triple()));

        unsafe {
            std::env::remove_var("STREAMLIB_HOME");
        }
    }

    /// THE trap-regression proof: editing a Rust package's source and
    /// re-materializing under `IfStale` produces a different staged
    /// cdylib — i.e. staleness is decided by cargo's fingerprint, which
    /// rebuilds on a source change. If a future refactor reintroduced an
    /// mtime shortcut (or skipped the build), the two staged artifacts
    /// would be byte-identical and this fails.
    ///
    /// `#[ignore]` because it shells out to `cargo build` (compiles a
    /// trivial standalone cdylib twice) — too heavy for the CI
    /// `--lib` run; invoke explicitly with `cargo test -- --ignored`.
    #[test]
    #[serial]
    #[ignore = "shells to cargo build twice; run explicitly via --ignored"]
    fn ifstale_rebuilds_after_source_edit() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("STREAMLIB_HOME", home.path());
        }

        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("streamlib.yaml"),
            concat!(
                "package:\n  org: tatolab\n  name: orch-rebuild\n  version: \"0.1.0\"\n",
                "processors:\n  - name: P\n    version: 1.0.0\n    description: x\n",
                "    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n",
            ),
        )
        .unwrap();
        std::fs::write(
            src.path().join("Cargo.toml"),
            concat!(
                "[package]\nname = \"orch-rebuild-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                "[lib]\ncrate-type = [\"cdylib\"]\n",
            ),
        )
        .unwrap();
        std::fs::create_dir(src.path().join("src")).unwrap();
        let lib_rs = src.path().join("src/lib.rs");
        std::fs::write(&lib_rs, "#[no_mangle]\npub extern \"C\" fn answer() -> u32 { 1 }\n").unwrap();

        let request = BuildRequest {
            package: streamlib_idents::PackageRef::new(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("orch-rebuild").unwrap(),
            ),
            source: BuildSource::PackageDir(src.path().to_path_buf()),
            policy: streamlib_engine::core::runtime::BuildPolicy::IfStale,
            host_triple: build::host_target_triple().to_string(),
        };
        let orch = PolyglotBuildOrchestrator::default();

        let read_cdylib = |dir: &Path| -> Vec<u8> {
            let triple_dir = dir.join("lib").join(build::host_target_triple());
            let entry = std::fs::read_dir(&triple_dir)
                .unwrap()
                .flatten()
                .find(|e| e.path().extension().is_some_and(|x| x == build::host_dylib_extension()))
                .expect("staged cdylib present");
            std::fs::read(entry.path()).unwrap()
        };

        let staged1 = orch.materialize(&request, &NullSink).expect("first materialize");
        let bytes1 = read_cdylib(&staged1.staged_dir);

        // Edit the source — change the function body.
        std::fs::write(&lib_rs, "#[no_mangle]\npub extern \"C\" fn answer() -> u32 { 2 }\n").unwrap();

        let staged2 = orch.materialize(&request, &NullSink).expect("second materialize");
        let bytes2 = read_cdylib(&staged2.staged_dir);

        assert_ne!(
            bytes1, bytes2,
            "editing source must produce a different cdylib — IfStale delegated to cargo's fingerprint and rebuilt"
        );

        unsafe {
            std::env::remove_var("STREAMLIB_HOME");
        }
    }

    /// IfStale skips when the package's inputs are unchanged
    /// (`rebuilt == false`) and rebuilds once a source file changes — the
    /// language-agnostic fingerprint gate, no cargo involved (schemas-only
    /// package). Reverting the hash check makes the second call rebuild
    /// and the assertion fails.
    #[test]
    #[serial]
    fn ifstale_skips_unchanged_then_rebuilds_on_edit_schemas_only() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("STREAMLIB_HOME", home.path());
        }

        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: orch-stale\n  version: \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir(src.path().join("schemas")).unwrap();
        let schema = src.path().join("schemas/foo.yaml");
        std::fs::write(&schema, "metadata:\n  type: Foo\n").unwrap();

        let request = BuildRequest {
            package: streamlib_idents::PackageRef::new(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("orch-stale").unwrap(),
            ),
            source: BuildSource::PackageDir(src.path().to_path_buf()),
            policy: BuildPolicy::IfStale,
            host_triple: build::host_target_triple().to_string(),
        };
        let orch = PolyglotBuildOrchestrator::default();

        let first = orch.materialize(&request, &NullSink).expect("first materialize");
        assert!(first.rebuilt, "first materialize must build");

        let second = orch.materialize(&request, &NullSink).expect("second materialize");
        assert!(
            !second.rebuilt,
            "unchanged inputs must skip the rebuild (IfStale fingerprint gate)"
        );

        // Edit a schema file → inputs fingerprint changes → rebuild.
        std::fs::write(&schema, "metadata:\n  type: Foo\n  max_payload_bytes: 4096\n").unwrap();
        let third = orch.materialize(&request, &NullSink).expect("third materialize");
        assert!(third.rebuilt, "an edited schema must trigger a rebuild");

        unsafe {
            std::env::remove_var("STREAMLIB_HOME");
        }
    }

    /// `ensure_tool` fast-fails with `ToolNotAvailable` for a missing tool
    /// and succeeds for a present one — the preflight that stops a build
    /// before a raw spawn error when the toolchain isn't installed.
    #[test]
    fn ensure_tool_detects_missing_toolchain() {
        let err = ensure_tool("streamlib-no-such-tool-xyz", "rust", "install it")
            .expect_err("a nonexistent tool must fail loud");
        assert!(matches!(err, BuildError::ToolNotAvailable { .. }), "got: {err:?}");
        // cargo is present in any environment that can run `cargo test`.
        ensure_tool("cargo", "rust", "install the Rust toolchain")
            .expect("cargo must be present in the test environment");
    }

    /// `Remote` / `SlpkgArchive` sources are rejected by the in-process
    /// builder (a build-service / the engine handle those respectively).
    #[test]
    fn rejects_remote_and_slpkg_sources() {
        let orch = PolyglotBuildOrchestrator::default();
        let pkg = streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("x").unwrap(),
        );
        for source in [
            BuildSource::Remote("https://example.com/x.tar".into()),
            BuildSource::SlpkgArchive("/tmp/x.slpkg".into()),
        ] {
            let request = BuildRequest {
                package: pkg.clone(),
                source,
                policy: BuildPolicy::IfStale,
                host_triple: build::host_target_triple().to_string(),
            };
            let err = orch.materialize(&request, &NullSink).unwrap_err();
            assert!(matches!(err, BuildError::UnsupportedSource(_)));
        }
    }
}
