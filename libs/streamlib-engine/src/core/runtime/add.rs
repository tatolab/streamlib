// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `add` / `remove` — single-package adoption, the `npm install <pkg>` of
//! streamlib.
//!
//! [`add`] takes ONE published package from "exists in the registry" to
//! "usable by this runtime" in one step: resolve the caller's semver range to
//! a concrete version from the registry, materialize that version into the
//! installed-package cache the runtime reads from, and record it in the
//! installed-set record (`packages.yaml`, [`InstalledPackageManifest`]).
//! Afterward a bare `Runner::add_module(ident)` ([`Strategy::InstalledCache`])
//! finds the package offline. [`add`] returns a catalog-backed
//! [`AddReport`] — the processors the package contributes and their typed
//! input/output ports, read from the registry catalog artifacts (not by
//! opening the archive).
//!
//! [`remove`] is the inverse: un-record the package from `packages.yaml` and
//! evict its cache slot.
//!
//! This is deliberately NOT [`install`](super::install): `install` resolves +
//! locks a whole application *tree* (the app's `streamlib.yaml` →
//! `streamlib-app.lock`). `add` touches neither app code, nor any app
//! manifest, nor an application lockfile — it only mutates the local
//! installed-set the runtime resolves bare `add_module` calls against.
//!
//! [`Strategy::InstalledCache`]: super::Strategy::InstalledCache
//! [`InstalledPackageManifest`]: crate::core::config::InstalledPackageManifest

use std::path::{Path, PathBuf};

use streamlib_idents::{
    select_version, CatalogClient, Manifest, PackageCatalog, PackageRef, RegistryClient,
    RegistryConfig, ResolverError, SemVer, SemVerRange, DEFAULT_REGISTRY_URL,
};

use super::module_loader::host_target_triple;
use super::{BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest, BuildSource};
use crate::core::config::{InstalledPackageEntry, InstalledPackageManifest};
use crate::core::streamlib_home::{
    get_cached_package_dir, get_cached_package_dir_for_name_version, get_streamlib_data_dir,
};

/// Knobs for [`add`]. Defaults are the ordinary "add this package" posture —
/// resolve the registry with the zero-env default fallback, extract into the
/// shared resolver cache, materialize with [`BuildPolicy::AlwaysBuild`].
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    /// Registry to resolve from. Resolution order: this field, then
    /// [`RegistryConfig::from_env`], then [`DEFAULT_REGISTRY_URL`]. So [`add`]
    /// works with **zero environment variables** (it hits the first-party
    /// default tree) or a `file://` override for tests / local mirrors. This is
    /// the interim bridge until a persistent `streamlib registry use` config
    /// lands.
    pub registry: Option<RegistryConfig>,
    /// Build policy for the materialize. Defaults to
    /// [`BuildPolicy::AlwaysBuild`] — a freshly-downloaded registry `.slpkg`
    /// is source-only, so the host build always runs.
    pub materialize_policy: Option<BuildPolicy>,
}

/// Outcome of a successful [`add`].
#[derive(Debug, Clone)]
pub struct AddReport {
    /// The canonical `@org/name` that was added.
    pub package: PackageRef,
    /// The concrete version the range resolved to.
    pub version: SemVer,
    /// `true` when the package was already recorded at exactly this version —
    /// the add was a no-op materialize (idempotent). The catalog summary is
    /// still fetched and returned.
    pub already_present: bool,
    /// The installed-package cache slot the package now lives in
    /// (`cache/packages/<name>-<version>`).
    pub cache_dir: PathBuf,
    /// Catalog-backed summary of the package's processors + typed ports, read
    /// from the registry catalog artifacts. `None` when the tree publishes no
    /// catalog (a `pkg publish`-only tree) — the add still succeeds; discovery
    /// simply degrades to "no catalog metadata".
    pub catalog: Option<PackageCatalog>,
}

