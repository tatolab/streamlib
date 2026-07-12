// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::build_orchestrator::{BuildEventSink, BuildOrchestrator, BuildPolicy};
use super::errors::AddModuleError;
use super::locked::LockedResolution;
use super::processor_registration::register_manifest_processors;
use super::schema_registration::register_package_schemas;
use super::source::{read_version_from_manifest_dir, resolve_strategy_to_source, ResolvedSource, Strategy};
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

/// How long a load waits, after its own walk succeeds, for a concurrent
/// load that owned a skipped in-flight dependency to commit or fail.
/// Generous (builds can take minutes); the timeout exists so a wedged
/// concurrent load surfaces as a typed error, never a hang.
pub(super) const SKIPPED_IN_FLIGHT_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// A single requirer edge into a resolved package: the parent
/// [`PackageRef`] that declared the dependency (or `None` for a
/// top-level `add_module` call) plus the version range it declared.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
#[derive(Debug, Clone)]
pub(crate) struct RequirerRecord {
    /// The parent package that pulled this dependency in, or `None` when
    /// the package was the root of a top-level `add_module` call.
    pub requirer: Option<streamlib_idents::PackageRef>,
    /// The [`SemVerRange`] the requirer declared for this package (`Any`
    /// for path / git deps, which carry no range).
    ///
    /// [`SemVerRange`]: streamlib_idents::SemVerRange
    pub declared_range: streamlib_idents::SemVerRange,
}

/// The single-version resolution record for one `@org/name` package.
/// The concrete resolved [`SemVer`] is authoritative; every subsequent
/// encounter is checked against it.
///
/// [`SemVer`]: streamlib_idents::SemVer
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPackageRecord {
    /// The concrete on-disk version this package resolved to.
    pub version: streamlib_idents::SemVer,
    /// Where that version's manifest was resolved from.
    pub source_path: std::path::PathBuf,
    /// Every requirer that has pulled this package in so far.
    pub required_by: Vec<RequirerRecord>,
}

/// Terminal outcome of an in-flight package resolution, published via
/// [`PackageResolutionCompletionSignal`] when the owning load commits
/// (registration + transitive walk succeeded) or fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConcurrentPackageLoadOutcome {
    /// The owning load flipped the placeholder to a committed record.
    Committed,
    /// The owning load failed; the placeholder was removed.
    Failed,
}

/// One-shot completion signal for an in-flight package resolution.
/// Loads that skipped the package mid-walk wait on this at the end of
/// their own walk to verify the owner actually finished registering.
pub(crate) struct PackageResolutionCompletionSignal {
    outcome: Mutex<Option<ConcurrentPackageLoadOutcome>>,
    outcome_published: Condvar,
}

impl PackageResolutionCompletionSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(None),
            outcome_published: Condvar::new(),
        })
    }

    fn publish(&self, published_outcome: ConcurrentPackageLoadOutcome) {
        *self.outcome.lock() = Some(published_outcome);
        self.outcome_published.notify_all();
    }

    /// Block until the outcome is published or `timeout` elapses.
    /// `None` means timeout — the caller surfaces a typed error.
    pub(crate) fn wait_for_outcome(
        &self,
        timeout: Duration,
    ) -> Option<ConcurrentPackageLoadOutcome> {
        let deadline = Instant::now() + timeout;
        let mut outcome = self.outcome.lock();
        while outcome.is_none() {
            if self
                .outcome_published
                .wait_until(&mut outcome, deadline)
                .timed_out()
            {
                return *outcome;
            }
        }
        *outcome
    }
}

/// Per-package resolution state in the [`ResolutionMemo`].
pub(crate) enum PackageResolutionState {
    /// A load is mid-resolution for this package: the gate passed but
    /// registration + the transitive walk have not completed yet.
    InFlightPlaceholder {
        record: ResolvedPackageRecord,
        owner_load_id: u64,
        completion_signal: Arc<PackageResolutionCompletionSignal>,
    },
    /// Registration + transitive walk completed; the version is final.
    Committed { record: ResolvedPackageRecord },
}

/// A dependency this walk skipped because a concurrent load already had
/// it in flight at the same version. Verified at the end of the top-level
/// walk: the owner must have committed, or this load fails loudly.
pub(crate) struct SkippedInFlightDependency {
    pub package: streamlib_idents::PackageRef,
    pub version: streamlib_idents::SemVer,
    pub completion_signal: Arc<PackageResolutionCompletionSignal>,
}

