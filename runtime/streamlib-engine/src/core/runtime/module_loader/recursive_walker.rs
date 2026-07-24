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
use super::source::{
    ActiveLinkedCheckout, ResolvedSource, Strategy, read_version_from_manifest_dir,
    resolve_strategy_to_source,
};
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

/// How long a load waits, after its own walk succeeds, for a concurrent
/// load that owned a skipped in-flight dependency to commit or fail.
/// Generous (builds can take minutes); the timeout exists so a wedged
/// concurrent load surfaces as a typed error, never a hang.
pub(super) const SKIPPED_IN_FLIGHT_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// A single requirer edge into a resolved package: the parent
/// [`PackageRef`] that declared the dependency (or `None` for a
/// top-level `add_module` call).
///
/// [`PackageRef`]: streamlib_idents::PackageRef
#[derive(Debug, Clone)]
pub(crate) struct RequirerRecord {
    /// The parent package that pulled this dependency in, or `None` when
    /// the package was the root of a top-level `add_module` call.
    pub requirer: Option<streamlib_idents::PackageRef>,
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

    /// Publish [`ConcurrentPackageLoadOutcome::Committed`]. Called by the
    /// whole-load commit after the ledger records are written.
    pub(super) fn publish_committed(&self) {
        self.publish(ConcurrentPackageLoadOutcome::Committed);
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
    /// Already committed — requirer recorded; skip. A re-encounter at a
    /// different version deduped to the first-resolved winner lands here too.
    SkipAlreadyCommittedWinner,
    /// In flight on THIS load (a same-load diamond re-encounter) —
    /// requirer recorded; skip with no wait entry. Waiting on the owner
    /// would be waiting on ourselves: this load's placeholders only flip
    /// at its own whole-load commit, which runs after the wait phase.
    SkipOwnedByThisLoad,
    /// In flight on a concurrent load — requirer recorded; skip locally and
    /// verify the owner's outcome at the end of this walk via the carried
    /// signal. A concurrent re-encounter at a different version deduped to
    /// the winner lands here too.
    SkipInFlightWinner(Arc<PackageResolutionCompletionSignal>),
}

/// Runtime-lifetime memo of every package resolved by the live module
/// walker, keyed by `@org/name`. Persists across every `add_module` call
/// on a [`Runner`] so two independently-rooted diamond branches, two
/// successive `add_module` calls, or two concurrent loads dedupe the same
/// `@org/name` to a single first-resolved winner instead of
/// double-registering. A later encounter at a different concrete version
/// warns and reuses the winner (single-version model; an incompatibility
/// surfaces at runtime).
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
                tracing::warn!(
                    package = %pkg_ref,
                    resolved_version = %record.version,
                    resolved_from = %record.source_path.display(),
                    conflicting_version = %on_disk_version,
                    conflicting_from = %manifest_dir.display(),
                    "single-version gate: package already resolved to a \
                     different version from an earlier source — keeping the \
                     first-resolved winner and ignoring the later encounter \
                     (single-version model; if the two are incompatible it \
                     will surface at runtime)",
                );
            } else if record.source_path != manifest_dir {
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
                    "single-version gate: already resolved — skipping \
                     re-registration and re-recursion",
                );
                Ok(SingleVersionGateOutcome::SkipAlreadyCommittedWinner)
            }
            PackageResolutionState::InFlightPlaceholder {
                completion_signal,
                owner_load_id,
                ..
            } => {
                if *owner_load_id == load_id {
                    tracing::debug!(
                        package = %pkg_ref,
                        version = %on_disk_version,
                        "single-version gate: already in flight on THIS load \
                         (diamond re-encounter) — skipping with no wait entry",
                    );
                    return Ok(SingleVersionGateOutcome::SkipOwnedByThisLoad);
                }
                tracing::debug!(
                    package = %pkg_ref,
                    version = %on_disk_version,
                    owner_load_id = *owner_load_id,
                    "single-version gate: already in flight on a concurrent \
                     load — skipping locally; outcome verified at end of walk",
                );
                Ok(SingleVersionGateOutcome::SkipInFlightWinner(
                    Arc::clone(completion_signal),
                ))
            }
        }
    }

    /// Flip this package's in-flight placeholder to a committed record.
    /// Returns the record (final requirer list, read under the same lock
    /// as the flip) plus the completion signal for the caller to publish
    /// AFTER its ledger writes land — publishing is deliberately not done
    /// here so a waiter never observes `Committed` before the ledger.
    /// Requirers accumulated on the placeholder while in flight are
    /// preserved on the committed record.
    pub(super) fn flip_in_flight_placeholder_to_committed(
        &self,
        pkg_ref: &streamlib_idents::PackageRef,
    ) -> Option<(
        ResolvedPackageRecord,
        Arc<PackageResolutionCompletionSignal>,
    )> {
        let mut packages = self.packages.lock();
        match packages.remove(pkg_ref) {
            Some(PackageResolutionState::InFlightPlaceholder {
                record,
                completion_signal,
                ..
            }) => {
                let record_for_caller = record.clone();
                packages.insert(
                    pkg_ref.clone(),
                    PackageResolutionState::Committed { record },
                );
                Some((record_for_caller, completion_signal))
            }
            // Defensive: only the owning load flips its placeholder.
            Some(other) => {
                packages.insert(pkg_ref.clone(), other);
                None
            }
            None => None,
        }
    }

    /// Whether the package is currently mid-resolution on some load.
    pub(crate) fn is_package_in_flight(&self, pkg_ref: &streamlib_idents::PackageRef) -> bool {
        matches!(
            self.packages.lock().get(pkg_ref),
            Some(PackageResolutionState::InFlightPlaceholder { .. })
        )
    }

    /// Remove a package's committed resolution so a later `add_module`
    /// re-resolves it from scratch. Used by `remove_module`; in-flight
    /// placeholders are never removed here (removal refuses while a load
    /// is in flight).
    pub(crate) fn remove_committed_resolution(&self, pkg_ref: &streamlib_idents::PackageRef) {
        let mut packages = self.packages.lock();
        if matches!(
            packages.get(pkg_ref),
            Some(PackageResolutionState::Committed { .. })
        ) {
            packages.remove(pkg_ref);
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

    /// Test observability: every package ref with any resolution state.
    #[cfg(test)]
    pub(crate) fn resolved_package_refs(&self) -> Vec<streamlib_idents::PackageRef> {
        self.packages.lock().keys().cloned().collect()
    }
}

/// RAII cleanup for an armed in-flight placeholder: any failure exit
/// between the gate and the whole-load commit drops this guard, which
/// removes the placeholder and publishes `Failed` — the memo can never
/// wedge in a permanent "resolving" state, and concurrent skippers fail
/// loudly. Guards accumulate on the walk context and are consumed by the
/// whole-load commit, which flips each placeholder then disarms.
pub(super) struct InFlightPlaceholderGuard<'memo> {
    resolution_memo: &'memo ResolutionMemo,
    package: Option<streamlib_idents::PackageRef>,
}

