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
//! directory at the destination the engine hands over on the
//! [`BuildRequest`] (`staging_destination_slot_dir`). The orchestrator holds
//! no slot-path convention of its own — the engine owns the installed-package
//! slot seam and injects the exact write location.
//! A runtime-built staged dir is byte-identical to what extracting the
//! corresponding `.slpkg` would produce — dev, release, and
//! install-from-anywhere share the shape, so a package that loads in dev
//! can't silently break when distributed.
//!
//! Staging uses build-to-temp + atomic rename, with a
//! `.streamlib-build.json` sidecar recording the plugin-ABI version,
//! the ABI-layout + engine-transit build fingerprints (diagnostics),
//! host triple, profile, and the source-input fingerprint that drives
//! [`BuildPolicy::IfStale`] for non-Rust packages. Rust packages always
//! invoke cargo (its own fingerprint short-circuits when clean AND
//! catches out-of-package / transitive changes a package-local
//! fingerprint cannot — the original stale-artifact trap).
//!
//! [`BuildOrchestrator`]: streamlib_engine::core::runtime::BuildOrchestrator
//! [`BuildRequest`]: streamlib_engine::core::runtime::BuildRequest
//! [`Strategy::Path`]: streamlib_engine::core::runtime::Strategy::Path
//! [`Strategy::Git`]: streamlib_engine::core::runtime::Strategy::Git
//! [`BuildPolicy`]: streamlib_engine::core::runtime::BuildPolicy

mod deno_codegen;
mod python_venv;
mod session_ports;

