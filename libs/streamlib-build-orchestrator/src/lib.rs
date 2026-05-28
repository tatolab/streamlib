// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The default polyglot [`BuildOrchestrator`] implementation.
//!
//! [`PolyglotBuildOrchestrator`] is the in-process builder the SDK wires
//! (behind the `auto-build` feature) so build-requiring module loads
//! ([`Strategy::Path`] / [`Strategy::Git`] with a non-`NeverBuild`
//! [`BuildPolicy`]) materialize from source at runtime. A frozen
//! `.slpkg`-only deployment simply doesn't wire it and is therefore
//! compiler-free by construction.
//!
//! There is ONE materialization path, identical to installing a package
//! from a `.slpkg` or a GitHub repo: assemble the *complete* artifact
//! (via [`streamlib_pack::assemble_artifact`] — the same routine
//! `streamlib pack` uses: Rust cdylib via cargo, Python wheel via uv,
//! Deno bundle, schemas) and stage it as an extracted directory into the
//! package cache (`<STREAMLIB_HOME>/cache/packages/<name>-<version>/`).
//! A runtime-built staged dir is byte-identical to what extracting the
//! corresponding `.slpkg` would produce — dev, release, and
//! install-from-anywhere share the shape, so a package that loads in dev
//! can't silently break when distributed.
//!
//! Staging uses build-to-temp + atomic rename, with a
//! `.streamlib-build.json` sidecar recording the plugin-ABI version,
//! host triple, profile, and the source-input fingerprint that drives
//! [`BuildPolicy::IfStale`] for non-Rust packages. Rust packages always
//! invoke cargo (its own fingerprint short-circuits when clean AND
//! catches out-of-package / transitive changes a package-local
//! fingerprint cannot — the original stale-artifact trap).
//!
//! [`BuildOrchestrator`]: streamlib_engine::core::runtime::BuildOrchestrator
//! [`Strategy::Path`]: streamlib_engine::core::runtime::Strategy::Path
//! [`Strategy::Git`]: streamlib_engine::core::runtime::Strategy::Git
//! [`BuildPolicy`]: streamlib_engine::core::runtime::BuildPolicy

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use streamlib_cargo_build as build;
use streamlib_engine::core::runtime::{
    BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest,
    BuildSource, BuildStream, StagedArtifact,
};
use streamlib_pack::{
    assemble_artifact, AssembleOptions, AssembleTarget, PackEventSink, PackStream, PathDepPolicy,
};

/// Monotonic counter for unique per-build temp dir names (process-local;
/// combined with the PID to avoid cross-process collisions).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

const SIDECAR_NAME: &str = ".streamlib-build.json";

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
    /// host-profile default). Used by CI to force release builds
    /// regardless of how the host was compiled.
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
        let pkg_label = request.package.to_string();
        let triple = &request.host_triple;

        let config = build::read_minimal_project_config(pkg_dir)
            .map_err(|e| other(&pkg_label, format!("reading streamlib.yaml: {e}")))?
            .ok_or_else(|| other(&pkg_label, "no streamlib.yaml at source dir".into()))?;
        let package = config
            .package
            .as_ref()
            .ok_or_else(|| other(&pkg_label, "streamlib.yaml missing [package] section".into()))?;
        let cache_key = format!("{}-{}", package.name.as_str(), package.version);
        let cache_slot = streamlib_engine::core::get_cached_package_dir(&cache_key);

        let has_rust = build::has_rust_runtime_processors(&config);

        // Source-input fingerprint — drives IfStale for non-Rust packages
        // and is recorded in the sidecar regardless.
        let fingerprint = compute_inputs_hash(pkg_dir)
            .map_err(|e| other(&pkg_label, format!("fingerprinting inputs: {e}")))?;

        // ---- Staleness skip ----
        // IfStale + a package with NO Rust: a package-local content
        // fingerprint is a complete staleness oracle (nothing links code
        // outside the package). If the cached artifact matches the
        // fingerprint + toolchain context, skip the rebuild.
        //
        // A package WITH Rust always re-assembles: a Rust cdylib can link
        // code outside the package dir (the engine, in a dev workspace) a
        // package-local fingerprint can't see — so cargo's own fingerprint
        // (invoked inside `assemble_artifact`) is the only correct oracle,
        // and it short-circuits cheaply when clean.
        if matches!(request.policy, BuildPolicy::IfStale) && !has_rust && cache_slot.is_dir() {
            if let Some(side) = read_sidecar(&cache_slot) {
                if side.abi_version == streamlib_plugin_abi::STREAMLIB_ABI_VERSION
                    && side.triple == *triple
                    && side.profile == self.profile.label()
                    && side.inputs_hash == fingerprint
                {
                    tracing::debug!(package = %pkg_label, staged = %cache_slot.display(), "up to date — skipping rebuild");
                    return Ok(StagedArtifact {
                        staged_dir: cache_slot,
                        rebuilt: false,
                    });
                }
            }
        }

        // ---- Assemble + stage to the package cache ----
        // build-to-temp then atomic rename so a concurrent reader never
        // observes a half-staged slot.
        let temp_dir = stage_temp_dir(&cache_slot);
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| other(&pkg_label, format!("create temp stage dir: {e}")))?;

        let adapter = EngineSinkAdapter(sink);
        let outcome = assemble_artifact(
            pkg_dir,
            &AssembleTarget::StagedDir(temp_dir.clone()),
            &AssembleOptions {
                no_build: false,
                profile: self.profile,
                // The package is relocated into the cache; rewrite relative
                // `path:` deps to absolute so the transitive walk still
                // resolves each dep to its real source.
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
            },
            &adapter,
        )
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            build_failed(&pkg_label, format!("{e}"))
        })?;

        write_sidecar(&temp_dir, triple, self.profile, &fingerprint)
            .map_err(|e| other(&pkg_label, format!("writing build sidecar: {e}")))?;
        atomic_swap(&temp_dir, &cache_slot)
            .map_err(|e| other(&pkg_label, format!("atomic stage swap: {e}")))?;

        tracing::info!(package = %pkg_label, staged = %cache_slot.display(), rebuilt = outcome.rebuilt, "materialized package");
        Ok(StagedArtifact {
            staged_dir: cache_slot,
            rebuilt: outcome.rebuilt,
        })
    }
}