impl<'memo> InFlightPlaceholderGuard<'memo> {
    fn arm(resolution_memo: &'memo ResolutionMemo, package: streamlib_idents::PackageRef) -> Self {
        Self {
            resolution_memo,
            package: Some(package),
        }
    }

    /// The package this guard's placeholder covers. `None` only after
    /// [`Self::disarm`], which consumes the guard.
    pub(super) fn package_ref(&self) -> &streamlib_idents::PackageRef {
        self.package
            .as_ref()
            .expect("guard queried after disarm — commit consumes the guard")
    }

    /// Neutralize the guard after its placeholder has been flipped to
    /// committed. Dropping without disarm publishes `Failed`.
    pub(super) fn disarm(mut self) {
        self.package.take();
    }
}

impl Drop for InFlightPlaceholderGuard<'_> {
    fn drop(&mut self) {
        if let Some(package) = self.package.take() {
            self.resolution_memo.abandon_in_flight_resolution(&package);
        }
    }
}

/// Everything one top-level module load threads through its recursive
/// dependency walk: the immutable per-load inputs (node, orchestrator,
/// memo, staging buffer, lockfile / link context) plus the mutable
/// accumulation the whole-load commit consumes (armed placeholder
/// guards, skipped-in-flight wait entries, committed-dependency requirer
/// edges).
pub(super) struct ModuleLoadWalkContext<'load> {
    pub iceoryx2_node: &'load Iceoryx2Node,
    pub orchestrator: Option<&'load Arc<dyn BuildOrchestrator>>,
    pub sink: &'load dyn BuildEventSink,
    pub resolution_memo: &'load ResolutionMemo,
    pub load_id: u64,
    pub locked: Option<&'load LockedResolution>,
    pub link: Option<&'load ActiveLinkedCheckout>,
    /// Per-load registration staging buffer — nothing lands in the
    /// global registries until the whole-load commit.
    pub staging: &'load Arc<super::staging::ModuleLoadRegistrationStaging>,
    /// Every [`PackageRef`] currently on the recursion stack (O(1)
    /// membership for cycle detection).
    ///
    /// [`PackageRef`]: streamlib_idents::PackageRef
    pub seen: HashSet<streamlib_idents::PackageRef>,
    /// Recursion path in insertion order, so the dependency-cycle error
    /// carries the actual edge that re-entered.
    pub path: Vec<streamlib_idents::PackageRef>,
    /// Dependencies skipped because a CONCURRENT load had them in flight
    /// — verified at the end of the top-level walk.
    pub skipped_in_flight: Vec<SkippedInFlightDependency>,
    /// Armed placeholder guards for every package THIS load resolved —
    /// flipped + disarmed by the whole-load commit; dropped (abandon +
    /// `Failed` published) on any failure exit.
    pub armed_placeholder_guards: Vec<InFlightPlaceholderGuard<'load>>,
    /// `(dependency, requirer)` edges into packages an EARLIER load
    /// committed — appended onto their ledger records at commit.
    pub committed_dependency_requirer_edges:
        Vec<(streamlib_idents::PackageRef, streamlib_idents::PackageRef)>,
}