#[cfg(test)]
mod test_support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use streamlib_cargo_build as build;
use streamlib_engine::core::runtime::{
    BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest,
    BuildSource, BuildStream, PackageSourceProvenance, StagedArtifact,
};
use streamlib_pack::{
    AssembleOptions, AssembleTarget, PackEventSink, PackStream, PathDepPolicy,
    assemble_artifact_with_cargo_config,
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
                )));
            }
            // Remote fetch is a build-service concern (future streamlibd),
            // not the in-process builder's.
            BuildSource::Remote(url) => {
                return Err(BuildError::UnsupportedSource(format!(
                    "remote source {url} — the in-process orchestrator builds local \
                     sources only; a build-service orchestrator handles remotes"
                )));
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

/// Whether a package's Rust half may reuse a staged slot instead of invoking
/// cargo. A non-Rust package always may (its content fingerprint is a complete
/// oracle). A Rust package may only when its source is an IMMUTABLE managed
/// extract (a mutable user checkout may carry `path:` / workspace-inherited
/// crate deps resolving OUTSIDE the package dir, which the package-local
/// fingerprint cannot see, so cargo — whose own fingerprint tracks them — must
/// always re-run there) AND there is NO active `streamlib link` (a link
/// resolves deps to a mutable checkout, so cargo must re-run) AND a host-triple
/// cdylib is already staged (a sidecar match alone doesn't prove the loadable
/// artifact exists).
fn rust_reuse_permitted(
    has_rust: bool,
    source_is_mutable: bool,
    link_active: bool,
    cdylib_present: bool,
) -> bool {
    !has_rust || (!source_is_mutable && !link_active && cdylib_present)
}

/// Rebuild-time complement of [`rust_reuse_permitted`]: on the paths that reach
/// an actual rebuild, whether assemble must ignore a `.so` a prior build
/// promoted into the (co-located) source tree and let cargo's own fingerprint
/// be the oracle. True for a mutable checkout, an active `streamlib link`, or an
/// unconditional [`BuildPolicy::AlwaysBuild`]; false only for an immutable
/// extract under [`BuildPolicy::IfStale`], which keeps the prebuilt preference
/// so a venv-only re-provision stays compiler-free.
fn ignore_in_tree_prebuilt_cdylib(
    source_is_mutable: bool,
    link_active: bool,
    policy: BuildPolicy,
) -> bool {
    source_is_mutable || link_active || matches!(policy, BuildPolicy::AlwaysBuild)
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
        let package = config.package.as_ref().ok_or_else(|| {
            other(
                &pkg_label,
                "streamlib.yaml missing [package] section".into(),
            )
        })?;
        // The engine computed the staging destination via the installed-package
        // slot seam and handed it over on the request; the orchestrator holds no
        // slot-path convention of its own. `read_minimal_project_config` above is
        // still needed (has_rust, [package], the reserved-session org check).
        let cache_slot = request.staging_destination_slot_dir.clone();

        let has_rust = build::has_rust_runtime_processors(&config);

        // The subprocess native FFI host (`streamlib-python-native` /
        // `streamlib-deno-native`) is resolved lazily at spawn time by the
        // engine's `native_lib_resolver`: the `STREAMLIB_*_NATIVE_LIB` env
        // override, then the monorepo `target/{debug,release}` (in-tree dev /
        // link / tests). The orchestrator does not pre-build it.

        // Source-input fingerprint — drives IfStale reuse and is recorded in
        // the sidecar regardless.
        let fingerprint = compute_inputs_hash(pkg_dir)
            .map_err(|e| other(&pkg_label, format!("fingerprinting inputs: {e}")))?;

        // Discover the active `streamlib link` once, up front: it gates Rust
        // build-once-reuse below (a link resolves deps to a mutable checkout,
        // so cargo must re-run) and threads the checkout into the assemble
        // pass further down.
        let active_link = discover_active_build_link(&pkg_label)?;

        // ---- Staleness skip (build-once-reuse) ----
        // IfStale + a matching sidecar (abi/triple/profile/inputs_hash) + the
        // per-language build outputs present ⇒ reuse the slot, invoke no build.
        //
        // Non-Rust package: the package-local content fingerprint is a
        // complete staleness oracle (nothing links code outside the package
        // dir), so the fingerprint alone gates reuse.
        //
        // Rust package: reuse only when the source is an IMMUTABLE managed
        // extract (a mutable user checkout — `Strategy::Path` / a link
        // override / an install-time `path:` source — may carry crate deps
        // resolving outside the package dir, so its package-local fingerprint
        // is not a complete oracle and cargo must re-run), AND there is NO
        // active `streamlib link` (a link resolves deps to a mutable checkout),
        // AND a host-triple cdylib is already staged. This is the zero-cargo
        // second load for an installed Rust package; a mutable-source or
        // active-link edit still falls through to a full rebuild.
        let source_is_mutable = matches!(
            request.source_provenance,
            PackageSourceProvenance::MutableUserCheckout
        );
        let rust_reuse_permitted = rust_reuse_permitted(
            has_rust,
            source_is_mutable,
            active_link.is_some(),
            slot_has_host_cdylib(&cache_slot, triple),
        );
        if matches!(request.policy, BuildPolicy::IfStale)
            && cache_slot.is_dir()
            && rust_reuse_permitted
            && let Some(side) = read_sidecar(&cache_slot)
        {
            let venv_python_exists = cache_slot.join(".venv").join("bin").join("python").exists();
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
                tracing::debug!(package = %pkg_label, staged = %cache_slot.display(), has_rust, "up to date — skipping rebuild");
                return Ok(StagedArtifact {
                    staged_dir: cache_slot,
                    rebuilt: false,
                });
            }
        }

        // ---- Assemble + stage to the package cache ----
        // build-to-temp then atomic rename so a concurrent reader never
        // observes a half-staged slot.
        let temp_dir = stage_temp_dir(&cache_slot);
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| other(&pkg_label, format!("create temp stage dir: {e}")))?;

        // Under an active link (discovered up front) every staged build
        // resolves its streamlib crates from the linked checkout: the Rust
        // cdylib via the consumer's `streamlib link`-emitted `[patch]` cargo
        // config, and the Python venv via the checkout's Python SDK. Host +
        // plugin then come from one source tree, which is what removes the
        // mixed host/plugin ABI hazard from the dev loop.
        if let Some(link) = &active_link {
            tracing::info!(
                package = %pkg_label,
                checkout = %link.checkout.display(),
                cargo_config = ?link.consumer_cargo_config,
                python_sdk = %link.python_sdk_path.display(),
                "streamlib link active — building staged package against the linked checkout"
            );
        }
        let cargo_config_files: Vec<PathBuf> = active_link
            .as_ref()
            .and_then(|l| l.consumer_cargo_config.clone())
            .into_iter()
            .collect();
        // The checkout is threaded to the package's `build.rs` schema-dep
        // codegen (via `STREAMLIB_LINK_CHECKOUT`) so schema deps resolve from
        // the checkout too — the schema half of the dev loop that the cargo
        // `[patch]` above covers for crate deps. `None` off a link ⇒ unchanged.
        let link_checkout = active_link.as_ref().map(|l| l.checkout.as_path());

        // The staleness skip above already spared an immutable-extract slot
        // with a matching cdylib; reaching here means we ARE rebuilding. A
        // mutable checkout, an active link, or an unconditional `AlwaysBuild`
        // must not have assemble honor a `.so` the prior build promoted into
        // the (co-located) source tree — that would silently reuse a stale
        // artifact. Let cargo's own fingerprint be the oracle in those cases;
        // an immutable extract under `IfStale` keeps the prebuilt preference
        // so a venv-only re-provision stays compiler-free.
        let ignore_in_tree_prebuilt_cdylib = ignore_in_tree_prebuilt_cdylib(
            source_is_mutable,
            active_link.is_some(),
            request.policy,
        );

        let adapter = EngineSinkAdapter(sink);
        let outcome = assemble_artifact_with_cargo_config(
            pkg_dir,
            &AssembleTarget::StagedDir(temp_dir.clone()),
            &AssembleOptions {
                no_build: false,
                profile: self.profile,
                // The package is relocated into the cache; rewrite relative
                // `path:` deps to absolute so the transitive walk still
                // resolves each dep to its real source.
                path_deps: PathDepPolicy::RewriteRelativeToAbsolute,
                ignore_in_tree_prebuilt_cdylib,
            },
            &adapter,
            &cargo_config_files,
            link_checkout,
        )
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            build_failed(&pkg_label, format!("{e}"))
        })?;

        // Provision the package's Python venv inside the staged temp dir
        // (no-op when the package has no Python runtime). Building it into
        // `temp_dir` means the atomic rename below carries the venv into
        // place — no second rename. On failure, drop the half-staged temp.
        // Under an active link the venv installs the checkout's Python SDK.
        python_venv::provision_python_venv(
            &temp_dir,
            active_link.as_ref().map(|l| l.python_sdk_path.as_path()),
            active_link.as_ref().map(|l| l.checkout.as_path()),
            &pkg_label,
        )
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            e
        })?;

        // Regenerate the staged Deno package's `_generated_/` wire vocabulary
        // (no-op for non-Deno packages). `_generated_` is excluded from the
        // bundled source as a per-consumer artifact, so it must be rebuilt
        // here — the Deno mirror of the Python venv codegen above. Building
        // into `temp_dir` lets the atomic rename below carry it into place.
        deno_codegen::provision_deno_typescript(
            &temp_dir,
            active_link.as_ref().map(|l| l.checkout.as_path()),
            &pkg_label,
        )
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            e
        })?;

        // ---- session-source port extraction ----
        // A live-submitted `@session/<name>` package stages a placeholder
        // `inputs: []` / `outputs: []` manifest (the submit site cannot know the
        // ports without running the source). Now that the subprocess runtime is
        // provisioned, derive the REAL ports from the staged source by running
        // the language's import-and-enumerate extractor and splice them into the
        // staged manifest before the atomic rename carries it into the cache.
        // Gated on the reserved session org so a normal package (whose committed
        // `processors:` is the source of truth, drift-checked at `pkg build`) is
        // never rewritten here.
        if package.org.is_reserved_for_session() {
            session_ports::splice_session_manifest_ports(&temp_dir, active_link.as_ref(), &pkg_label)
                .map_err(|e| {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    e
                })?;
        }

        // Land the staged temp dir into the destination. Two shapes:
        //
        // - Detached destination (source root_dir ≠ the co-located slot — a
        //   git-rev or by-version checkout materialized into a distinct
        //   `streamlib_modules` slot): whole-dir atomic swap into the slot. The
        //   sidecar completion marker is written into the temp dir LAST, just
        //   before the swap, so a slot lacking it (an aborted build) is treated
        //   as needing a rebuild rather than loaded half-built. This is the
        //   permanent copy-to-slot materialize path for a detached checkout.
        //
        // - In-place destination (the destination IS the package's own source
        //   dir — the #1506 co-located slot): the source files already sit at
        //   their destination, so promote ONLY the regenerated build-output
        //   units (`lib/<triple>/`, `.venv/`, `_generated_/`) into the slot and
        //   leave every source file untouched. Each unit lands via a
        //   displace-to-`.old` atomic swap and the sidecar completion marker is
        //   the single atomic flip written LAST, so a reader gating on the
        //   marker sees the prior or the new complete state, never a torn one.
        if destination_is_source_dir(&cache_slot, pkg_dir) {
            promote_build_outputs_in_place(
                &temp_dir,
                &cache_slot,
                triple,
                self.profile,
                &fingerprint,
                source_is_mutable,
            )
            .map_err(|e| other(&pkg_label, format!("promoting build outputs in place: {e}")))?;
            let _ = std::fs::remove_dir_all(&temp_dir);
        } else {
            write_sidecar(&temp_dir, triple, self.profile, &fingerprint)
                .map_err(|e| other(&pkg_label, format!("writing build sidecar: {e}")))?;
            atomic_swap(&temp_dir, &cache_slot)
                .map_err(|e| other(&pkg_label, format!("atomic stage swap: {e}")))?;
        }

        // Artifact-only retention: `cargo build` runs in the package SOURCE dir
        // (`current_dir(pkg_dir)`), so a Rust build leaves a `target/` there —
        // heavy, regenerable scratch that is never part of the loadable
        // artifact (the cdylib is copied into `lib/<triple>/`). Reclaim it now
        // so a co-located source tree keeps only its artifact
        // (`lib/<triple>/*.so` + `.venv/` + `_generated_/` + manifest), not the
        // build scratch. `streamlib pkg cache-gc` reclaims it across slots for
        // a build that was interrupted before reaching here.
        //
        // ONLY for an immutable managed extract with NO active `streamlib link`.
        // For a mutable user checkout (`Strategy::Path` / a link override /
        // an install-time `path:` source) `pkg_dir` IS the user's own source
        // tree, so `target/` is the user's cargo incremental state — reclaiming
        // it would recompile the world on every unlinked local-dev edit. A
        // linked package is likewise denied the Rust reuse gate above and
        // full-rebuilds each iteration, so its scratch is kept too.
        if !source_is_mutable && active_link.is_none() {
            prune_build_scratch(pkg_dir);
        }

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
/// fingerprint it was built from. The `inputs_hash` drives `IfStale` for
/// non-Rust packages; abi/triple/profile are defense-in-depth atop the
/// runtime `PluginDeclaration` handshake. The build fingerprint is
/// recorded for diagnostics — the runtime `validate_plugin_declaration`
/// check is the authoritative gate; it is not part of the staleness
/// comparison here.
fn write_sidecar(
    dest: &Path,
    triple: &str,
    profile: build::CargoProfile,
    inputs_hash: &str,
) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "abi_version": streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
        "abi_layout_fingerprint":
            format!("{:#018x}", streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT),
        "triple": triple,
        "profile": profile.label(),
        "inputs_hash": inputs_hash,
    }))?;
    // The marker must appear whole: write to a same-dir temp, then rename it into
    // place so a concurrent reader never observes a half-written sidecar.
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dest.join(format!("{SIDECAR_NAME}.tmp-{}-{seq}", std::process::id()));
    std::fs::write(&temp, body)?;
    std::fs::rename(&temp, dest.join(SIDECAR_NAME))?;
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