/// Per-failure-mode error from [`add`].
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    /// Listing the package's published versions, or selecting one satisfying
    /// the requested range, failed. Wraps [`ResolverError`] — in particular
    /// [`ResolverError::RegistryNoMatchingVersion`], which names the available
    /// versions.
    #[error("resolving '{package}' from the registry failed: {source}")]
    Resolve {
        package: PackageRef,
        #[source]
        source: ResolverError,
    },

    /// Downloading the resolved version's `.slpkg` failed.
    #[error("downloading '{package}' v{version} failed: {source}")]
    Download {
        package: PackageRef,
        version: SemVer,
        #[source]
        source: ResolverError,
    },

    /// Persisting the downloaded `.slpkg` bytes into the resolver cache failed.
    #[error("persisting the downloaded .slpkg for '{package}' failed: {detail}")]
    Persist { package: PackageRef, detail: String },

    /// Extracting the `.slpkg` archive into the package cache failed.
    #[error("extracting {} failed: {detail}", archive.display())]
    Extract { archive: PathBuf, detail: String },

    /// Promoting the temp-extracted package into its cache slot (the atomic
    /// rename that runs only after the identity check passes) failed.
    #[error(
        "promoting the extracted package for '{package}' into its cache slot {} failed: {detail}",
        cache_dir.display()
    )]
    Promote {
        package: PackageRef,
        cache_dir: PathBuf,
        detail: String,
    },

    /// The materialized package's `streamlib.yaml` failed to parse or lacked a
    /// `package:` block, so its identity/metadata couldn't be read.
    #[error("reading the materialized manifest at {} failed: {detail}", dir.display())]
    ManifestRead { dir: PathBuf, detail: String },

    /// The registry served a `.slpkg` whose `package:` block declares a
    /// different identity than the one requested (its store location and its
    /// manifest disagree — a mis-published package). Fail loud instead of
    /// recording a divergent identity in `packages.yaml`.
    #[error(
        "identity mismatch: requested '{requested}' v{requested_version} but the \
         downloaded package declares '{found}' v{found_version}"
    )]
    IdentityMismatch {
        requested: PackageRef,
        requested_version: SemVer,
        found: PackageRef,
        found_version: SemVer,
    },

    /// The injected [`BuildOrchestrator`] failed to materialize the package.
    #[error("materializing '{package}' failed: {source}")]
    Materialize {
        package: PackageRef,
        #[source]
        source: super::BuildError,
    },

    /// Loading the installed-package manifest (`packages.yaml`) failed.
    #[error("loading the installed-package manifest failed: {detail}")]
    LoadManifest { detail: String },

    /// Saving the installed-package manifest (`packages.yaml`) failed.
    #[error("saving the installed-package manifest failed: {detail}")]
    SaveManifest { detail: String },
}

/// Outcome of a successful [`remove`].
#[derive(Debug, Clone)]
pub struct RemoveReport {
    /// The canonical `@org/name` that was removed.
    pub package: PackageRef,
    /// The version the removed entry was recorded at.
    pub version: SemVer,
    /// The cache slot that was evicted (`cache/packages/<name>-<version>`).
    pub cache_dir: PathBuf,
    /// `true` when the cache slot existed on disk and was deleted; `false`
    /// when the record existed but the slot was already gone.
    pub cache_dir_removed: bool,
}

/// Per-failure-mode error from [`remove`].
#[derive(Debug, thiserror::Error)]
pub enum RemoveError {
    /// No installed-package record matches `@org/name` — nothing to remove.
    #[error("'{package}' is not installed")]
    NotInstalled { package: PackageRef },

    /// Loading the installed-package manifest (`packages.yaml`) failed.
    #[error("loading the installed-package manifest failed: {detail}")]
    LoadManifest { detail: String },

    /// Evicting the package's cache slot failed.
    #[error("evicting the cache slot {} for '{package}' failed: {detail}", cache_dir.display())]
    EvictCache {
        package: PackageRef,
        cache_dir: PathBuf,
        detail: String,
    },

    /// Saving the installed-package manifest (`packages.yaml`) failed.
    #[error("saving the installed-package manifest failed: {detail}")]
    SaveManifest { detail: String },
}

/// Add ONE published package by semver range — resolve range→concrete from the
/// registry, materialize into the installed-package cache, record in
/// `packages.yaml`, and return a catalog-backed summary.
///
/// This does NOT touch app code, any app `streamlib.yaml`, or an application
/// lockfile — it only mutates the local installed-set. Re-adding a package
/// already recorded at the resolved version is idempotent
/// ([`AddReport::already_present`] is `true`, no re-materialize).
#[tracing::instrument(skip(orchestrator, sink, options), fields(package = %pkg_ref, req = %version_req))]
pub fn add(
    pkg_ref: &PackageRef,
    version_req: &SemVerRange,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    options: &AddOptions,
) -> std::result::Result<AddReport, AddError> {
    let registry = resolve_registry(options);
    tracing::info!(registry = %registry.base_url, "add: resolving package from registry");

    let client = RegistryClient::new(&registry);
    let available = client
        .list_versions(pkg_ref)
        .map_err(|source| AddError::Resolve {
            package: pkg_ref.clone(),
            source,
        })?;
    let selected =
        select_version(pkg_ref, version_req, &available).map_err(|source| AddError::Resolve {
            package: pkg_ref.clone(),
            source,
        })?;
    tracing::info!(version = %selected, "add: selected version");

    // Idempotency: an entry already pinned to exactly the selected version whose
    // cache slot still exists on disk is a no-op materialize. The `is_dir` guard
    // is self-healing — a recorded-but-evicted slot (someone `rm -rf`'d the
    // cache) falls through and re-materializes rather than reporting a present
    // package whose slot is gone. Reuse fetches the catalog so the caller gets
    // the same summary shape as a fresh add.
    let existing_slot = InstalledPackageManifest::load()
        .map_err(|e| AddError::LoadManifest {
            detail: e.to_string(),
        })?
        .find_by_ref(pkg_ref)
        .filter(|e| e.version == selected)
        .map(|e| get_cached_package_dir(&e.cache_dir))
        .filter(|dir| dir.is_dir());

    let (cache_dir, already_present) = if let Some(dir) = existing_slot {
        tracing::info!(version = %selected, "add: already present at selected version — skipping materialize");
        (dir, true)
    } else {
        let policy = options.materialize_policy.unwrap_or(BuildPolicy::AlwaysBuild);
        let extracted = download_and_extract(&client, pkg_ref, selected)?;
        // The registry download path is keyed by the requested `pkg_ref` + the
        // selected version, so the materialized package's own manifest MUST
        // declare that same identity — reconcile so a mis-published `.slpkg`
        // (its `package:` block disagreeing with its store location) fails loud
        // instead of silently recording a divergent identity. The identity
        // check gates promotion of the temp extraction into the cache slot, so
        // a mismatch also leaves no orphaned slot behind.
        let (_ref, _version, staged_dir) = materialize_and_record(
            extracted,
            orchestrator,
            sink,
            policy,
            format!("registry:{}", registry.base_url),
            Some((pkg_ref, selected)),
        )?;
        (staged_dir, false)
    };

    let catalog = fetch_catalog(&registry, pkg_ref, selected);

    Ok(AddReport {
        package: pkg_ref.clone(),
        version: selected,
        already_present,
        cache_dir,
        catalog,
    })
}

