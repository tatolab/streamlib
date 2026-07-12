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
//! `streamlib pack` uses: Rust cdylib via cargo + crate source, Python as
//! full source, Deno bundle, schemas) and stage it as an extracted
//! directory into the package cache
//! (`<STREAMLIB_HOME>/.streamlib/cache/packages/<name>-<version>/`).
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

mod deno_codegen;
mod native_host;
mod python_venv;
mod release_check;

#[cfg(test)]
mod test_support;

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

/// A fingerprint-matched cache slot is reusable only if every language's
/// build-output dir is present: a Python package's provisioned `.venv`, and a
/// Deno package's regenerated `_generated_` wire vocabulary. Either missing
/// (out-of-band deletion, or a slot staged by an older orchestrator that never
/// ran that language's provision tail) forces a re-materialize; otherwise the
/// reused slot would run broken.
fn cache_slot_is_reusable(
    has_python: bool,
    venv_python_exists: bool,
    has_deno: bool,
    generated_exists: bool,
) -> bool {
    (!has_python || venv_python_exists) && (!has_deno || generated_exists)
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

        // Ensure the subprocess native FFI host(s) this package needs are built
        // + cached in the streamlib home, independent of the package slot. A
        // reused venv/slot can still be missing the host — they're independent
        // handoffs, the gap that left a registry consumer with a dead relative
        // path — so this runs before the staleness skip. It is itself
        // IfStale-cached per host triple + version (a cheap existence check
        // once present) and a no-op when the `STREAMLIB_*_NATIVE_LIB` env
        // override points at a prebuilt host.
        let host_version = env!("CARGO_PKG_VERSION");
        let ensure_host = |rt: native_host::NativeRuntime| {
            // Best-effort pre-build: if it can't (no registry configured,
            // network down), don't fail materialize — the spawn-time resolver
            // is the real gate and also covers the env override and the
            // monorepo `target/` (in-tree dev / tests). Only a pipeline that
            // actually spawns the runtime needs the host, and it gets an
            // actionable error there if it's truly absent.
            if let Err(e) = native_host::ensure_native_host(rt, host_version, self.profile) {
                tracing::warn!(
                    error = %e,
                    "could not pre-build the subprocess native host; it must be resolvable \
                     at spawn time (STREAMLIB_*_NATIVE_LIB, the home cache, or the monorepo target/)"
                );
            }
        };
        if build::has_python_runtime_processors(&config) {
            ensure_host(native_host::NativeRuntime::Python);
        }
        if build::has_typescript_runtime_processors(&config) {
            ensure_host(native_host::NativeRuntime::Deno);
        }

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
                let venv_python_exists =
                    cache_slot.join(".venv").join("bin").join("python").exists();
                let generated_exists = cache_slot.join("_generated_").is_dir();
                if side.abi_version == streamlib_plugin_abi::STREAMLIB_ABI_VERSION
                    && side.triple == *triple
                    && side.profile == self.profile.label()
                    && side.inputs_hash == fingerprint
                    && cache_slot_is_reusable(
                        python_venv::staged_package_has_python(&cache_slot),
                        venv_python_exists,
                        deno_codegen::staged_package_has_deno(&cache_slot),
                        generated_exists,
                    )
                {
                    tracing::debug!(package = %pkg_label, staged = %cache_slot.display(), "up to date — skipping rebuild");
                    return Ok(StagedArtifact {
                        staged_dir: cache_slot,
                        rebuilt: false,
                    });
                }
            }
        }

        // ---- Consumer-side release-completeness pre-check ----
        // A Rust package resolves its gitea-registry deps via cargo below. If
        // the configured registry holds a partial/mid-publish release of the
        // pinned version, fail fast here naming the missing artifacts instead
        // of surfacing it as a cryptic cargo `failed to select a version …`
        // deep in the build. No-op for dev/path builds (no registry) and
        // pre-atomic-release registries (no manifest) — see `release_check`.
        if has_rust {
            let pins = build::read_gitea_registry_pins(pkg_dir)
                .map_err(|e| other(&pkg_label, format!("reading gitea-registry pins: {e}")))?;
            release_check::assert_release_complete(&pkg_label, &pins)?;
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

        // Provision the package's Python venv inside the staged temp dir
        // (no-op when the package has no Python runtime). Building it into
        // `temp_dir` means the atomic rename below carries the venv into
        // place — no second rename. On failure, drop the half-staged temp.
        python_venv::provision_python_venv(&temp_dir, pkg_dir, &pkg_label).map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            e
        })?;

        // Regenerate the staged Deno package's `_generated_/` wire vocabulary
        // (no-op for non-Deno packages). `_generated_` is excluded from the
        // bundled source as a per-consumer artifact, so it must be rebuilt
        // here — the Deno mirror of the Python venv codegen above. Building
        // into `temp_dir` lets the atomic rename below carry it into place.
        deno_codegen::provision_deno_typescript(&temp_dir, &pkg_label).map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            e
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

    fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // Same "what is source" definition the staging copy uses, so
            // the fingerprint tracks exactly the files that get staged —
            // and never recurses into a dev `.venv` (huge, symlink-laden).
            if streamlib_pack::is_non_source_artifact(&entry.file_name()) {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
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

pub(crate) fn other(package: &str, detail: String) -> BuildError {
    BuildError::Other {
        package: package.to_string(),
        detail,
    }
}

pub(crate) fn build_failed(package: &str, detail: String) -> BuildError {
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

    // Mentally-revert check: a constant-`true` impl of `cache_slot_is_reusable`
    // would fail the venv-less and `_generated_`-less cases below (the
    // warm-cache build-output-less bugs).
    #[test]
    fn cache_slot_is_reusable_requires_build_outputs_per_language() {
        // (has_python, venv_exists, has_deno, generated_exists)
        // Plain slot (no Python, no Deno): always reusable.
        assert!(cache_slot_is_reusable(false, false, false, false));
        // Python slot without its venv: must re-materialize (the bug).
        assert!(!cache_slot_is_reusable(true, false, false, false));
        // Python slot with its venv: reusable.
        assert!(cache_slot_is_reusable(true, true, false, false));
        // Deno slot without its regenerated `_generated_`: must re-materialize.
        assert!(!cache_slot_is_reusable(false, false, true, false));
        // Deno slot with `_generated_` present: reusable.
        assert!(cache_slot_is_reusable(false, false, true, true));
        // Polyglot slot needs BOTH outputs present.
        assert!(!cache_slot_is_reusable(true, true, true, false));
        assert!(!cache_slot_is_reusable(true, false, true, true));
        assert!(cache_slot_is_reusable(true, true, true, true));
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

    /// Recursively collect `(relative_path, bytes)` for every file under
    /// `dir`, excluding `.pyc` (compileall artifacts vary by interpreter).
    /// Used to compare two generated-code trees for an exact file-set +
    /// content match.
    fn collect_tree(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(
            dir: &Path,
            root: &Path,
            out: &mut std::collections::BTreeMap<String, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "pyc") {
                    continue;
                }
                if entry.file_type().unwrap().is_dir() {
                    if path.file_name().is_some_and(|n| n == "__pycache__") {
                        continue;
                    }
                    walk(&path, root, out);
                } else {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn py_request(pkg_dir: &Path, policy: BuildPolicy) -> BuildRequest {
        BuildRequest {
            package: pkg_ref("tatolab", "py-source"),
            source: BuildSource::PackageDir(pkg_dir.to_path_buf()),
            policy,
            host_triple: build::host_target_triple().to_string(),
        }
    }

    #[test]
    #[serial]
    fn python_package_reuse_then_rebuild_via_unified_fingerprint() {
        // A pure-Python SOURCE package materialized with IfStale: the venv
        // tail runs (interpreter + populated _generated_ land), an unchanged
        // re-materialize SKIPS via the sidecar fingerprint (cache slot left
        // untouched), and a source edit busts the fingerprint so the next
        // materialize re-stages (cache slot wiped + rewritten with the new
        // content).
        //
        // The skip-vs-restage signal is a SENTINEL file we plant in the
        // cache slot after the first stage: `atomic_swap` does
        // `remove_dir_all(slot)` + rename, so a re-stage destroys the
        // sentinel while a skip (which returns the cached slot verbatim,
        // never touching it) leaves it intact. (`StagedArtifact.rebuilt` is
        // NOT a usable signal here: source-only Python invokes no
        // compiler/wheel-builder, so assemble reports rebuilt=false even on
        // a real re-stage — only Rust packages flip it.)
        //
        // Mentally-revert: if the sidecar staleness comparison were removed
        // (always re-assemble), the unchanged re-materialize would re-stage
        // and wipe the sentinel — the "sentinel survives unchanged
        // re-materialize" assertion fails. If it were inverted to "always
        // skip", the post-edit materialize would NOT pick up the edited
        // source — the "edited content re-staged" assertion fails.
        if crate::test_support::which_uv().is_none() {
            eprintln!("skipping: `uv` not on PATH");
            return;
        }
        let _home = HomeGuard::new();
        let root = tempfile::tempdir().unwrap();
        let sdk = crate::test_support::write_fixture_streamlib_sdk(root.path());
        let src = tempfile::tempdir().unwrap();
        crate::test_support::write_python_source_package(src.path(), &sdk);
        let orch = PolyglotBuildOrchestrator::default();

        // First materialize: assembles + provisions the venv.
        let first = orch
            .materialize(&py_request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("first materialize of a python source package must succeed offline");
        let venv_python = first.staged_dir.join(".venv").join("bin").join("python");
        assert!(
            venv_python.exists(),
            "venv interpreter must exist after first materialize at {}",
            venv_python.display()
        );
        // The installed (editable) streamlib's _generated_ is populated by
        // codegen — the unified provision tail ran.
        let generated = sdk.join("src").join("streamlib").join("_generated_");
        let populated = generated
            .read_dir()
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name() != "__init__.py");
        assert!(populated, "_generated_ must be populated after first materialize");

        // Plant a sentinel in the staged cache slot. A skip leaves it; a
        // re-stage (atomic_swap → remove_dir_all + rename) destroys it.
        let sentinel = first.staged_dir.join(".reuse-sentinel");
        std::fs::write(&sentinel, b"present").unwrap();

        // Second materialize, source UNCHANGED → fingerprint match → skip.
        let second = orch
            .materialize(&py_request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("second materialize (unchanged) must succeed");
        assert_eq!(
            second.staged_dir, first.staged_dir,
            "skip path returns the same staged dir"
        );
        assert!(
            sentinel.exists(),
            "unchanged source must SKIP (sidecar fingerprint match) — the cache slot \
             must be left untouched, so the sentinel survives"
        );
        assert!(
            venv_python.exists(),
            "venv must still be present after the skip (the skip returns the cached slot intact)"
        );

        // Edit a source file → fingerprint busts → re-stage.
        std::fs::write(
            src.path().join("pyproc.py"),
            "class PyProc:\n    VERSION = 2\n",
        )
        .unwrap();
        let third = orch
            .materialize(&py_request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("third materialize (after edit) must succeed");
        assert!(
            !sentinel.exists(),
            "edited source must bust the fingerprint and RE-STAGE — atomic_swap wipes \
             the old cache slot, destroying the sentinel"
        );
        let restaged = std::fs::read_to_string(third.staged_dir.join("pyproc.py")).unwrap();
        assert!(
            restaged.contains("VERSION = 2"),
            "edited source must be re-staged into the cache, got: {restaged}"
        );
        // The re-staged slot is freshly provisioned: venv present again.
        assert!(
            third.staged_dir.join(".venv").join("bin").join("python").exists(),
            "re-staged slot must carry a freshly provisioned venv"
        );
    }

    #[test]
    #[serial]
    fn orchestrator_generated_matches_standalone_codegen() {
        // Identical-output lock: the streamlib wire vocabulary the
        // orchestrator's venv tail writes into the installed SDK's
        // `_generated_` must be byte-for-byte identical to what
        // `streamlib_jtd_codegen::generate` produces directly for the SAME
        // installed SDK manifest with the SAME options (Python target,
        // project_dir = installed streamlib dir). This is the STRONGER lock
        // than asserting hardcoded expected content: it can't drift when
        // codegen output legitimately changes, and it directly asserts the
        // contract "orchestrator output == canonical codegen output" rather
        // than "orchestrator output == some frozen snapshot".
        //
        // Mentally-revert: if the orchestrator stopped running codegen (or
        // ran it with a different runtime/target/project_dir), the file-set
        // or contents would diverge and the equality assertion fails.
        if crate::test_support::which_uv().is_none() {
            eprintln!("skipping: `uv` not on PATH");
            return;
        }
        let _home = HomeGuard::new();
        let root = tempfile::tempdir().unwrap();
        let sdk = crate::test_support::write_fixture_streamlib_sdk(root.path());
        let src = tempfile::tempdir().unwrap();
        crate::test_support::write_python_source_package(src.path(), &sdk);
        let orch = PolyglotBuildOrchestrator::default();

        orch.materialize(&py_request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("materialize must succeed offline against the fixture SDK");

        // The editable install points `streamlib` at the SDK src; the venv
        // tail wrote codegen into <sdk>/src/streamlib/_generated_.
        let streamlib_dir = sdk.join("src").join("streamlib");
        let orchestrator_generated = streamlib_dir.join("_generated_");

        // Reproduce the exact codegen call the venv tail makes, into a
        // scratch dir.
        let scratch = tempfile::tempdir().unwrap();
        let standalone = scratch.path().join("_generated_");
        std::fs::create_dir_all(&standalone).unwrap();
        std::fs::write(standalone.join("__init__.py"), "").unwrap();
        streamlib_jtd_codegen::generate(streamlib_jtd_codegen::GenerateOptions {
            runtime: streamlib_jtd_codegen::RuntimeTarget::Python,
            output: standalone.clone(),
            project_dir: Some(streamlib_dir.clone()),
            schema_file: None,
            schema_dir: None,
            workspace_root: streamlib_dir.clone(),
            write_lockfile: false,
        })
        .expect("standalone codegen must succeed against the same installed manifest");

        let from_orch = collect_tree(&orchestrator_generated);
        let from_codegen = collect_tree(&standalone);

        assert!(
            from_codegen.len() > 1,
            "standalone codegen must emit generated module(s) beyond __init__.py, got: {:?}",
            from_codegen.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            from_orch.keys().collect::<Vec<_>>(),
            from_codegen.keys().collect::<Vec<_>>(),
            "orchestrator-generated file SET must equal standalone codegen's"
        );
        for (rel, bytes) in &from_codegen {
            assert_eq!(
                from_orch.get(rel),
                Some(bytes),
                "generated file `{rel}` must match standalone codegen byte-for-byte"
            );
        }
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