/// Replace `final_path` with `temp` atomically (best-effort). A prior slot is
/// first renamed AWAY to a `.old-*` sibling, then `temp` is renamed into place,
/// then the displaced slot is removed. This NARROWS the remove-then-rename
/// absent window to two back-to-back renames (rename `final`→`.old`, then
/// `temp`→`final`): a concurrent reader observing between the two still sees
/// nothing, but the gap is a rename apart rather than spanning a full
/// `remove_dir_all`. True closure needs `renameat2(RENAME_EXCHANGE)`; this is a
/// strict improvement short of that. Each rename moves a fully-staged dir, so
/// no torn reads.
fn atomic_swap(temp: &Path, final_path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| "create cache parent")?;
    }
    let displaced = if final_path.exists() {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = final_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "slot".to_string());
        let old = final_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".old-{name}-{}-{seq}", std::process::id()));
        // A stale `.old-*` from a prior interrupted swap is cleared first so
        // the rename-away never fails on a pre-existing target.
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(final_path, &old).with_context(|| "displace stale cache slot")?;
        Some(old)
    } else {
        None
    };
    match std::fs::rename(temp, final_path) {
        Ok(()) => {
            if let Some(old) = displaced {
                let _ = std::fs::remove_dir_all(&old);
            }
            Ok(())
        }
        Err(e) => {
            // Restore the displaced slot so a failed swap leaves the prior
            // (loadable) artifact in place rather than an empty slot.
            if let Some(old) = &displaced {
                let _ = std::fs::rename(old, final_path);
            }
            Err(e).with_context(|| "rename temp into cache slot")
        }
    }
}

/// The regenerated build-output units an in-place promote lands into a
/// co-located source slot. `lib/<triple>/` is host-triple-specific so a slot
/// carrying another triple's prebuilt cdylib keeps it. A package's SOURCE files
/// are never in this set — an in-place promote leaves them untouched.
fn promoted_build_output_units(triple: &str) -> [PathBuf; 3] {
    [
        Path::new("lib").join(triple),
        PathBuf::from(".venv"),
        PathBuf::from("_generated_"),
    ]
}

/// Whether the engine-injected staging destination IS the package's own source
/// dir (the #1506 co-located slot), selecting the in-place promote over the
/// whole-dir atomic swap. Both paths must canonicalize to the same real dir; a
/// not-yet-created detached destination fails to canonicalize and is therefore
/// never the source dir.
fn destination_is_source_dir(destination: &Path, pkg_dir: &Path) -> bool {
    streamlib_pack::is_same_existing_file(destination, pkg_dir)
}

/// Append the in-tree build-output ignore lines (host-triple cdylib dir, venv,
/// generated wire vocabulary, completion marker) to a mutable dev checkout's own
/// `.gitignore` so they never show as untracked. Idempotent: only absent lines
/// are appended, so repeated builds and additional host triples accumulate
/// without duplication.
fn ensure_build_outputs_gitignored(destination: &Path, triple: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::collections::HashSet;

    let required = [
        format!("/lib/{triple}/"),
        "/.venv/".to_string(),
        "/_generated_/".to_string(),
        format!("/{SIDECAR_NAME}"),
    ];
    let gitignore = destination.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let present: HashSet<&str> = existing.lines().map(str::trim).collect();

    let missing: Vec<&str> = required
        .iter()
        .map(String::as_str)
        .filter(|line| !present.contains(*line))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for line in missing {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(&gitignore, out).with_context(|| format!("writing {}", gitignore.display()))
}

/// Promote ONLY the regenerated build-output units from the staging temp dir
/// into a destination that IS the package's own source dir, leaving every
/// source file untouched — except the beside-source `.gitignore`, which is the
/// one intentional source-file write: for a mutable dev checkout
/// (`source_is_mutable`) it is created/updated to keep the promoted outputs out
/// of git. An immutable managed extract (a git-rev-pinned clone) skips even that.
///
/// The publish is atomic at the completion-marker granularity. Each output unit
/// lands via [`atomic_swap`] (rename the prior unit AWAY to a `.old-*` sibling,
/// then rename the staged unit in), so the slot never has an absent window for
/// that unit — a concurrent reader sees either the prior or the freshly staged
/// unit, never a missing one. The `.streamlib-build.json` completion marker is
/// cleared BEFORE any unit moves and rewritten via a same-dir temp+rename as the
/// LAST step, so it appears in a single atomic flip: a reader that gates on the
/// marker observes either the prior complete state (marker absent/old) or the
/// new complete state (marker present), and a crash mid-promote leaves the
/// marker absent so no partially-published slot is ever marked complete. The
/// temp dir is a same-filesystem sibling of the destination, so each unit moves
/// by rename.
fn promote_build_outputs_in_place(
    stage_temp_dir: &Path,
    destination: &Path,
    triple: &str,
    profile: build::CargoProfile,
    inputs_hash: &str,
    source_is_mutable: bool,
) -> anyhow::Result<()> {
    use anyhow::Context;

    // Only a mutable dev checkout is the user's own git tree; a disposable
    // rev-pinned managed clone the user never sees carries its own ignore rules,
    // so writing into it is pointless churn. Best-effort: a `.gitignore` is
    // hygiene, never a correctness gate, so a write failure is logged.
    if source_is_mutable {
        if let Err(e) = ensure_build_outputs_gitignored(destination, triple) {
            tracing::warn!(
                dir = %destination.display(),
                error = %e,
                "could not gitignore in-tree build outputs; build outputs may show as untracked"
            );
        }
    }

    // Invalidate the completion marker before touching any output unit: until
    // the final marker flip, the slot must read as needing a rebuild.
    let _ = std::fs::remove_file(destination.join(SIDECAR_NAME));

    for unit in promoted_build_output_units(triple) {
        let from = stage_temp_dir.join(&unit);
        if !from.exists() {
            continue;
        }
        let to = destination.join(&unit);
        atomic_swap(&from, &to)
            .with_context(|| format!("promote {} → {}", from.display(), to.display()))?;
    }

    // Completion marker LAST — the single atomic flip that publishes the set.
    write_sidecar(destination, triple, profile, inputs_hash)
        .with_context(|| "write build sidecar")?;
    Ok(())
}

/// Whether the slot carries a host-triple cdylib under `lib/<triple>/` (any
/// `*.so` / `*.dylib` / `*.dll`). The presence gate for Rust build-once-reuse:
/// a sidecar match is not enough — the loadable artifact must actually be
/// staged, or a reuse would return a slot the loader can't dlopen.
fn slot_has_host_cdylib(slot: &Path, triple: &str) -> bool {
    let lib_dir = slot.join("lib").join(triple);
    let Ok(entries) = std::fs::read_dir(&lib_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "so" | "dylib" | "dll"))
    })
}

/// Reclaim a package dir's regenerable `cargo` build scratch (`target/`),
/// keeping the loadable artifact (`lib/<triple>/*.so` + `.venv/` +
/// `_generated_/` + manifest). Best-effort — a removal failure is logged and
/// ignored, since scratch reclamation is never a correctness gate.
fn prune_build_scratch(pkg_dir: &Path) {
    let target = pkg_dir.join("target");
    if target.is_dir()
        && let Err(e) = std::fs::remove_dir_all(&target)
    {
        tracing::debug!(
            dir = %target.display(),
            error = %e,
            "prune_build_scratch: failed to reclaim target/"
        );
    }
}

/// An active `streamlib link` resolved for a staged package build: the
/// checkout-derived toolchain overrides that redirect the build at the linked
/// checkout instead of the package source.
#[derive(Debug)]
struct ActiveBuildLink {
    /// The canonicalized linked checkout root. Threaded to the staged package's
    /// `cargo build` via [`streamlib_idents::LINK_CHECKOUT_ENV`] so the
    /// package's `build.rs` schema-dep codegen resolves a dep present in
    /// `<checkout>/packages/<name>` from the checkout — the schema half of the
    /// link dev loop, mirroring the cargo `[patch]` (crate half) below.
    checkout: PathBuf,
    /// The consumer's `streamlib link`-emitted `.cargo/config.toml`, when
    /// present — carries the `[patch."<index>"]` block redirecting every
    /// `streamlib*` crate at the checkout. `None` when the consumer has no
    /// cargo config (a Python/Deno-only consumer); the Rust build then falls
    /// back to by-version resolution.
    consumer_cargo_config: Option<PathBuf>,
    /// The linked checkout's Python SDK path (uv editable source target).
    python_sdk_path: PathBuf,
    /// The linked checkout's Deno SDK entrypoint (`.../streamlib-deno/mod.ts`).
    /// The session port-extraction tail resolves the Deno `extract_processors.ts`
    /// as its sibling under a link.
    deno_sdk_entrypoint_path: PathBuf,
}

