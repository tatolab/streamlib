// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Error;

/// Per-failure-mode error returned by [`Runner::add_module`].
///
/// [`Runner::add_module`]: super::super::Runner::add_module
#[derive(Debug, thiserror::Error)]
pub enum AddModuleError {
    /// No installed-package cache entry matches `@org/name`. Bare
    /// [`add_module`] resolves cache-only; load from source instead, or
    /// install the package.
    ///
    /// [`add_module`]: super::super::Runner::add_module
    #[error(
        "Module '{package}' not found in the installed-package cache. \
         Load it from source with `add_module_with(_, Strategy::Path {{ build: \
         BuildPolicy::IfStale, .. }})` (dev / runtime-authoring), or add it \
         with `streamlib add @org/name` (distribution)."
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

    /// The resolved source's `streamlib.yaml` `package: { org, name }`
    /// doesn't match the requested ident (wrong path, stale rename,
    /// clobbered cache).
    #[error(
        "Module '{module}' identity mismatch at {}: \
         the manifest declares `{actual}`. \
         Point the strategy at the correct package source.",
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

    /// The recursive dep walker (or the strategy resolver under it)
    /// rejected the resolved source path. Wraps the underlying engine
    /// [`Error`] so callers can introspect.
    #[error("add_module failed for '{module}': {source}")]
    LoadProjectFailed {
        module: streamlib_idents::ModuleIdent,
        #[source]
        source: Box<Error>,
    },

    /// [`Strategy::Path`] (or a path-flavored dep / patch) pointed at a
    /// directory that does not contain a `streamlib.yaml`.
    ///
    /// [`Strategy::Path`]: super::Strategy::Path
    #[error("Manifest directory has no streamlib.yaml at {}", path.display())]
    ManifestDirectoryMissing { path: std::path::PathBuf },

    /// A [`Strategy::Git`] source's git fetch failed (network, auth, bad
    /// rev, etc.).
    ///
    /// [`Strategy::Git`]: super::Strategy::Git
    #[error("Git fetch failed for '{package}' from {url}@{rev}: {detail}")]
    GitFetchFailed {
        package: streamlib_idents::PackageRef,
        url: String,
        rev: String,
        detail: String,
    },

    /// A [`Strategy::Url`] source's remote `.slpkg` fetch failed —
    /// unreadable `file://` path, HTTP error, unsupported scheme, or a
    /// cache I/O failure. Network-only; mirrors [`Self::GitFetchFailed`].
    ///
    /// [`Strategy::Url`]: super::Strategy::Url
    #[error("Remote .slpkg fetch failed for '{package}' from {url}: {detail}")]
    UrlFetchFailed {
        package: streamlib_idents::PackageRef,
        url: String,
        detail: String,
    },

    /// A [`Strategy::Url`] fetch produced bytes whose digest didn't match
    /// the caller-supplied [`ArtifactChecksum`]. Fail-loud — never load an
    /// artifact that doesn't match its integrity pin. `detail` names the
    /// algorithm and the expected-vs-actual digests.
    ///
    /// [`Strategy::Url`]: super::Strategy::Url
    /// [`ArtifactChecksum`]: super::ArtifactChecksum
    #[error("Integrity check failed for '{package}' from {url}: {detail}")]
    IntegrityCheckFailed {
        package: streamlib_idents::PackageRef,
        url: String,
        detail: String,
    },

