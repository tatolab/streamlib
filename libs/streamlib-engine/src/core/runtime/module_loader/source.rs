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
        Strategy::Url { url, build, checksum } => {
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
///   prebuilt is present (no-op when there's nothing to build).
fn source_for_fetched_slpkg(
    pkg_ref: &streamlib_idents::PackageRef,
    dir: PathBuf,
    build: BuildPolicy,
) -> ResolvedSource {
    match build {
        BuildPolicy::NeverBuild => ResolvedSource::Ready(dir),
        BuildPolicy::IfStale => source_for_resolved_dir(pkg_ref, dir),
        BuildPolicy::AlwaysBuild => {
            if has_buildable_rust_source(&dir) {
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
    let cache_dir = crate::core::streamlib_home::get_streamlib_home()
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
        let resolved =
            source_for_fetched_slpkg(&pkg_ref(), dir.path().to_path_buf(), BuildPolicy::NeverBuild);
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
        let cached =
            fetch_remote_slpkg(&pkg_ref(), &url, None).expect("first fetch must succeed");
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
}
