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

use super::build_orchestrator::{BuildPolicy, BuildRequest, BuildSource};
use super::errors::AddModuleError;
use super::processor_registration::host_target_triple;
use super::slpkg::extract_slpkg_to_cache;

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
    /// Installed-package cache (`~/.streamlib/cache/packages/...`) only.
    /// Never builds. The default for bare [`Runner::add_module`] and for
    /// transitive registry-flavored deps.
    ///
    /// [`Runner::add_module`]: super::super::Runner::add_module
    InstalledCache,

    /// A directory containing `streamlib.yaml` plus per-language sources.
    /// `build` governs whether the orchestrator (re)builds before load.
    Path {
        path: PathBuf,
        build: BuildPolicy,
    },

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

    /// A remote archive/dir fetched over the wire. The engine performs
    /// no HTTP itself — the URL is handed to the orchestrator's
    /// [`BuildSource::Remote`] arm (a daemon / build-service impl
    /// handles it; the in-process default rejects it).
    Url {
        url: String,
        build: BuildPolicy,
    },
}

/// Outcome of resolving a [`Strategy`]: either a directory the engine
/// can load immediately, or a [`BuildRequest`] the orchestrator must
/// materialize first.
pub(super) enum ResolvedSource {
    /// A ready-to-load manifest directory (no build needed).
    Ready(PathBuf),
    /// Needs the injected orchestrator to materialize before load.
    NeedsBuild(BuildRequest),
}

/// Resolve a [`Strategy`] to a [`ResolvedSource`]. Pure source-location
/// logic (cache lookup, `.slpkg` extract, git checkout); never invokes a
/// build tool.
pub(super) fn resolve_strategy_to_source(
    strategy: &Strategy,
    pkg_ref: &streamlib_idents::PackageRef,
) -> std::result::Result<ResolvedSource, AddModuleError> {
    match strategy {
        Strategy::InstalledCache => {
            let (dir, _version) = lookup_installed_cache(pkg_ref)?
                .ok_or_else(|| AddModuleError::ModuleNotFound { package: pkg_ref.clone() })?;
            Ok(ResolvedSource::Ready(dir))
        }
        Strategy::Slpkg { path } => {
            let extracted = extract_slpkg_to_cache(path).map_err(|e| {
                AddModuleError::SlpkgExtractionFailed {
                    archive: path.clone(),
                    detail: e.to_string(),
                }
            })?;
            Ok(ResolvedSource::Ready(extracted))
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
        Strategy::Url { url, build } => {
            // The engine never performs HTTP itself (marketplace fetch
            // is deferred, and engine resolution must stay on the local
            // filesystem). Hand the URL to the orchestrator's Remote
            // arm; the in-process default rejects it, a build-service
            // impl handles it.
            Ok(ResolvedSource::NeedsBuild(BuildRequest {
                package: pkg_ref.clone(),
                source: BuildSource::Remote(url.clone()),
                policy: *build,
                host_triple: host_target_triple().to_string(),
            }))
        }
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

/// Fetch (or reuse a cached) git checkout for `pkg_ref` at `url`@`rev`
/// into `~/.streamlib/resolver-cache/`. Network I/O only — no build.
fn fetch_git_checkout(
    pkg_ref: &streamlib_idents::PackageRef,
    url: &str,
    rev: &str,
) -> std::result::Result<PathBuf, AddModuleError> {
    let cache_dir = crate::core::streamlib_home::get_streamlib_home().join("resolver-cache");
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
    manifest
        .package
        .as_ref()
        .map(|p| p.version)
        .ok_or_else(|| AddModuleError::StrategyManifestLoadFailed {
            source_path: dir.to_path_buf(),
            detail: "manifest has no `package:` block".into(),
        })
}
