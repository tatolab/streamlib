// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `install` — the resolve+materialize+lock half of the install/run split.
//!
//! [`install`] takes an application's `streamlib.yaml` root, resolves its
//! full transitive package tree range→concrete via the shared
//! [`streamlib_idents`] resolver, materializes every package into the app's
//! co-located `streamlib_modules/@org/name` slots through an injected
//! [`BuildOrchestrator`], and writes an application lockfile
//! ([`streamlib_idents::APP_LOCKFILE_NAME`]) pinning the exact resolved set. A
//! later locked run ([`Runner::add_modules_from_lockfile`]) consumes that
//! lockfile strictly from those slots, offline, with no live re-resolution.
//!
//! The lockfile is the resolver handoff: range logic lives only here (at
//! install), concrete enforcement only at run — the two resolvers stay
//! physically separate.
//!
//! [`BuildOrchestrator`]: super::BuildOrchestrator
//! [`Runner::add_modules_from_lockfile`]: super::Runner::add_modules_from_lockfile

use std::path::{Path, PathBuf};

use streamlib_idents::{
    PackageRef, PackageSource, ResolvedPackage, ResolverOptions, SemVer, resolve_with,
};

use super::module_loader::host_target_triple;
use super::{
    BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest, BuildSource,
    PackageSourceProvenance,
};

/// Knobs for [`install`]. Defaults are the ordinary "install this app"
/// posture — write the lockfile next to the manifest, resolve the package
/// source from the environment, materialize with [`BuildPolicy::IfStale`].
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Where to write the application lockfile. `None` ⇒
    /// `<root_dir>/streamlib-app.lock`.
    pub lockfile_path: Option<PathBuf>,
    /// Resolver cache dir for by-version `.slpkg` extraction + git checkouts.
    /// `None` ⇒ `$HOME/.streamlib/resolver-cache/`.
    pub resolver_cache_dir: Option<PathBuf>,
    /// Package source for resolving version-range deps. `None` ⇒ read from the
    /// environment ([`PackageSource::from_env`]); still `None` after that means
    /// "no package source configured" and a version dep fails loud.
    pub package_source: Option<PackageSource>,
    /// Build policy for each package's materialize. Defaults to
    /// [`BuildPolicy::IfStale`] — build when the tool's fingerprint says so,
    /// reuse an up-to-date cache slot otherwise. Native hosts + the
    /// release-completeness check ride inside `materialize` regardless.
    pub materialize_policy: Option<BuildPolicy>,
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
    /// conflict, package-source error, unreadable manifest, …).
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
}

/// Resolve + materialize + lock an application's package tree.
///
/// This is the programmatic `streamlib install`. It:
///
/// 1. Resolves `root_dir`'s `streamlib.yaml` range→concrete over the full
///    transitive tree (network at install time is expected — package source
///    listing / download / git fetch).
/// 2. Materializes every resolved package into the app's co-located
///    `streamlib_modules/@org/name` slots via `orchestrator` (building cdylibs
///    / provisioning venvs / **pre-building the subprocess native hosts** so a
///    later polyglot run is offline). The orchestrator's own release-completeness pre-check fires
///    here, so a partial package-source release fails install before any lockfile
///    is written — the lockfile always pins a completeness-checked set.
/// 3. Writes the application lockfile pinning the exact resolved set.
pub fn install(
    root_dir: &Path,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    options: &InstallOptions,
) -> std::result::Result<InstallReport, InstallError> {
    let package_source = options.package_source.clone().or_else(PackageSource::from_env);
    let resolver_options = ResolverOptions {
        cache_dir: options.resolver_cache_dir.clone(),
        package_source,
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

    // Materialize every package the resolver produced. The project-flavor
    // root (no `[package]`) is the consumer, not a package to stage — skip
    // it; every dependency is a real package.

    // Per-package content hash of the STAGED cache slot, keyed by the
    // canonical lockfile key. The lockfile pins this staged hash (not the
    // resolver-source hash) because staging may legitimately rewrite the
    // manifest (relative `path:` deps become absolute), and the locked run
    // verifies against the slot it actually loads.
    let mut staged_hashes: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    // The app root whose `streamlib_modules/` slot seam the install writes into:
    // the lockfile's parent dir (default `<root_dir>/streamlib-app.lock`), or
    // `root_dir` itself when no explicit lockfile path is configured.
    let app_root: PathBuf = options
        .lockfile_path
        .as_deref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root_dir.to_path_buf());

    for pkg in resolved.iter_all() {
        let Some((pkg_ref, _version)) = package_ref_of(pkg) else {
            // Root project with no `[package]` — nothing to materialize.
            continue;
        };
        // A package-flavor root IS materializable (installing a library),
        // but the resolver's `Root` source carries the root dir directly.
        tracing::info!(package = %pkg_ref, "install: materializing");
        let request = BuildRequest {
            package: pkg_ref.clone(),
            source: BuildSource::PackageDir(pkg.root_dir.clone()),
            source_provenance: provenance_of(&pkg.source),
            policy,
            host_triple: host_target_triple().to_string(),
            staging_destination_slot_dir: crate::core::installed_package_slot_dir(
                Some(app_root.as_path()),
                &pkg_ref,
            ),
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
        .filter_map(package_ref_of)
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

/// The canonical [`PackageRef`] and version of a resolved package, or `None`
/// for a project-flavor manifest (no `[package]` block).
fn package_ref_of(pkg: &ResolvedPackage) -> Option<(PackageRef, SemVer)> {
    pkg.manifest
        .package
        .as_ref()
        .map(|m| (PackageRef::new(m.org.clone(), m.name.clone()), m.version))
}

/// Map a resolver source kind to the orchestrator's build-time provenance: a
/// `path:` dep or the root manifest is the user's own editable tree (cargo deps
/// may resolve outside it, `target/` is the user's), while a git-rev / `.slpkg`
/// / by-version source is a self-contained managed extract.
fn provenance_of(source: &streamlib_idents::ResolvedSource) -> PackageSourceProvenance {
    use streamlib_idents::ResolvedSource;
    match source {
        ResolvedSource::Root | ResolvedSource::Path { .. } => {
            PackageSourceProvenance::MutableUserCheckout
        }
        ResolvedSource::Git { .. }
        | ResolvedSource::Slpkg { .. }
        | ResolvedSource::ByVersion { .. } => PackageSourceProvenance::ImmutableManagedExtract,
    }
}

