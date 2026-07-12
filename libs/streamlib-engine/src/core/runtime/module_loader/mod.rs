// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading subsystem: the [`Strategy`] source enum, the injected
//! [`BuildOrchestrator`] build seam, and the eager-async
//! [`Runner::add_module`] / [`Runner::add_module_with`] /
//! [`Runner::await_modules`] public API surface.
//!
//! The engine resolves *where* a package's source lives and *loads* the
//! staged result; it never invokes a toolchain. A [`BuildPolicy`] that
//! requires a (re)build is handed to the injected [`BuildOrchestrator`],
//! which lives outside the engine. Loads run eagerly on the runtime's
//! existing tokio handle and surface as [`AddedModule`] futures.
//!
//! Files in this directory:
//!
//! - [`errors`] — `AddModuleError`, `RemoveModuleError` typed enums.
//! - [`source`] — the [`Strategy`] enum + source resolver.
//! - [`build_orchestrator`] — the injected [`BuildOrchestrator`] trait
//!   and its request/result/event types.
//! - [`added_module`] — the eager [`AddedModule`] future +
//!   [`ModuleLoadEvent`].
//! - [`recursive_walker`] — recursive transitive-dep walk, cycle
//!   detection, per-dep strategy derivation, and the materialize step.
//! - [`processor_registration`] — manifest-driven processor registration.
//! - [`schema_registration`] — manifest-driven schema registration.
//! - [`slpkg`] — `.slpkg` archive extraction.
//!
//! [`Runner::add_module`]: super::Runner::add_module
//! [`Runner::add_module_with`]: super::Runner::add_module_with
//! [`Runner::await_modules`]: super::Runner::await_modules

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{broadcast, mpsc};

use super::runtime::TokioRuntimeVariant;
use super::Runner;
use crate::iceoryx2::Iceoryx2Node;

mod added_module;
mod build_orchestrator;
mod errors;
mod locked;
mod processor_registration;
mod recursive_walker;
mod schema_registration;
mod slpkg;
mod source;

#[cfg(test)]
mod tests;

pub use added_module::{AddedModule, LoadedModule, ModuleLoadEvent};
pub use build_orchestrator::{
    BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy, BuildRequest,
    BuildSource, BuildStream, StagedArtifact,
};
pub use errors::{AddModuleError, RemoveModuleError};
pub(crate) use locked::LockedResolution;
pub use processor_registration::host_target_triple;
pub(crate) use recursive_walker::ResolutionMemo;
pub use slpkg::extract_slpkg_to_cache;
pub use source::{ArtifactChecksum, SemVerRange, Strategy};

use added_module::MODULE_EVENT_CHANNEL_CAPACITY;

/// Engine-side [`BuildEventSink`] that re-emits a [`BuildOrchestrator`]'s
/// build diagnostics as [`ModuleLoadEvent`]s on the load's broadcast
/// channel AND through `tracing` — never to `stdout`.
struct ModuleEventSink {
    ident: streamlib_idents::ModuleIdent,
    events: broadcast::Sender<ModuleLoadEvent>,
}

impl BuildEventSink for ModuleEventSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => {
                tracing::info!(module = %self.ident, language, "module build started");
                let _ = self.events.send(ModuleLoadEvent::Building {
                    ident: self.ident.clone(),
                    language,
                });
            }
            BuildEvent::Line { stream, line } => {
                match stream {
                    BuildStream::Stderr => {
                        tracing::debug!(module = %self.ident, build_log = %line)
                    }
                    BuildStream::Stdout => {
                        tracing::trace!(module = %self.ident, build_log = %line)
                    }
                };
                let _ = self.events.send(ModuleLoadEvent::BuildLog {
                    ident: self.ident.clone(),
                    line,
                });
            }
            BuildEvent::Finished { language } => {
                tracing::debug!(module = %self.ident, language, "module build finished");
            }
        }
    }
}

