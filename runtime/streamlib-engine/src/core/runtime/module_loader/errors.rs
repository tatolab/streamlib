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

    /// The manifest for `package` declares two subprocess (Python /
    /// TypeScript) processors that compose the SAME structured
    /// `processor_type` ident — a duplicate short name within one
    /// `processors:` list. Refused at end-of-walk, before any staged
    /// registration is committed, so the load leaves zero partial state.
    /// Give each processor a distinct PascalCase short name.
    #[error(
        "Manifest for '{package}' declares processor type '{processor_type}' \
         more than once (two subprocess processors compose the same ident). \
         Give each processor a distinct PascalCase short name."
    )]
    DuplicateProcessorTypeInModule {
        package: streamlib_idents::PackageRef,
        processor_type: crate::core::descriptors::SchemaIdent,
    },

    /// A subprocess (Python / TypeScript) processor `package` declares
    /// composes a `processor_type` ident that is ALREADY present in the
    /// global processor registry (registered by other code — e.g. a
    /// direct `register_dynamic`, or a prior load of the same ident that
    /// wasn't removed). Refused at end-of-walk, before any staged
    /// registration is committed, so the load leaves zero partial state.
    /// Remove the existing registration first, or rename the processor.
    #[error(
        "Processor type '{processor_type}' declared by '{package}' is already \
         registered in the runtime. Remove the existing registration \
         (remove_module) or give the processor a distinct short name."
    )]
    ProcessorTypeAlreadyRegistered {
        package: streamlib_idents::PackageRef,
        processor_type: crate::core::descriptors::SchemaIdent,
    },

    /// [`Runner::add_local`] was handed a type whose
    /// [`GeneratedProcessor::descriptor`] returned `None` — it carries no
    /// registerable descriptor, so there is nothing to register under
    /// `@session/…`. Annotate the type with `#[processor(...)]`.
    ///
    /// [`Runner::add_local`]: super::super::Runner::add_local
    /// [`GeneratedProcessor::descriptor`]: crate::core::processors::GeneratedProcessor::descriptor
    #[error(
        "add_local::<{type_name}>() failed: the type carries no processor \
         descriptor. Annotate it with `#[processor(...)]` so it has a \
         registerable identity."
    )]
    SessionProcessorHasNoDescriptor { type_name: String },

    /// [`Runner::add_local`] derived a session package name that fails the
    /// ident grammar (`[a-z][a-z0-9-]*`). `detail` carries the underlying
    /// [`streamlib_idents::IdentError`].
    ///
    /// [`Runner::add_local`]: super::super::Runner::add_local
    #[error(
        "add_local::<{type_name}>() failed: cannot mint a `@session/<name>` \
         ident from the type name: {detail}"
    )]
    SessionProcessorNameInvalid { type_name: String, detail: String },

    /// [`Runner::add_local`] was handed a config that does not deserialize
    /// into the processor type's `Config` — refused before registering, so a
    /// session type never registers with a config its own schema rejects.
    ///
    /// [`Runner::add_local`]: super::super::Runner::add_local
    #[error(
        "add_local::<{type_name}>() failed: the supplied config is not valid \
         for the processor's Config type: {detail}"
    )]
    SessionProcessorConfigInvalid { type_name: String, detail: String },

    /// [`Runner::add_local`] found a live `@session/<name>` registration
    /// already in the ledger — the same session-local name is registered and
    /// was not removed. Never silently overwritten. Remove it first
    /// ([`Runner::remove_module`]), or register the new type under a distinct
    /// name.
    ///
    /// [`Runner::add_local`]: super::super::Runner::add_local
    /// [`Runner::remove_module`]: super::super::Runner::remove_module
    #[error(
        "add_local failed: a session-local processor is already registered as \
         '{module}'. Remove it (remove_module) before re-registering the same \
         name, or use a distinct type name."
    )]
    DuplicateSessionProcessorName {
        module: streamlib_idents::ModuleIdent,
    },

    /// [`Runner::register_processor_from_source`] was handed a `language` that
    /// live source submit does not support. Only the subprocess languages
    /// (Python / TypeScript) run from source with no host compile; Rust from
    /// source is a full cargo build (the `streamlib pkg build` flow), never a
    /// live graph mutation.
    ///
    /// [`Runner::register_processor_from_source`]: super::super::Runner::register_processor_from_source
    #[error(
        "register_processor_from_source: language '{language}' is not supported \
         for live source submit — only Python and TypeScript run from source. \
         Build a Rust processor with `streamlib pkg build` and load the package."
    )]
    SourceLanguageUnsupportedForLiveSubmit { language: String },

    /// [`Runner::register_processor_from_source`] was handed a submission with
    /// neither a `requested_name` nor a `processor_type_name` — there is no
    /// identity to mint a `@session/<name>` under.
    ///
    /// [`Runner::register_processor_from_source`]: super::super::Runner::register_processor_from_source
    #[error(
        "register_processor_from_source: the submission carries neither a \
         requested name nor a processor type name — supply one so a \
         `@session/<name>` identity can be minted."
    )]
    SubmittedSourceMissingName,

    /// [`Runner::register_processor_from_source`] failed to stage the submitted
    /// source to disk (directory creation, source write, or manifest write).
    /// `detail` names the failing step.
    ///
    /// [`Runner::register_processor_from_source`]: super::super::Runner::register_processor_from_source
    #[error("register_processor_from_source: failed to stage '{module}' to disk: {detail}")]
    SubmittedSourceStagingFailed {
        module: streamlib_idents::ModuleIdent,
        detail: String,
    },

    /// [`Runner::replace_processor_from_source`] was given a replacement whose
    /// minted `@session/<name>` segment does not match the target's — a replace
    /// only ever re-registers the SAME session name at a fresh `0.0.N`. Refused
    /// before any mutation (no removal, no staging), so the target is untouched.
    ///
    /// [`Runner::replace_processor_from_source`]: super::super::Runner::replace_processor_from_source
    #[error(
        "replace_processor_from_source: the replacement resolves to session name \
         '{replacement_name}' but the target is '{target}' — a replace re-registers \
         the same `@session/<name>`. Submit the replacement under the target's name, \
         or use remove_module + register_processor_from_source for a rename."
    )]
    ReplaceTargetNameMismatch {
        target: streamlib_idents::ModuleIdent,
        replacement_name: String,
    },

    /// [`Runner::replace_processor_from_source`] refused because the target's
    /// loaded version is not source-backed on disk — no staged
    /// `session-source/<name>/<loaded-version>/` dir exists to restore from if
    /// the replacement fails. This is the case for an `add_local` host-vtable
    /// registration (registered from a compiled type, never staged as source),
    /// which is outside the live-source-iteration use case. Refused before any
    /// mutation, so the target is untouched.
    ///
    /// [`Runner::replace_processor_from_source`]: super::super::Runner::replace_processor_from_source
    #[error(
        "replace_processor_from_source: target '{target}' is not source-backed \
         (no staged source at {}), so it cannot be transactionally restored if the \
         replacement fails — this is an `add_local` host-vtable registration. Use \
         remove_module + register_processor_from_source to swap it explicitly.",
        expected_dir.display()
    )]
    ReplaceTargetNotSourceBacked {
        target: streamlib_idents::ModuleIdent,
        expected_dir: std::path::PathBuf,
    },

    /// [`Runner::replace_processor_from_source`] removed the target and then the
    /// replacement's registration failed — but the target's prior registration
    /// was restored from its on-disk staged source, so the runtime is back to
    /// its pre-replace state. `cause` carries the replacement's failure.
    ///
    /// [`Runner::replace_processor_from_source`]: super::super::Runner::replace_processor_from_source
    #[error(
        "replace_processor_from_source: the replacement for '{target}' failed to \
         register ({cause}); the prior registration was restored — the runtime is \
         unchanged. Fix the replacement source and retry."
    )]
    ReplacementRegistrationFailedPriorRegistrationRestored {
        target: streamlib_idents::ModuleIdent,
        cause: String,
    },

    /// [`Runner::replace_processor_from_source`] removed the target, the
    /// replacement's registration failed, AND the compensating restore of the
    /// prior registration ALSO failed — the runtime is now missing the target.
    /// Reachable only if the previously-built target no longer rebuilds from its
    /// still-present staged source (a degraded, unexpected state).
    ///
    /// [`Runner::replace_processor_from_source`]: super::super::Runner::replace_processor_from_source
    #[error(
        "replace_processor_from_source: the replacement for '{target}' failed AND \
         restoring the prior registration also failed — the target is no longer \
         registered. Re-register it with register_processor_from_source."
    )]
    ReplacementRegistrationFailedRestoreAlsoFailed {
        target: streamlib_idents::ModuleIdent,
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

/// Per-failure-mode error returned by [`Runner::remove_module`]. Every
/// variant leaves the runtime's registries exactly as they were.
///
/// [`Runner::remove_module`]: super::super::Runner::remove_module
#[derive(Debug, thiserror::Error)]
pub enum RemoveModuleError {
    /// No committed load matches the requested module — either the
    /// package was never loaded (`loaded_version: None`) or the loaded
    /// version doesn't satisfy the requested range (`loaded_version`
    /// names what IS loaded).
    #[error(
        "remove_module('{module}'): {}. Load the module first via \
         add_module, or request a range matching the loaded version.",
        match loaded_version {
            Some(v) => format!("the loaded version {v} does not satisfy the requested range"),
            None => "no loaded module matches".to_string(),
        }
    )]
    ModuleNotLoaded {
        module: streamlib_idents::ModuleIdent,
        loaded_version: Option<streamlib_idents::SemVer>,
    },

    /// A load of this module is still in flight — removal would race the
    /// walk. Await the pending load (e.g. via [`Runner::await_modules`]),
    /// then retry.
    ///
    /// [`Runner::await_modules`]: super::super::Runner::await_modules
    #[error(
        "remove_module('{module}'): a load of this module is still in \
         flight. Await the pending load (Runner::await_modules), then \
         retry the removal."
    )]
    LoadInFlight {
        module: streamlib_idents::ModuleIdent,
    },

    /// Other loaded modules still declare this module as a dependency.
    /// Removal never cascades — remove the requirers first, then retry.
    #[error(
        "remove_module('{module}'): still required by loaded module(s) {}. \
         Removal never cascades — remove_module each requirer first, then \
         retry.",
        requirers.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    RequiredByLoadedModules {
        module: streamlib_idents::ModuleIdent,
        requirers: Vec<streamlib_idents::PackageRef>,
    },

    /// Graph nodes still instantiate this module's processor types.
    /// Remove those processors from the graph
    /// ([`RuntimeOperations::remove_processor`]), then retry.
    ///
    /// [`RuntimeOperations::remove_processor`]: crate::core::runtime::RuntimeOperations::remove_processor
    #[error(
        "remove_module('{module}'): {} graph processor(s) still use its \
         processor type(s): [{}] (types: [{}]). Remove those processors \
         from the graph first, then retry.",
        processor_ids.len(),
        processor_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "),
        processor_types.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
    )]
    ProcessorsInUse {
        module: streamlib_idents::ModuleIdent,
        processor_ids: Vec<crate::core::graph::ProcessorUniqueId>,
        processor_types: Vec<crate::core::descriptors::SchemaIdent>,
    },
}

impl From<RemoveModuleError> for Error {
    fn from(err: RemoveModuleError) -> Self {
        Error::Configuration(err.to_string())
    }
}
