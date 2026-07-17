// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How [`Runner::add_module_with`] sources a module's manifest, and the
//! resolver that turns a [`Strategy`] into either a ready-to-load
//! directory or a [`BuildRequest`] for the injected
//! [`BuildOrchestrator`].
//!
//! The resolver is pure source-location logic — filesystem lookup,
//! `.slpkg` extraction, git checkout. It NEVER invokes a build tool;
//! anything that needs a (re)build is handed to the orchestrator.
//!
//! [`Runner::add_module_with`]: super::super::Runner::add_module_with
//! [`BuildOrchestrator`]: super::BuildOrchestrator

use std::path::PathBuf;

use streamlib_idents::{RegistryClient, RegistryConfig};

use super::build_orchestrator::{BuildPolicy, BuildRequest, BuildSource};
use super::errors::AddModuleError;
use super::processor_registration::host_target_triple;
use super::slpkg::extract_slpkg_to_cache;

/// Semver requirement carried by [`Strategy::Registry`]. Re-exported from
/// `streamlib-idents` so callers can name it next to [`Strategy`] without a
/// separate dependency — it's the same range type a `streamlib.yaml`
/// `Registry` dependency declares.
pub use streamlib_idents::SemVerRange;

/// How [`Runner::add_module_with`] should source a module — the in-code
/// equivalent of the manifest's `dependencies:` / `patch:` declarations.
///
/// The conservative default ([`Runner::add_module`]) uses
/// [`Strategy::InstalledCache`]: cache only, no build, fail loud if
/// absent. Anything rebuildable-from-source is requested explicitly
/// through [`Strategy::Path`] / [`Strategy::Git`] with a [`BuildPolicy`],
/// so a stale artifact can never be silently loaded.
///
/// [`Runner::add_module_with`]: super::super::Runner::add_module_with
/// [`Runner::add_module`]: super::super::Runner::add_module
#[derive(Debug, Clone)]
pub enum Strategy {
    /// The per-app modules folder (`<cwd>/streamlib_modules/@org/name`,
    /// populated by `streamlib add`) first, then the installed-package cache
    /// (`<STREAMLIB_HOME>/.streamlib/cache/packages/...`). Never builds a
    /// package that carries a matching prebuilt. The default for bare
    /// top-level [`Runner::add_module`] loads (transitive registry-flavored
    /// deps map to [`Strategy::Registry`] instead).
    /// Precedence: active `streamlib link` > app modules > installed cache.
    ///
    /// [`Runner::add_module`]: super::super::Runner::add_module
    InstalledCache,

    /// A directory containing `streamlib.yaml` plus per-language sources.
    /// `build` governs whether the orchestrator (re)builds before load.
    Path { path: PathBuf, build: BuildPolicy },

    /// A `.slpkg` archive. Extracted to the cache, then loaded as-is —
    /// pre-built, never rebuilt.
    Slpkg { path: PathBuf },

    /// A git checkout (fetched into the resolver cache), then built per
    /// `build`.
    Git {
        url: String,
        rev: String,
        build: BuildPolicy,
    },

    /// A remote `.slpkg` fetched over the wire (`file://`, `http://`, or
    /// `https://`). The engine resolver fetches the archive into its cache
    /// as network-only I/O (no build), optionally verifies it against
    /// `checksum`, then resolves it exactly like [`Strategy::Slpkg`]:
    /// prefer a matching prebuilt cdylib, else build the bundled source
    /// per `build`. A cached prior fetch of the same URL skips the
    /// download.
    Url {
        url: String,
        /// Governs the build fallback when the fetched box has no matching
        /// prebuilt for this host: [`BuildPolicy::IfStale`] is the
        /// prefer-prebuilt-else-build default (identical to
        /// [`Strategy::Slpkg`]); [`BuildPolicy::NeverBuild`] loads the
        /// staged artifact as-is (a source-only box then fails loud at
        /// dlopen); [`BuildPolicy::AlwaysBuild`] rebuilds the bundled
        /// source even when a prebuilt is present.
        build: BuildPolicy,
        /// Optional integrity pin for the fetched bytes. When `Some`, the
        /// download (or cache hit) must match or the load fails with
        /// [`AddModuleError::IntegrityCheckFailed`]. Signature/trust
        /// verification is a separate concern.
        checksum: Option<ArtifactChecksum>,
    },

    /// Resolve from the configured the static registry **generic** registry by semver
    /// requirement — the cross-repo consumer path. Lists the package's
    /// published versions from its anonymous, cargo-sparse-shaped version
    /// index (`/api/packages/{org}/generic/{name}/index/index.json`),
    /// selects the highest satisfying `version_req` (cargo/npm semantics),
    /// downloads that version's `.slpkg`, then resolves it exactly like
    /// [`Strategy::Url`]: prefer a matching prebuilt, else build the bundled
    /// source per `build`.
    ///
    /// The registry endpoint comes from the environment
    /// (`STREAMLIB_REGISTRY_URL`, the tree root) — the same config the engine's
    /// schema codegen reads, via [`RegistryConfig::from_env`]. The read path
    /// (list + download) is anonymous and tokenless; publishing is
    /// `file://`-only (an emit writes the tree). The package
    /// org + name come from the requested module ident. Absent registry
    /// config fails loud with [`AddModuleError::RegistryNotConfigured`]
    /// rather than silently falling back to a local source.
    ///
    /// [`RegistryConfig::from_env`]: streamlib_idents::RegistryConfig::from_env
    Registry {
        /// Semver requirement matched against the registry's published
        /// versions. [`SemVerRange::Any`] (`*`) accepts the latest.
        version_req: SemVerRange,
        /// Build fallback when the resolved `.slpkg` carries no prebuilt
        /// matching this host triple. Same semantics as [`Strategy::Url`].
        build: BuildPolicy,
    },
}

/// Integrity pin for a fetched [`Strategy::Url`] artifact. Only a content
/// digest today; cryptographic-signature verification is deferred to the
/// trust work and would add a variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactChecksum {
    /// Lowercase or uppercase hex-encoded SHA-256 of the fetched bytes.
    Sha256(String),
}

/// Outcome of resolving a [`Strategy`]: either a directory the engine
/// can load immediately, or a [`BuildRequest`] the orchestrator must
/// materialize first.
#[derive(Debug)]
pub(super) enum ResolvedSource {
    /// A ready-to-load manifest directory (no build needed).
    Ready(PathBuf),
    /// Needs the injected orchestrator to materialize before load.
    NeedsBuild(BuildRequest),
}

/// An active `streamlib link` discovered for the current run, carrying the
/// linked checkout so the resolver can redirect module resolution to the
/// checkout's `packages/` tree.
///
/// npm-link semantics: a package present in the checkout takes precedence over
/// whatever [`Strategy`] the caller declared (registry / path / url / …), so
/// editing a linked package and re-running picks up the edit even for the
/// dominant `add_module(ident, registry())` app shape. A package NOT in the
/// checkout is unaffected and resolves from its declared strategy.
#[derive(Debug, Clone)]
pub(super) struct ActiveLinkedCheckout {
    /// Canonicalized path of the linked streamlib checkout.
    checkout: PathBuf,
}

impl ActiveLinkedCheckout {
    /// Discover an active link from the process working directory — the
    /// consumer's run dir, where `streamlib link` wrote `.streamlib/link.json`.
    /// `Ok(None)` when no link is active; a corrupt marker is a loud typed
    /// error, never a silent skip.
    #[tracing::instrument]
    pub(super) fn discover_from_cwd() -> std::result::Result<Option<Self>, AddModuleError> {
        let cwd = std::env::current_dir().map_err(|e| AddModuleError::LinkStateUnreadable {
            detail: format!("resolving current working directory: {e}"),
        })?;
        Self::discover_from(&cwd)
    }