/// Monotonic id for each top-level `add_module` load. Identifies the
/// load in the resolution memo's in-flight placeholders and guards the
/// `loading_modules` completion-removal against a same-package overwrite
/// by a later load.
static NEXT_MODULE_LOAD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The blocking body of a single module load. Runs on `spawn_blocking`:
/// it resolves the strategy, materializes via the orchestrator when a
/// build is required (blocking), recursively loads transitive deps, and
/// registers schemas + processors. Emits terminal events on `events`.
fn run_module_load(
    iceoryx2_node: Iceoryx2Node,
    orchestrator: Option<Arc<dyn BuildOrchestrator>>,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
    events: broadcast::Sender<ModuleLoadEvent>,
    resolution_memo: Arc<ResolutionMemo>,
    load_id: u64,
    locked: Option<Arc<LockedResolution>>,
) -> std::result::Result<LoadedModule, AddModuleError> {
    let start = Instant::now();
    let _ = events.send(ModuleLoadEvent::Started {
        ident: module.clone(),
    });
    let sink = ModuleEventSink {
        ident: module.clone(),
        events: events.clone(),
    };
    let mut seen: HashSet<streamlib_idents::PackageRef> = HashSet::new();
    let mut path: Vec<streamlib_idents::PackageRef> = Vec::new();
    let mut skipped_in_flight: Vec<recursive_walker::SkippedInFlightDependency> = Vec::new();
    let result = recursive_walker::add_module_recursively(
        &iceoryx2_node,
        orchestrator.as_ref(),
        &sink,
        module.clone(),
        strategy,
        &mut seen,
        &mut path,
        &resolution_memo,
        load_id,
        &mut skipped_in_flight,
        locked.as_deref(),
    );
    // End-of-walk verification: this walk skipped some packages because a
    // CONCURRENT load had them in flight at the same version. Before
    // reporting success, verify each owner actually committed — an owner
    // that failed (or wedged) means this load's graph is missing a
    // registration, and Ok would be a false success. Mid-walk nobody ever
    // blocks; these waits depend only on other loads' per-package commits,
    // which flip as their subtrees unwind without needing this waiter —
    // structurally deadlock-free.
    let result = result.and_then(|()| {
        use recursive_walker::{ConcurrentPackageLoadOutcome, SKIPPED_IN_FLIGHT_WAIT_TIMEOUT};
        for skipped in skipped_in_flight {
            match skipped
                .completion_signal
                .wait_for_outcome(SKIPPED_IN_FLIGHT_WAIT_TIMEOUT)
            {
                Some(ConcurrentPackageLoadOutcome::Committed) => {}
                Some(ConcurrentPackageLoadOutcome::Failed) => {
                    return Err(AddModuleError::ConcurrentLoadOfSkippedDependencyFailed {
                        package: skipped.package,
                        version: skipped.version,
                    });
                }
                None => {
                    return Err(AddModuleError::ConcurrentLoadOfSkippedDependencyTimedOut {
                        package: skipped.package,
                        version: skipped.version,
                        waited_secs: SKIPPED_IN_FLIGHT_WAIT_TIMEOUT.as_secs(),
                    });
                }
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            let _ = events.send(ModuleLoadEvent::Completed {
                ident: module.clone(),
                took: start.elapsed(),
            });
            Ok(LoadedModule { ident: module })
        }
        Err(e) => {
            let _ = events.send(ModuleLoadEvent::Failed {
                ident: module.clone(),
                error: e.to_string(),
            });
            Err(e)
        }
    }
}

impl Runner {
    // =========================================================================
    // Module Loading — public API surface
    // =========================================================================

    /// Load a `streamlib.yaml`-packaged module by typed
    /// [`streamlib_idents::ModuleIdent`] from the installed-package
    /// cache. Conservative: never builds, fails loud if the package is
    /// not in the cache. Returns an [`AddedModule`] (a [`Future`] whose
    /// work is already running); a cache hit resolves almost immediately.
    ///
    /// For workspace dev, runtime-authored packages, or git sources —
    /// anything rebuildable from source — use [`Runner::add_module_with`]
    /// with an explicit [`Strategy`] + [`BuildPolicy`].
    ///
    /// [`Future`]: std::future::Future
    #[must_use = "the returned AddedModule cancels on drop — await it or pass it to await_modules"]
    pub fn add_module(&self, module: streamlib_idents::ModuleIdent) -> AddedModule {
        self.add_module_with(module, Strategy::InstalledCache)
    }

    /// Load a module via an explicit [`Strategy`]. The work is spawned
    /// eagerly onto the runtime's tokio handle; the returned
    /// [`AddedModule`] is a [`Future`] you `.await` (or drive via
    /// [`Runner::await_modules`]).
    ///
    /// Transitive dependencies declared in the loaded module's
    /// `streamlib.yaml` are recursively routed through the same flow.
    /// Cycles surface as [`AddModuleError::DependencyCycleDetected`].
    ///
    /// [`Future`]: std::future::Future
    #[must_use = "the returned AddedModule cancels on drop — await it or pass it to await_modules"]
    pub fn add_module_with(
        &self,
        module: streamlib_idents::ModuleIdent,
        strategy: Strategy,
    ) -> AddedModule {
        self.spawn_module_load(module, strategy, None)
    }

    /// Shared spawn body for [`Self::add_module_with`] (live resolution,
    /// `locked = None`) and the locked-run path (strict-from-lockfile,
    /// `locked = Some`). The `locked` context, when present, forces every
    /// transitive dep edge to its lockfile pin inside the recursive walk.
    fn spawn_module_load(
        &self,
        module: streamlib_idents::ModuleIdent,
        strategy: Strategy,
        locked: Option<Arc<LockedResolution>>,
    ) -> AddedModule {
        let pkg_ref = module.package_ref();
        let load_id = NEXT_MODULE_LOAD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Subscribe BEFORE spawning so a driver can't miss early events.
        let (tx, initial_rx) = broadcast::channel(MODULE_EVENT_CHANNEL_CAPACITY);

        // Mark in-flight so `start()` can refuse to run the graph while
        // loads are pending. Keyed by package ref; the load id lets the
        // completion below remove only ITS OWN entry — a later load of
        // the same package ref that overwrote this entry stays tracked.
        self.loading_modules
            .lock()
            .insert(pkg_ref.clone(), (load_id, module.clone()));

        let node = self.iceoryx2_node.clone();
        let orchestrator = self.build_orchestrator.lock().clone();
        let loading = Arc::clone(&self.loading_modules);
        let memo = Arc::clone(&self.resolution_memo);
        let events = tx.clone();
        let module_for_task = module.clone();
        let pkg_for_task = pkg_ref;

        let join = self.tokio_runtime_variant.handle().spawn_blocking(move || {
            let result = run_module_load(
                node,
                orchestrator,
                module_for_task,
                strategy,
                events,
                memo,
                load_id,
                locked,
            );
            {
                let mut loading = loading.lock();
                if loading
                    .get(&pkg_for_task)
                    .is_some_and(|(owner_load_id, _)| *owner_load_id == load_id)
                {
                    loading.remove(&pkg_for_task);
                }
            }
            result
        });

        AddedModule::new(module, join, tx, initial_rx)
    }

    /// Load every package pinned in an application lockfile
    /// ([`streamlib_idents::APP_LOCKFILE_NAME`], written by `streamlib
    /// install`) strictly from the installed-package cache — the **locked
    /// run**. No live re-resolution: every package, top-level and
    /// transitive, is forced to its pinned version's cache slot, so the run
    /// works **offline** (no registry, git, or `.slpkg` fetch reachable)
    /// and is byte-reproducible against the pinned set.
    ///
    /// The lockfile is the flat resolved closure, so each pinned package is
    /// spawned as a top-level load; the single-version gate dedups the
    /// transitive re-encounters. Returns one [`AddedModule`] per pinned
    /// package — drive them via [`Runner::await_modules`].
    ///
    /// A dep a manifest declares that the lockfile doesn't pin, or a pinned
    /// package whose cache slot is missing, fails loud with a typed error
    /// naming `streamlib install` — never a silent live resolve.
    ///
    /// [`Runner::await_modules`]: Self::await_modules
    pub fn add_modules_from_lockfile(
        &self,
        lockfile_path: &std::path::Path,
    ) -> std::result::Result<Vec<AddedModule>, AddModuleError> {
        let locked = Arc::new(LockedResolution::from_lockfile_path(lockfile_path)?);
        let mut added = Vec::new();
        for (pkg_ref, _version) in locked.pinned_packages() {
            // Each pinned package resolves to its own slot as a top-level
            // load; `required_by` is "top-level" for the root add.
            let (ident, strategy) = locked.resolve(&pkg_ref, "top-level")?;
            added.push(self.spawn_module_load(ident, strategy, Some(Arc::clone(&locked))));
        }
        Ok(added)
    }

    /// Blocking convenience for [`Self::add_modules_from_lockfile`]: drive
    /// every pinned load to completion. For simple `fn main` examples and
    /// tests. Returns [`AddModuleError::BlockingCallFromAsyncContext`] when
    /// called from inside a tokio runtime — use the async surface there.
    pub fn add_modules_from_lockfile_blocking(
        &self,
        lockfile_path: &std::path::Path,
    ) -> std::result::Result<(), AddModuleError> {
        if matches!(
            self.tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ) {
            // No module ident to name here (this is a batch load); surface
            // the same async-context guard as the single-module path.
            return Err(AddModuleError::BlockingCallFromAsyncContext {
                module: streamlib_idents::ModuleIdent::new(
                    streamlib_idents::Org::new("tatolab").expect("static org valid"),
                    streamlib_idents::Package::new("locked-run").expect("static package valid"),
                    SemVerRange::Any,
                ),
            });
        }
        let added = self.add_modules_from_lockfile(lockfile_path)?;
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.block_on(async {
                self.await_modules(added, |_| {}).await
            }),
            TokioRuntimeVariant::ExternalTokioHandle(_) => unreachable!("guarded above"),
        }
    }

    /// Synchronous convenience: drive a single cache-only load to
    /// completion. For simple `fn main` examples and tests. Returns
    /// [`AddModuleError::BlockingCallFromAsyncContext`] (never panics)
    /// when called from inside a tokio runtime — use the async surface
    /// there.
    pub fn add_module_blocking(
        &self,
        module: streamlib_idents::ModuleIdent,
    ) -> std::result::Result<(), AddModuleError> {
        self.add_module_with_blocking(module, Strategy::InstalledCache)
    }

    /// Synchronous convenience for [`Runner::add_module_with`]. See
    /// [`Runner::add_module_blocking`] for the async-context caveat.
    pub fn add_module_with_blocking(
        &self,
        module: streamlib_idents::ModuleIdent,
        strategy: Strategy,
    ) -> std::result::Result<(), AddModuleError> {
        // Refuse before spawning: block_on from a tokio worker panics.
        if matches!(
            self.tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ) {
            return Err(AddModuleError::BlockingCallFromAsyncContext { module });
        }
        let added = self.add_module_with(module, strategy);
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.block_on(added).map(|_| ()),
            // Guarded above — the external arm returned already.
            TokioRuntimeVariant::ExternalTokioHandle(_) => unreachable!(),
        }
    }

    /// Drive a batch of [`AddedModule`] loads concurrently, invoking
    /// `on_event` for every [`ModuleLoadEvent`] from every module as it
    /// happens (interleaved — not one module at a time). Returns the
    /// first load error if any module failed; every failure is also
    /// surfaced through `on_event` as [`ModuleLoadEvent::Failed`].
    pub async fn await_modules<I, F>(
        &self,
        modules: I,
        mut on_event: F,
    ) -> std::result::Result<(), AddModuleError>
    where
        I: IntoIterator<Item = AddedModule>,
        F: FnMut(ModuleLoadEvent),
    {
        let handle = self.tokio_runtime_variant.handle();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<ModuleLoadEvent>();
        let (res_tx, mut res_rx) = mpsc::unbounded_channel::<AddModuleError>();

        for mut added in modules {
            // Forward this module's progress events into the shared sink.
            if let Some(mut rx) = added.take_event_receiver() {
                let etx = ev_tx.clone();
                handle.spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(ev) => {
                                if etx.send(ev).is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }
            // Await this module's terminal result.
            let rtx = res_tx.clone();
            handle.spawn(async move {
                if let Err(e) = added.await {
                    let _ = rtx.send(e);
                }
            });
        }
        drop(ev_tx);
        drop(res_tx);

        let mut first_err: Option<AddModuleError> = None;
        loop {
            tokio::select! {
                Some(ev) = ev_rx.recv() => on_event(ev),
                Some(err) = res_rx.recv() => {
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
                else => break,
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Unload a previously-added module.
    ///
    /// **Not yet implemented** — module-level unload requires the
    /// hot-reload lifecycle work that's out of scope for the current
    /// All-Dynamic Package Loading milestone. Returns
    /// [`RemoveModuleError::HotReloadLifecycleNotYetImplemented`]
    /// without altering runtime state.
    pub fn remove_module(
        &self,
        module: streamlib_idents::ModuleIdent,
    ) -> std::result::Result<(), RemoveModuleError> {
        Err(RemoveModuleError::HotReloadLifecycleNotYetImplemented { module })
    }

    /// The set of modules whose loads have not yet settled. Used by
    /// [`Runner::start`] to refuse running the graph while loads are
    /// pending.
    pub(crate) fn pending_module_loads(&self) -> Vec<streamlib_idents::ModuleIdent> {
        self.loading_modules
            .lock()
            .values()
            .map(|(_, module)| module.clone())
            .collect()
    }
}