    /// A [`Strategy::Registry`] resolution found no usable registry
    /// endpoint: neither `STREAMLIB_REGISTRY_URL` nor its `STREAMLIB_REGISTRY_URL`
    /// fallback is set. Point one at the static registry base URL (e.g.
    /// `file:///path/to/registry-tree`) so the generic registry is reachable.
    /// Fail-loud — never silently fall back to a local source for a
    /// dependency the caller asked to resolve from the registry.
    ///
    /// [`Strategy::Registry`]: super::Strategy::Registry
    #[error(
        "Registry not configured for '{package}': set {env} (e.g. \
         file:///path/to/registry-tree) to resolve from the static generic store"
    )]
    RegistryNotConfigured {
        package: streamlib_idents::PackageRef,
        env: String,
    },

    /// A [`Strategy::Registry`] source failed while listing the package's
    /// published versions, selecting one for the requested
    /// [`SemVerRange`], downloading the resolved `.slpkg`, or caching the
    /// downloaded bytes. `detail` names the failing step.
    ///
    /// [`Strategy::Registry`]: super::Strategy::Registry
    /// [`SemVerRange`]: streamlib_idents::SemVerRange
    #[error("Registry resolution failed for '{package}': {detail}")]
    RegistryResolutionFailed {
        package: streamlib_idents::PackageRef,
        detail: String,
    },

    /// A [`BuildPolicy`] required a (re)build but no
    /// [`BuildOrchestrator`] is wired on the [`Runner`]. The conservative
    /// posture — never silently load a stale or absent artifact. Wire one
    /// via [`Runner::new_with_orchestrator`] / [`Runner::set_build_orchestrator`],
    /// or enable the SDK's `auto-build` feature.
    ///
    /// [`BuildPolicy`]: super::BuildPolicy
    /// [`BuildOrchestrator`]: super::BuildOrchestrator
    /// [`Runner`]: super::super::Runner
    /// [`Runner::new_with_orchestrator`]: super::super::Runner::new_with_orchestrator
    /// [`Runner::set_build_orchestrator`]: super::super::Runner::set_build_orchestrator
    #[error(
        "Module '{package}' needs a build ({policy:?}) but no BuildOrchestrator \
         is wired. Construct the runtime via `Runner::new_with_orchestrator(...)` \
         (or enable the SDK `auto-build` feature), or load a prebuilt artifact \
         with a `NeverBuild` strategy / `.slpkg`."
    )]
    BuildRequiredButNoOrchestrator {
        package: streamlib_idents::PackageRef,
        policy: super::BuildPolicy,
    },

    /// The wired [`BuildOrchestrator`] failed to materialize the package.
    ///
    /// [`BuildOrchestrator`]: super::BuildOrchestrator
    #[error("Build orchestrator failed to materialize '{package}': {detail}")]
    MaterializeFailed {
        package: streamlib_idents::PackageRef,
        detail: String,
    },

    /// An active `streamlib link` marker exists but its `.streamlib/link.json`
    /// could not be parsed. Never silently ignored — a corrupt marker would
    /// leave resolution in a mixed state (some modules from the checkout, some
    /// from the registry), the exact failure mode link mode exists to prevent.
    /// Run `streamlib unlink` to clear the torn state, then re-link.
    #[error(
        "active streamlib link marker is corrupt, refusing to resolve modules \
         against an ambiguous link: {detail}. Run `streamlib unlink` and re-link."
    )]
    LinkStateCorrupt { detail: String },

    /// Discovering the active `streamlib link` for the current run failed at
    /// the filesystem level (working directory unreadable, or the linked
    /// checkout's `packages/` tree could not be enumerated).
    #[error("could not read streamlib link state for module resolution: {detail}")]
    LinkStateUnreadable { detail: String },

    /// A graph-mutating call ([`Runner::add_processor`] / `connect` /
    /// `start`) ran while one or more modules were still loading. Await
    /// the pending loads (e.g. via [`Runner::await_modules`]) before
    /// building the graph.
    ///
    /// [`Runner::add_processor`]: crate::core::runtime::RuntimeOperations::add_processor
    /// [`Runner::await_modules`]: super::super::Runner::await_modules
    #[error(
        "{} module(s) still loading: {}. Await them before mutating the graph.",
        idents.len(),
        idents.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
    )]
    ModulesStillLoading {
        idents: Vec<streamlib_idents::ModuleIdent>,
    },

    /// The load was cancelled (the [`AddedModule`] handle was dropped or
    /// `cancel()`ed before it resolved).
    ///
    /// [`AddedModule`]: super::AddedModule
    #[error("Module load for '{module}' was cancelled")]
    LoadCancelled {
        module: streamlib_idents::ModuleIdent,
    },

    /// The spawned load task panicked or was otherwise lost.
    #[error("Module load task for '{module}' failed: {detail}")]
    LoadTaskPanicked {
        module: streamlib_idents::ModuleIdent,
        detail: String,
    },

    /// [`Runner::add_module_blocking`] was called from within a tokio
    /// runtime (external-handle mode), where blocking would deadlock /
    /// panic. Use the async `.await` surface instead.
    ///
    /// [`Runner::add_module_blocking`]: super::super::Runner::add_module_blocking
    #[error(
        "add_module_blocking('{module}') called from inside a tokio runtime — \
         block_on would panic. Await the AddedModule future instead."
    )]
    BlockingCallFromAsyncContext {
        module: streamlib_idents::ModuleIdent,
    },

    /// Strategy was [`Strategy::Slpkg`] and the extraction step failed
    /// (I/O, malformed ZIP, missing embedded manifest, etc.).
    ///
    /// [`Strategy::Slpkg`]: super::Strategy::Slpkg
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

    /// Two requirers resolved the same `@org/name` package to different
    /// concrete versions within the runtime's lifetime — a diamond
    /// version conflict. The engine enforces a single version per package
    /// across the whole module graph (and across successive `add_module`
    /// calls), so this is a hard error rather than a silent
    /// double-registration. Resolve it by pinning a single version via a
    /// `patch:` entry in the requiring `streamlib.yaml` that redirects the
    /// package to one `path:` / `git:` source, or by aligning the two
    /// requirers on a single declared version.
    #[error(
        "Single-version conflict for package '{package}': version \
         {existing_version} (required by {existing_required_by}) conflicts \
         with version {conflicting_version} (required by \
         {conflicting_required_by}). streamlib enforces one version per \
         package across the module graph — pin a single version via a \
         `patch:` entry in the requiring streamlib.yaml (redirecting \
         '{package}' to one path/git source), or align the two requirers \
         on the same version."
    )]
    SingleVersionConflict {
        package: streamlib_idents::PackageRef,
        existing_version: streamlib_idents::SemVer,
        existing_required_by: String,
        conflicting_version: streamlib_idents::SemVer,
        conflicting_required_by: String,
    },

    /// This load skipped registering `package` because a concurrent
    /// `add_module` load was already resolving the same version — and
    /// that concurrent load subsequently failed, so the package never
    /// registered. Fails loudly rather than reporting a false success
    /// over an unregistered dependency.
    #[error(
        "Concurrent load of '{package}' (version {version}) failed after \
         this load skipped it as already-in-flight — the package never \
         registered. Retry this add_module call."
    )]
    ConcurrentLoadOfSkippedDependencyFailed {
        package: streamlib_idents::PackageRef,
        version: streamlib_idents::SemVer,
    },

    /// This load skipped `package` as already-in-flight on a concurrent
    /// `add_module` load, and that load neither committed nor failed
    /// within the wait window. Defensive bound — a wedged concurrent
    /// load surfaces as this typed error, never as a hang.
    #[error(
        "Timed out after {waited_secs}s waiting for a concurrent load of \
         '{package}' (version {version}) that this load skipped as \
         already-in-flight. The concurrent load may be wedged; retry this \
         add_module call once it settles."
    )]
    ConcurrentLoadOfSkippedDependencyTimedOut {
        package: streamlib_idents::PackageRef,
        version: streamlib_idents::SemVer,
        waited_secs: u64,
    },

    /// A locked run reached a transitive dependency `package` that the
    /// application lockfile does not pin. The lockfile is stale relative to
    /// the manifest graph (a dep was added since the last install), so the
    /// run can't resolve it offline. Re-run `streamlib install` to refresh
    /// the lockfile, then run again.
    #[error(
        "Locked run: dependency '{package}' (required by '{required_by}') is \
         not pinned in the application lockfile — the lockfile is stale \
         relative to the manifest graph. Re-run `streamlib install` to \
         refresh the lockfile, then run again."
    )]
    LockfileMiss {
        package: streamlib_idents::PackageRef,
        required_by: String,
    },

    /// A locked run resolved `package` to its pinned version, but that
    /// version's installed-cache slot is missing on disk. The lockfile is
    /// consistent but the package was never materialized (or the cache was
    /// cleared). Re-run `streamlib install` to re-populate the cache.
    #[error(
        "Locked run: package '{package}' is pinned at {version} but its \
         installed-cache slot is missing at {}. Re-run `streamlib install` \
         to re-materialize the pinned set, then run again.",
        expected_dir.display()
    )]
    LockedSlotMissing {
        package: streamlib_idents::PackageRef,
        version: streamlib_idents::SemVer,
        expected_dir: std::path::PathBuf,
    },

    /// The application lockfile could not be read or parsed for a locked
    /// run — the file is missing, malformed, or carries a lockfile key that
    /// isn't a canonical `@org/name`. `detail` names the failing step.
    #[error("Failed to read application lockfile at {}: {detail}", path.display())]
    LockfileReadFailed {
        path: std::path::PathBuf,
        detail: String,
    },

    /// A locked run found `package`'s installed-cache slot, but its
    /// manifest + schema content no longer hashes to the lockfile's pinned
    /// `content_hash` — the slot was tampered with or republished in place
    /// after install. The lockfile's reproducibility promise requires the
    /// bytes it pinned; re-run `streamlib install` to re-materialize and
    /// re-pin a consistent set.
    #[error(
        "Locked run: package '{package}' failed the content-hash integrity \
         check — lockfile pins {expected} but the installed slot hashes to \
         {actual}. The slot was modified after install; re-run \
         `streamlib install` to re-materialize and re-pin."
    )]
    LockedSlotContentMismatch {
        package: streamlib_idents::PackageRef,
        expected: String,
        actual: String,
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