/// Add ONE package from a local `.slpkg` archive (a hand-off bundle), rather
/// than resolving it by version from the registry. Extracts, materializes, and
/// records it exactly like [`add`], deriving the identity + version from the
/// archive's own manifest. There is no catalog for a local artifact (the
/// catalog lives in the registry tree), so [`AddReport::catalog`] is `None`.
#[tracing::instrument(skip(orchestrator, sink), fields(archive = %archive.display()))]
pub fn add_slpkg(
    archive: &Path,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
) -> std::result::Result<AddReport, AddError> {
    let extracted = extract_slpkg_to_temp(archive)?;
    // A local artifact has no external identity to reconcile against — its
    // `package:` block IS the identity — so no `expected` check.
    let (package, version, cache_dir) = materialize_and_record(
        extracted,
        orchestrator,
        sink,
        // A `.slpkg` may already carry a matching prebuilt; prefer it, else
        // build the bundled source (identical to `Strategy::Slpkg`).
        BuildPolicy::IfStale,
        format!("slpkg:{}", archive.display()),
        None,
    )?;
    Ok(AddReport {
        package,
        version,
        already_present: false,
        cache_dir,
        catalog: None,
    })
}

/// Remove ONE installed package: un-record it from `packages.yaml` and evict
/// its cache slot. Errors with [`RemoveError::NotInstalled`] when no record
/// matches.
#[tracing::instrument(fields(package = %pkg_ref))]
pub fn remove(pkg_ref: &PackageRef) -> std::result::Result<RemoveReport, RemoveError> {
    let mut manifest =
        InstalledPackageManifest::load().map_err(|e| RemoveError::LoadManifest {
            detail: e.to_string(),
        })?;
    let entry = manifest
        .remove_by_ref(pkg_ref)
        .ok_or_else(|| RemoveError::NotInstalled {
            package: pkg_ref.clone(),
        })?;

    let cache_dir = get_cached_package_dir(&entry.cache_dir);
    let cache_dir_removed = if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir).map_err(|e| RemoveError::EvictCache {
            package: pkg_ref.clone(),
            cache_dir: cache_dir.clone(),
            detail: e.to_string(),
        })?;
        true
    } else {
        false
    };

    manifest.save().map_err(|e| RemoveError::SaveManifest {
        detail: e.to_string(),
    })?;

    tracing::info!(version = %entry.version, removed_slot = cache_dir_removed, "remove: package un-recorded");
    Ok(RemoveReport {
        package: pkg_ref.clone(),
        version: entry.version,
        cache_dir,
        cache_dir_removed,
    })
}

/// Resolve the registry to use: an explicit `options.registry`, else the
/// environment ([`RegistryConfig::from_env`]), else the first-party
/// [`DEFAULT_REGISTRY_URL`]. Unlike [`install`](super::install) and
/// [`Strategy::Registry`](super::Strategy::Registry) (which fail loud when no
/// registry is configured), `add` defaults to the first-party tree so the bare
/// `streamlib add @org/name` works with zero configuration.
fn resolve_registry(options: &AddOptions) -> RegistryConfig {
    options
        .registry
        .clone()
        .or_else(RegistryConfig::from_env)
        .unwrap_or_else(|| RegistryConfig {
            base_url: DEFAULT_REGISTRY_URL.to_string(),
        })
}

