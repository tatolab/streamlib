// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Error;

/// Per-failure-mode error returned by [`Runner::add_module`].
///
/// [`Runner::add_module`]: super::super::Runner::add_module
#[derive(Debug, thiserror::Error)]
pub enum AddModuleError {
    /// No workspace stage dir AND no installed-package cache entry
    /// matches `@org/name`. Surface the canonical ref so callers can
    /// suggest `streamlib pkg install`, `cargo xtask build-plugins`,
    /// or a typo fix.
    #[error(
        "Module '{package}' not found — no workspace stage dir at \
         `target/streamlib-plugins/<org>__<name>/` and no installed-cache \
         entry. Run `cargo xtask build-plugins --package {package}` (dev) \
         or `streamlib pkg install <slpkg>` (distribution)."
    )]
    ModuleNotFound {
        package: streamlib_idents::PackageRef,
    },

    /// Workspace stage dir or installed-cache entry was found but the
    /// `streamlib.yaml` failed to parse / lacked a `package:` block.
    #[error(
        "Failed to load manifest for '{module}' from {}: {detail}",
        source_path.display()
    )]
    ManifestLoadFailed {
        module: streamlib_idents::ModuleIdent,
        source_path: std::path::PathBuf,
        detail: String,
    },

    /// Workspace stage dir was found but its `streamlib.yaml`'s
    /// `package: { org, name }` doesn't match the requested ident
    /// (manual clobbering, stale rename, wrong dir).
    #[error(
        "Module '{module}' identity mismatch at {}: \
         staged manifest declares `{actual}`. \
         Re-run `cargo xtask build-plugins` to regenerate.",
        source_path.display()
    )]
    ManifestIdentityMismatch {
        module: streamlib_idents::ModuleIdent,
        source_path: std::path::PathBuf,
        actual: String,
    },

    /// On-disk version doesn't satisfy the ident's [`SemVerRange`].
    ///
    /// [`SemVerRange`]: streamlib_idents::SemVerRange
    #[error(
        "Module '{module}' resolved to version {found} at {} which doesn't \
         satisfy the requested range. Install a matching version or relax \
         the range.",
        source_path.display()
    )]
    VersionRangeUnsatisfied {
        module: streamlib_idents::ModuleIdent,
        found: streamlib_idents::SemVer,
        source_path: std::path::PathBuf,
    },

    /// `InstalledPackageManifest::load()` errored before lookup could
    /// run. Catches I/O / parse failures distinct from "no entry."
    #[error("Failed to load installed-package cache: {detail}")]
    InstalledCacheLoadFailed { detail: String },

    /// `STREAMLIB_WORKSPACE_ROOT` was set but its value isn't a
    /// directory. Treats the env var as the user's stated intent —
    /// don't silently fall through to the installed cache when the
    /// override is broken. Mirrors `load_workspace_packages`'s
    /// `WorkspaceRootNotFound` behavior on env-var-set-but-invalid.
    #[error(
        "STREAMLIB_WORKSPACE_ROOT is set to `{env_value}` but that path \
         is not a directory. Fix the env var or unset it (in which case \
         the resolver falls through to the installed-package cache)."
    )]
    WorkspaceRootInvalid { env_value: String },

    /// The recursive dep walker (or the strategy resolver under it)
    /// rejected the resolved source path. Wraps the underlying engine
    /// [`Error`] so callers can introspect.
    #[error("load_project failed for '{module}': {source}")]
    LoadProjectFailed {
        module: streamlib_idents::ModuleIdent,
        #[source]
        source: Box<Error>,
    },

    /// `Runner::add_module_with` was called with
    /// [`ModuleResolverStrategy::WorkspaceStaged`] but no streamlib.yaml
    /// exists at the workspace stage dir. Surface the expected path so
    /// callers see exactly where the resolver looked.
    ///
    /// [`Runner::add_module_with`]: super::super::Runner::add_module_with
    /// [`ModuleResolverStrategy::WorkspaceStaged`]: super::ModuleResolverStrategy::WorkspaceStaged
    #[error(
        "Module '{package}' not staged under target/streamlib-plugins. \
         Expected `streamlib.yaml` at {expected_path}. \
         Run `cargo xtask build-plugins --package {package}` first."
    )]
    WorkspaceStageMiss {
        package: streamlib_idents::PackageRef,
        expected_path: std::path::PathBuf,
    },

    /// Workspace stage dir resolution requires a workspace root but
    /// neither `STREAMLIB_WORKSPACE_ROOT` nor `cargo locate-project`
    /// returned one. Distinct from
    /// [`Self::WorkspaceRootInvalid`] (set-but-broken env var).
    #[error(
        "Workspace root not found — set STREAMLIB_WORKSPACE_ROOT or run \
         from within a Cargo workspace"
    )]
    WorkspaceRootNotFound,

    /// A `Rust`-impl workspace-staged package has no cdylib at
    /// `lib/<host_triple>/`. The staged manifest declares Rust
    /// processors but `cargo xtask build-plugins` either was never
    /// run or produced no artifact for this host triple.
    #[error(
        "Cdylib missing for Rust-impl package '{package}' — expected at \
         {expected_path}. Re-run `cargo xtask build-plugins` to rebuild."
    )]
    CdylibMissingForRustImpl {
        package: streamlib_idents::PackageRef,
        expected_path: std::path::PathBuf,
    },

    /// [`ModuleResolverStrategy::ManifestDirectory`] pointed at a
    /// directory that does not contain a `streamlib.yaml`. Catches
    /// the `load_project("./does-not-exist")` and patch-points-at-
    /// missing-path cases at the strategy layer.
    ///
    /// [`ModuleResolverStrategy::ManifestDirectory`]: super::ModuleResolverStrategy::ManifestDirectory
    #[error("Manifest directory has no streamlib.yaml at {}", path.display())]
    ManifestDirectoryMissing { path: std::path::PathBuf },

    /// Strategy was [`ModuleResolverStrategy::SlpkgArchive`] and the
    /// extraction step failed (I/O, malformed ZIP, missing embedded
    /// manifest, etc.).
    ///
    /// [`ModuleResolverStrategy::SlpkgArchive`]: super::ModuleResolverStrategy::SlpkgArchive
    #[error(
        "Failed to extract .slpkg archive at {}: {detail}",
        archive.display()
    )]
    SlpkgExtractionFailed {
        archive: std::path::PathBuf,
        detail: String,
    },

    /// Strategy resolver failed while reading the manifest at the
    /// resolved directory (parse error, missing `[package]` block).
    /// Distinct from [`Self::ManifestLoadFailed`] because the caller
    /// hasn't bound a [`ModuleIdent`] yet at this stage.
    ///
    /// [`ModuleIdent`]: streamlib_idents::ModuleIdent
    #[error(
        "Failed to read manifest at {}: {detail}",
        source_path.display()
    )]
    StrategyManifestLoadFailed {
        source_path: std::path::PathBuf,
        detail: String,
    },

    /// Strategy was ident-keyed
    /// ([`ModuleResolverStrategy::DefaultChain`] /
    /// [`ModuleResolverStrategy::WorkspaceStaged`] /
    /// [`ModuleResolverStrategy::InstalledCache`]) but no
    /// [`PackageRef`] was supplied. Internal invariant — callers route
    /// through `Runner::add_module_with` which always supplies the ref.
    ///
    /// [`ModuleResolverStrategy::DefaultChain`]: super::ModuleResolverStrategy::DefaultChain
    /// [`ModuleResolverStrategy::WorkspaceStaged`]: super::ModuleResolverStrategy::WorkspaceStaged
    /// [`ModuleResolverStrategy::InstalledCache`]: super::ModuleResolverStrategy::InstalledCache
    /// [`PackageRef`]: streamlib_idents::PackageRef
    #[error(
        "Strategy '{strategy}' requires a PackageRef but none was supplied — \
         internal invariant violation"
    )]
    StrategyNeedsPackageRef { strategy: String },

    /// A dependency cycle was detected during recursive dep walking.
    /// `cycle` lists the full recursion path — the first and last
    /// entries are the repeated vertex, and any entries between trace
    /// the edges that re-entered.
    #[error(
        "Dependency cycle detected — package {} is already mid-load on the \
         recursion stack while a transitive dep tries to load it again",
        cycle.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" → ")
    )]
    DependencyCycleDetected {
        cycle: Vec<streamlib_idents::PackageRef>,
    },
}

