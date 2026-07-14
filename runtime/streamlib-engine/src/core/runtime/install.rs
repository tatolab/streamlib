// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `install` — the resolve+materialize+lock half of the install/run split.
//!
//! [`install`] takes an application's `streamlib.yaml` root, resolves its
//! full transitive package tree range→concrete via the shared
//! [`streamlib_idents`] resolver, materializes every package into the
//! installed-package cache through an injected [`BuildOrchestrator`], and
//! writes an application lockfile ([`streamlib_idents::APP_LOCKFILE_NAME`])
//! pinning the exact resolved set. A later locked run
//! ([`Runner::add_modules_from_lockfile`]) consumes that lockfile strictly
//! from the cache, offline, with no live re-resolution.
//!
//! The lockfile is the resolver handoff: range logic lives only here (at
//! install), concrete enforcement only at run — the two resolvers stay
//! physically separate.
//!
//! [`BuildOrchestrator`]: super::BuildOrchestrator
//! [`Runner::add_modules_from_lockfile`]: super::Runner::add_modules_from_lockfile

use std::path::{Path, PathBuf};

use streamlib_idents::{
    PackageRef, RegistryConfig, ResolvedPackage, ResolverOptions, SemVer, resolve_with,
};

use super::module_loader::host_target_triple;
use super::{BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest, BuildSource};
use crate::core::config::{InstalledPackageEntry, InstalledPackageManifest};

/// Knobs for [`install`]. Defaults are the ordinary "install this app"
/// posture — write the lockfile next to the manifest, resolve the registry
/// from the environment, materialize with [`BuildPolicy::IfStale`].
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Where to write the application lockfile. `None` ⇒
    /// `<root_dir>/streamlib-app.lock`.
    pub lockfile_path: Option<PathBuf>,
    /// Resolver cache dir for registry `.slpkg` extraction + git checkouts.
    /// `None` ⇒ `$HOME/.streamlib/resolver-cache/`.
    pub resolver_cache_dir: Option<PathBuf>,
    /// Registry config for resolving registry-flavored deps. `None` ⇒ read
    /// from the environment ([`RegistryConfig::from_env`]); still `None`
    /// after that means "no registry" and a registry dep fails loud.
    pub registry: Option<RegistryConfig>,
    /// Build policy for each package's materialize. Defaults to
    /// [`BuildPolicy::IfStale`] — build when the tool's fingerprint says so,
    /// reuse an up-to-date cache slot otherwise. Native hosts + the
    /// release-completeness check ride inside `materialize` regardless.
    pub materialize_policy: Option<BuildPolicy>,
    /// Also record each materialized package in the installed-package
    /// manifest (`packages.yaml`) so `streamlib pkg list` and bare
    /// `Strategy::InstalledCache` loads see them. Defaults to `true`.
    pub update_installed_manifest: Option<bool>,
}

/// Outcome of a successful [`install`].
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// Path the application lockfile was written to.
    pub lockfile_path: PathBuf,
    /// Every package pinned in the lockfile (the resolved dep closure),
    /// sorted by canonical name.
    pub packages: Vec<(PackageRef, SemVer)>,
}

/// Per-failure-mode error from [`install`].
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The `streamlib.yaml` graph failed to resolve (missing dep, version
    /// conflict, registry error, unreadable manifest, …).
    #[error("resolving the package graph at {} failed: {source}", root_dir.display())]
    Resolve {
        root_dir: PathBuf,
        #[source]
        source: streamlib_idents::ResolverError,
    },

    /// The injected [`BuildOrchestrator`] failed to materialize a package.
    /// Includes the incomplete-release and native-host-fetch failures the
    /// orchestrator surfaces during materialize.
    #[error("materializing '{package}' failed: {source}")]
    Materialize {
        package: PackageRef,
        #[source]
        source: super::BuildError,
    },

    /// Writing the application lockfile to disk failed.
    #[error("writing the application lockfile to {} failed: {source}", path.display())]
    WriteLockfile {
        path: PathBuf,
        #[source]
        source: streamlib_idents::ResolverError,
    },

    /// Updating the installed-package manifest failed.
    #[error("updating the installed-package manifest failed: {detail}")]
    UpdateManifest { detail: String },
}