/// Gate decision for one package encounter.
pub(super) enum SingleVersionGateOutcome {
    /// First encounter — placeholder inserted; register + recurse.
    ProceedAsFirstResolution,
    /// Already committed at the same version — requirer recorded; skip.
    SkipAlreadyCommittedSameVersion,
    /// In flight on a concurrent load at the same version — requirer
    /// recorded; skip locally and verify the owner's outcome at the end
    /// of this walk via the carried signal.
    SkipInFlightSameVersion(Arc<PackageResolutionCompletionSignal>),
}

/// Runtime-lifetime memo of every package resolved by the live module
/// walker, keyed by `@org/name`. Persists across every `add_module` call
/// on a [`Runner`] so two independently-rooted diamond branches, two
/// successive `add_module` calls, or two concurrent loads that resolve
/// the same package to different concrete versions conflict instead of
/// silently double-registering.
///
/// [`Runner`]: crate::core::runtime::Runner
pub(crate) struct ResolutionMemo {
    packages: Mutex<HashMap<streamlib_idents::PackageRef, PackageResolutionState>>,
}

impl ResolutionMemo {
    pub(crate) fn new() -> Self {
        Self {
            packages: Mutex::new(HashMap::new()),
        }
    }

    /// The single-version gate: classify this package encounter under one
    /// lock acquisition. Inserts the in-flight placeholder on first
    /// encounter so concurrent loads observe the resolution immediately —
    /// nobody ever blocks inside the gate.
    fn gate(
        &self,
        load_id: u64,
        pkg_ref: &streamlib_idents::PackageRef,
        on_disk_version: streamlib_idents::SemVer,
        manifest_dir: &std::path::Path,
        requirer: RequirerRecord,
    ) -> std::result::Result<SingleVersionGateOutcome, AddModuleError> {
        let mut packages = self.packages.lock();
        let Some(state) = packages.get_mut(pkg_ref) else {
            packages.insert(
                pkg_ref.clone(),
                PackageResolutionState::InFlightPlaceholder {
                    record: ResolvedPackageRecord {
                        version: on_disk_version,
                        source_path: manifest_dir.to_path_buf(),
                        required_by: vec![requirer],
                    },
                    owner_load_id: load_id,
                    completion_signal: PackageResolutionCompletionSignal::new(),
                },
            );
            return Ok(SingleVersionGateOutcome::ProceedAsFirstResolution);
        };

        {
            let record = match state {
                PackageResolutionState::InFlightPlaceholder { record, .. } => record,
                PackageResolutionState::Committed { record } => record,
            };
            if record.version != on_disk_version {
                return Err(AddModuleError::SingleVersionConflict {
                    package: pkg_ref.clone(),
                    existing_version: record.version,
                    existing_required_by: format!(
                        "{} [resolved from {}]",
                        describe_requirers(&record.required_by),
                        record.source_path.display(),
                    ),
                    conflicting_version: on_disk_version,
                    conflicting_required_by: format!(
                        "{} [resolved from {}]",
                        describe_requirer(&requirer),
                        manifest_dir.display(),
                    ),
                });
            }
            if record.source_path != manifest_dir {
                tracing::warn!(
                    package = %pkg_ref,
                    version = %on_disk_version,
                    resolved_from = %record.source_path.display(),
                    skipped_source = %manifest_dir.display(),
                    "single-version gate: package already resolved at this \
                     version from a different source path — this source is \
                     skipped; edits to it will not take effect in this runtime",
                );
            }
            record.required_by.push(requirer);
        }

        match state {
            PackageResolutionState::Committed { .. } => {
                tracing::debug!(
                    package = %pkg_ref,
                    version = %on_disk_version,
                    "single-version gate: already resolved at this version — \
                     skipping re-registration and re-recursion",
                );
                Ok(SingleVersionGateOutcome::SkipAlreadyCommittedSameVersion)
            }
            PackageResolutionState::InFlightPlaceholder {
                completion_signal,
                owner_load_id,
                ..
            } => {
                tracing::debug!(
                    package = %pkg_ref,
                    version = %on_disk_version,
                    owner_load_id = *owner_load_id,
                    "single-version gate: same version in flight on a \
                     concurrent load — skipping locally; outcome verified at \
                     end of walk",
                );
                Ok(SingleVersionGateOutcome::SkipInFlightSameVersion(
                    Arc::clone(completion_signal),
                ))
            }
        }
    }