/// Download the selected version's `.slpkg`, persist it into the resolver
/// cache, and extract it into a temp staging dir (promoted into the cache slot
/// by [`materialize_and_record`] only after the identity check passes).
/// Returns the temp dir.
fn download_and_extract(
    client: &RegistryClient<'_>,
    pkg_ref: &PackageRef,
    version: SemVer,
) -> std::result::Result<PathBuf, AddError> {
    let (bytes, url) = client
        .download_slpkg(pkg_ref, version)
        .map_err(|source| AddError::Download {
            package: pkg_ref.clone(),
            version,
            source,
        })?;
    tracing::debug!(%url, bytes = bytes.len(), "add: downloaded .slpkg");
    let archive = persist_slpkg(pkg_ref, version, &bytes)?;
    extract_slpkg_to_temp(&archive)
}

/// Persist downloaded `.slpkg` bytes into
/// `<STREAMLIB_HOME>/.streamlib/resolver-cache/add/` so
/// [`extract_slpkg_to_temp`] can read them, with an atomic temp-then-rename
/// publish.
fn persist_slpkg(
    pkg_ref: &PackageRef,
    version: SemVer,
    bytes: &[u8],
) -> std::result::Result<PathBuf, AddError> {
    let dir = get_streamlib_data_dir().join("resolver-cache").join("add");
    let persist_err = |detail: String| AddError::Persist {
        package: pkg_ref.clone(),
        detail,
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| persist_err(format!("creating {} : {e}", dir.display())))?;
    let target = dir.join(format!("{}-{}.slpkg", pkg_ref.name.as_str(), version));
    let tmp = dir.join(format!("{}-{}.slpkg.partial", pkg_ref.name.as_str(), version));
    std::fs::write(&tmp, bytes).map_err(|e| persist_err(format!("writing {} : {e}", tmp.display())))?;
    std::fs::rename(&tmp, &target)
        .map_err(|e| persist_err(format!("publishing {} : {e}", target.display())))?;
    Ok(target)
}

/// Extract a `.slpkg` archive into a fresh temp staging dir under the
/// package-cache parent — the same directory the final cache slots live in, so
/// [`materialize_and_record`]'s later promotion is an atomic same-filesystem
/// rename. The temp dir is NOT a cache slot; it is promoted into
/// `cache/packages/<name>-<version>` only after the identity check passes, so a
/// mis-published `.slpkg` never leaves an orphaned slot. The partial extraction
/// is cleaned up on error.
fn extract_slpkg_to_temp(archive: &Path) -> std::result::Result<PathBuf, AddError> {
    let temp_dir = temp_stage_dir();
    super::module_loader::extract_slpkg_to_dir(archive, &temp_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&temp_dir);
        AddError::Extract {
            archive: archive.to_path_buf(),
            detail: e.to_string(),
        }
    })?;
    Ok(temp_dir)
}

/// A unique temp staging directory under `cache/packages/` — pid + a
/// process-local counter keep concurrent adds (including two in the same
/// process) from colliding on the staging path.
fn temp_stage_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);
    get_cached_package_dir(&format!(
        ".tmp-add-{}-{}",
        std::process::id(),
        STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Promote a temp-extracted package dir into its final cache slot with an
/// atomic rename, clearing any pre-existing slot first. Cleans up the temp dir
/// on failure so a promote error never strands it. Called only after the
/// identity check has passed, so writing `cache/packages/<name>-<version>` is
/// gated solely by that check — a rejected package leaves no cache slot.
fn promote_extracted_dir(
    temp_dir: &Path,
    cache_slot: &Path,
    package: &PackageRef,
) -> std::result::Result<(), AddError> {
    let promote_err = |detail: String| {
        let _ = std::fs::remove_dir_all(temp_dir);
        AddError::Promote {
            package: package.clone(),
            cache_dir: cache_slot.to_path_buf(),
            detail,
        }
    };
    if cache_slot.exists() {
        std::fs::remove_dir_all(cache_slot)
            .map_err(|e| promote_err(format!("clearing the existing slot: {e}")))?;
    }
    if let Some(parent) = cache_slot.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            promote_err(format!("creating the cache parent {}: {e}", parent.display()))
        })?;
    }
    std::fs::rename(temp_dir, cache_slot)
        .map_err(|e| promote_err(format!("renaming {} into place: {e}", temp_dir.display())))?;
    Ok(())
}