    /// [`Self::discover_from_cwd`] anchored at an explicit start dir (test seam).
    pub(super) fn discover_from(
        start: &std::path::Path,
    ) -> std::result::Result<Option<Self>, AddModuleError> {
        match streamlib_idents::link_marker::find_and_load_active_link(start) {
            Ok(Some((_marker, manifest))) => Ok(Some(Self {
                checkout: manifest.checkout,
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(AddModuleError::LinkStateCorrupt {
                detail: e.to_string(),
            }),
        }
    }

    /// The linked checkout path.
    pub(super) fn checkout(&self) -> &std::path::Path {
        &self.checkout
    }

    /// If `pkg_ref` (`@org/name`) is present in the checkout's `packages/`
    /// tree, return its source dir. The match is by the manifest's declared
    /// `package.org` + `package.name` (not the directory name), so a package
    /// whose directory differs from its name still resolves. `Ok(None)` when
    /// the package is absent — the caller then resolves it from its declared
    /// strategy, unchanged.
    pub(super) fn checkout_package_dir(
        &self,
        pkg_ref: &streamlib_idents::PackageRef,
    ) -> std::result::Result<Option<PathBuf>, AddModuleError> {
        let packages_root = self.checkout.join("packages");
        if !packages_root.is_dir() {
            return Ok(None);
        }
        // Fast path: the standard monorepo layout is `packages/<name>`.
        let by_name = packages_root.join(pkg_ref.name.as_str());
        if manifest_declares_package(&by_name, pkg_ref) {
            return Ok(Some(by_name));
        }
        // Fallback: scan for a package dir whose manifest declares this ident
        // (covers a package whose directory name differs from its package name).
        let entries =
            std::fs::read_dir(&packages_root).map_err(|e| AddModuleError::LinkStateUnreadable {
                detail: format!(
                    "reading linked checkout packages dir {}: {e}",
                    packages_root.display()
                ),
            })?;
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir == by_name || !dir.is_dir() {
                continue;
            }
            if manifest_declares_package(&dir, pkg_ref) {
                return Ok(Some(dir));
            }
        }
        Ok(None)
    }
}

/// Resolve the active link to thread into a top-level module load, honoring
/// the locked-run contract: a **locked** run (`add_modules_from_lockfile`)
/// IGNORES links — it must be reproducible / offline — so it always resolves
/// to `None`. A live run discovers the link from the process working directory.
/// This is the single gate that keeps locked runs link-free.
pub(super) fn discover_active_link_for_load(
    is_locked: bool,
) -> std::result::Result<Option<ActiveLinkedCheckout>, AddModuleError> {
    if is_locked {
        Ok(None)
    } else {
        ActiveLinkedCheckout::discover_from_cwd()
    }
}

/// [`discover_active_link_for_load`] with the discovery anchored at an explicit
/// start dir (test seam) so the locked gate can be locked without touching the
/// process working directory.
#[cfg(test)]
pub(super) fn discover_active_link_for_load_from(
    is_locked: bool,
    start: &std::path::Path,
) -> std::result::Result<Option<ActiveLinkedCheckout>, AddModuleError> {
    if is_locked {
        Ok(None)
    } else {
        ActiveLinkedCheckout::discover_from(start)
    }
}

/// Whether the `streamlib.yaml` at `dir` declares a package whose org + name
/// equal `pkg_ref`. A missing / unreadable / malformed manifest is treated as
/// "no match" — this dir just isn't the package we're looking for; a genuine
/// manifest error surfaces on the real load path with a precise message.
fn manifest_declares_package(
    dir: &std::path::Path,
    pkg_ref: &streamlib_idents::PackageRef,
) -> bool {
    use streamlib_idents::Manifest;
    if !dir.join(Manifest::FILE_NAME).exists() {
        return false;
    }
    match Manifest::load(dir) {
        Ok(m) => m
            .package
            .as_ref()
            .is_some_and(|p| p.org == pkg_ref.org && p.name == pkg_ref.name),
        Err(_) => false,
    }
}

/// Resolve a [`Strategy`] to a [`ResolvedSource`]. Pure source-location
/// logic (cache lookup, `.slpkg` extract, git checkout); never invokes a
/// build tool.
///
/// When `link` is `Some` (a non-locked run with an active `streamlib link`), a
/// package present in the linked checkout's `packages/` tree is resolved from
/// there regardless of `strategy` — see [`ActiveLinkedCheckout`]. Locked runs
/// pass `None` (reproducible / offline by contract), and a package absent from
/// the checkout falls through to `strategy` unchanged.
pub(super) fn resolve_strategy_to_source(
    strategy: &Strategy,
    pkg_ref: &streamlib_idents::PackageRef,
    link: Option<&ActiveLinkedCheckout>,
) -> std::result::Result<ResolvedSource, AddModuleError> {
    // Link-aware short-circuit (npm-link semantics): an active link redirects
    // resolution of ANY package present in the linked checkout, overriding the
    // caller's strategy. This is what makes `streamlib link` → edit → re-run
    // reflect a checkout edit for the dominant `add_module(ident, registry())`
    // app shape. IfStale rebuilds on edit via the build tool's own fingerprint.
    if let Some(link) = link {
        if let Some(pkg_dir) = link.checkout_package_dir(pkg_ref)? {
            tracing::info!(
                package = %pkg_ref,
                checkout = %link.checkout().display(),
                source = %pkg_dir.display(),
                "streamlib link active — resolving module from linked checkout (overriding strategy)"
            );
            return Ok(source_for_dir(pkg_ref, pkg_dir, BuildPolicy::IfStale));
        }
    }
    match strategy {
        Strategy::InstalledCache => {
            resolve_installed_cache_strategy(pkg_ref, app_modules_root().as_deref())
        }
        Strategy::Slpkg { path } => {
            let extracted = extract_slpkg_to_cache(path).map_err(|e| {
                AddModuleError::SlpkgExtractionFailed {
                    archive: path.clone(),
                    detail: e.to_string(),
                }
            })?;
            // A `.slpkg` may carry source and/or a prebuilt cdylib. Prefer
            // a prebuilt matching this host; otherwise build the bundled
            // source on the host (pip wheel-vs-sdist for Rust).
            Ok(source_for_resolved_dir(pkg_ref, extracted))
        }
        Strategy::Path { path, build } => {
            if !path.join("streamlib.yaml").exists() {
                return Err(AddModuleError::ManifestDirectoryMissing { path: path.clone() });
            }
            Ok(source_for_dir(pkg_ref, path.clone(), *build))
        }
        Strategy::Git { url, rev, build } => {
            let checkout = fetch_git_checkout(pkg_ref, url, rev)?;
            Ok(source_for_dir(pkg_ref, checkout, *build))
        }
        Strategy::Url {
            url,
            build,
            checksum,
        } => {
            // Network-only fetch in the resolver (the same shape as
            // `fetch_git_checkout` for `Strategy::Git`): download the
            // `.slpkg` into the resolver cache, verify integrity, then
            // route it through the SAME extract + prefer-prebuilt-else-
            // build path a local `.slpkg` takes. No build happens here —
            // any build is deferred to the injected orchestrator.
            let slpkg = fetch_remote_slpkg(pkg_ref, url, checksum.as_ref())?;
            let extracted = extract_slpkg_to_cache(&slpkg).map_err(|e| {
                AddModuleError::SlpkgExtractionFailed {
                    archive: slpkg.clone(),
                    detail: e.to_string(),
                }
            })?;
            Ok(source_for_fetched_slpkg(pkg_ref, extracted, *build))
        }
        Strategy::Registry { version_req, build } => {
            // Same resolve shape as `Strategy::Url`, except the download URL
            // is derived from the registry's published versions instead of
            // being supplied by the caller. The `streamlib-idents` registry
            // client is reused verbatim — list + semver-select + token-aware
            // download — so engine schema codegen and runtime module loading
            // share one resolver rather than maintaining parallel ones.
            let config = RegistryConfig::from_env().ok_or_else(|| {
                AddModuleError::RegistryNotConfigured {
                    package: pkg_ref.clone(),
                    env: streamlib_idents::REGISTRY_URL_ENV.to_string(),
                }
            })?;
            let client = RegistryClient::new(&config);
            let available = client.list_versions(pkg_ref).map_err(|e| {
                AddModuleError::RegistryResolutionFailed {
                    package: pkg_ref.clone(),
                    detail: format!("listing versions: {e}"),
                }
            })?;
            let selected = streamlib_idents::select_version(pkg_ref, version_req, &available)
                .map_err(|e| AddModuleError::RegistryResolutionFailed {
                    package: pkg_ref.clone(),
                    detail: e.to_string(),
                })?;
            // IfStale fast path: a `.slpkg` already materialized for this exact
            // version is reused instead of re-downloaded and re-extracted.
            // `extract_slpkg_to_cache` rm -rf's the cache slot on every call,
            // which wipes any cdylib a prior run built into `lib/<triple>/` (and
            // any provisioned `.venv`) — so without this check `IfStale` rebuilt
            // on every run even when the registry had not changed. Registry
            // versions are immutable (a content change ships a new version);
            // `streamlib pkg clean` clears the cache to force a re-fetch when a
            // version is republished in place during development.
            let slot = crate::core::streamlib_home::get_cached_package_dir_for_name_version(
                pkg_ref.name.as_str(),
                selected,
            );
            let extracted = if matches!(build, BuildPolicy::IfStale) && slot.is_dir() {
                tracing::debug!(
                    package = %pkg_ref,
                    version = %selected,
                    slot = %slot.display(),
                    "registry slot already materialized for selected version — reusing (no re-download/extract)"
                );
                slot
            } else {
                let (bytes, url) = client.download_slpkg(pkg_ref, selected).map_err(|e| {
                    AddModuleError::RegistryResolutionFailed {
                        package: pkg_ref.clone(),
                        detail: format!("downloading {selected}: {e}"),
                    }
                })?;
                tracing::debug!(
                    package = %pkg_ref,
                    version = %selected,
                    %url,
                    "resolved module from static generic store"
                );
                let archive = persist_registry_slpkg(pkg_ref, &url, &bytes)?;
                extract_slpkg_to_cache(&archive).map_err(|e| {
                    AddModuleError::SlpkgExtractionFailed {
                        archive: archive.clone(),
                        detail: e.to_string(),
                    }
                })?
            };
            Ok(source_for_fetched_slpkg(pkg_ref, extracted, *build))
        }
    }
}

/// Resolve [`Strategy::InstalledCache`]: the per-app modules folder wins over
/// the installed-package cache. `app_root` is the exact directory probed for
/// `streamlib_modules/` — the process working directory in production, an
/// explicit dir in tests; `None` (unresolvable cwd) skips straight to the
/// installed cache.
fn resolve_installed_cache_strategy(
    pkg_ref: &streamlib_idents::PackageRef,
    app_root: Option<&std::path::Path>,
) -> std::result::Result<ResolvedSource, AddModuleError> {
    if let Some(root) = app_root {
        if let Some(modules_package_dir) = lookup_app_modules_package_dir(root, pkg_ref) {
            tracing::info!(
                package = %pkg_ref,
                source = %modules_package_dir.display(),
                "resolving module from the app's streamlib_modules folder"
            );
            return Ok(source_for_resolved_dir(pkg_ref, modules_package_dir));
        }
    }
    let (dir, _version) =
        lookup_installed_cache(pkg_ref)?.ok_or_else(|| AddModuleError::ModuleNotFound {
            package: pkg_ref.clone(),
        })?;
    // Same prefer-prebuilt-else-build-source decision as `.slpkg`:
    // a cached package may carry source needing a host build.
    Ok(source_for_resolved_dir(pkg_ref, dir))
}

/// Environment override for the directory that contains the app's
/// `streamlib_modules/` folder — the GST_PLUGIN_PATH-style default a
/// daemon/host sets. A runtime override ([`set_app_modules_root_override`])
/// takes precedence.
pub(crate) const APP_MODULES_DIR_ENV: &str = "STREAMLIB_MODULES_DIR";

/// Process-wide override for the app-modules root, set via
/// [`Runner::set_app_modules_dir`]. `None` falls back to the env var, then the
/// process working directory.
///
/// [`Runner::set_app_modules_dir`]: super::super::Runner::set_app_modules_dir
static APP_MODULES_ROOT_OVERRIDE: std::sync::RwLock<Option<PathBuf>> =
    std::sync::RwLock::new(None);

/// Tell the module loader which directory contains the app's
/// `streamlib_modules/` folder for lazy discovery and [`Strategy::InstalledCache`]
/// resolution. `None` clears the override (back to env / cwd).
pub(crate) fn set_app_modules_root_override(root: Option<PathBuf>) {
    *APP_MODULES_ROOT_OVERRIDE
        .write()
        .expect("app-modules root override lock poisoned") = root;
}

/// The app-modules root: the runtime-set override, else the
/// `STREAMLIB_MODULES_DIR` env var, else the exact process working directory
/// (no walk-up). `None` only when the cwd is unresolvable and neither override
/// nor env is set — resolution then proceeds with the installed cache alone.
pub(crate) fn app_modules_root() -> Option<PathBuf> {
    if let Some(root) = APP_MODULES_ROOT_OVERRIDE
        .read()
        .expect("app-modules root override lock poisoned")
        .clone()
    {
        return Some(root);
    }
    if let Some(env) = std::env::var_os(APP_MODULES_DIR_ENV).filter(|env| !env.is_empty()) {
        return Some(PathBuf::from(env));
    }
    std::env::current_dir().ok()
}

/// `<app_root>/streamlib_modules/@org/name` when it exists and its manifest
/// declares `pkg_ref`; `None` otherwise (a present-but-mismatched entry warns
/// and falls through to the installed cache).
fn lookup_app_modules_package_dir(
    app_root: &std::path::Path,
    pkg_ref: &streamlib_idents::PackageRef,
) -> Option<PathBuf> {
    let dir = app_root
        .join(streamlib_idents::app_modules::APP_MODULES_DIR_NAME)
        .join(format!("@{}", pkg_ref.org))
        .join(pkg_ref.name.as_str());
    if !dir.is_dir() {
        return None;
    }
    if manifest_declares_package(&dir, pkg_ref) {
        Some(dir)
    } else {
        tracing::warn!(
            package = %pkg_ref,
            dir = %dir.display(),
            "streamlib_modules entry does not declare the requested package — \
             falling through to the installed cache"
        );
        None
    }
}

/// Decide whether a resolved package directory loads as-is or needs a
/// build, based on its [`BuildPolicy`].
fn source_for_dir(
    pkg_ref: &streamlib_idents::PackageRef,
    dir: PathBuf,
    build: BuildPolicy,
) -> ResolvedSource {
    if build.requires_orchestrator() {
        ResolvedSource::NeedsBuild(BuildRequest {
            package: pkg_ref.clone(),
            source: BuildSource::PackageDir(dir),
            policy: build,
            host_triple: host_target_triple().to_string(),
        })
    } else {
        ResolvedSource::Ready(dir)
    }
}

/// Decide how to load an already-resolved package directory (an extracted
/// `.slpkg` or an installed-cache entry) that may carry **source and/or a
/// prebuilt cdylib**. Prefer a prebuilt matching this host (compiler-free,
/// instant); otherwise build the bundled Rust source on the host. This is
/// the pip wheel-vs-sdist model for Rust: one artifact runs everywhere,
/// and a toolchain is needed only when there's no matching prebuilt.
///
/// A Python/Deno package that has not yet been provisioned (no `.venv` /
/// no regenerated `_generated_/`) also routes to the orchestrator — an
/// extracted `.slpkg` or installed-cache entry carries source but no venv,
/// and `materialize` is the only place the venv is provisioned. Loading it
/// as-is would leave its subprocess spawn with no interpreter.
fn source_for_resolved_dir(pkg_ref: &streamlib_idents::PackageRef, dir: PathBuf) -> ResolvedSource {
    if needs_host_build(&dir) || needs_polyglot_provisioning(&dir) {
        ResolvedSource::NeedsBuild(BuildRequest {
            package: pkg_ref.clone(),
            source: BuildSource::PackageDir(dir),
            // No explicit policy on these arms — a build is required only
            // because the prebuilt / provisioning is absent, so `IfStale`
            // (build iff the output isn't already staged) is the right
            // semantics; the orchestrator's own staleness-skip short-circuits
            // an already-provisioned cache slot, so there is no rebuild loop.
            policy: BuildPolicy::IfStale,
            host_triple: host_target_triple().to_string(),
        })
    } else {
        ResolvedSource::Ready(dir)
    }
}

/// Resolve a fetched-and-extracted `.slpkg` ([`Strategy::Url`]) honoring
/// the caller's [`BuildPolicy`] on top of the prefer-prebuilt-else-build
/// model. A `.slpkg` is the crates.io / pip / npm-shaped box: source plus
/// (optionally) a matching prebuilt. The policy decides the build
/// fallback:
///
/// - [`BuildPolicy::NeverBuild`] — load the staged dir as-is. A matching
///   prebuilt loads compiler-free; a source-only box fails loud at dlopen.
/// - [`BuildPolicy::IfStale`] — prefer a matching prebuilt, else build the
///   bundled source. Identical to how a local [`Strategy::Slpkg`] resolves.
/// - [`BuildPolicy::AlwaysBuild`] — rebuild the bundled source even when a
///   prebuilt is present. A box with no Rust source is not automatically a
///   no-op: an unprovisioned Python/Deno box still routes through
///   `materialize` (same reason as [`source_for_resolved_dir`]) so its venv /
///   `_generated_/` gets provisioned before load; only a fully-provisioned
///   non-Rust box loads as-is.
fn source_for_fetched_slpkg(
    pkg_ref: &streamlib_idents::PackageRef,
    dir: PathBuf,
    build: BuildPolicy,
) -> ResolvedSource {
    match build {
        BuildPolicy::NeverBuild => ResolvedSource::Ready(dir),
        BuildPolicy::IfStale => source_for_resolved_dir(pkg_ref, dir),
        BuildPolicy::AlwaysBuild => {
            // `has_buildable_rust_source` covers the "rebuild the Rust cdylib"
            // intent; `needs_polyglot_provisioning` covers an unprovisioned
            // Python/Deno box (no `.venv` / no `_generated_/`) that would
            // otherwise load as-is and die at subprocess spawn with
            // `.venv/bin/python: No such file or directory`. Both are LIVE
            // AlwaysBuild paths — `streamlib add` and `Strategy::Registry`
            // resolve with AlwaysBuild through here.
            if has_buildable_rust_source(&dir) || needs_polyglot_provisioning(&dir) {
                ResolvedSource::NeedsBuild(BuildRequest {
                    package: pkg_ref.clone(),
                    source: BuildSource::PackageDir(dir),
                    policy: BuildPolicy::AlwaysBuild,
                    host_triple: host_target_triple().to_string(),
                })
            } else {
                ResolvedSource::Ready(dir)
            }
        }
    }
}

/// Whether a resolved package dir needs an on-host Rust build before it
/// can load: it has buildable Rust source but **no** prebuilt cdylib for
/// this host triple. A package with a matching prebuilt (or no Rust at
/// all) loads as-is; a Rust package with neither prebuilt nor source loads
/// as-is and fails loud at dlopen (no artifact, nothing to build).
fn needs_host_build(dir: &std::path::Path) -> bool {
    has_buildable_rust_source(dir) && !has_matching_prebuilt(dir)
}

/// Whether a resolved package dir carries a Python or Deno runtime but is
/// missing the build-time provisioning those runtimes need — a Python package
/// with no `.venv/bin/python` interpreter, or a Deno package with no
/// regenerated `_generated_/` wire vocabulary. Such a dir must route through
/// the orchestrator's `materialize` (which provisions the venv / regenerates
/// `_generated_/` at the cache slot it loads from), not load as-is: loaded
/// as-is, a Python package's subprocess spawn dies with `.venv/bin/python: No
/// such file or directory`. An already-provisioned dir returns `false` and
/// loads directly; the orchestrator's own `IfStale` staleness-skip then
/// short-circuits any redundant rebuild (no rebuild loop).
///
/// Each language's presence oracle is the SAME one the orchestrator's
/// staleness-skip uses, so the two resolvers can never disagree (a disagreement
/// would ping-pong: source.rs routes to `NeedsBuild` while the orchestrator's
/// guard deems the venv-less slot reusable, loading it broken):
/// - Python — filesystem-detected (a `python/` source dir or a `pyproject.toml`
///   at the package root), matching `python_venv::staged_package_has_python`.
/// - Deno — manifest-detected (a `TypeScript` runtime processor), matching
///   `deno_codegen::staged_package_has_deno`.
fn needs_polyglot_provisioning(dir: &std::path::Path) -> bool {
    use streamlib_processor_schema::ProcessorLanguage;
    // Python: filesystem oracle, aligned with `staged_package_has_python`.
    let has_python = dir.join("python").is_dir() || dir.join("pyproject.toml").is_file();
    let python_unprovisioned =
        has_python && !dir.join(".venv").join("bin").join("python").exists();
    // Deno: manifest oracle, aligned with `staged_package_has_deno`.
    let declares_deno = match crate::core::config::ProjectConfig::load(dir) {
        Ok(c) => c
            .processors
            .iter()
            .any(|p| p.runtime.language == ProcessorLanguage::TypeScript),
        Err(_) => false,
    };
    let deno_unprovisioned = declares_deno && !dir.join("_generated_").is_dir();
    python_unprovisioned || deno_unprovisioned
}

/// Whether `dir` declares Rust processors AND carries a `Cargo.toml` to
/// build them from. An unreadable / malformed manifest is treated as
/// "nothing to build here" — the loader's own manifest read surfaces the
/// parse error with a clear message rather than a build failure.
fn has_buildable_rust_source(dir: &std::path::Path) -> bool {
    use streamlib_processor_schema::ProcessorLanguage;
    let config = match crate::core::config::ProjectConfig::load(dir) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let has_rust = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust));
    has_rust && dir.join("Cargo.toml").exists()
}