/// Adapts the engine's [`BuildEventSink`] to [`streamlib_pack`]'s lean
/// [`PackEventSink`], so assembly build logs flow through the engine's
/// event/`tracing` path.
struct EngineSinkAdapter<'a>(&'a dyn BuildEventSink);

fn lang_static(language: &str) -> &'static str {
    match language {
        "rust" => "rust",
        "python" => "python",
        "deno" => "deno",
        _ => "build",
    }
}

impl PackEventSink for EngineSinkAdapter<'_> {
    fn started(&self, language: &str) {
        self.0.emit(BuildEvent::Started {
            language: lang_static(language),
        });
    }
    fn line(&self, stream: PackStream, line: &str) {
        let stream = match stream {
            PackStream::Stdout => BuildStream::Stdout,
            PackStream::Stderr => BuildStream::Stderr,
        };
        self.0.emit(BuildEvent::Line {
            stream,
            line: line.to_string(),
        });
    }
    fn finished(&self, language: &str) {
        self.0.emit(BuildEvent::Finished {
            language: lang_static(language),
        });
    }
}

/// Parsed `.streamlib-build.json` sidecar contents.
struct Sidecar {
    abi_version: u32,
    triple: String,
    profile: String,
    inputs_hash: String,
}

/// Sidecar recording the staged artifact's toolchain context + the input
/// fingerprint it was built from. The fingerprint drives `IfStale` for
/// non-Rust packages; abi/triple/profile are defense-in-depth atop the
/// runtime `PluginDeclaration.abi_version` handshake.
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
/// every source file under `pkg_dir`, excluding build artifacts
/// (`target/`, staged `lib/`), VCS, and caches. Paths sorted so the
/// digest is stable. The orchestrator's OWN staleness oracle for
/// non-Rust packages — it doesn't depend on cargo, so it works for a
/// standalone package repo with no enclosing workspace.
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
            if is_excluded(&entry.file_name()) {
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

/// Replace `final_path` with `temp` atomically (best-effort): create the
/// cache parent, remove any existing slot, then rename. Each rename moves
/// a fully-staged dir, so no torn reads.
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

fn other(package: &str, detail: String) -> BuildError {
    BuildError::Other {
        package: package.to_string(),
        detail,
    }
}

fn build_failed(package: &str, detail: String) -> BuildError {
    BuildError::BuildFailed {
        tool: "assemble".to_string(),
        package: package.to_string(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use streamlib_idents::{Org, Package, PackageRef};

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    /// No-op engine event sink for tests that don't assert on build logs.
    struct NoopSink;
    impl BuildEventSink for NoopSink {
        fn emit(&self, _event: BuildEvent) {}
    }

    /// Point STREAMLIB_HOME at a fresh tempdir for the duration of a test
    /// so staging writes into an isolated package cache. Restores the
    /// previous value on drop. Tests using it are `#[serial]` (process-
    /// global env).
    struct HomeGuard {
        _tmp: tempfile::TempDir,
        prev: Option<String>,
    }
    impl HomeGuard {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let prev = std::env::var("STREAMLIB_HOME").ok();
            // SAFETY: tests are `#[serial]`, so no other thread races the
            // process-global environment during the guard's lifetime.
            unsafe { std::env::set_var("STREAMLIB_HOME", tmp.path()) };
            Self { _tmp: tmp, prev }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: see `HomeGuard::new` — serial tests, no env race.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
            }
        }
    }

    fn schemas_only_pkg(dir: &Path) {
        std::fs::write(
            dir.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: schemas-only\n  version: 0.1.0\nschemas:\n  TestSchema:\n    file: schemas/test_schema.yaml\n",
        )
        .unwrap();
        std::fs::create_dir(dir.join("schemas")).unwrap();
        std::fs::write(
            dir.join("schemas/test_schema.yaml"),
            "metadata:\n  type: TestSchema\n  max_payload_bytes: 1024\n",
        )
        .unwrap();
    }

    fn request(pkg_dir: &Path, policy: BuildPolicy) -> BuildRequest {
        BuildRequest {
            package: pkg_ref("tatolab", "schemas-only"),
            source: BuildSource::PackageDir(pkg_dir.to_path_buf()),
            policy,
            host_triple: build::host_target_triple().to_string(),
        }
    }

    #[test]
    #[serial]
    fn schemas_only_stages_into_package_cache() {
        // A schemas-only package (no compiler involved) assembles into the
        // package cache at cache/packages/<name>-<version>/ — the same
        // location an extracted .slpkg / GitHub install lands in — with
        // its streamlib.yaml + schemas/ present. rebuilt=false because no
        // build tool ran.
        let _home = HomeGuard::new();
        let src = tempfile::tempdir().unwrap();
        schemas_only_pkg(src.path());

        let orch = PolyglotBuildOrchestrator::default();
        let staged = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("schemas-only must materialize");

        let expected =
            streamlib_engine::core::get_cached_package_dir("schemas-only-0.1.0");
        assert_eq!(staged.staged_dir, expected, "must stage into the package cache");
        assert!(staged.staged_dir.join("streamlib.yaml").is_file());
        assert!(staged.staged_dir.join("schemas/test_schema.yaml").is_file());
        assert!(!staged.rebuilt, "no compiler ran for a schemas-only package");
        assert!(
            staged.staged_dir.join(SIDECAR_NAME).is_file(),
            "sidecar must be written for the IfStale skip-check"
        );
    }

    #[test]
    #[serial]
    fn ifstale_skips_when_unchanged_then_restages_after_edit() {
        // First materialize stages + writes the fingerprint sidecar. A
        // second IfStale materialize with unchanged source must skip
        // (fingerprint match). Editing a source file must bust the
        // fingerprint and re-stage. Mentally reverting the fingerprint
        // comparison would make the skip unconditional (or never) — this
        // locks "skip iff unchanged".
        let _home = HomeGuard::new();
        let src = tempfile::tempdir().unwrap();
        schemas_only_pkg(src.path());
        let orch = PolyglotBuildOrchestrator::default();

        let first = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .unwrap();
        assert!(!first.rebuilt);

        // Unchanged → skip path returns the same staged dir.
        let second = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .unwrap();
        assert_eq!(second.staged_dir, first.staged_dir);
        assert!(!second.rebuilt);

        // Edit a schema → fingerprint changes → re-stage (new content
        // lands in the cache slot).
        std::fs::write(
            src.path().join("schemas/test_schema.yaml"),
            "metadata:\n  type: TestSchema\n  max_payload_bytes: 2048\n",
        )
        .unwrap();
        let third = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .unwrap();
        let restaged = std::fs::read_to_string(third.staged_dir.join("schemas/test_schema.yaml"))
            .unwrap();
        assert!(
            restaged.contains("2048"),
            "edited schema must be re-staged into the cache, got: {restaged}"
        );
    }

    #[test]
    fn rejects_remote_and_slpkg_sources() {
        // The in-process orchestrator builds local package dirs only;
        // remote fetch is a build-service concern and .slpkg extraction is
        // the engine's job. Both must fail loud with UnsupportedSource.
        let orch = PolyglotBuildOrchestrator::default();
        for source in [
            BuildSource::Remote("https://example.com/pkg.tar.gz".into()),
            BuildSource::SlpkgArchive(PathBuf::from("/tmp/x.slpkg")),
        ] {
            let req = BuildRequest {
                package: pkg_ref("tatolab", "x"),
                source,
                policy: BuildPolicy::IfStale,
                host_triple: build::host_target_triple().to_string(),
            };
            let err = orch.materialize(&req, &NoopSink).expect_err("must reject");
            assert!(matches!(err, BuildError::UnsupportedSource(_)));
        }
    }
}