/// Reconcile a temp-extracted package dir's identity, promote it into its cache
/// slot, materialize it through the orchestrator, and record it in
/// `packages.yaml`. Reads the package's identity + metadata from its own
/// `streamlib.yaml` (the authoritative source for what was materialized).
///
/// When `expected` is `Some`, the manifest's identity must match it or
/// [`AddError::IdentityMismatch`] is returned **before** the extraction is
/// promoted into the cache — the registry path passes the requested
/// `(pkg_ref, version)` so a mis-published `.slpkg` can't silently record a
/// divergent identity, and the failed add leaves no orphan slot (the temp dir
/// is removed). A local `.slpkg` passes `None` (its own manifest IS the
/// identity). On a version-upgrade re-add the superseded version's cache slot
/// is evicted after the new record is durable. Returns
/// `(package, version, staged_dir)`.
fn materialize_and_record(
    temp_extracted_dir: PathBuf,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    policy: BuildPolicy,
    installed_from: String,
    expected: Option<(&PackageRef, SemVer)>,
) -> std::result::Result<(PackageRef, SemVer, PathBuf), AddError> {
    let meta = Manifest::load(&temp_extracted_dir)
        .map_err(|e| AddError::ManifestRead {
            dir: temp_extracted_dir.clone(),
            detail: e.to_string(),
        })?
        .package
        .ok_or_else(|| AddError::ManifestRead {
            dir: temp_extracted_dir.clone(),
            detail: "manifest has no `package:` block".into(),
        })?;
    let package = PackageRef::new(meta.org.clone(), meta.name.clone());
    let version = meta.version;

    // Identity gate — the sole promotion gate. A mismatch removes the temp
    // extraction and returns before anything lands in the cache slot.
    if let Some((expected_ref, expected_version)) = expected {
        if &package != expected_ref || version != expected_version {
            let _ = std::fs::remove_dir_all(&temp_extracted_dir);
            return Err(AddError::IdentityMismatch {
                requested: expected_ref.clone(),
                requested_version: expected_version,
                found: package,
                found_version: version,
            });
        }
    }

    // Promote the temp extraction into the package's cache slot now that the
    // identity check has passed.
    let cache_slot = get_cached_package_dir_for_name_version(package.name.as_str(), version);
    promote_extracted_dir(&temp_extracted_dir, &cache_slot, &package)?;

    let request = BuildRequest {
        package: package.clone(),
        source: BuildSource::PackageDir(cache_slot),
        policy,
        host_triple: host_target_triple().to_string(),
    };
    let staged = orchestrator
        .materialize(&request, sink)
        .map_err(|source| AddError::Materialize {
            package: package.clone(),
            source,
        })?;

    let cache_key = staged
        .staged_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Record the new entry, then evict any superseded version's cache slot. A
    // version-upgrade re-add replaces the `packages.yaml` entry (`add` replaces
    // same-ref), so the previous version's slot would otherwise linger on disk.
    // The evict is conditional on a *different* slot key so a same-version
    // re-materialize (the evicted-slot self-heal path) never deletes the slot
    // it just promoted.
    let mut manifest =
        InstalledPackageManifest::load().map_err(|e| AddError::LoadManifest {
            detail: e.to_string(),
        })?;
    let superseded_slot = manifest
        .find_by_ref(&package)
        .filter(|e| e.cache_dir != cache_key)
        .map(|e| get_cached_package_dir(&e.cache_dir));
    manifest.add(InstalledPackageEntry {
        name: package.clone(),
        version,
        description: meta.description.clone(),
        installed_from,
        installed_at: super::install::rfc3339_utc_now(),
        cache_dir: cache_key,
    });
    manifest.save().map_err(|e| AddError::SaveManifest {
        detail: e.to_string(),
    })?;

    if let Some(old_slot) = superseded_slot {
        if old_slot.is_dir() {
            match std::fs::remove_dir_all(&old_slot) {
                Ok(()) => tracing::info!(
                    package = %package,
                    slot = %old_slot.display(),
                    "add: evicted superseded version cache slot"
                ),
                // Disk hygiene only — the new version is already recorded and
                // durable, so a failed evict must not fail the add.
                Err(e) => tracing::warn!(
                    package = %package,
                    slot = %old_slot.display(),
                    error = %e,
                    "add: failed to evict superseded version cache slot (leaving it in place)"
                ),
            }
        }
    }

    Ok((package, version, staged.staged_dir))
}