/// Resolve + materialize + lock an application's package tree.
///
/// This is the programmatic `streamlib install`. It:
///
/// 1. Resolves `root_dir`'s `streamlib.yaml` range→concrete over the full
///    transitive tree (network at install time is expected — registry
///    listing / download / git fetch).
/// 2. Materializes every resolved package into the installed-package cache
///    via `orchestrator` (building cdylibs / provisioning venvs / **pre-
///    building the subprocess native hosts** so a later polyglot run is
///    offline). The orchestrator's own release-completeness pre-check fires
///    here, so a partial registry release fails install before any lockfile
///    is written — the lockfile always pins a completeness-checked set.
/// 3. Writes the application lockfile pinning the exact resolved set.
pub fn install(
    root_dir: &Path,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    options: &InstallOptions,
) -> std::result::Result<InstallReport, InstallError> {
    let registry = options.registry.clone().or_else(RegistryConfig::from_env);
    let resolver_options = ResolverOptions {
        cache_dir: options.resolver_cache_dir.clone(),
        registry,
        // Install is the reproducible distribution seam: it resolves range →
        // concrete and pins a lockfile, so it is deliberately NOT link-aware — a
        // dev-loop `streamlib link` override must never leak into a lockfile.
        link_checkout: None,
    };

    tracing::info!(root = %root_dir.display(), "install: resolving package graph");
    let resolved =
        resolve_with(root_dir, &resolver_options).map_err(|source| InstallError::Resolve {
            root_dir: root_dir.to_path_buf(),
            source,
        })?;

    let policy = options.materialize_policy.unwrap_or(BuildPolicy::IfStale);
    let update_manifest = options.update_installed_manifest.unwrap_or(true);

    // Materialize every package the resolver produced. The project-flavor
    // root (no `[package]`) is the consumer, not a package to stage — skip
    // it; every dependency is a real package.
    let mut installed = if update_manifest {
        Some(
            InstalledPackageManifest::load().map_err(|e| InstallError::UpdateManifest {
                detail: e.to_string(),
            })?,
        )
    } else {
        None
    };

    // Per-package content hash of the STAGED cache slot, keyed by the
    // canonical lockfile key. The lockfile pins this staged hash (not the
    // resolver-source hash) because staging may legitimately rewrite the
    // manifest (relative `path:` deps become absolute), and the locked run
    // verifies against the slot it actually loads.
    let mut staged_hashes: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for pkg in resolved.iter_all() {
        let Some(pkg_ref) = package_ref_of(pkg) else {
            // Root project with no `[package]` — nothing to materialize.
            continue;
        };
        // A package-flavor root IS materializable (installing a library),
        // but the resolver's `Root` source carries the root dir directly.
        tracing::info!(package = %pkg_ref, "install: materializing");
        let request = BuildRequest {
            package: pkg_ref.clone(),
            source: BuildSource::PackageDir(pkg.root_dir.clone()),
            policy,
            host_triple: host_target_triple().to_string(),
        };
        let staged = orchestrator.materialize(&request, sink).map_err(|source| {
            InstallError::Materialize {
                package: pkg_ref.clone(),
                source,
            }
        })?;

        let staged_hash = streamlib_idents::content_hash_for_package_dir(&staged.staged_dir)
            .map_err(|source| InstallError::Resolve {
                root_dir: staged.staged_dir.clone(),
                source,
            })?;
        staged_hashes.insert(pkg_ref.to_string(), staged_hash);

        if let (Some(manifest), Some(meta)) = (installed.as_mut(), pkg.manifest.package.as_ref()) {
            manifest.add(InstalledPackageEntry {
                name: pkg_ref.clone(),
                version: meta.version,
                description: meta.description.clone(),
                installed_from: describe_source(pkg),
                installed_at: rfc3339_utc_now(),
                cache_dir: staged
                    .staged_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });
        }
    }

    if let Some(mut manifest) = installed {
        manifest.save().map_err(|e| InstallError::UpdateManifest {
            detail: e.to_string(),
        })?;
    }

    // Write the application lockfile from the resolved dep closure (root
    // excluded — the lock records dependencies, mirroring Cargo.lock).
    // Each entry's content_hash is replaced with the STAGED slot's hash so
    // the locked run's integrity gate verifies the exact bytes it loads.
    let mut lockfile = resolved.to_lockfile();
    for (key, entry) in lockfile.packages.iter_mut() {
        if let Some(staged_hash) = staged_hashes.get(key) {
            entry.content_hash = staged_hash.clone();
        }
    }
    let lockfile_path = options
        .lockfile_path
        .clone()
        .unwrap_or_else(|| root_dir.join(streamlib_idents::APP_LOCKFILE_NAME));
    streamlib_idents::write_app_lockfile(&lockfile_path, &lockfile).map_err(|source| {
        InstallError::WriteLockfile {
            path: lockfile_path.clone(),
            source,
        }
    })?;

    let mut packages: Vec<(PackageRef, SemVer)> = resolved
        .packages
        .values()
        .filter_map(|p| {
            package_ref_of(p).map(|r| (r, p.manifest.package.as_ref().unwrap().version))
        })
        .collect();
    packages.sort_by(|a, b| a.0.to_string().cmp(&b.0.to_string()));

    tracing::info!(
        lockfile = %lockfile_path.display(),
        packages = packages.len(),
        "install: wrote application lockfile"
    );
    Ok(InstallReport {
        lockfile_path,
        packages,
    })
}