    /// Flip this package's in-flight placeholder to a committed record
    /// and publish [`ConcurrentPackageLoadOutcome::Committed`]. Requirers
    /// accumulated on the placeholder while in flight are preserved.
    fn commit_in_flight_resolution(&self, pkg_ref: &streamlib_idents::PackageRef) {
        let signal = {
            let mut packages = self.packages.lock();
            match packages.remove(pkg_ref) {
                Some(PackageResolutionState::InFlightPlaceholder {
                    record,
                    completion_signal,
                    ..
                }) => {
                    packages.insert(
                        pkg_ref.clone(),
                        PackageResolutionState::Committed { record },
                    );
                    Some(completion_signal)
                }
                // Defensive: only the owning walk commits its placeholder.
                Some(other) => {
                    packages.insert(pkg_ref.clone(), other);
                    None
                }
                None => None,
            }
        };
        if let Some(signal) = signal {
            signal.publish(ConcurrentPackageLoadOutcome::Committed);
        }
    }

    /// Remove this package's in-flight placeholder after a failed load
    /// and publish [`ConcurrentPackageLoadOutcome::Failed`], so concurrent
    /// loads that skipped it fail loudly instead of assuming it registered.
    fn abandon_in_flight_resolution(&self, pkg_ref: &streamlib_idents::PackageRef) {
        let signal = {
            let mut packages = self.packages.lock();
            match packages.remove(pkg_ref) {
                Some(PackageResolutionState::InFlightPlaceholder {
                    completion_signal, ..
                }) => Some(completion_signal),
                // Defensive: never remove a committed record on abandon.
                Some(other) => {
                    packages.insert(pkg_ref.clone(), other);
                    None
                }
                None => None,
            }
        };
        if let Some(signal) = signal {
            signal.publish(ConcurrentPackageLoadOutcome::Failed);
        }
    }

    /// Test observability: the committed record for a package, if its
    /// resolution has completed. `None` while absent or still in flight.
    #[cfg(test)]
    pub(crate) fn committed_record(
        &self,
        pkg_ref: &streamlib_idents::PackageRef,
    ) -> Option<ResolvedPackageRecord> {
        match self.packages.lock().get(pkg_ref) {
            Some(PackageResolutionState::Committed { record }) => Some(record.clone()),
            _ => None,
        }
    }

    /// Test observability: whether any resolution state (in-flight or
    /// committed) exists for a package.
    #[cfg(test)]
    pub(crate) fn contains_package(&self, pkg_ref: &streamlib_idents::PackageRef) -> bool {
        self.packages.lock().contains_key(pkg_ref)
    }

    /// Test observability: requirer count on an in-flight placeholder.
    #[cfg(test)]
    pub(crate) fn in_flight_requirer_count(
        &self,
        pkg_ref: &streamlib_idents::PackageRef,
    ) -> Option<usize> {
        match self.packages.lock().get(pkg_ref) {
            Some(PackageResolutionState::InFlightPlaceholder { record, .. }) => {
                Some(record.required_by.len())
            }
            _ => None,
        }
    }
}

/// RAII cleanup for an armed in-flight placeholder: any failure exit
/// between the gate and the commit drops this guard, which removes the
/// placeholder and publishes `Failed` — the memo can never wedge in a
/// permanent "resolving" state, and concurrent skippers fail loudly.
struct InFlightPlaceholderGuard<'memo> {
    resolution_memo: &'memo ResolutionMemo,
    package: Option<streamlib_idents::PackageRef>,
}

impl<'memo> InFlightPlaceholderGuard<'memo> {
    fn arm(
        resolution_memo: &'memo ResolutionMemo,
        package: streamlib_idents::PackageRef,
    ) -> Self {
        Self {
            resolution_memo,
            package: Some(package),
        }
    }

    fn commit(mut self) {
        if let Some(package) = self.package.take() {
            self.resolution_memo.commit_in_flight_resolution(&package);
        }
    }
}

impl Drop for InFlightPlaceholderGuard<'_> {
    fn drop(&mut self) {
        if let Some(package) = self.package.take() {
            self.resolution_memo.abandon_in_flight_resolution(&package);
        }
    }
}

