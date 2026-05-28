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
    BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildRequest, BuildSource,
    BuildStream, StagedArtifact,
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

        let triple = &request.host_triple;
        let dylib_ext = build::host_dylib_extension();

        // ---- per-language build (Rust is the only real compile) ----
        let built_cdylib: Option<PathBuf> = if build::has_rust_runtime_processors(&config) {
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
        let cache_slot = self.cache_slot(package);
        let temp_dir = stage_temp_dir(&cache_slot);
        // Clean any prior temp residue from an interrupted build.
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| other(&pkg_label, format!("create temp stage dir: {e}")))?;

        stage_into(pkg_dir, &temp_dir, built_cdylib.as_deref(), triple, dylib_ext)
            .map_err(|e| other(&pkg_label, format!("staging: {e}")))?;
        write_sidecar(&temp_dir, triple, self.profile)
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

    // streamlib.yaml (always).
    std::fs::copy(pkg_dir.join("streamlib.yaml"), dest.join("streamlib.yaml"))
        .with_context(|| "copy streamlib.yaml")?;

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

/// Sidecar recording the toolchain context of the staged artifact —
/// defense-in-depth atop the runtime `PluginDeclaration.abi_version`
/// handshake, so a cached artifact built against a stale plugin ABI is
/// detectable before dlopen.
fn write_sidecar(dest: &Path, triple: &str, profile: build::CargoProfile) -> anyhow::Result<()> {
    let body = format!(
        "{{\n  \"abi_version\": {},\n  \"triple\": \"{}\",\n  \"profile\": \"{}\"\n}}\n",
        streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
        triple,
        profile.label(),
    );
    std::fs::write(dest.join(".streamlib-build.json"), body)?;
    Ok(())
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