impl From<AddModuleError> for Error {
    fn from(err: AddModuleError) -> Self {
        match err {
            AddModuleError::LoadProjectFailed { source, .. } => *source,
            other => Error::Configuration(other.to_string()),
        }
    }
}

/// Per-failure-mode error returned by [`Runner::remove_module`].
///
/// Today the only variant is the milestone-deferral marker; new
/// variants land when the hot-reload lifecycle work ships.
///
/// [`Runner::remove_module`]: super::super::Runner::remove_module
#[derive(Debug, thiserror::Error)]
pub enum RemoveModuleError {
    /// Module unload requires the hot-reload lifecycle work that's
    /// explicitly out of scope for the current All-Dynamic Package
    /// Loading milestone. Calling `remove_module` returns this without
    /// altering any runtime state.
    #[error(
        "remove_module('{module}') is not yet implemented — \
         hot-reload lifecycle is deferred to a future milestone. \
         The runtime currently supports load-only, runtime-lifetime \
         module registration."
    )]
    HotReloadLifecycleNotYetImplemented {
        module: streamlib_idents::ModuleIdent,
    },
}

impl From<RemoveModuleError> for Error {
    fn from(err: RemoveModuleError) -> Self {
        Error::Configuration(err.to_string())
    }
}

/// Per-failure-mode error returned by [`Runner::load_workspace_packages`].
///
/// The variants surface enough context (offending name, expected path,
/// underlying engine error) that callers can match for retry vs. abort
/// or surface an actionable message to the developer.
///
/// [`Runner::load_workspace_packages`]: super::super::Runner::load_workspace_packages
#[derive(Debug, thiserror::Error)]
pub enum LoadWorkspacePackagesError {
    /// Name did not parse as `@<org>/<name>` per the typed `streamlib-idents`
    /// org / name validators (charset, leading-letter, length).
    #[error("Invalid package id '{0}' — expected `@<org>/<name>` with lowercase org and name")]
    InvalidPackageId(String),