impl ModuleLoadWalkContext<'_> {
    /// The package that required the node currently being resolved — the
    /// penultimate element of the recursion path (the last is the current
    /// node). `None` for a top-level `add_module` (path length < 2).
    fn requirer(&self) -> Option<&streamlib_idents::PackageRef> {
        (self.path.len() >= 2).then(|| &self.path[self.path.len() - 2])
    }
}

/// Recursive worker: resolves the [`Strategy`] to a source, materializes
/// via the injected [`BuildOrchestrator`] when a build is required,
/// validates the manifest's identity + version range, stages the
/// package's schemas, walks dependencies (each routed through this same
/// helper), then stages the package's processors. Nothing is applied to
/// the global registries here — the whole-load commit does that after
/// the entire walk succeeds.
pub(super) fn add_module_recursively(
    walk_context: &mut ModuleLoadWalkContext<'_>,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
) -> std::result::Result<(), AddModuleError> {
    let pkg_ref = module.package_ref();
    if !walk_context.seen.insert(pkg_ref.clone()) {
        let mut cycle = walk_context.path.clone();
        cycle.push(pkg_ref);
        return Err(AddModuleError::DependencyCycleDetected { cycle });
    }
    walk_context.path.push(pkg_ref.clone());
    let result = add_module_recursive_body(walk_context, module, strategy);
    walk_context.seen.remove(&pkg_ref);
    walk_context.path.pop();
    result
}