/// The canonical [`PackageRef`] of a resolved package, or `None` for a
/// project-flavor manifest (no `[package]` block).
fn package_ref_of(pkg: &ResolvedPackage) -> Option<PackageRef> {
    pkg.manifest
        .package
        .as_ref()
        .map(|m| PackageRef::new(m.org.clone(), m.name.clone()))
}

/// Current UTC time as an RFC-3339 timestamp, dependency-free (the engine
/// carries no `chrono` / `time` dep). Shared with [`super::add`] — both write
/// the same `installed_at` field on `packages.yaml` entries.
pub(super) fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix_secs(secs)
}

/// Format `secs` since the Unix epoch as an RFC-3339 UTC timestamp. Uses
/// Howard Hinnant's civil-from-days algorithm to avoid a wall-clock
/// dependency just for an informational manifest field.
fn rfc3339_from_unix_secs(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil-from-days (days since 1970-01-01).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Human-readable provenance for the installed-manifest `installed_from`
/// field.
fn describe_source(pkg: &ResolvedPackage) -> String {
    use streamlib_idents::ResolvedSource;
    match &pkg.source {
        ResolvedSource::Root => "root".into(),
        ResolvedSource::Path { relative } => format!("path:{}", relative.display()),
        ResolvedSource::Git { url, rev } => format!("git:{url}@{rev}"),
        ResolvedSource::Slpkg { archive } => format!("slpkg:{}", archive.display()),
        ResolvedSource::Registry { url } => format!("registry:{url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_civil_from_days_matches_known_epochs() {
        // Mentally-revert: any off-by-one in the civil-from-days math shifts
        // these known instants, so the equality pins the algorithm.
        assert_eq!(rfc3339_from_unix_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_from_unix_secs(1_700_000_000),
            "2023-11-14T22:13:20Z"
        );
        // A leap day (2024-02-29) — the algorithm must place it correctly.
        assert_eq!(
            rfc3339_from_unix_secs(1_709_208_000),
            "2024-02-29T12:00:00Z"
        );
    }
}