/// Fetch the catalog summary for `(pkg_ref, version)` from the registry tree.
/// A missing catalog (or a transport error) degrades to `None` — an add never
/// fails because discovery metadata is absent.
fn fetch_catalog(
    registry: &RegistryConfig,
    pkg_ref: &PackageRef,
    version: SemVer,
) -> Option<PackageCatalog> {
    let client = CatalogClient::new(registry.base_url.clone(), None);
    match client.fetch_package_catalog(pkg_ref, &version) {
        Ok(catalog) => catalog,
        Err(e) => {
            tracing::warn!(
                package = %pkg_ref,
                %version,
                error = %e,
                "add: catalog fetch failed — reporting no catalog metadata"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{Org, Package};

    // ---------------------------------------------------------------------
    // Test harness: a mock orchestrator + a hand-built `file://` scratch tree.
    //
    // The mock orchestrator makes the extracted cache slot the staged dir
    // WITHOUT compiling anything — the add flow (resolve → materialize →
    // record → catalog → offline InstalledCache resolve) is exercised without
    // a real Rust/Python build. `#[serial]` + a per-test `STREAMLIB_HOME`
    // tempdir sandbox isolates `packages.yaml` and the cache between tests.
    // ---------------------------------------------------------------------

    /// Records the extracted slot as staged without building — enough to drive
    /// the full add flow deterministically in-process.
    struct MockOrchestrator;
    impl BuildOrchestrator for MockOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<super::super::StagedArtifact, super::super::BuildError> {
            let dir = match &request.source {
                BuildSource::PackageDir(p) => p.clone(),
                other => {
                    return Err(super::super::BuildError::UnsupportedSource(format!("{other:?}")))
                }
            };
            Ok(super::super::StagedArtifact {
                staged_dir: dir,
                rebuilt: true,
            })
        }
    }

    struct NullSink;
    impl BuildEventSink for NullSink {
        fn emit(&self, _event: super::super::BuildEvent) {}
    }

    /// Restores `STREAMLIB_HOME` on drop so a sandboxed override doesn't leak
    /// across `#[serial]` tests.
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
    fn sandbox_home(dir: &Path) -> HomeGuard {
        let prev = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", dir);
        }
        HomeGuard(prev)
    }

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    fn manifest_yaml(org: &str, name: &str, version: &str) -> String {
        format!(
            "package:\n  org: {org}\n  name: {name}\n  version: {version}\n  \
             description: a test package\nprocessors:\n  - name: Foo\n    version: 1.0.0\n    \
             description: does foo\n    runtime: rust\n    execution: manual\n    inputs: []\n    \
             outputs: []\n"
        )
    }

    /// Build a minimal source-only `.slpkg` (a zip with `streamlib.yaml`) at
    /// `dest`. `extract_slpkg_to_cache` reads the manifest from it to derive
    /// the cache slot, so a real archive is required (not just a stub file).
    fn write_slpkg(dest: &Path, org: &str, name: &str, version: &str) {
        let file = std::fs::File::create(dest).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts =
            zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("streamlib.yaml", opts).unwrap();
        std::io::Write::write_all(&mut zip, manifest_yaml(org, name, version).as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    /// Populate a `file://` registry tree with the package's `.slpkg` at each
    /// version under `slpkg/<name>/<version>/<name>.slpkg`. Optionally write a
    /// `.catalog.json` for the given `catalog_version`.
    fn scratch_tree(
        tree: &Path,
        org: &str,
        name: &str,
        versions: &[&str],
        catalog_version: Option<&str>,
    ) {
        for v in versions {
            let dir = tree.join("slpkg").join(name).join(v);
            std::fs::create_dir_all(&dir).unwrap();
            write_slpkg(&dir.join(format!("{name}.slpkg")), org, name, v);
        }
        if let Some(cv) = catalog_version {
            let catalog = PackageCatalog {
                package: pkg_ref(org, name),
                version: cv.parse().unwrap(),
                processors: vec![streamlib_idents::CatalogProcessor {
                    name: "Foo".into(),
                    description: Some("does foo".into()),
                    runtime: streamlib_idents::CatalogRuntime::Rust,
                    entrypoint: None,
                    config: None,
                    inputs: vec![streamlib_idents::CatalogPort {
                        name: "in".into(),
                        description: None,
                        schema: streamlib_idents::CatalogSchemaRef::Any,
                        read_mode: None,
                    }],
                    outputs: vec![streamlib_idents::CatalogPort {
                        name: "out".into(),
                        description: None,
                        schema: streamlib_idents::CatalogSchemaRef::Schema(
                            streamlib_idents::SchemaIdent::new(
                                Org::new(org).unwrap(),
                                Package::new(name).unwrap(),
                                streamlib_idents::TypeName::new("FooFrame").unwrap(),
                                cv.parse().unwrap(),
                            ),
                        ),
                        read_mode: None,
                    }],
                }],
            };
            let dir = tree.join("slpkg").join(name).join(cv);
            std::fs::write(
                dir.join(format!("{name}.catalog.json")),
                serde_json::to_vec_pretty(&catalog).unwrap(),
            )
            .unwrap();
        }
    }

    fn file_registry(tree: &Path) -> RegistryConfig {
        RegistryConfig {
            base_url: format!("file://{}", tree.display()),
        }
    }

    fn opts(tree: &Path) -> AddOptions {
        AddOptions {
            registry: Some(file_registry(tree)),
            materialize_policy: None,
        }
    }

    #[test]
    #[serial_test::serial]
    fn add_selects_highest_records_and_resolves_offline() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.0.0", "1.1.0"], Some("1.1.0"));

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let report = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path()))
            .expect("add must succeed");

        // Highest satisfying version selected.
        assert_eq!(report.version, SemVer::new(1, 1, 0));
        assert!(!report.already_present);
        // Materialized into `cache/packages/foo-1.1.0/`.
        assert!(report.cache_dir.ends_with("cache/packages/foo-1.1.0"));
        assert!(report.cache_dir.join("streamlib.yaml").is_file());

        // Recorded in packages.yaml under the canonical ref — AND the recorded
        // slot exists on disk with its manifest. This is exactly what the
        // `Strategy::InstalledCache` resolver's `lookup_installed_cache` step
        // reads (`find_by_ref(pkg_ref).map(|e| get_cached_package_dir(&e.cache_dir))`),
        // so it locks the offline-resolve contract without reaching into that
        // private resolver. Mentally-revert `manifest.save()` in
        // `materialize_and_record` and `find_by_ref` returns `None` here — the
        // InstalledCache resolve would then miss with `ModuleNotFound`.
        let manifest = InstalledPackageManifest::load().unwrap();
        let entry = manifest.find_by_ref(&pr).expect("recorded");
        assert_eq!(entry.version, SemVer::new(1, 1, 0));
        assert_eq!(entry.cache_dir, "foo-1.1.0");
        let offline_slot = get_cached_package_dir(&entry.cache_dir);
        assert!(
            offline_slot.join("streamlib.yaml").is_file(),
            "the offline InstalledCache lookup must land a loadable slot"
        );
    }

    #[test]
    #[serial_test::serial]
    fn add_unsatisfiable_range_names_available_versions() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.0.0", "1.1.0"], None);

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^2.0.0").unwrap();
        let err = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path()))
            .expect_err("^2 matches nothing");
        match err {
            AddError::Resolve {
                source: ResolverError::RegistryNoMatchingVersion { available, .. },
                ..
            } => {
                assert!(available.contains("1.0.0"), "available: {available}");
                assert!(available.contains("1.1.0"), "available: {available}");
            }
            other => panic!("expected Resolve(RegistryNoMatchingVersion), got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn add_is_idempotent_at_same_version() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.1.0"], Some("1.1.0"));

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let first = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        assert!(!first.already_present);

        let second = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        assert!(second.already_present, "re-add at same version must be a no-op");
        assert_eq!(second.version, SemVer::new(1, 1, 0));
        // Still exactly one entry — `add` replaces same-ref, never duplicates.
        let manifest = InstalledPackageManifest::load().unwrap();
        assert_eq!(manifest.packages.iter().filter(|e| e.name == pr).count(), 1);
        // Catalog still surfaced on the idempotent path.
        assert!(second.catalog.is_some());
    }

    #[test]
    #[serial_test::serial]
    fn add_surfaces_catalog_processors_and_ports() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.1.0"], Some("1.1.0"));

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let report = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();

        // Mentally-revert the `fetch_catalog` call (return None) and this
        // assertion fails — it locks that the summary is actually read.
        let catalog = report.catalog.expect("catalog present on a catalog-carrying tree");
        assert_eq!(catalog.processors.len(), 1);
        let proc = &catalog.processors[0];
        assert_eq!(proc.name, "Foo");
        assert_eq!(proc.inputs.len(), 1);
        assert_eq!(proc.outputs.len(), 1);
        assert_eq!(proc.outputs[0].name, "out");
    }

    #[test]
    #[serial_test::serial]
    fn add_degrades_gracefully_without_catalog() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        // No catalog written — a `pkg publish`-only tree.
        scratch_tree(tree.path(), "tatolab", "foo", &["1.1.0"], None);

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let report = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        // Add still succeeds; discovery degrades to no metadata.
        assert!(report.catalog.is_none());
        assert_eq!(report.version, SemVer::new(1, 1, 0));
    }

    #[test]
    #[serial_test::serial]
    fn remove_evicts_slot_and_unrecords() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.1.0"], None);

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let added = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        assert!(added.cache_dir.is_dir());

        let report = remove(&pr).expect("remove must succeed");
        assert_eq!(report.version, SemVer::new(1, 1, 0));
        assert!(report.cache_dir_removed);
        assert!(!report.cache_dir.exists(), "cache slot must be gone");
        assert!(InstalledPackageManifest::load().unwrap().find_by_ref(&pr).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn remove_absent_package_is_typed_error() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let pr = pkg_ref("tatolab", "never-installed");
        let err = remove(&pr).expect_err("removing an absent package must fail loud");
        assert!(matches!(err, RemoveError::NotInstalled { .. }), "got {err:?}");
    }

    #[test]
    #[serial_test::serial]
    fn add_refetches_when_recorded_slot_is_evicted() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.1.0"], None);
        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();

        let first = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        assert!(!first.already_present);
        // Evict the cache slot out from under the recorded entry.
        std::fs::remove_dir_all(&first.cache_dir).unwrap();
        assert!(!first.cache_dir.exists());

        // Mentally-revert the `.filter(|dir| dir.is_dir())` self-heal guard: the
        // second add would report `already_present = true` pointing at a gone
        // slot. With the guard it re-materializes and the slot is loadable again.
        let second = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path())).unwrap();
        assert!(!second.already_present, "an evicted slot must re-materialize");
        assert!(second.cache_dir.join("streamlib.yaml").is_file());
    }

    #[test]
    #[serial_test::serial]
    fn add_identity_mismatch_fails_loud() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        // The store location says foo@1.1.0, but the `.slpkg`'s own manifest
        // declares `bar` — a mis-published package.
        let dir = tree.path().join("slpkg").join("foo").join("1.1.0");
        std::fs::create_dir_all(&dir).unwrap();
        write_slpkg(&dir.join("foo.slpkg"), "tatolab", "bar", "1.1.0");

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        // Mentally-revert the `expected` reconcile in `materialize_and_record`
        // and this records `@tatolab/bar` under a `foo` request silently.
        let err = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path()))
            .expect_err("a mis-published .slpkg must fail loud");
        match err {
            AddError::IdentityMismatch {
                requested, found, ..
            } => {
                assert_eq!(requested.name.as_str(), "foo");
                assert_eq!(found.name.as_str(), "bar");
            }
            other => panic!("expected IdentityMismatch, got {other:?}"),
        }
        // Nothing recorded on the failed add.
        assert!(InstalledPackageManifest::load()
            .unwrap()
            .find_by_ref(&pr)
            .is_none());
    }

    /// The list of `cache/packages/` slot names currently on disk.
    fn cache_slots() -> Vec<String> {
        let dir = get_streamlib_data_dir().join("cache").join("packages");
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                let mut names: Vec<String> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
            Err(_) => Vec::new(),
        }
    }

    #[test]
    #[serial_test::serial]
    fn add_identity_mismatch_leaves_no_orphan_cache_slot() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        // Store location says foo@1.1.0, but the `.slpkg`'s manifest declares
        // `bar` — a mis-published package (same setup as the fail-loud test).
        let dir = tree.path().join("slpkg").join("foo").join("1.1.0");
        std::fs::create_dir_all(&dir).unwrap();
        write_slpkg(&dir.join("foo.slpkg"), "tatolab", "bar", "1.1.0");

        let pr = pkg_ref("tatolab", "foo");
        let req = SemVerRange::from_str("^1.0.0").unwrap();
        let err = add(&pr, &req, &MockOrchestrator, &NullSink, &opts(tree.path()))
            .expect_err("a mis-published .slpkg must fail loud");
        assert!(matches!(err, AddError::IdentityMismatch { .. }), "got {err:?}");

        // The identity check is the sole promotion gate: no slot may survive in
        // cache/packages — not the `bar-1.1.0` orphan the old extract-to-slot
        // path left, not a `foo-1.1.0` slot, not the `.tmp-add-*` staging dir.
        // Mentally-revert the extract-to-temp-then-promote (extract straight
        // into the cache slot) and `bar-1.1.0` reappears here → this fails.
        let leftover = cache_slots();
        assert!(
            leftover.is_empty(),
            "IdentityMismatch must leave no cache/packages slot, found: {leftover:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn add_version_upgrade_evicts_superseded_slot() {
        let home = tempfile::tempdir().unwrap();
        let _guard = sandbox_home(home.path());
        let tree = tempfile::tempdir().unwrap();
        scratch_tree(tree.path(), "tatolab", "foo", &["1.0.0", "2.0.0"], None);
        let pr = pkg_ref("tatolab", "foo");

        // First add pins 1.0.0 — caret `^1.0.0` excludes the 2.0.0 major bump.
        let first = add(
            &pr,
            &SemVerRange::from_str("^1.0.0").unwrap(),
            &MockOrchestrator,
            &NullSink,
            &opts(tree.path()),
        )
        .unwrap();
        assert_eq!(first.version, SemVer::new(1, 0, 0));
        assert!(first.cache_dir.ends_with("cache/packages/foo-1.0.0"));
        assert_eq!(cache_slots(), vec!["foo-1.0.0".to_string()]);

        // Upgrade to 2.0.0.
        let second = add(
            &pr,
            &SemVerRange::from_str("^2.0.0").unwrap(),
            &MockOrchestrator,
            &NullSink,
            &opts(tree.path()),
        )
        .unwrap();
        assert_eq!(second.version, SemVer::new(2, 0, 0));
        assert!(second.cache_dir.ends_with("cache/packages/foo-2.0.0"));

        // Exactly one foo slot survives — the superseded foo-1.0.0 slot is
        // evicted. Mentally-revert the evict and both foo-1.0.0 and foo-2.0.0
        // remain → two slots → this fails.
        assert_eq!(
            cache_slots(),
            vec!["foo-2.0.0".to_string()],
            "the superseded version's cache slot must be evicted on upgrade"
        );

        // packages.yaml records the single upgraded entry.
        let manifest = InstalledPackageManifest::load().unwrap();
        assert_eq!(manifest.packages.iter().filter(|e| e.name == pr).count(), 1);
        assert_eq!(manifest.find_by_ref(&pr).unwrap().version, SemVer::new(2, 0, 0));
    }
}