    /// Workspace root could not be resolved — neither the
    /// `STREAMLIB_WORKSPACE_ROOT` env var nor `cargo locate-project`
    /// returned a usable directory.
    #[error(
        "Workspace root not found — set STREAMLIB_WORKSPACE_ROOT or run \
         from within a Cargo workspace"
    )]
    WorkspaceRootNotFound,

    /// Staged dir does not exist for this package. Most likely cause:
    /// the dev hasn't run `cargo xtask build-plugins` yet (or pruned
    /// `target/` since the last run).
    #[error(
        "Package '{name}' not staged at {expected_path}. \
         Run `cargo xtask build-plugins` first."
    )]
    PackageNotStaged {
        name: String,
        expected_path: std::path::PathBuf,
    },

    /// Staged dir exists and parses, but its `[package]` org / name
    /// don't match the requested id. Catches the case where the
    /// staged tree was clobbered out-of-band (manual `cp`, stale
    /// rename) before the runtime registers the wrong processors.
    #[error(
        "Package identity mismatch at {staged_path}: \
         requested `@{requested_org}/{requested_name}`, found \
         `@{actual_org}/{actual_name}` in staged streamlib.yaml. \
         Re-run `cargo xtask build-plugins` to regenerate."
    )]
    PackageIdentityMismatch {
        staged_path: std::path::PathBuf,
        requested_org: String,
        requested_name: String,
        actual_org: String,
        actual_name: String,
    },

    /// Staged dir is present and identity matches, but a Rust-impl
    /// package's expected cdylib is missing under `lib/<host_triple>/`.
    /// Distinguishes "staging succeeded but cargo build silently
    /// produced no artifact" from a generic load_project failure.
    #[error(
        "Cdylib missing for Rust-impl package '{name}' — expected at \
         {expected_path}. Re-run `cargo xtask build-plugins` to rebuild."
    )]
    CdylibMissing {
        name: String,
        expected_path: std::path::PathBuf,
    },

    /// `load_project` rejected the staged dir. Carries the engine
    /// `Error` so callers can introspect further.
    #[error("load_project failed for '{name}': {source}")]
    LoadProjectFailed {
        name: String,
        #[source]
        source: Box<Error>,
    },
}

impl From<LoadWorkspacePackagesError> for Error {
    fn from(err: LoadWorkspacePackagesError) -> Self {
        match err {
            LoadWorkspacePackagesError::LoadProjectFailed { source, .. } => *source,
            other => Error::Configuration(other.to_string()),
        }
    }
}