/// Discover the active `streamlib link` for the current build from the process
/// working directory (the consumer's run dir, where `streamlib link` wrote
/// `.streamlib/link.json`). `Ok(None)` when no link is active; a corrupt marker
/// is a loud error — silently ignoring it would produce a mixed build (some
/// crates from the checkout, some by version from the package source), the exact failure mode
/// link mode exists to prevent.
fn discover_active_build_link(pkg_label: &str) -> Result<Option<ActiveBuildLink>, BuildError> {
    let cwd = std::env::current_dir().map_err(|e| {
        other(
            pkg_label,
            format!("resolving current working directory: {e}"),
        )
    })?;
    discover_active_build_link_from(&cwd, pkg_label)
}

/// [`discover_active_build_link`] anchored at an explicit start dir (test seam).
fn discover_active_build_link_from(
    start: &Path,
    pkg_label: &str,
) -> Result<Option<ActiveBuildLink>, BuildError> {
    let link = streamlib_idents::link_marker::find_and_load_active_link(start)
        .map_err(|e| build_failed(pkg_label, format!("reading streamlib link state: {e}")))?;
    let Some((marker, manifest)) = link else {
        return Ok(None);
    };
    // marker = <consumer_root>/.streamlib/link.json → the consumer's cargo
    // config sits at <consumer_root>/.cargo/config.toml (written by the link).
    let consumer_cargo_config = marker
        .parent()
        .and_then(|state_dir| state_dir.parent())
        .map(|consumer_root| consumer_root.join(".cargo").join("config.toml"))
        .filter(|cfg| cfg.is_file());
    Ok(Some(ActiveBuildLink {
        checkout: manifest.checkout,
        consumer_cargo_config,
        python_sdk_path: manifest.python_sdk_path,
        deno_sdk_entrypoint_path: manifest.deno_sdk_entrypoint_path,
    }))
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

    // Mentally-revert check: reverting `rust_reuse_permitted` to a constant
    // `true` (the pre-#1506 behavior where a Rust package short-circuited on
    // the fingerprint alone) flips the three `false` cases below — a
    // mutable-source Rust package, a linked Rust package, and a Rust package
    // whose cdylib isn't staged yet, must NOT reuse. Dropping only the
    // `!source_is_mutable` clause flips the mutable-source case (the #1506
    // stale-cdylib trap: a `Strategy::Path` / linked package whose out-of-dir
    // crate deps the package-local fingerprint can't see). Reverting to a
    // constant `false` flips the non-Rust and fully-staged-immutable cases.
    #[test]
    fn rust_reuse_permitted_requires_immutable_source_no_link_and_a_staged_cdylib() {
        // (has_rust, source_is_mutable, link_active, cdylib_present)
        // Non-Rust: always reuses (fingerprint is a complete oracle), even for
        // a mutable source under a link.
        assert!(rust_reuse_permitted(false, false, false, false));
        assert!(rust_reuse_permitted(false, true, true, true));
        // Rust, immutable source, no link, cdylib staged: the zero-cargo second
        // load for an installed / by-version / `.slpkg` / git-rev package.
        assert!(rust_reuse_permitted(true, false, false, true));
        // Rust, MUTABLE user checkout, no link, cdylib staged: must rebuild —
        // its `path:` / workspace deps resolve outside the package dir, so the
        // package-local fingerprint is not a complete oracle and cargo (whose
        // own fingerprint short-circuits cheaply when clean) must re-run. This
        // is the #1506 stale-cdylib trap.
        assert!(!rust_reuse_permitted(true, true, false, true));
        // Rust under an active link: must rebuild (deps resolve to a mutable
        // checkout), even with a cdylib present.
        assert!(!rust_reuse_permitted(true, false, true, true));
        // Rust, immutable, no link, but no cdylib staged yet: must build.
        assert!(!rust_reuse_permitted(true, false, false, false));
    }

    // Mentally-revert check: dropping the `link_active` clause flips the
    // active-link case below to `false`, silently reintroducing the #1550 bug
    // (assemble honors a stale in-tree `.so` under an active `streamlib link`).
    #[test]
    fn ignore_in_tree_prebuilt_cdylib_holds_for_mutable_linked_or_always_build() {
        // (source_is_mutable, link_active, policy)
        // Mutable checkout: ignore the promoted `.so`, let cargo re-decide.
        assert!(ignore_in_tree_prebuilt_cdylib(
            true,
            false,
            BuildPolicy::IfStale
        ));
        // Active link: ignore (deps resolve to a mutable checkout) — the #1550
        // case.
        assert!(ignore_in_tree_prebuilt_cdylib(
            false,
            true,
            BuildPolicy::IfStale
        ));
        // AlwaysBuild: ignore unconditionally.
        assert!(ignore_in_tree_prebuilt_cdylib(
            false,
            false,
            BuildPolicy::AlwaysBuild
        ));
        // Immutable extract under IfStale: keep the prebuilt preference so a
        // venv-only re-provision stays compiler-free.
        assert!(!ignore_in_tree_prebuilt_cdylib(
            false,
            false,
            BuildPolicy::IfStale
        ));
    }

    #[test]
    fn slot_has_host_cdylib_detects_only_dylibs_for_the_host_triple() {
        let slot = tempfile::tempdir().unwrap();
        let triple = "x86_64-unknown-linux-gnu";
        let other = "aarch64-apple-darwin";
        // Empty slot: no cdylib.
        assert!(!slot_has_host_cdylib(slot.path(), triple));
        // A cdylib for a DIFFERENT triple does not count.
        let other_lib = slot.path().join("lib").join(other);
        std::fs::create_dir_all(&other_lib).unwrap();
        std::fs::write(other_lib.join("libpkg.so"), b"").unwrap();
        assert!(!slot_has_host_cdylib(slot.path(), triple));
        // A non-dylib file under the host triple dir does not count.
        let host_lib = slot.path().join("lib").join(triple);
        std::fs::create_dir_all(&host_lib).unwrap();
        std::fs::write(host_lib.join("README.txt"), b"").unwrap();
        assert!(!slot_has_host_cdylib(slot.path(), triple));
        // The host-triple cdylib present ⇒ reusable.
        std::fs::write(host_lib.join("libpkg.so"), b"").unwrap();
        assert!(slot_has_host_cdylib(slot.path(), triple));
    }

    #[test]
    fn atomic_swap_replaces_a_prior_slot_narrowing_the_absent_window() {
        // A prior slot is displaced to `.old-*` then the new one renamed in;
        // the swept-in content must win and the parent must resolve to the
        // slot on the success resting state. The displace-then-rename NARROWS
        // (does not close) the absent window vs a bare `remove_dir_all` +
        // rename — this test checks the success resting state, which both
        // shapes pass; the sibling `..._when_the_rename_in_fails` pins the
        // load-bearing failure-path difference.
        let root = tempfile::tempdir().unwrap();
        let slot = root.path().join("pkg");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("marker"), b"old").unwrap();

        let temp = root.path().join(".tmp-pkg");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("marker"), b"new").unwrap();

        atomic_swap(&temp, &slot).unwrap();

        assert!(slot.is_dir(), "slot must be present after the swap");
        assert_eq!(std::fs::read_to_string(slot.join("marker")).unwrap(), "new");
        assert!(!temp.exists(), "temp must be consumed by the rename");
        // No `.old-*` residue left behind on the success path.
        let residue: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".old-"))
            .collect();
        assert!(residue.is_empty(), "displaced slot must be reclaimed");
    }

    #[test]
    fn atomic_swap_preserves_the_prior_slot_when_the_rename_in_fails() {
        // The load-bearing invariant the displace-to-`.old-*` shape buys over
        // the old remove-then-rename: if the rename of `temp` into the slot
        // FAILS (here: the temp source never exists), the prior slot's content
        // must still be present afterward — a failed swap never destroys the
        // last loadable artifact.
        //
        // Mentally-revert to `remove_dir_all(final)` + `rename(temp, final)`:
        // the remove wipes the prior slot BEFORE the rename fails, and there is
        // no restore, so the slot is gone — this assertion fails. The current
        // displace-then-restore shape keeps "old" in place. The sibling
        // `..._narrowing_the_absent_window` test only checks the SUCCESS
        // resting state and passes under both shapes; this one pins the
        // failure path.
        let root = tempfile::tempdir().unwrap();
        let slot = root.path().join("pkg");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("marker"), b"old").unwrap();

        // A temp path that does not exist ⇒ `rename(temp, final)` fails.
        let missing_temp = root.path().join(".tmp-never-created");
        let result = atomic_swap(&missing_temp, &slot);

        assert!(result.is_err(), "swap with a missing temp source must fail");
        assert!(
            slot.is_dir(),
            "the prior slot must survive a failed rename-in — remove-then-rename \
             would have wiped it before the (failing) rename"
        );
        assert_eq!(
            std::fs::read_to_string(slot.join("marker")).unwrap(),
            "old",
            "the prior slot's content must be intact after the failed swap"
        );
        // The displaced backup must be restored, not orphaned as `.old-*`.
        let residue: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".old-"))
            .collect();
        assert!(
            residue.is_empty(),
            "the displaced slot must be restored into place, not left as .old-* residue"
        );
    }

    #[test]
    fn prune_build_scratch_drops_target_keeps_artifact() {
        let pkg = tempfile::tempdir().unwrap();
        // Artifact that must survive.
        let lib = pkg.path().join("lib").join("x86_64-unknown-linux-gnu");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libpkg.so"), b"cdylib").unwrap();
        std::fs::write(pkg.path().join("streamlib.yaml"), b"package:\n").unwrap();
        // Regenerable scratch that must be reclaimed.
        let target = pkg.path().join("target").join("debug");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("junk.o"), b"scratch").unwrap();

        prune_build_scratch(pkg.path());

        assert!(!pkg.path().join("target").exists(), "target/ must be reclaimed");
        assert!(lib.join("libpkg.so").is_file(), "cdylib artifact must survive");
        assert!(
            pkg.path().join("streamlib.yaml").is_file(),
            "manifest must survive"
        );
    }

    #[test]
    fn destination_is_source_dir_true_when_equal_false_when_detached() {
        let src = tempfile::tempdir().unwrap();
        // Destination == source dir ⇒ in-place.
        assert!(destination_is_source_dir(src.path(), src.path()));
        // A `.`/`..`-decorated spelling of the same dir still canonicalizes equal.
        let dotted = src.path().join(".");
        assert!(destination_is_source_dir(&dotted, src.path()));
        // A distinct existing dir ⇒ detached.
        let other = tempfile::tempdir().unwrap();
        assert!(!destination_is_source_dir(other.path(), src.path()));
        // A not-yet-created destination (the first-build detached case) ⇒
        // never the source dir (canonicalize fails on the missing path).
        let missing = src.path().parent().unwrap().join("does-not-exist-slot");
        assert!(!destination_is_source_dir(&missing, src.path()));
    }

    /// A synthetic staging temp dir carrying all three build-output units.
    fn write_stage_temp_outputs(temp: &Path, triple: &str) {
        let lib = temp.join("lib").join(triple);
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libpkg.so"), b"fresh-cdylib").unwrap();
        let venv_bin = temp.join(".venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::fs::write(venv_bin.join("python"), b"fresh-venv").unwrap();
        let generated = temp.join("_generated_");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("wire.ts"), b"fresh-generated").unwrap();
    }

    #[test]
    fn promote_build_outputs_in_place_leaves_source_and_writes_sidecar_last() {
        // The in-place promote lands ONLY the build-output units into a
        // destination that is the source dir, leaving source files byte-for-byte
        // untouched, and writes the sidecar completion marker. Mentally-revert
        // the unit allowlist to "rename the whole temp dir over the source" and
        // the source-file assertions below fail (source would be clobbered).
        let triple = build::host_target_triple();
        let root = tempfile::tempdir().unwrap();

        // Destination IS the source dir: source files + a STALE cdylib for the
        // host triple + a valid prior sidecar from an earlier build.
        let dest = root.path().join("pkg");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("streamlib.yaml"), b"package: source-manifest").unwrap();
        std::fs::create_dir_all(dest.join("src")).unwrap();
        std::fs::write(dest.join("src").join("proc.py"), b"source-code").unwrap();
        let stale_lib = dest.join("lib").join(triple);
        std::fs::create_dir_all(&stale_lib).unwrap();
        std::fs::write(stale_lib.join("stale.so"), b"stale-cdylib").unwrap();
        write_sidecar(
            &dest,
            triple,
            build::CargoProfile::Dev,
            "prior-fingerprint",
        )
        .unwrap();

        // Sibling staging temp dir with freshly built outputs.
        let temp = root.path().join(".tmp-pkg");
        std::fs::create_dir_all(&temp).unwrap();
        write_stage_temp_outputs(&temp, triple);

        promote_build_outputs_in_place(
            &temp,
            &dest,
            triple,
            build::CargoProfile::Dev,
            "new-fingerprint",
            true,
        )
        .unwrap();

        // Source files untouched.
        assert_eq!(
            std::fs::read(dest.join("streamlib.yaml")).unwrap(),
            b"package: source-manifest",
            "source manifest must be left untouched by an in-place promote"
        );
        assert_eq!(
            std::fs::read(dest.join("src").join("proc.py")).unwrap(),
            b"source-code",
            "source code must be left untouched by an in-place promote"
        );

        // Build-output units promoted; the stale cdylib for the same triple is
        // replaced (the whole `lib/<triple>` unit is swapped).
        assert_eq!(
            std::fs::read(dest.join("lib").join(triple).join("libpkg.so")).unwrap(),
            b"fresh-cdylib"
        );
        assert!(
            !dest.join("lib").join(triple).join("stale.so").exists(),
            "the stale prior cdylib must be replaced when the lib/<triple> unit is promoted"
        );
        assert_eq!(
            std::fs::read(dest.join(".venv").join("bin").join("python")).unwrap(),
            b"fresh-venv"
        );
        assert_eq!(
            std::fs::read(dest.join("_generated_").join("wire.ts")).unwrap(),
            b"fresh-generated"
        );

        // Sidecar rewritten as the completion marker with the new fingerprint.
        let side = read_sidecar(&dest).expect("completion sidecar must be present after promote");
        assert_eq!(side.inputs_hash, "new-fingerprint");
        assert_eq!(side.triple, triple);
    }

    #[test]
    fn promote_build_outputs_in_place_only_promotes_present_units() {
        // A schemas-only (no lib/.venv/_generated_) in-place promote writes only
        // the sidecar and touches nothing else — no output unit is fabricated.
        let triple = build::host_target_triple();
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("pkg");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("streamlib.yaml"), b"package: schemas-only").unwrap();
        let temp = root.path().join(".tmp-pkg");
        std::fs::create_dir_all(&temp).unwrap();

        promote_build_outputs_in_place(&temp, &dest, triple, build::CargoProfile::Dev, "fp", true)
            .unwrap();

        assert!(!dest.join("lib").exists(), "no lib/ unit to promote");
        assert!(!dest.join(".venv").exists(), "no venv unit to promote");
        assert!(!dest.join("_generated_").exists(), "no generated unit to promote");
        assert!(read_sidecar(&dest).is_some(), "sidecar completion marker still written");
    }

    #[test]
    fn promote_build_outputs_in_place_failure_clears_sidecar_and_preserves_source() {
        // A promote that ERRORS before any unit publishes (here `lib/<triple>`
        // can't be staged because a FILE sits where its `lib/` parent dir must
        // be, so the swap's parent-create fails) must leave the slot with NO
        // completion sidecar — a prior valid sidecar is cleared BEFORE any unit
        // moves — and must never touch the source files. Clear-first + write-last
        // ordering is what keeps an interrupted promote from marking a torn slot
        // complete. Mentally-revert "clear the sidecar first" and the prior
        // sidecar survives the failure, marking a torn slot complete.
        let triple = build::host_target_triple();
        let root = tempfile::tempdir().unwrap();

        let dest = root.path().join("pkg");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("streamlib.yaml"), b"package: source-manifest").unwrap();
        // A valid prior completion marker that MUST be gone after a failed promote.
        write_sidecar(&dest, triple, build::CargoProfile::Dev, "prior").unwrap();
        let temp = root.path().join(".tmp-pkg");
        std::fs::create_dir_all(&temp).unwrap();
        write_stage_temp_outputs(&temp, triple);
        // Block the first unit (`lib/<triple>`): a FILE sits at `lib`, so
        // creating the `lib/` parent for the swap fails with ENOTDIR.
        std::fs::write(dest.join("lib"), b"a-file-not-a-dir").unwrap();

        let err = promote_build_outputs_in_place(
            &temp,
            &dest,
            triple,
            build::CargoProfile::Dev,
            "new",
            true,
        )
        .expect_err("promote must fail when a unit cannot be swapped into place");
        let _ = err;

        // No completion marker after the failure — the slot reads as needing a
        // rebuild.
        assert!(
            read_sidecar(&dest).is_none(),
            "the prior sidecar must be cleared before promotion and NOT rewritten on failure"
        );
        // Source untouched.
        assert_eq!(
            std::fs::read(dest.join("streamlib.yaml")).unwrap(),
            b"package: source-manifest",
            "source files must survive a failed in-place promote"
        );
    }

    #[cfg(unix)]
    #[test]
    fn promote_build_outputs_in_place_interrupted_between_units_leaves_no_published_marker() {
        use std::os::unix::fs::PermissionsExt;
        // Crash-between-units regression: a promote that fails AFTER an earlier
        // unit has published but BEFORE the completion marker must leave the slot
        // with NO sidecar, so a reader gating on the marker treats the torn slot
        // as needing a rebuild rather than loading a half-promoted set. The first
        // unit (`lib/<triple>`, parent = the writable `lib/`) lands; the second
        // (`.venv`, parent = the destination dir) then fails because the
        // destination is read-only. Mentally-revert "write the marker LAST" and
        // the partial slot below carries a completion marker over a torn set.
        let triple = build::host_target_triple();
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("pkg");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("streamlib.yaml"), b"package: source-manifest").unwrap();
        // Pre-create a writable `lib/` so the FIRST unit publishes into it even
        // after the destination itself is made read-only.
        std::fs::create_dir_all(dest.join("lib")).unwrap();

        let temp = root.path().join(".tmp-pkg");
        std::fs::create_dir_all(&temp).unwrap();
        write_stage_temp_outputs(&temp, triple);

        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores the read-only bit; skip rather than assert a non-failure.
        if std::fs::write(dest.join(".root-probe"), b"x").is_ok() {
            let _ = std::fs::remove_file(dest.join(".root-probe"));
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
            return;
        }

        let err = promote_build_outputs_in_place(
            &temp,
            &dest,
            triple,
            build::CargoProfile::Dev,
            "new",
            true,
        )
        .expect_err("promote must fail when a later unit cannot be published");
        let _ = err;

        // Restore write access for the assertions + tempdir cleanup.
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The first unit HAS landed — proving the failure is genuinely mid-set...
        assert!(
            dest.join("lib").join(triple).join("libpkg.so").is_file(),
            "the first unit must have published before the mid-set failure"
        );
        // ...yet NO completion marker exists, so the torn slot is never published.
        assert!(
            read_sidecar(&dest).is_none(),
            "a promote interrupted between units must leave NO completion marker"
        );
        // Source untouched.
        assert_eq!(
            std::fs::read(dest.join("streamlib.yaml")).unwrap(),
            b"package: source-manifest",
            "source files must survive an interrupted in-place promote"
        );
    }

    /// A fresh in-tree destination gets a `.gitignore` covering every promoted
    /// build-output unit plus the completion marker, so a dev source's build
    /// outputs never show as untracked git noise. Mentally drop any required
    /// line and the corresponding `contains` assertion fails.
    #[test]
    fn ensure_build_outputs_gitignored_writes_all_units() {
        let dest = tempfile::tempdir().unwrap();
        let triple = "x86_64-unknown-linux-gnu";
        ensure_build_outputs_gitignored(dest.path(), triple).unwrap();
        let body = std::fs::read_to_string(dest.path().join(".gitignore")).unwrap();
        assert!(body.contains(&format!("/lib/{triple}/")), "{body}");
        assert!(body.contains("/.venv/"), "{body}");
        assert!(body.contains("/_generated_/"), "{body}");
        assert!(body.contains(&format!("/{SIDECAR_NAME}")), "{body}");
    }

    /// Idempotent + additive: a second call for the SAME triple adds nothing
    /// (no duplicate lines), a call for a DIFFERENT triple appends only its own
    /// `lib/<triple>/` line, and a pre-existing user entry is preserved.
    #[test]
    fn ensure_build_outputs_gitignored_is_idempotent_and_preserves_user_entries() {
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(dest.path().join(".gitignore"), "node_modules/\n").unwrap();
        let triple_a = "x86_64-unknown-linux-gnu";
        let triple_b = "aarch64-apple-darwin";

        ensure_build_outputs_gitignored(dest.path(), triple_a).unwrap();
        ensure_build_outputs_gitignored(dest.path(), triple_a).unwrap();
        let after_a = std::fs::read_to_string(dest.path().join(".gitignore")).unwrap();
        assert_eq!(
            after_a.matches(&format!("/lib/{triple_a}/")).count(),
            1,
            "a repeated same-triple call must not duplicate a line: {after_a}"
        );
        assert!(
            after_a.contains("node_modules/"),
            "a pre-existing user entry must be preserved: {after_a}"
        );

        ensure_build_outputs_gitignored(dest.path(), triple_b).unwrap();
        let after_b = std::fs::read_to_string(dest.path().join(".gitignore")).unwrap();
        assert!(after_b.contains(&format!("/lib/{triple_a}/")), "{after_b}");
        assert!(after_b.contains(&format!("/lib/{triple_b}/")), "{after_b}");
        assert_eq!(
            after_b.matches("/.venv/").count(),
            1,
            "the venv entry must not be re-appended for a second triple: {after_b}"
        );
    }

    /// The in-place promote writes the `.gitignore` alongside the promoted
    /// units — proving the source-tree keeps its build outputs out of git.
    #[test]
    fn promote_build_outputs_in_place_gitignores_the_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let triple = build::host_target_triple();
        let lib = temp.path().join("lib").join(triple);
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libpkg.so"), b"cdylib").unwrap();
        std::fs::write(dest.path().join("streamlib.yaml"), b"package: src").unwrap();

        promote_build_outputs_in_place(
            temp.path(),
            dest.path(),
            triple,
            build::CargoProfile::Dev,
            "fp",
            true,
        )
        .unwrap();

        let body = std::fs::read_to_string(dest.path().join(".gitignore")).unwrap();
        assert!(body.contains(&format!("/lib/{triple}/")), "{body}");
        assert!(body.contains(&format!("/{SIDECAR_NAME}")), "{body}");
        assert_eq!(
            std::fs::read(dest.path().join("streamlib.yaml")).unwrap(),
            b"package: src",
            "source must be untouched"
        );
    }

    /// An immutable managed extract (a disposable rev-pinned git clone,
    /// `source_is_mutable == false`) gets its build outputs promoted in-tree but
    /// NO `.gitignore` write — the clone is not the user's tree. Mentally-revert
    /// the `source_is_mutable` guard and a stray `.gitignore` appears here.
    #[test]
    fn promote_build_outputs_in_place_skips_gitignore_for_immutable_extract() {
        let temp = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let triple = build::host_target_triple();
        let lib = temp.path().join("lib").join(triple);
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libpkg.so"), b"cdylib").unwrap();
        std::fs::write(dest.path().join("streamlib.yaml"), b"package: src").unwrap();

        promote_build_outputs_in_place(
            temp.path(),
            dest.path(),
            triple,
            build::CargoProfile::Dev,
            "fp",
            false,
        )
        .unwrap();

        assert!(
            !dest.path().join(".gitignore").exists(),
            "an immutable managed extract must not get a beside-source .gitignore"
        );
        assert!(
            dest.path().join("lib").join(triple).join("libpkg.so").is_file(),
            "the build output is still promoted in-tree for an immutable extract"
        );
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

    /// A detached staging destination under the sandboxed `STREAMLIB_HOME` — the
    /// engine injects `staging_destination_slot_dir` on every request, and these
    /// tests exercise the DETACHED-destination promote path (source ≠ slot). Keyed
    /// so distinct keys yield distinct slots, letting a test pin write==read.
    fn detached_slot(key: &str) -> PathBuf {
        streamlib_engine::core::get_streamlib_data_dir()
            .join("test-slots")
            .join(key)
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
            "metadata:\n  type: TestSchema\n  expected_payload_bytes: 1024\n",
        )
        .unwrap();
    }

    fn request(pkg_dir: &Path, policy: BuildPolicy) -> BuildRequest {
        BuildRequest {
            package: pkg_ref("tatolab", "schemas-only"),
            source: BuildSource::PackageDir(pkg_dir.to_path_buf()),
            source_provenance: PackageSourceProvenance::MutableUserCheckout,
            policy,
            host_triple: build::host_target_triple().to_string(),
            staging_destination_slot_dir: detached_slot("schemas-only-0.1.0"),
        }
    }

    #[test]
    #[serial]
    fn schemas_only_stages_into_package_cache() {
        // A schemas-only package (no compiler involved) assembles into the
        // engine-injected staging destination — the same slot an extracted
        // .slpkg / GitHub install lands in — with its streamlib.yaml +
        // schemas/ present. rebuilt=false because no build tool ran.
        let _home = HomeGuard::new();
        let src = tempfile::tempdir().unwrap();
        schemas_only_pkg(src.path());

        let orch = PolyglotBuildOrchestrator::default();
        let staged = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .expect("schemas-only must materialize");

        let expected = detached_slot("schemas-only-0.1.0");
        assert_eq!(
            staged.staged_dir, expected,
            "must stage into the package cache"
        );
        assert!(staged.staged_dir.join("streamlib.yaml").is_file());
        assert!(staged.staged_dir.join("schemas/test_schema.yaml").is_file());
        assert!(
            !staged.rebuilt,
            "no compiler ran for a schemas-only package"
        );
        assert!(
            staged.staged_dir.join(SIDECAR_NAME).is_file(),
            "sidecar must be written for the IfStale skip-check"
        );
    }

    /// write==read handoff: the orchestrator stages into EXACTLY the
    /// `staging_destination_slot_dir` the engine injected on the request — it
    /// re-derives no slot of its own. The injected destination deliberately
    /// carries a name that a `{package.name}-{package.version}` self-derivation
    /// would NOT produce, so restoring the deleted self-derivation would land
    /// the staged dir at `schemas-only-0.1.0` and fail this assertion.
    #[test]
    #[serial]
    fn stages_into_injected_destination_not_a_self_derived_slot() {
        let _home = HomeGuard::new();
        let src = tempfile::tempdir().unwrap();
        schemas_only_pkg(src.path());

        let injected =
            detached_slot("schemas-only-injected-slot");
        let mut req = request(src.path(), BuildPolicy::IfStale);
        req.staging_destination_slot_dir = injected.clone();

        let orch = PolyglotBuildOrchestrator::default();
        let staged = orch
            .materialize(&req, &NoopSink)
            .expect("schemas-only must materialize");

        assert_eq!(
            staged.staged_dir, injected,
            "orchestrator must stage into the engine-injected destination, not a re-derived slot"
        );
        assert!(staged.staged_dir.join("streamlib.yaml").is_file());
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
            "metadata:\n  type: TestSchema\n  expected_payload_bytes: 2048\n",
        )
        .unwrap();
        let third = orch
            .materialize(&request(src.path(), BuildPolicy::IfStale), &NoopSink)
            .unwrap();
        let restaged =
            std::fs::read_to_string(third.staged_dir.join("schemas/test_schema.yaml")).unwrap();
        assert!(
            restaged.contains("2048"),
            "edited schema must be re-staged into the cache, got: {restaged}"
        );
    }

    #[test]
    #[serial]
    fn in_place_materialize_reuses_despite_the_engine_written_gitignore() {
        // A dev source builds IN-PLACE (staging destination IS the source dir),
        // so the promote writes a beside-source `.gitignore`. That engine write
        // must NOT perturb the reuse fingerprint: a second unchanged in-place
        // materialize must reuse. Mentally-revert the `.gitignore` exclusion from
        // the source fingerprint and the sidecar hash (recorded before the
        // `.gitignore` existed) diverges from a recompute that now sees it, so
        // the skip is missed and the source needlessly re-materializes.
        let _home = HomeGuard::new();
        let src = tempfile::tempdir().unwrap();
        schemas_only_pkg(src.path());

        // Destination IS the source dir → the in-place promote path.
        let mut req = request(src.path(), BuildPolicy::IfStale);
        req.staging_destination_slot_dir = src.path().to_path_buf();

        let orch = PolyglotBuildOrchestrator::default();
        let first = orch.materialize(&req, &NoopSink).unwrap();
        assert_eq!(first.staged_dir, src.path());
        assert!(
            src.path().join(".gitignore").is_file(),
            "in-place promote of a mutable source must write the beside-source .gitignore"
        );

        // The sidecar hash was recorded BEFORE the `.gitignore` write; a recompute
        // now sees the `.gitignore` on disk. They must still agree — proving the
        // engine-written ignore file is fingerprint-neutral.
        let side = read_sidecar(src.path()).expect("first in-place build writes a sidecar");
        let recomputed = compute_inputs_hash(src.path()).unwrap();
        assert_eq!(
            side.inputs_hash, recomputed,
            "the engine-written .gitignore must not perturb the reuse fingerprint"
        );

        let second = orch.materialize(&req, &NoopSink).unwrap();
        assert_eq!(second.staged_dir, src.path());
        assert!(
            !second.rebuilt,
            "an unchanged second in-place materialize must reuse, not rebuild"
        );
    }

    /// Recursively collect `(relative_path, bytes)` for every file under
    /// `dir`, excluding `.pyc` (compileall artifacts vary by interpreter).
    /// Used to compare two generated-code trees for an exact file-set +
    /// content match.
    fn collect_tree(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
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
            source_provenance: PackageSourceProvenance::MutableUserCheckout,
            policy,
            host_triple: build::host_target_triple().to_string(),
            staging_destination_slot_dir: detached_slot("py-source-0.1.0"),
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
        assert!(
            populated,
            "_generated_ must be populated after first materialize"
        );

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
            third
                .staged_dir
                .join(".venv")
                .join("bin")
                .join("python")
                .exists(),
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
            link_checkout: None,
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

    /// Write a `streamlib link` marker under `consumer_root/.streamlib/link.json`
    /// pointing at `checkout`, and a consumer cargo config next to it.
    fn write_link_marker(consumer_root: &Path, checkout: &Path, with_cargo_config: bool) {
        let state = consumer_root.join(".streamlib");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("link.json"),
            format!(
                r#"{{"checkout":"{c}","python_sdk_path":"{c}/sdk/streamlib-python","deno_sdk_entrypoint_path":"{c}/sdk/streamlib-deno/mod.ts","linked_at":"t","linked_crate_count":1,"state":"active","files":[]}}"#,
                c = checkout.display()
            ),
        )
        .unwrap();
        if with_cargo_config {
            let cargo = consumer_root.join(".cargo");
            std::fs::create_dir_all(&cargo).unwrap();
            std::fs::write(cargo.join("config.toml"), "[patch.\"x\"]\n").unwrap();
        }
    }

    #[test]
    fn discover_build_link_none_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            discover_active_build_link_from(dir.path(), "tatolab/x")
                .unwrap()
                .is_none()
        );
    }

    /// The orchestrator's in-process `generate()` codegen (the venv +
    /// deno_codegen paths) is LINK-AUTHORITATIVE: it resolves the checkout from
    /// the caller-supplied `link_checkout`, NEVER by walking up from
    /// `project_dir` to a marker. This is the polyglot mirror of the Rust
    /// suppression boundary — a relocated venv/deno codegen runs in the
    /// orchestrator's own process (so the child-env sentinel that guards the
    /// Rust `build.rs` can't reach it), and `project_dir` sits under the staged
    /// cache whose `.streamlib` dir name collides with the link-state dir, so an
    /// unconditional marker walk-up would redirect a distribution build to a dev
    /// checkout.
    ///
    /// Setup: a stray `.streamlib/link.json` marker sits UP-TREE of
    /// `project_dir`, pointing at a checkout that provides `@tatolab/core`; no
    /// package source is configured. Case A (`link_checkout = None`, the distribution /
    /// non-linked build) must NOT redirect — with no package source the bare range is
    /// unresolvable, so `generate` errors. Mentally-revert the explicit-link
    /// threading (fall back to `from_env_or_marker(&project_dir)` inside
    /// `generate`) and Case A resolves `@tatolab/core` from the marker's checkout
    /// and SUCCEEDS — failing the `is_err()` assertion. Case B
    /// (`link_checkout = Some(checkout)`) must keep resolving from the checkout so
    /// the linked polyglot path is not regressed.
    #[test]
    #[serial]
    fn generate_is_link_authoritative_not_marker_discovered() {
        // A checkout providing @tatolab/core. Schema-less: it resolves cleanly,
        // and the schema-less project below yields an empty codegen task set, so
        // the whole test is independent of the external jtd-codegen binary.
        let checkout = tempfile::tempdir().unwrap();
        let core = checkout.path().join("packages").join("core");
        std::fs::create_dir_all(&core).unwrap();
        std::fs::write(
            core.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        )
        .unwrap();

        // Consumer root carrying a stray link marker UP-TREE of the project dir.
        let consumer = tempfile::tempdir().unwrap();
        write_link_marker(consumer.path(), checkout.path(), false);
        let project_dir = consumer.path().join("app").join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("streamlib.yaml"),
            "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n",
        )
        .unwrap();

        // Precondition: the marker IS discoverable up-tree, so a redirect (if the
        // code walked up) would resolve @tatolab/core from the checkout — this is
        // what makes Case A's Err prove suppression, not mere absence.
        assert!(
            streamlib_idents::link_marker::find_active_link_marker(&project_dir).is_some(),
            "test setup: a stray marker must be discoverable up-tree of project_dir"
        );

        let out = tempfile::tempdir().unwrap();

        // Control the env: no package source (so a non-link resolve of the bare range
        // fails loud) and no ambient link env. SAFETY: `#[serial]` serializes the
        // env-mutating tests.
        let prev_package_source = std::env::var_os("STREAMLIB_PACKAGE_SOURCE");
        let prev_link = std::env::var_os(streamlib_idents::LINK_CHECKOUT_ENV);
        unsafe {
            std::env::remove_var("STREAMLIB_PACKAGE_SOURCE");
            std::env::remove_var(streamlib_idents::LINK_CHECKOUT_ENV);
        }

        // Case A — authoritative NO link (distribution / non-linked build).
        let no_link = streamlib_jtd_codegen::generate(streamlib_jtd_codegen::GenerateOptions {
            runtime: streamlib_jtd_codegen::RuntimeTarget::Rust,
            output: out.path().join("no_link"),
            project_dir: Some(project_dir.clone()),
            schema_file: None,
            schema_dir: None,
            workspace_root: project_dir.clone(),
            write_lockfile: false,
            link_checkout: None,
        });

        // Case B — authoritative link ACTIVE (the caller supplies the checkout).
        let with_link = streamlib_jtd_codegen::generate(streamlib_jtd_codegen::GenerateOptions {
            runtime: streamlib_jtd_codegen::RuntimeTarget::Rust,
            output: out.path().join("with_link"),
            project_dir: Some(project_dir.clone()),
            schema_file: None,
            schema_dir: None,
            workspace_root: project_dir,
            write_lockfile: false,
            link_checkout: Some(checkout.path().to_path_buf()),
        });

        unsafe {
            match prev_package_source {
                Some(v) => std::env::set_var("STREAMLIB_PACKAGE_SOURCE", v),
                None => std::env::remove_var("STREAMLIB_PACKAGE_SOURCE"),
            }
            match prev_link {
                Some(v) => std::env::set_var(streamlib_idents::LINK_CHECKOUT_ENV, v),
                None => std::env::remove_var(streamlib_idents::LINK_CHECKOUT_ENV),
            }
        }

        assert!(
            no_link.is_err(),
            "link_checkout=None must NOT resolve @tatolab/core from a stray up-tree \
             marker (no package source ⇒ unresolvable). Reverting to marker discovery makes \
             this Ok and reintroduces the distribution→dev-checkout redirect."
        );
        assert!(
            with_link.is_ok(),
            "link_checkout=Some(checkout) must resolve @tatolab/core from the checkout — \
             the linked polyglot path must not regress: {with_link:?}"
        );
    }

    #[test]
    fn discover_build_link_resolves_cargo_config_and_sdk_path() {
        // An active link with a consumer cargo config: discovery returns the
        // config path (the [patch] injected into the cdylib build) and the
        // checkout's python_sdk_path (the venv override). Reverting the
        // parent().parent() derivation would miss the consumer cargo config.
        let consumer = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        write_link_marker(consumer.path(), checkout.path(), true);

        let link = discover_active_build_link_from(consumer.path(), "tatolab/x")
            .unwrap()
            .expect("active link must be discovered");
        assert_eq!(
            link.checkout.as_path(),
            checkout.path(),
            "must carry the checkout root — threaded to build.rs schema-dep codegen \
             via STREAMLIB_LINK_CHECKOUT so schema deps resolve from the checkout"
        );
        assert_eq!(
            link.consumer_cargo_config,
            Some(consumer.path().join(".cargo").join("config.toml")),
            "must locate the consumer's link-emitted cargo config"
        );
        assert_eq!(
            link.python_sdk_path,
            checkout.path().join("sdk").join("streamlib-python"),
            "must carry the checkout's python SDK path from the marker"
        );
    }

    #[test]
    fn discover_build_link_without_cargo_config_still_resolves_sdk() {
        // A Python/Deno-only consumer has no cargo config; discovery still
        // returns the link (sdk path present, cargo config None) so the venv
        // override fires even when the cargo override can't.
        let consumer = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        write_link_marker(consumer.path(), checkout.path(), false);

        let link = discover_active_build_link_from(consumer.path(), "tatolab/x")
            .unwrap()
            .expect("active link must be discovered");
        assert!(link.consumer_cargo_config.is_none());
        assert_eq!(
            link.python_sdk_path,
            checkout.path().join("sdk").join("streamlib-python")
        );
    }

    #[test]
    fn discover_build_link_corrupt_marker_is_a_loud_error() {
        let consumer = tempfile::tempdir().unwrap();
        let state = consumer.path().join(".streamlib");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("link.json"), "{ not json").unwrap();
        let err = discover_active_build_link_from(consumer.path(), "tatolab/x")
            .expect_err("corrupt marker must fail loudly");
        assert!(matches!(err, BuildError::BuildFailed { .. }), "got {err:?}");
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
                source_provenance: PackageSourceProvenance::ImmutableManagedExtract,
                policy: BuildPolicy::IfStale,
                host_triple: build::host_target_triple().to_string(),
                staging_destination_slot_dir: detached_slot("x-0.1.0"),
            };
            let err = orch.materialize(&req, &NoopSink).expect_err("must reject");
            assert!(matches!(err, BuildError::UnsupportedSource(_)));
        }
    }

}