/// Whether `dir` carries a prebuilt cdylib for this host triple under
/// `lib/<triple>/`.
fn has_matching_prebuilt(dir: &std::path::Path) -> bool {
    let triple_dir = dir.join("lib").join(host_target_triple());
    std::fs::read_dir(&triple_dir)
        .map(|it| {
            it.flatten()
                .any(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        })
        .unwrap_or(false)
}

/// Fetch (or reuse a cached) remote `.slpkg` for `pkg_ref` from `url` into
/// `~/.streamlib/resolver-cache/url/`. Network I/O only — no build. When
/// `checksum` is `Some`, the bytes must match or the load fails loud. A
/// prior fetch of the same URL is reused (the download is skipped); a
/// cache hit is re-verified against `checksum` to catch a corrupted cache.
fn fetch_remote_slpkg(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
    checksum: Option<&ArtifactChecksum>,
) -> std::result::Result<PathBuf, AddModuleError> {
    let cache_dir = crate::core::streamlib_home::get_streamlib_data_dir()
        .join("resolver-cache")
        .join("url");
    // Content-stable cache key derived from the URL (mirrors the git
    // checkout cache's URL sanitization).
    let safe: String = url
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let target = cache_dir.join(format!("{safe}.slpkg"));

    if target.exists() {
        // Cache hit — re-verify integrity (cheap, catches a corrupted
        // cache entry) and skip the download.
        if let Some(expected) = checksum {
            let bytes = std::fs::read(&target).map_err(|e| AddModuleError::UrlFetchFailed {
                package: pkg_ref.clone(),
                url: url.to_string(),
                detail: format!("reading cached artifact {}: {e}", target.display()),
            })?;
            verify_artifact_checksum(pkg_ref, url, &bytes, expected)?;
        }
        return Ok(target);
    }

    std::fs::create_dir_all(&cache_dir).map_err(|e| AddModuleError::UrlFetchFailed {
        package: pkg_ref.clone(),
        url: url.to_string(),
        detail: format!("creating resolver cache dir {}: {e}", cache_dir.display()),
    })?;

    let bytes = download_url_bytes(pkg_ref, url)?;

    if let Some(expected) = checksum {
        verify_artifact_checksum(pkg_ref, url, &bytes, expected)?;
    }

    // Atomic publish: write a temp sibling then rename, so an interrupted
    // download never leaves a half-written file a later run treats as a
    // cache hit.
    let tmp = cache_dir.join(format!("{safe}.slpkg.partial"));
    std::fs::write(&tmp, &bytes).map_err(|e| AddModuleError::UrlFetchFailed {
        package: pkg_ref.clone(),
        url: url.to_string(),
        detail: format!("writing fetched artifact: {e}"),
    })?;
    std::fs::rename(&tmp, &target).map_err(|e| AddModuleError::UrlFetchFailed {
        package: pkg_ref.clone(),
        url: url.to_string(),
        detail: format!("publishing fetched artifact to cache: {e}"),
    })?;
    Ok(target)
}

/// Persist already-downloaded registry `.slpkg` bytes into the resolver
/// cache so [`extract_slpkg_to_cache`] can read them. Keyed by the canonical
/// download URL (which embeds the package name + concrete version), with an
/// atomic temp-then-rename publish so an interrupted write never leaves a
/// half-file a later run treats as complete. This is the
/// [`fetch_remote_slpkg`] write path minus the download — the bytes are
/// already in hand from [`RegistryClient::download_slpkg`], which (unlike the
/// `Strategy::Url` fetch) carries the registry auth token.
fn persist_registry_slpkg(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
    bytes: &[u8],
) -> std::result::Result<PathBuf, AddModuleError> {
    let cache_dir = crate::core::streamlib_home::get_streamlib_data_dir()
        .join("resolver-cache")
        .join("registry");
    let safe: String = url
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let target = cache_dir.join(format!("{safe}.slpkg"));

    let persist_err = |detail: String| AddModuleError::RegistryResolutionFailed {
        package: pkg_ref.clone(),
        detail,
    };
    std::fs::create_dir_all(&cache_dir).map_err(|e| {
        persist_err(format!(
            "creating resolver cache dir {}: {e}",
            cache_dir.display()
        ))
    })?;
    let tmp = cache_dir.join(format!("{safe}.slpkg.partial"));
    std::fs::write(&tmp, bytes)
        .map_err(|e| persist_err(format!("writing fetched artifact: {e}")))?;
    std::fs::rename(&tmp, &target)
        .map_err(|e| persist_err(format!("publishing fetched artifact to cache: {e}")))?;
    Ok(target)
}

/// Download the raw bytes of `url`. `file://` reads from disk (the
/// hermetic path used by tests and local mirrors); `http(s)://` performs a
/// blocking HTTP GET. Any other scheme is rejected loud.
fn download_url_bytes(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
) -> std::result::Result<Vec<u8>, AddModuleError> {
    let fetch_err = |detail: String| AddModuleError::UrlFetchFailed {
        package: pkg_ref.clone(),
        url: url.to_string(),
        detail,
    };

    if let Some(path) = url.strip_prefix("file://") {
        // `file:///abs/path` → `/abs/path`; an authority (`file://host/…`)
        // is uncommon for local artifacts and not supported here.
        return std::fs::read(path).map_err(|e| fetch_err(format!("reading {path}: {e}")));
    }

    if url.starts_with("http://") || url.starts_with("https://") {
        let response = ureq::get(url)
            .call()
            .map_err(|e| fetch_err(format!("HTTP request failed: {e}")))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
            .map_err(|e| fetch_err(format!("reading HTTP response body: {e}")))?;
        return Ok(bytes);
    }

    Err(fetch_err(
        "unsupported URL scheme (expected file://, http://, or https://)".to_string(),
    ))
}

/// Verify `bytes` against the expected [`ArtifactChecksum`], returning
/// [`AddModuleError::IntegrityCheckFailed`] on mismatch.
fn verify_artifact_checksum(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
    bytes: &[u8],
    expected: &ArtifactChecksum,
) -> std::result::Result<(), AddModuleError> {
    match expected {
        ArtifactChecksum::Sha256(hex) => {
            let actual = sha256_hex(bytes);
            if actual.eq_ignore_ascii_case(hex.trim()) {
                Ok(())
            } else {
                Err(AddModuleError::IntegrityCheckFailed {
                    package: pkg_ref.clone(),
                    url: url.to_string(),
                    detail: format!("sha256 expected {}, got {actual}", hex.trim()),
                })
            }
        }
    }
}

/// Lowercase hex-encoded SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fetch (or reuse a cached) git checkout for `pkg_ref` at `url`@`rev`
/// into `~/.streamlib/resolver-cache/`. Network I/O only — no build.
fn fetch_git_checkout(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
    rev: &str,
) -> std::result::Result<PathBuf, AddModuleError> {
    let cache_dir = crate::core::streamlib_home::get_streamlib_data_dir().join("resolver-cache");
    streamlib_idents::fetch_git(&pkg_ref.to_string(), url, rev, &cache_dir).map_err(|e| {
        AddModuleError::GitFetchFailed {
            package: pkg_ref.clone(),
            url: url.to_string(),
            rev: rev.to_string(),
            detail: e.to_string(),
        }
    })
}

/// Look the canonical [`PackageRef`] up in the installed-package cache.
/// Returns `(cache_dir, version)` when present.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
fn lookup_installed_cache(
    pkg_ref: &streamlib_idents::PackageRef,
) -> std::result::Result<Option<(PathBuf, streamlib_idents::SemVer)>, AddModuleError> {
    use crate::core::config::InstalledPackageManifest;
    use crate::core::streamlib_home::get_cached_package_dir;

    let manifest =
        InstalledPackageManifest::load().map_err(|e| AddModuleError::InstalledCacheLoadFailed {
            detail: e.to_string(),
        })?;
    Ok(manifest
        .find_by_ref(pkg_ref)
        .map(|entry| (get_cached_package_dir(&entry.cache_dir), entry.version)))
}

/// Read the `[package].version` field from the `streamlib.yaml` at
/// `dir`. Used by the recursive walker to validate the resolved
/// manifest against the requested [`SemVerRange`].
///
/// [`SemVerRange`]: streamlib_idents::SemVerRange
pub(super) fn read_version_from_manifest_dir(
    dir: &std::path::Path,
) -> std::result::Result<streamlib_idents::SemVer, AddModuleError> {
    use streamlib_idents::Manifest;
    let manifest_path = dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Err(AddModuleError::ManifestDirectoryMissing {
            path: dir.to_path_buf(),
        });
    }
    let manifest = Manifest::load(dir).map_err(|e| AddModuleError::StrategyManifestLoadFailed {
        source_path: dir.to_path_buf(),
        detail: e.to_string(),
    })?;
    manifest.package.as_ref().map(|p| p.version).ok_or_else(|| {
        AddModuleError::StrategyManifestLoadFailed {
            source_path: dir.to_path_buf(),
            detail: "manifest has no `package:` block".into(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_YAML: &str = "package:\n  org: tatolab\n  name: rp\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n";
    const PY_YAML: &str = "package:\n  org: tatolab\n  name: py\n  version: 0.1.0\nprocessors:\n  - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n";

    fn manifest(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("streamlib.yaml"), body).unwrap();
    }

    #[test]
    fn needs_host_build_when_rust_source_and_no_prebuilt() {
        // A Rust package carrying source but no matching-triple cdylib
        // must build on the host. Revert the Cargo.toml check and this
        // would never build (load a non-existent cdylib instead).
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        assert!(needs_host_build(dir.path()));
    }

    #[test]
    fn no_host_build_when_matching_prebuilt_present() {
        // A prebuilt cdylib for this host triple is preferred — compiler-
        // free load. Revert the prebuilt check and we'd rebuild needlessly
        // (and require a toolchain on a frozen host that shipped a binary).
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(triple_dir.join("librp.so"), b"prebuilt").unwrap();
        assert!(!needs_host_build(dir.path()));
    }

    #[test]
    fn no_host_build_for_non_rust_package() {
        // Python/Deno/schema packages run from source — no Rust build.
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::write(dir.path().join("p.py"), b"#").unwrap();
        assert!(!needs_host_build(dir.path()));
    }

    #[test]
    fn no_host_build_for_rust_without_prebuilt_or_source() {
        // Neither a cdylib nor Cargo.toml: load as-is and let the dlopen
        // loader fail loud — there's nothing to build.
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        assert!(!needs_host_build(dir.path()));
    }

    // =====================================================================
    // source_for_fetched_slpkg — Strategy::Url policy-aware resolution
    // =====================================================================

    fn pkg_ref() -> streamlib_idents::PackageRef {
        streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("rp").unwrap(),
        )
    }

    /// A Rust box carrying source but no matching prebuilt: `NeverBuild`
    /// must load it as-is (the prebuilt-only / frozen posture — dlopen
    /// then fails loud), NOT request a build. Revert the `NeverBuild` arm
    /// and this would wrongly emit a build request.
    #[test]
    fn fetched_slpkg_never_build_loads_as_is_even_when_source_only() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        let resolved = source_for_fetched_slpkg(
            &pkg_ref(),
            dir.path().to_path_buf(),
            BuildPolicy::NeverBuild,
        );
        assert!(matches!(resolved, ResolvedSource::Ready(_)));
    }

    /// `IfStale` mirrors `Strategy::Slpkg`: a source-only Rust box builds.
    #[test]
    fn fetched_slpkg_if_stale_builds_source_only_box() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        let resolved =
            source_for_fetched_slpkg(&pkg_ref(), dir.path().to_path_buf(), BuildPolicy::IfStale);
        assert!(matches!(resolved, ResolvedSource::NeedsBuild(_)));
    }

    /// `IfStale` prefers a matching prebuilt — compiler-free load.
    #[test]
    fn fetched_slpkg_if_stale_prefers_matching_prebuilt() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(triple_dir.join("librp.so"), b"prebuilt").unwrap();
        let resolved =
            source_for_fetched_slpkg(&pkg_ref(), dir.path().to_path_buf(), BuildPolicy::IfStale);
        assert!(matches!(resolved, ResolvedSource::Ready(_)));
    }

    /// `AlwaysBuild` rebuilds the bundled source even when a matching
    /// prebuilt is present. Revert the `AlwaysBuild` arm (fall through to
    /// `source_for_resolved_dir`) and the present prebuilt would short-
    /// circuit to `Ready`, defeating the "distrust the prebuilt" intent.
    #[test]
    fn fetched_slpkg_always_build_rebuilds_even_with_prebuilt() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), RUST_YAML);
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        std::fs::write(triple_dir.join("librp.so"), b"prebuilt").unwrap();
        let resolved = source_for_fetched_slpkg(
            &pkg_ref(),
            dir.path().to_path_buf(),
            BuildPolicy::AlwaysBuild,
        );
        match resolved {
            ResolvedSource::NeedsBuild(req) => assert_eq!(req.policy, BuildPolicy::AlwaysBuild),
            other => panic!("expected NeedsBuild(AlwaysBuild), got {other:?}"),
        }
    }

    /// `AlwaysBuild` on a non-buildable box (no Rust source) is a no-op →
    /// load as-is.
    #[test]
    fn fetched_slpkg_always_build_noop_without_rust_source() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        let resolved = source_for_fetched_slpkg(
            &pkg_ref(),
            dir.path().to_path_buf(),
            BuildPolicy::AlwaysBuild,
        );
        assert!(matches!(resolved, ResolvedSource::Ready(_)));
    }

    /// COMPLETENESS (bug-reproduce, LIVE `Strategy::Url` / `Strategy::Registry`
    /// + `streamlib add` path): `AlwaysBuild` on an UNPROVISIONED Python box
    /// (a `pyproject.toml` present but no `.venv`) must route through
    /// `materialize`, NOT load as-is — the same `.venv/bin/python: No such file
    /// or directory` bug as the `IfStale` path, on the AlwaysBuild arm. Mentally
    /// revert the `|| needs_polyglot_provisioning(&dir)` clause in the
    /// `AlwaysBuild` arm and this resolves to `Ready`, failing the assertion.
    #[test]
    fn fetched_slpkg_always_build_provisions_unprovisioned_python() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname = \"py\"\n",
        )
        .unwrap();
        let resolved = source_for_fetched_slpkg(
            &pkg_ref(),
            dir.path().to_path_buf(),
            BuildPolicy::AlwaysBuild,
        );
        match resolved {
            ResolvedSource::NeedsBuild(req) => assert_eq!(req.policy, BuildPolicy::AlwaysBuild),
            other => panic!("expected NeedsBuild(AlwaysBuild), got {other:?}"),
        }
    }

    // =====================================================================
    // Polyglot provisioning — an unprovisioned Python/Deno package must
    // route through the orchestrator's `materialize` (which provisions the
    // venv / regenerates `_generated_/` at the cache slot it loads from),
    // not load as-is. The bug: a Python-only package loaded as-is has no
    // `.venv/bin/python`, so its subprocess spawn dies at runtime with
    // `.venv/bin/python: No such file or directory`.
    // =====================================================================

    const DENO_YAML: &str = "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n";

    /// CRUX (bug-reproduce): a resolved (extracted `.slpkg` / installed-cache /
    /// `streamlib_modules`) Python-only package (no Rust, no `Cargo.toml`) with
    /// no provisioned `.venv` must resolve to `NeedsBuild` so the orchestrator
    /// provisions its venv — NOT `Ready` (loaded as-is, the subprocess spawn
    /// then fails with `.venv/bin/python: No such file or directory`). Mentally
    /// revert the `needs_polyglot_provisioning` clause in
    /// `source_for_resolved_dir` and this resolves to `Ready`, failing the
    /// assertion.
    #[test]
    fn resolved_dir_routes_unprovisioned_python_to_build() {
        // Python presence is filesystem-detected (a `pyproject.toml`, matching
        // the orchestrator's `staged_package_has_python`), so the fixture
        // stages that on-disk layout rather than relying on the manifest.
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname = \"py\"\n",
        )
        .unwrap();
        let resolved = source_for_resolved_dir(&pkg_ref(), dir.path().to_path_buf());
        assert!(
            matches!(resolved, ResolvedSource::NeedsBuild(_)),
            "unprovisioned Python-only package must route through materialize, got {resolved:?}"
        );
    }

    /// A Python package (a `pyproject.toml` on disk) with no `.venv/bin/python`
    /// needs provisioning.
    #[test]
    fn needs_polyglot_provisioning_for_python_without_venv() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname = \"py\"\n",
        )
        .unwrap();
        assert!(needs_polyglot_provisioning(dir.path()));
    }

    /// A `python/` source dir alone (no `pyproject.toml`) is the other half of
    /// the filesystem oracle — a package staged that way with no `.venv` still
    /// needs provisioning, matching `staged_package_has_python`.
    #[test]
    fn needs_polyglot_provisioning_for_python_dir_without_venv() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        assert!(needs_polyglot_provisioning(dir.path()));
    }

    /// Once the venv interpreter is present, the package loads as-is — the
    /// short-circuit that stops a rebuild loop. Mentally revert the
    /// `.venv/bin/python` existence check and this stays `true`, re-materializing
    /// an already-provisioned slot on every load.
    #[test]
    fn no_polyglot_provisioning_when_python_venv_present() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        std::fs::write(
            dir.path().join("pyproject.toml"),
            b"[project]\nname = \"py\"\n",
        )
        .unwrap();
        let bin = dir.path().join(".venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python"), b"#!/bin/sh\n").unwrap();
        assert!(!needs_polyglot_provisioning(dir.path()));
    }

    /// Oracle-parity (Finding 2): a manifest that declares Python WITHOUT the
    /// on-disk layout (no `python/` dir, no `pyproject.toml`) is NOT detected —
    /// matching the orchestrator's `staged_package_has_python`, which is
    /// filesystem-only. A manifest-driven oracle here would disagree with the
    /// orchestrator's staleness-skip and ping-pong. Mentally revert the
    /// filesystem detection to manifest-based and this returns `true`, breaking
    /// parity.
    #[test]
    fn no_polyglot_provisioning_for_manifest_python_without_filesystem_layout() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), PY_YAML);
        assert!(!needs_polyglot_provisioning(dir.path()));
    }

    /// A Deno package with no regenerated `_generated_/` needs provisioning.
    #[test]
    fn needs_polyglot_provisioning_for_deno_without_generated() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), DENO_YAML);
        assert!(needs_polyglot_provisioning(dir.path()));
    }

    /// A Deno package whose `_generated_/` is already present loads as-is.
    #[test]
    fn no_polyglot_provisioning_when_deno_generated_present() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), DENO_YAML);
        std::fs::create_dir_all(dir.path().join("_generated_")).unwrap();
        assert!(!needs_polyglot_provisioning(dir.path()));
    }

    /// A Deno-only package also routes through `source_for_resolved_dir` when
    /// its `_generated_/` is missing — the codegen-provisioning analog of the
    /// Python-venv bug.
    #[test]
    fn resolved_dir_routes_unprovisioned_deno_to_build() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), DENO_YAML);
        let resolved = source_for_resolved_dir(&pkg_ref(), dir.path().to_path_buf());
        assert!(
            matches!(resolved, ResolvedSource::NeedsBuild(_)),
            "unprovisioned Deno-only package must route through materialize, got {resolved:?}"
        );
    }

    /// Rust-path non-regression: a Rust package (whose build decision is
    /// `needs_host_build`, not the polyglot path) must NOT be treated as
    /// needing polyglot provisioning — no venv/`_generated_` is ever expected
    /// for it. A schemas-only package likewise needs no provisioning.
    #[test]
    fn no_polyglot_provisioning_for_rust_or_schemas_only() {
        let rust = tempfile::tempdir().unwrap();
        manifest(rust.path(), RUST_YAML);
        std::fs::write(rust.path().join("Cargo.toml"), b"[package]\nname='rp'\n").unwrap();
        assert!(!needs_polyglot_provisioning(rust.path()));

        let schemas_only = tempfile::tempdir().unwrap();
        std::fs::write(
            schemas_only.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: s\n  version: 0.1.0\n",
        )
        .unwrap();
        assert!(!needs_polyglot_provisioning(schemas_only.path()));
    }

    // =====================================================================
    // fetch_remote_slpkg — fetch, integrity check, cache reuse
    // =====================================================================

    /// Restores `STREAMLIB_HOME` on drop so a sandboxed override doesn't
    /// leak across `#[serial]` tests.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: `#[serial]` makes these tests mutually exclusive.
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
            }
        }
    }
    fn sandbox_home(dir: &std::path::Path) -> HomeGuard {
        let prev = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", dir);
        }
        HomeGuard(prev)
    }

    fn file_url(path: &std::path::Path) -> String {
        format!("file://{}", path.display())
    }

    /// A `file://` fetch lands the bytes in the resolver cache, and a
    /// second fetch of the same URL reuses the cache even after the source
    /// disappears — proving the download is skipped. Revert the
    /// `target.exists()` early-return and the second fetch would try to
    /// re-read the deleted source and fail.
    #[test]
    #[serial_test::serial]
    fn fetch_file_url_caches_and_skips_redownload() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let src = tempfile::tempdir().unwrap();
        let slpkg = src.path().join("pkg.slpkg");
        std::fs::write(&slpkg, b"slpkg-bytes").unwrap();

        let url = file_url(&slpkg);
        let cached = fetch_remote_slpkg(&pkg_ref(), &url, None).expect("first fetch must succeed");
        assert_eq!(std::fs::read(&cached).unwrap(), b"slpkg-bytes");

        // Source gone — a cache hit must still resolve.
        std::fs::remove_file(&slpkg).unwrap();
        let cached2 = fetch_remote_slpkg(&pkg_ref(), &url, None)
            .expect("second fetch must hit the cache, not re-read the source");
        assert_eq!(cached, cached2);
        assert_eq!(std::fs::read(&cached2).unwrap(), b"slpkg-bytes");
    }

    /// A matching checksum passes; a mismatch fails loud with
    /// `IntegrityCheckFailed`. Revert the verify call and the mismatch
    /// would load silently.
    #[test]
    #[serial_test::serial]
    fn fetch_verifies_checksum_match_and_mismatch() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let src = tempfile::tempdir().unwrap();

        let good = src.path().join("good.slpkg");
        std::fs::write(&good, b"payload").unwrap();
        let good_sum = ArtifactChecksum::Sha256(sha256_hex(b"payload"));
        fetch_remote_slpkg(&pkg_ref(), &file_url(&good), Some(&good_sum))
            .expect("matching checksum must pass");

        let bad = src.path().join("bad.slpkg");
        std::fs::write(&bad, b"payload").unwrap();
        let wrong = ArtifactChecksum::Sha256("00".repeat(32));
        let err = fetch_remote_slpkg(&pkg_ref(), &file_url(&bad), Some(&wrong))
            .expect_err("mismatched checksum must fail loud");
        assert!(
            matches!(err, AddModuleError::IntegrityCheckFailed { .. }),
            "expected IntegrityCheckFailed, got {err:?}"
        );
    }

    /// An unsupported scheme fails loud rather than silently doing nothing.
    #[test]
    #[serial_test::serial]
    fn fetch_rejects_unsupported_scheme() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let err = fetch_remote_slpkg(&pkg_ref(), "ftp://example.com/x.slpkg", None)
            .expect_err("ftp scheme must be rejected");
        assert!(matches!(err, AddModuleError::UrlFetchFailed { .. }));
    }

    /// The blocking `http://` path downloads the bytes from a one-shot
    /// localhost server. Locks the ureq GET path end-to-end.
    #[test]
    #[serial_test::serial]
    fn fetch_http_url_downloads_bytes() {
        use std::io::{Read, Write};

        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());

        let body = b"http-slpkg-bytes".to_vec();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body_for_server = body.clone();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request headers (up to the blank line).
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body_for_server);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/pkg.slpkg");
        let cached = fetch_remote_slpkg(&pkg_ref(), &url, None).expect("http fetch must succeed");
        assert_eq!(std::fs::read(&cached).unwrap(), body);
        server.join().unwrap();
    }

    // =====================================================================
    // ActiveLinkedCheckout — link-aware resolution (npm-link semantics)
    // =====================================================================

    fn pkg_ref_named(name: &str) -> streamlib_idents::PackageRef {
        streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new(name).unwrap(),
        )
    }

    /// Build a fake linked checkout containing `packages/<dir_name>/streamlib.yaml`
    /// declaring `@tatolab/<pkg_name>`. `dir_name` and `pkg_name` differ only in
    /// the scan-fallback test.
    fn fake_checkout_with_package(dir_name: &str, pkg_name: &str) -> tempfile::TempDir {
        let checkout = tempfile::tempdir().unwrap();
        let pkg = checkout.path().join("packages").join(dir_name);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: {pkg_name}\n  version: 0.1.0\nprocessors:\n  \
                 - name: P\n    version: 1.0.0\n    description: d\n    runtime: python\n    \
                 execution: manual\n    entrypoint: \"p:P\"\n    inputs: []\n    outputs: []\n"
            ),
        )
        .unwrap();
        checkout
    }

    /// Write a link marker under `consumer_root/.streamlib/link.json` pointing at
    /// `checkout`, then discover it. The discovery walks up from `consumer_root`
    /// (no CWD dependency — hermetic).
    fn active_link_for(
        consumer_root: &std::path::Path,
        checkout: &std::path::Path,
    ) -> ActiveLinkedCheckout {
        let marker_dir = consumer_root.join(".streamlib");
        std::fs::create_dir_all(&marker_dir).unwrap();
        std::fs::write(
            marker_dir.join("link.json"),
            format!(
                r#"{{"checkout":"{c}","python_sdk_path":"{c}/sdk/streamlib-python","deno_sdk_entrypoint_path":"{c}/sdk/streamlib-deno/mod.ts","linked_at":"t","linked_crate_count":1,"state":"active","files":[]}}"#,
                c = checkout.display()
            ),
        )
        .unwrap();
        ActiveLinkedCheckout::discover_from(consumer_root)
            .expect("discovery must not error")
            .expect("an active link must be found")
    }

    fn assert_builds_from(resolved: ResolvedSource, expected_dir: &std::path::Path) {
        match resolved {
            ResolvedSource::NeedsBuild(req) => match req.source {
                BuildSource::PackageDir(dir) => assert_eq!(
                    dir, expected_dir,
                    "must build from the checkout package dir"
                ),
                other => panic!("expected PackageDir source, got {other:?}"),
            },
            other => panic!("expected NeedsBuild(checkout), got {other:?}"),
        }
    }

    /// CRUX (issue #1246): an active link redirects a `Strategy::Registry` load
    /// of a checkout-present package to the checkout, OVERRIDING the explicit
    /// registry strategy (npm-link semantics — a linked name takes precedence).
    /// Mentally revert the link short-circuit at the top of
    /// `resolve_strategy_to_source` and this resolves from the registry instead
    /// (RegistryNotConfigured), failing the assertion.
    #[test]
    fn active_link_overrides_registry_strategy_for_checkout_present_package() {
        let checkout = fake_checkout_with_package("foo", "foo");
        let consumer = tempfile::tempdir().unwrap();
        let link = active_link_for(consumer.path(), checkout.path());

        let resolved = resolve_strategy_to_source(
            &Strategy::Registry {
                version_req: SemVerRange::Any,
                build: BuildPolicy::IfStale,
            },
            &pkg_ref_named("foo"),
            Some(&link),
        )
        .expect("linked checkout-present package must resolve from the checkout, not error");
        assert_builds_from(resolved, &checkout.path().join("packages").join("foo"));
    }

    /// A package ABSENT from the checkout falls through to its declared strategy
    /// even under an active link — registry strategies stay available. Using
    /// `Strategy::Path` to a nonexistent dir gives an env-independent signal
    /// that the strategy dispatch (not the link branch) ran.
    #[test]
    fn active_link_leaves_absent_package_on_its_declared_strategy() {
        let checkout = fake_checkout_with_package("foo", "foo");
        let consumer = tempfile::tempdir().unwrap();
        let link = active_link_for(consumer.path(), checkout.path());

        let missing = consumer.path().join("nope");
        let err = resolve_strategy_to_source(
            &Strategy::Path {
                path: missing.clone(),
                build: BuildPolicy::IfStale,
            },
            &pkg_ref_named("bar"), // NOT in the checkout
            Some(&link),
        )
        .expect_err("absent-from-checkout package must use its declared strategy");
        assert!(
            matches!(err, AddModuleError::ManifestDirectoryMissing { .. }),
            "expected the Path strategy to run unchanged, got {err:?}"
        );
    }

    /// UNLINKED regression: with `link = None`, resolution is byte-for-byte the
    /// pre-change behavior — a checkout-present package name resolves from its
    /// declared strategy, NOT any checkout. Mentally revert nothing; this must
    /// pass on both the pre- and post-change code (the `None` path is untouched).
    #[test]
    fn no_link_resolution_is_unchanged() {
        // Same package name ("foo") that the linked test redirects — but with no
        // link, the strategy is authoritative. A nonexistent Path errors exactly
        // as before.
        let consumer = tempfile::tempdir().unwrap();
        let missing = consumer.path().join("nope");
        let err = resolve_strategy_to_source(
            &Strategy::Path {
                path: missing,
                build: BuildPolicy::IfStale,
            },
            &pkg_ref_named("foo"),
            None,
        )
        .expect_err("without a link the declared strategy is authoritative");
        assert!(
            matches!(err, AddModuleError::ManifestDirectoryMissing { .. }),
            "unlinked resolution must be unchanged, got {err:?}"
        );
    }

    /// The scan fallback matches a package whose directory name differs from its
    /// declared package name (match is by manifest org+name, not dir name).
    #[test]
    fn active_link_scan_fallback_matches_by_manifest_not_dir_name() {
        // dir = "weird-dir", but the manifest declares @tatolab/camera.
        let checkout = fake_checkout_with_package("weird-dir", "camera");
        let consumer = tempfile::tempdir().unwrap();
        let link = active_link_for(consumer.path(), checkout.path());

        let resolved = resolve_strategy_to_source(
            &Strategy::Registry {
                version_req: SemVerRange::Any,
                build: BuildPolicy::IfStale,
            },
            &pkg_ref_named("camera"),
            Some(&link),
        )
        .expect("scan fallback must find the package by manifest identity");
        assert_builds_from(
            resolved,
            &checkout.path().join("packages").join("weird-dir"),
        );
    }

    /// A corrupt link marker is a loud typed error at discovery, never a silent
    /// skip that would leave resolution in a mixed checkout/registry state.
    #[test]
    fn corrupt_link_marker_is_a_loud_error() {
        let consumer = tempfile::tempdir().unwrap();
        let marker_dir = consumer.path().join(".streamlib");
        std::fs::create_dir_all(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("link.json"), "{ not json at all").unwrap();

        let err = ActiveLinkedCheckout::discover_from(consumer.path())
            .expect_err("a corrupt marker must fail loudly");
        assert!(
            matches!(err, AddModuleError::LinkStateCorrupt { .. }),
            "expected LinkStateCorrupt, got {err:?}"
        );
    }

    /// No marker anywhere above the start dir ⇒ no active link ⇒ resolution is
    /// unaffected.
    #[test]
    fn no_marker_yields_no_active_link() {
        let empty = tempfile::tempdir().unwrap();
        let link =
            ActiveLinkedCheckout::discover_from(empty.path()).expect("no marker is not an error");
        assert!(link.is_none(), "no marker must yield no active link");
    }

    // =====================================================================
    // App-modules bridge — streamlib_modules/ wins over the installed cache
    // =====================================================================

    /// Write `<app_root>/streamlib_modules/@tatolab/<pkg_name>/streamlib.yaml`
    /// declaring `@tatolab/<declared_name>` (differ only in the mismatch test).
    /// Stages the on-disk Python layout (`pyproject.toml`) so the fixture trips
    /// the filesystem provisioning oracle — the realistic shape a Python
    /// `streamlib_modules` entry ships with.
    fn write_app_modules_package(
        app_root: &std::path::Path,
        pkg_name: &str,
        declared_name: &str,
    ) -> PathBuf {
        let dir = app_root
            .join(streamlib_idents::app_modules::APP_MODULES_DIR_NAME)
            .join("@tatolab")
            .join(pkg_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: {declared_name}\n  version: 0.9.0\n\
                 processors:\n  - name: P\n    version: 1.0.0\n    description: d\n    \
                 runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    \
                 inputs: []\n    outputs: []\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("pyproject.toml"), b"[project]\nname = \"p\"\n").unwrap();
        dir
    }

    /// Record `@tatolab/<name>` in the sandboxed installed cache and create
    /// its slot on disk. Returns the slot dir. Stages the on-disk Python layout
    /// (`pyproject.toml`) so the slot trips the filesystem provisioning oracle —
    /// the realistic shape an extracted Python `.slpkg` cache slot carries.
    fn record_installed_cache_package(name: &str) -> PathBuf {
        use crate::core::config::{InstalledPackageEntry, InstalledPackageManifest};
        let slot = crate::core::streamlib_home::get_cached_package_dir(&format!("{name}-1.0.0"));
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(
            slot.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: {name}\n  version: 1.0.0\n\
                 processors:\n  - name: P\n    version: 1.0.0\n    description: d\n    \
                 runtime: python\n    execution: manual\n    entrypoint: \"p:P\"\n    \
                 inputs: []\n    outputs: []\n"
            ),
        )
        .unwrap();
        std::fs::write(slot.join("pyproject.toml"), b"[project]\nname = \"p\"\n").unwrap();
        let mut manifest = InstalledPackageManifest::load().unwrap();
        manifest.add(InstalledPackageEntry {
            name: pkg_ref_named(name),
            version: streamlib_idents::SemVer::new(1, 0, 0),
            description: None,
            installed_from: "test".into(),
            installed_at: "t".into(),
            cache_dir: format!("{name}-1.0.0"),
        });
        manifest.save().unwrap();
        slot
    }

    /// The resolved package dir regardless of whether it loads directly
    /// (`Ready`) or routes through the orchestrator (`NeedsBuild`) — used by
    /// the precedence tests below, which assert *which* dir won independent of
    /// the build/provision decision. A Python-only fixture (the realistic
    /// installed-cache / `streamlib_modules` shape) routes to `NeedsBuild`
    /// because it needs its venv provisioned.
    fn resolved_dir(resolved: &ResolvedSource) -> PathBuf {
        match resolved {
            ResolvedSource::Ready(dir) => dir.clone(),
            ResolvedSource::NeedsBuild(req) => match &req.source {
                BuildSource::PackageDir(dir) => dir.clone(),
                other => panic!("expected PackageDir source, got {other:?}"),
            },
        }
    }

    /// CRUX (D7 bridge): with BOTH an app-modules entry and an installed-cache
    /// record present, `Strategy::InstalledCache` resolves the app-modules
    /// dir. Mentally revert the modules probe in
    /// `resolve_installed_cache_strategy` and this resolves the cache slot
    /// instead, failing the assertion. (The Python-only fixtures route to
    /// `NeedsBuild` — see [`resolved_dir`] — so the assertion is on the winning
    /// dir, not the `Ready`/`NeedsBuild` variant.)
    #[test]
    #[serial_test::serial]
    fn installed_cache_strategy_prefers_app_modules_dir() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        record_installed_cache_package("foo");
        let app_root = tempfile::tempdir().unwrap();
        let modules_dir = write_app_modules_package(app_root.path(), "foo", "foo");

        let resolved =
            resolve_installed_cache_strategy(&pkg_ref_named("foo"), Some(app_root.path()))
                .expect("must resolve");
        assert_eq!(
            resolved_dir(&resolved),
            modules_dir,
            "app modules must win over the installed cache"
        );
    }

    /// A package absent from `streamlib_modules/` falls through to the
    /// installed cache unchanged.
    #[test]
    #[serial_test::serial]
    fn installed_cache_strategy_falls_through_when_app_modules_absent() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let slot = record_installed_cache_package("foo");
        let app_root = tempfile::tempdir().unwrap(); // no streamlib_modules

        let resolved =
            resolve_installed_cache_strategy(&pkg_ref_named("foo"), Some(app_root.path()))
                .expect("must resolve from the installed cache");
        assert_eq!(resolved_dir(&resolved), slot);
    }

    /// A `streamlib_modules/@org/name` dir whose manifest declares a DIFFERENT
    /// package is skipped (warn + fall through), not loaded.
    #[test]
    #[serial_test::serial]
    fn installed_cache_strategy_skips_mismatched_app_modules_entry() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let slot = record_installed_cache_package("foo");
        let app_root = tempfile::tempdir().unwrap();
        // Dir named foo, but the manifest declares @tatolab/bar.
        write_app_modules_package(app_root.path(), "foo", "bar");

        let resolved =
            resolve_installed_cache_strategy(&pkg_ref_named("foo"), Some(app_root.path()))
                .expect("must fall through to the installed cache");
        assert_eq!(resolved_dir(&resolved), slot);
    }

    /// CRUX (bug path): a Python-only package resolved from the app's
    /// `streamlib_modules/` folder with no provisioned venv must resolve to
    /// `NeedsBuild` (source = the app-modules dir) so `materialize` provisions
    /// the venv at the cache slot it loads from — the exact
    /// `resolve_installed_cache_strategy` path that shipped the module as-is and
    /// left the subprocess spawn with `.venv/bin/python: No such file or
    /// directory`. Mentally revert the `needs_polyglot_provisioning` clause in
    /// `source_for_resolved_dir` and this resolves to `Ready`, failing
    /// `assert_builds_from`.
    #[test]
    #[serial_test::serial]
    fn installed_cache_strategy_routes_unprovisioned_python_app_module_to_build() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let app_root = tempfile::tempdir().unwrap();
        let modules_dir = write_app_modules_package(app_root.path(), "foo", "foo");

        let resolved =
            resolve_installed_cache_strategy(&pkg_ref_named("foo"), Some(app_root.path()))
                .expect("must resolve");
        assert_builds_from(resolved, &modules_dir);
    }

    /// Neither app modules nor installed cache ⇒ typed ModuleNotFound.
    #[test]
    #[serial_test::serial]
    fn installed_cache_strategy_module_not_found_when_neither_source_has_it() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let app_root = tempfile::tempdir().unwrap();
        let err = resolve_installed_cache_strategy(&pkg_ref_named("foo"), Some(app_root.path()))
            .expect_err("nothing to resolve");
        assert!(
            matches!(err, AddModuleError::ModuleNotFound { .. }),
            "expected ModuleNotFound, got {err:?}"
        );
    }

    /// Precedence: an active link outranks the app-modules folder — the link
    /// short-circuit runs before the InstalledCache arm ever probes
    /// `streamlib_modules/`.
    #[test]
    fn active_link_wins_over_app_modules_for_installed_cache_strategy() {
        let checkout = fake_checkout_with_package("foo", "foo");
        let consumer = tempfile::tempdir().unwrap();
        // The consumer ALSO has an app-modules entry for the same package.
        write_app_modules_package(consumer.path(), "foo", "foo");
        let link = active_link_for(consumer.path(), checkout.path());

        let resolved = resolve_strategy_to_source(
            &Strategy::InstalledCache,
            &pkg_ref_named("foo"),
            Some(&link),
        )
        .expect("linked checkout-present package must resolve from the checkout");
        assert_builds_from(resolved, &checkout.path().join("packages").join("foo"));
    }

    /// A LOCKED run ignores an active link (reproducible / offline by
    /// contract), while an unlocked run over the SAME marker discovers it.
    /// Mentally revert the `is_locked` gate in `discover_active_link_for_load`
    /// (always discover) and the locked case would return `Some`, failing the
    /// `is_none` assertion.
    #[test]
    fn locked_run_ignores_active_link() {
        let checkout = fake_checkout_with_package("foo", "foo");
        let consumer = tempfile::tempdir().unwrap();
        // Writes an active marker under `consumer`.
        active_link_for(consumer.path(), checkout.path());

        let locked = discover_active_link_for_load_from(true, consumer.path())
            .expect("locked discovery must not error");
        assert!(locked.is_none(), "a locked run must ignore the active link");

        let unlocked = discover_active_link_for_load_from(false, consumer.path())
            .expect("unlocked discovery must not error");
        assert!(
            unlocked.is_some(),
            "an unlocked run over the same marker must discover the link"
        );
    }
}