/// Render one requirer edge for a
/// [`AddModuleError::SingleVersionConflict`] message.
fn describe_requirer(requirer: &RequirerRecord) -> String {
    match &requirer.requirer {
        Some(pkg) => format!("{pkg} (declared `{}`)", requirer.declared_range),
        None => format!("top-level add_module (declared `{}`)", requirer.declared_range),
    }
}

/// Render the accumulated requirers of an already-resolved package.
fn describe_requirers(requirers: &[RequirerRecord]) -> String {
    requirers
        .iter()
        .map(describe_requirer)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Recursive worker: resolves the [`Strategy`] to a source, materializes
/// via the injected [`BuildOrchestrator`] when a build is required,
/// validates the manifest's identity + version range, registers the
/// package's schemas, walks dependencies (each routed through this same
/// helper), then registers the package's processors.
///
/// `seen` tracks every [`PackageRef`] currently on the recursion stack
/// (O(1) membership); `path` preserves insertion order so the
/// dependency-cycle error carries the actual edge that re-entered.
/// `resolution_memo` is the runtime-lifetime single-version record
/// shared across every `add_module` call — the gate that turns a
/// diamond version divergence into a typed
/// [`AddModuleError::SingleVersionConflict`] instead of a silent
/// double-registration. `load_id` identifies the top-level load this
/// walk belongs to; `skipped_in_flight` accumulates dependencies this
/// walk skipped because a concurrent load had them in flight — verified
/// at the end of the top-level walk.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
#[allow(clippy::too_many_arguments)]
pub(super) fn add_module_recursively(
    iceoryx2_node: &Iceoryx2Node,
    orchestrator: Option<&Arc<dyn BuildOrchestrator>>,
    sink: &dyn BuildEventSink,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
    resolution_memo: &ResolutionMemo,
    load_id: u64,
    skipped_in_flight: &mut Vec<SkippedInFlightDependency>,
    locked: Option<&LockedResolution>,
) -> std::result::Result<(), AddModuleError> {
    let pkg_ref = module.package_ref();
    if !seen.insert(pkg_ref.clone()) {
        let mut cycle = path.clone();
        cycle.push(pkg_ref);
        return Err(AddModuleError::DependencyCycleDetected { cycle });
    }
    path.push(pkg_ref.clone());
    let result = add_module_recursive_body(
        iceoryx2_node,
        orchestrator,
        sink,
        module,
        strategy,
        seen,
        path,
        resolution_memo,
        load_id,
        skipped_in_flight,
        locked,
    );
    seen.remove(&pkg_ref);
    path.pop();
    result
}

#[allow(clippy::too_many_arguments)]
fn add_module_recursive_body(
    iceoryx2_node: &Iceoryx2Node,
    orchestrator: Option<&Arc<dyn BuildOrchestrator>>,
    sink: &dyn BuildEventSink,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
    resolution_memo: &ResolutionMemo,
    load_id: u64,
    skipped_in_flight: &mut Vec<SkippedInFlightDependency>,
    locked: Option<&LockedResolution>,
) -> std::result::Result<(), AddModuleError> {
    use crate::core::config::ProjectConfig;

    let pkg_ref = module.package_ref();

    // Resolve where the source lives (pure filesystem / cache / git),
    // then materialize via the orchestrator if a build is required.
    let manifest_dir = match resolve_strategy_to_source(&strategy, &pkg_ref)? {
        ResolvedSource::Ready(dir) => dir,
        ResolvedSource::NeedsBuild(request) => match orchestrator {
            // An orchestrator is wired — materialize (fetch/build/stage).
            Some(orch) => {
                let staged = orch.materialize(&request, sink).map_err(|e| {
                    AddModuleError::MaterializeFailed {
                        package: pkg_ref.clone(),
                        detail: e.to_string(),
                    }
                })?;
                staged.staged_dir
            }
            // No orchestrator wired. Any build-requiring policy
            // (`IfStale` or `AlwaysBuild`) fails loud — never silently
            // load a possibly-stale or unbuilt artifact, and never branch
            // behavior on package shape. A no-build deployment uses
            // `NeverBuild` / `InstalledCache` / `.slpkg`; a building one
            // wires an orchestrator (the SDK `auto-build` feature does so
            // by default).
            None => {
                return Err(AddModuleError::BuildRequiredButNoOrchestrator {
                    package: pkg_ref.clone(),
                    policy: request.policy,
                })
            }
        },
    };

    let on_disk_version = read_version_from_manifest_dir(&manifest_dir)?;

    // Read the manifest; authoritative source of identity for the
    // package at the resolved location.
    let config = ProjectConfig::load(&manifest_dir).map_err(|e| {
        AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        }
    })?;

    config
        .check_streamlib_version_compatibility()
        .map_err(|e| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        })?;

    // Identity check: the manifest's `[package]` org/name must match
    // the requested ident.
    let pkg_meta = config
        .package
        .as_ref()
        .ok_or_else(|| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: "manifest has no `package:` block".into(),
        })?;
    if pkg_meta.org != module.org || pkg_meta.name != module.name {
        return Err(AddModuleError::ManifestIdentityMismatch {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            actual: format!("@{}/{}", pkg_meta.org.as_str(), pkg_meta.name.as_str()),
        });
    }
    if !module.version.matches(on_disk_version) {
        return Err(AddModuleError::VersionRangeUnsatisfied {
            module: module.clone(),
            found: on_disk_version,
            source_path: manifest_dir.clone(),
        });
    }

    tracing::info!(
        "add_module: '{}' → {} (on-disk version {})",
        module,
        manifest_dir.display(),
        on_disk_version,
    );

    // Single-version-per-package gate. The memo persists across the whole
    // runtime lifetime (not per walk), so two independently-rooted diamond
    // branches, two successive `add_module` calls, or two concurrent
    // loads that resolve the same package to different concrete versions
    // conflict here instead of silently double-registering. Compares
    // concrete resolved `SemVer`s, never ranges: path / git deps enter
    // with range `Any`, so a range-only check would never fire.
    //
    // First encounter inserts an in-flight placeholder under the same
    // lock as the check (no check-then-commit window for a concurrent
    // load to slip through). A same-version re-encounter skips
    // re-registration + re-recursion; when the same version is in flight
    // on a CONCURRENT load, the skip is recorded in `skipped_in_flight`
    // and the owner's outcome is verified at the end of this walk —
    // nobody ever blocks mid-walk, so concurrent walks cannot deadlock.
    let requirer = RequirerRecord {
        requirer: (path.len() >= 2).then(|| path[path.len() - 2].clone()),
        declared_range: module.version.clone(),
    };
    match resolution_memo.gate(load_id, &pkg_ref, on_disk_version, &manifest_dir, requirer)? {
        SingleVersionGateOutcome::SkipAlreadyCommittedSameVersion => return Ok(()),
        SingleVersionGateOutcome::SkipInFlightSameVersion(completion_signal) => {
            skipped_in_flight.push(SkippedInFlightDependency {
                package: pkg_ref.clone(),
                version: on_disk_version,
                completion_signal,
            });
            return Ok(());
        }
        SingleVersionGateOutcome::ProceedAsFirstResolution => {}
    }

    // The placeholder is armed: any failure exit below drops the guard,
    // which removes the placeholder and publishes `Failed` — a retried
    // add_module re-runs the full resolution, and concurrent loads that
    // skipped this package fail loudly instead of assuming it registered.
    let placeholder_guard = InFlightPlaceholderGuard::arm(resolution_memo, pkg_ref.clone());

    // Schemas are leaves — register before recursing into deps.
    register_package_schemas(&manifest_dir, &config).map_err(|e| {
        AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        }
    })?;

    // Walk transitive deps, each routed through this same helper.
    //
    // Locked mode forces every dep edge to its lockfile pin (the pinned
    // installed-cache slot, loaded as-is with `NeverBuild`) instead of
    // deriving a live source strategy — so a locked run never touches the
    // registry / git / a `.slpkg` re-fetch / a build. A dep the lockfile
    // doesn't pin is a stale-lockfile hard error, not a silent live
    // resolve.
    for (dep_ref, spec) in &config.dependencies {
        let (dep_ident, dep_strategy) = match locked {
            Some(lock) => lock.resolve(dep_ref, &pkg_ref.to_string())?,
            None => derive_dep_strategy_and_ident(&manifest_dir, dep_ref, spec, &config.patch)
                .map_err(|e| AddModuleError::LoadProjectFailed {
                    module: module.clone(),
                    source: Box::new(e),
                })?,
        };
        tracing::info!(
            "Loading dependency '{}' (strategy {:?})",
            dep_ident,
            dep_strategy
        );
        add_module_recursively(
            iceoryx2_node,
            orchestrator,
            sink,
            dep_ident,
            dep_strategy,
            seen,
            path,
            resolution_memo,
            load_id,
            skipped_in_flight,
            locked,
        )?;
    }

    // Now register this package's own processors.
    register_manifest_processors(iceoryx2_node, &manifest_dir, &config).map_err(|e| {
        AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        }
    })?;

    // Registration + the transitive walk succeeded — flip the in-flight
    // placeholder to a committed record and publish `Committed` to any
    // concurrent loads that skipped this package. Note that registration
    // itself is NOT transactional: schemas / processors registered before
    // a later failure remain in the process-global registries (no module
    // unload / rollback exists yet), so retrying a multi-processor
    // package that failed partway can hit "already registered". The
    // guard's commit-after-success still guarantees the memo never claims
    // a package resolved when its load didn't complete.
    placeholder_guard.commit();

    Ok(())
}