fn add_module_recursive_body(
    walk_context: &mut ModuleLoadWalkContext<'_>,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
) -> std::result::Result<(), AddModuleError> {
    use crate::core::config::ProjectConfig;

    let pkg_ref = module.package_ref();

    // Resolve where the source lives (pure filesystem / cache / git),
    // then materialize via the orchestrator if a build is required. `link`,
    // when present, redirects a checkout-present package to the linked
    // checkout regardless of `strategy` (npm-link semantics; locked runs pass
    // `None`).
    let manifest_dir = match resolve_strategy_to_source(&strategy, &pkg_ref, walk_context.link)? {
        ResolvedSource::Ready(dir) => dir,
        ResolvedSource::NeedsBuild(request) => match walk_context.orchestrator {
            // An orchestrator is wired — materialize (fetch/build/stage).
            Some(orch) => {
                let staged = orch.materialize(&request, walk_context.sink).map_err(|e| {
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
                });
            }
        },
    };

    let on_disk_version = read_version_from_manifest_dir(&manifest_dir)?;

    // Read the manifest; authoritative source of identity for the
    // package at the resolved location. Thread the active link so the
    // manifest's bare-name schema-dep resolution resolves a dep present in the
    // checkout from the checkout (the load-time half of the link dev
    // loop). `link` is `None` on locked runs / no active link → unchanged.
    let config =
        ProjectConfig::load_with_link(&manifest_dir, walk_context.link.map(|l| l.checkout()))
            .map_err(|e| AddModuleError::ManifestLoadFailed {
                module: module.clone(),
                source_path: manifest_dir.clone(),
                detail: e.to_string(),
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
        // A locked run resolves each dep to its lockfile `Exact` pin, so a
        // mismatch here is a slot whose on-disk version drifted from the pin
        // (an in-place republish after install) — a reproducibility/integrity
        // failure, hard by the locked-run contract. Only the live (install- or
        // dev-derived) walk is lenient: a declared range that no installed
        // version satisfies warns and loads the installed version, matching the
        // install-time resolver's single-version model — a version mismatch
        // never blocks a live load.
        if walk_context.locked.is_some() {
            return Err(AddModuleError::VersionRangeUnsatisfied {
                module: module.clone(),
                found: on_disk_version,
                source_path: manifest_dir.clone(),
            });
        }
        let requirer_for_warning = walk_context
            .requirer()
            .map(ToString::to_string)
            .unwrap_or_else(|| "top-level add_module".to_string());
        tracing::warn!(
            module = %module,
            requested_range = %module.version,
            installed_version = %on_disk_version,
            requirer = %requirer_for_warning,
            source_path = %manifest_dir.display(),
            "loading the installed version although the requirer declared a \
             range no installed version satisfies — no matching version \
             installed; loading it anyway (single-version model: a version \
             mismatch never blocks a load)",
        );
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
    // loads dedupe the same `@org/name` to one first-resolved winner here
    // instead of double-registering. A later encounter resolving a
    // DIFFERENT concrete version warns and reuses the winner (single-version
    // model; an incompatibility surfaces at compile-on-install for source
    // packages — a consumer codegen'd against the winner's older schema idents
    // fails the ident lookup / type-check — or at runtime for prebuilt slots),
    // never blocks the load.
    //
    // First encounter inserts an in-flight placeholder under the same
    // lock as the check (no check-then-commit window for a concurrent
    // load to slip through). A re-encounter skips re-registration +
    // re-recursion; when the winner is in flight on a CONCURRENT load, the
    // skip is recorded in `skipped_in_flight` and the owner's outcome is
    // verified at the end of this walk — nobody ever blocks mid-walk, so
    // concurrent walks cannot deadlock.
    let requirer_package = walk_context.requirer().cloned();
    let requirer = RequirerRecord {
        requirer: requirer_package.clone(),
    };
    match walk_context.resolution_memo.gate(
        walk_context.load_id,
        &pkg_ref,
        on_disk_version,
        &manifest_dir,
        requirer,
    )? {
        SingleVersionGateOutcome::SkipAlreadyCommittedWinner => {
            // The package belongs to an EARLIER committed load; record the
            // requirer edge so this load's commit appends it onto the
            // dependency's ledger record.
            if let Some(requirer_package) = requirer_package {
                walk_context
                    .committed_dependency_requirer_edges
                    .push((pkg_ref.clone(), requirer_package));
            }
            return Ok(());
        }
        SingleVersionGateOutcome::SkipOwnedByThisLoad => return Ok(()),
        SingleVersionGateOutcome::SkipInFlightWinner(completion_signal) => {
            walk_context
                .skipped_in_flight
                .push(SkippedInFlightDependency {
                    package: pkg_ref.clone(),
                    version: on_disk_version,
                    completion_signal,
                });
            return Ok(());
        }
        SingleVersionGateOutcome::ProceedAsFirstResolution => {}
    }

    // The placeholder is armed: any failure exit below drops the guard
    // (via the walk context), which removes the placeholder and publishes
    // `Failed` — a retried add_module re-runs the full resolution, and
    // concurrent loads that skipped this package fail loudly instead of
    // assuming it registered. On success the whole-load commit flips the
    // placeholder and disarms.
    let placeholder_guard =
        InFlightPlaceholderGuard::arm(walk_context.resolution_memo, pkg_ref.clone());
    walk_context
        .armed_placeholder_guards
        .push(placeholder_guard);

    // Schemas are leaves — stage before recursing into deps.
    register_package_schemas(&manifest_dir, &config, walk_context.staging, &pkg_ref).map_err(
        |e| AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        },
    )?;

    // Walk transitive deps, each routed through this same helper.
    //
    // Locked mode forces every dep edge to its lockfile pin (the pinned
    // installed-cache slot, loaded as-is with `NeverBuild`) instead of
    // deriving a live source strategy — so a locked run never touches the
    // package source / git / a `.slpkg` re-fetch / a build. A dep the lockfile
    // doesn't pin is a stale-lockfile hard error, not a silent live
    // resolve.
    for (dep_ref, spec) in &config.dependencies {
        let (dep_ident, dep_strategy) = match walk_context.locked {
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
        add_module_recursively(walk_context, dep_ident, dep_strategy)?;
    }

    // Now stage this package's own processors. Thread the active link so a
    // config schema dep present in the checkout resolves from the checkout too
    // (load-time link dev loop); `None` off a link → unchanged.
    register_manifest_processors(
        walk_context.iceoryx2_node,
        &manifest_dir,
        &config,
        walk_context.link.map(|l| l.checkout()),
        walk_context.staging,
        &pkg_ref,
    )
    .map_err(|e| AddModuleError::LoadProjectFailed {
        module: module.clone(),
        source: Box::new(e),
    })?;

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
/// version-range deps resolve by version from the configured package source.
///
/// [`ModuleIdent`]: streamlib_idents::ModuleIdent
fn derive_dep_strategy_and_ident(
    consumer_dir: &std::path::Path,
    dep_ref: &streamlib_idents::PackageRef,
    spec: &streamlib_idents::DependencySpec,
    patch: &std::collections::BTreeMap<
        streamlib_idents::PackageRef,
        streamlib_idents::DependencySpec,
    >,
) -> Result<(streamlib_idents::ModuleIdent, Strategy)> {
    use streamlib_idents::{DependencySpec, ModuleIdent, SemVerRange};

    // Version-range deps carry a range that constrains resolution even when
    // patched. Path / git deps don't — the source's manifest version
    // becomes authoritative (range = any).
    let declared_range = match spec {
        DependencySpec::Version(r) => r.version.clone(),
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
        DependencySpec::Version(r) => {
            if is_patch {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' is a version range — a patch \
                     must redirect a dependency to a `path:` or `git:` source, \
                     not another version range.",
                )));
            }
            // A version-range dependency resolves by version from the configured
            // package source's generic store: pull the `.slpkg` and build it from
            // source on the host (IfStale prefers a matching prebuilt). The
            // package source location comes from the environment
            // (STREAMLIB_PACKAGE_SOURCE).
            Strategy::ByVersion {
                version_req: r.version.clone(),
                build: BuildPolicy::IfStale,
            }
        }
    };

    let ident = ModuleIdent::new(dep_ref.org.clone(), dep_ref.name.clone(), declared_range);
    Ok((ident, strategy))
}
