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
            // Same prefer-prebuilt-else-build-source decision as `.slpkg`:
            // a cached package may carry source needing a host build.
            Ok(source_for_resolved_dir(pkg_ref, dir))
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

/// Decide how to load an already-resolved package directory (an extracted
/// `.slpkg` or an installed-cache entry) that may carry **source and/or a
/// prebuilt cdylib**. Prefer a prebuilt matching this host (compiler-free,
/// instant); otherwise build the bundled Rust source on the host. This is
/// the pip wheel-vs-sdist model for Rust: one artifact runs everywhere,
/// and a toolchain is needed only when there's no matching prebuilt.
fn source_for_resolved_dir(
    pkg_ref: &streamlib_idents::PackageRef,
    dir: PathBuf,
) -> ResolvedSource {
    if needs_host_build(&dir) {
        ResolvedSource::NeedsBuild(BuildRequest {
            package: pkg_ref.clone(),
            source: BuildSource::PackageDir(dir),
            // No explicit policy on these arms — a build is required only
            // because the prebuilt is absent, so `IfStale` (build iff the
            // cdylib isn't already staged) is the right semantics.
            policy: BuildPolicy::IfStale,
            host_triple: host_target_triple().to_string(),
        })
    } else {
        ResolvedSource::Ready(dir)
    }
}

/// Whether a resolved package dir needs an on-host Rust build before it
/// can load: it declares Rust processors, has **no** prebuilt cdylib for
/// this host triple, and carries `Cargo.toml` to build from. A package
/// with a matching prebuilt (or no Rust at all) loads as-is; a Rust
/// package with neither prebuilt nor source loads as-is and fails loud at
/// dlopen (no artifact, nothing to build).
fn needs_host_build(dir: &std::path::Path) -> bool {
    use streamlib_processor_schema::ProcessorLanguage;
    let config = match crate::core::config::ProjectConfig::load(dir) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let has_rust = config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust));
    if !has_rust {
        return false;
    }
    let triple_dir = dir.join("lib").join(host_target_triple());
    let has_prebuilt = std::fs::read_dir(&triple_dir)
        .map(|it| {
            it.flatten()
                .any(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        })
        .unwrap_or(false);
    if has_prebuilt {
        return false;
    }
    dir.join("Cargo.toml").exists()
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
}