/// Map a single declared dep (with optional consumer `patch:` override)
/// to the [`ModuleIdent`] + [`Strategy`] pair that recursively re-enters
/// [`add_module_recursively`].
///
/// Patch precedence mirrors Cargo's `[patch.crates-io]`: consumer's
/// patch wins when present; otherwise the dep declaration's source
/// variant decides. Path / git source deps are dev-shaped and default to
/// [`BuildPolicy::IfStale`] (rebuild-on-change via the build tool);
/// registry deps resolve from the installed cache.
///
/// [`ModuleIdent`]: streamlib_idents::ModuleIdent
fn derive_dep_strategy_and_ident(
    consumer_dir: &std::path::Path,
    dep_ref: &streamlib_idents::PackageRef,
    spec: &streamlib_idents::DependencySpec,
    patch: &std::collections::BTreeMap<streamlib_idents::PackageRef, streamlib_idents::DependencySpec>,
) -> Result<(streamlib_idents::ModuleIdent, Strategy)> {
    use streamlib_idents::{DependencySpec, ModuleIdent, SemVerRange};

    // Registry deps carry a range that constrains resolution even when
    // patched. Path / git deps don't — the source's manifest version
    // becomes authoritative (range = any).
    let declared_range = match spec {
        DependencySpec::Registry(r) => r.version.clone(),
        DependencySpec::Path(_) | DependencySpec::Git(_) => SemVerRange::Any,
    };

    let (strategy_spec, is_patch) = match patch.get(dep_ref) {
        Some(patched) => (patched, true),
        None => (spec, false),
    };

    let strategy = match strategy_spec {
        DependencySpec::Path(p) => {
            // Path deps are dev-time sources resolved relative to the
            // CWD (the consumer's run dir); a missing patch path is a
            // hard error so the dev fixes the manifest immediately.
            let abs = if p.path.is_absolute() {
                p.path.clone()
            } else {
                consumer_dir.join(&p.path)
            };
            if is_patch && !abs.exists() {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' points at `{}` which does not \
                     exist. Path patches are dev-time overrides — they must \
                     resolve to a real directory.",
                    abs.display(),
                )));
            }
            Strategy::Path {
                path: abs,
                build: BuildPolicy::IfStale,
            }
        }
        DependencySpec::Git(g) => Strategy::Git {
            url: g.git.clone(),
            rev: g.rev.clone(),
            build: BuildPolicy::IfStale,
        },
        DependencySpec::Registry(r) => {
            if is_patch {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' is registry-flavored — a patch \
                     must redirect a dependency to a `path:` or `git:` source, \
                     not another registry range.",
                )));
            }
            // A registry-version dependency resolves from the configured the static registry
            // generic registry by version: pull the `.slpkg` and build it from
            // source on the host (IfStale prefers a matching prebuilt). The
            // registry endpoint comes from the environment
            // (STREAMLIB_REGISTRY_URL / STREAMLIB_REGISTRY_URL).
            Strategy::Registry {
                version_req: r.version.clone(),
                build: BuildPolicy::IfStale,
            }
        }
    };

    let ident = ModuleIdent::new(dep_ref.org.clone(), dep_ref.name.clone(), declared_range);
    Ok((ident, strategy))
}
