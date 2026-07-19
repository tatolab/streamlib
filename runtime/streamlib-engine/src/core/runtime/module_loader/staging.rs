// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::descriptors::{ProcessorDescriptor, SchemaIdent};
use crate::core::processors::DynamicProcessorConstructorFn;
use streamlib_plugin_abi::ProcessorVTable;

/// Serializes every module-load commit and every `remove_module` against
/// each other. Held briefly — never while waiting on another load.
pub(crate) static MODULE_REGISTRY_COMMIT_LOCK: Mutex<()> = Mutex::new(());

/// A plugin dylib image retained for the process lifetime. `dlclose` is
/// never called: registered vtables, `'static` descriptor strings, and
/// host-service bridge state point into the image, so unloading it would
/// dangle them. A rebuilt plugin at the same path is a NEW image — entries
/// are never deduplicated by path.
pub(crate) struct RetainedPluginLibrary {
    /// The live `dlopen` handle. Present to keep the image mapped; never
    /// read after retention.
    #[allow(dead_code)]
    pub library: libloading::Library,
    /// Path the image was loaded from (diagnostics only).
    #[allow(dead_code)]
    pub dylib_path: std::path::PathBuf,
    /// The package whose load first retained this image.
    #[allow(dead_code)]
    pub first_loaded_for: streamlib_idents::PackageRef,
}

/// Keeps loaded dylib plugin images alive for the process lifetime.
static LOADED_PLUGIN_LIBRARIES: std::sync::LazyLock<Mutex<Vec<RetainedPluginLibrary>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Number of plugin dylib images retained for the process lifetime.
pub fn loaded_plugin_library_count() -> usize {
    LOADED_PLUGIN_LIBRARIES.lock().len()
}

/// One schema registration staged by a module load, pending commit.
pub(super) struct StagedSchemaRegistration {
    pub canonical_id: String,
    pub body: Arc<str>,
    pub owner_package: streamlib_idents::PackageRef,
}

/// How a staged processor registration dispatches once committed.
pub(super) enum StagedProcessorRegistrationKind {
    /// Cdylib / in-process vtable registration → `register_via_vtable`.
    VTable {
        vtable: &'static ProcessorVTable,
        cdylib_resident: bool,
    },
    /// Subprocess host-wrapper registration → `register_dynamic`.
    Dynamic {
        constructor: DynamicProcessorConstructorFn,
    },
}

/// One processor registration staged by a module load, pending commit.
pub(super) struct StagedProcessorRegistration {
    pub descriptor: ProcessorDescriptor,
    pub kind: StagedProcessorRegistrationKind,
    pub owner_package: streamlib_idents::PackageRef,
}

/// One dlopen'd plugin image staged by a module load. Retained for the
/// process lifetime whether the load commits or fails.
pub(super) struct StagedPluginLibrary {
    pub library: libloading::Library,
    pub dylib_path: std::path::PathBuf,
    pub owner_package: streamlib_idents::PackageRef,
}

/// Per-top-level-load staging buffer for every registration a module load
/// produces. Nothing here is visible to the global registries until
/// [`commit_staged_registrations_locked`] runs; dropping the staging (the
/// failure path) discards schemas + processors and retains dylib images.
pub(crate) struct ModuleLoadRegistrationStaging {
    schemas: Mutex<Vec<StagedSchemaRegistration>>,
    processors: Mutex<Vec<StagedProcessorRegistration>>,
    libraries: Mutex<Vec<StagedPluginLibrary>>,
}

impl ModuleLoadRegistrationStaging {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            schemas: Mutex::new(Vec::new()),
            processors: Mutex::new(Vec::new()),
            libraries: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn stage_schema(
        &self,
        canonical_id: String,
        body: Arc<str>,
        owner_package: streamlib_idents::PackageRef,
    ) {
        self.schemas.lock().push(StagedSchemaRegistration {
            canonical_id,
            body,
            owner_package,
        });
    }

    pub(super) fn stage_processor(
        &self,
        descriptor: ProcessorDescriptor,
        kind: StagedProcessorRegistrationKind,
        owner_package: streamlib_idents::PackageRef,
    ) {
        self.processors.lock().push(StagedProcessorRegistration {
            descriptor,
            kind,
            owner_package,
        });
    }

    pub(super) fn stage_plugin_library(
        &self,
        library: libloading::Library,
        dylib_path: std::path::PathBuf,
        owner_package: streamlib_idents::PackageRef,
    ) {
        self.libraries.lock().push(StagedPluginLibrary {
            library,
            dylib_path,
            owner_package,
        });
    }

    /// Whether a processor matching this ident's `(org, package, type)`
    /// tuple is staged, ignoring the version — mid-walk
    /// declared-but-not-registered validation reads this, not the global
    /// registry (registrations are invisible globally until commit).
    ///
    /// Version-blind by construction: a cdylib always stages its own
    /// processor identity at `0.0.0` (the `#[processor]` grammar rejects
    /// any inline version), while the loader composes the expected ident
    /// from the package manifest's version. Matching on the full
    /// `SchemaIdent` would miss every real load, since the normal graph
    /// lookup is already version-blind (`highest_registered_for_tuple`).
    pub(super) fn contains_staged_processor_for_tuple(&self, ident: &SchemaIdent) -> bool {
        self.processors.lock().iter().any(|staged| {
            staged.descriptor.name.org == ident.org
                && staged.descriptor.name.package == ident.package
                && staged.descriptor.name.r#type == ident.r#type
        })
    }

    /// End-of-walk gate for subprocess (`Dynamic`-kind) processors:
    /// reject a staged Dynamic processor ident that is duplicated within
    /// this load OR already present in the global processor registry.
    ///
    /// Rust/VTable duplicates are silently deduped at commit
    /// (`register_via_vtable` skips a duplicate ident), but a Dynamic
    /// duplicate makes `register_dynamic` error at commit — after other
    /// staged items already applied — which would yield a
    /// silently-incomplete load that returned Ok. Catching it here, before
    /// the commit lock is taken, keeps the whole load fail-loud with zero
    /// residue (the walk-context guards abandon the staged state on the
    /// error exit). This is what makes the commit genuinely
    /// infallible-by-construction; the commit-time `register_dynamic` Err
    /// path remains as defensive dead code.
    pub(super) fn validate_no_dynamic_processor_collisions(
        &self,
    ) -> std::result::Result<(), super::errors::AddModuleError> {
        let processors = self.processors.lock();
        for (index, staged) in processors.iter().enumerate() {
            if !matches!(
                staged.kind,
                StagedProcessorRegistrationKind::Dynamic { .. }
            ) {
                continue;
            }
            let ident = &staged.descriptor.name;
            // (a) Duplicate within this load — collides with ANY other
            // staged processor of the same ident (Dynamic or VTable),
            // scanning only earlier entries so the collision is reported
            // once, against the first occurrence.
            if processors
                .iter()
                .take(index)
                .any(|earlier| earlier.descriptor.name == *ident)
            {
                return Err(super::errors::AddModuleError::DuplicateProcessorTypeInModule {
                    package: staged.owner_package.clone(),
                    processor_type: ident.clone(),
                });
            }
            // (b) Already globally registered by non-module-load code. A
            // staged ident is never globally visible mid-walk (staging is
            // per-load), and the single-version gate prevents re-staging a
            // package's own already-committed idents, so a hit here is a
            // genuine external collision `register_dynamic` would reject.
            if crate::core::processors::PROCESSOR_REGISTRY.is_registered(ident) {
                return Err(super::errors::AddModuleError::ProcessorTypeAlreadyRegistered {
                    package: staged.owner_package.clone(),
                    processor_type: ident.clone(),
                });
            }
        }
        Ok(())
    }

    /// Latest staged body for a canonical schema id (cdylib registration
    /// prologues may look up schemas their own package just staged).
    pub(super) fn staged_schema_body(&self, canonical_id: &str) -> Option<Arc<str>> {
        self.schemas
            .lock()
            .iter()
            .rev()
            .find(|staged| staged.canonical_id == canonical_id)
            .map(|staged| Arc::clone(&staged.body))
    }
}

impl Drop for ModuleLoadRegistrationStaging {
    fn drop(&mut self) {
        // Failure-path retention: `dlclose` is never called. The cdylib's
        // register callback already ran (host-service bridge state points
        // into the image), so the image is retained even when its load's
        // registrations are discarded. Commit drains `libraries` first, so
        // this is a no-op on the success path.
        let mut staged_libraries = self.libraries.lock();
        if staged_libraries.is_empty() {
            return;
        }
        let mut retained = LOADED_PLUGIN_LIBRARIES.lock();
        for staged in staged_libraries.drain(..) {
            tracing::debug!(
                dylib_path = %staged.dylib_path.display(),
                package = %staged.owner_package,
                "retaining plugin dylib image from a failed module load",
            );
            retained.push(RetainedPluginLibrary {
                library: staged.library,
                dylib_path: staged.dylib_path,
                first_loaded_for: staged.owner_package,
            });
        }
    }
}

// =============================================================================
// Thread-local cdylib registration sink
// =============================================================================

/// TLS state installed around a cdylib's `(decl.register)(...)` call so
/// the host's `host_schema_register` / `host_processor_register`
/// callbacks — which run synchronously on the same thread — stage into
/// the active load instead of writing the global registries.
struct ActiveCdylibRegistrationScope {
    staging: Arc<ModuleLoadRegistrationStaging>,
    owner_package: streamlib_idents::PackageRef,
}

thread_local! {
    static ACTIVE_CDYLIB_REGISTRATION_SINK: std::cell::RefCell<Option<ActiveCdylibRegistrationScope>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII scope for [`ACTIVE_CDYLIB_REGISTRATION_SINK`] — installs on
/// construction, clears on drop (all exits, including panic unwind).
pub(super) struct CdylibRegistrationSinkGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl CdylibRegistrationSinkGuard {
    pub(super) fn install(
        staging: Arc<ModuleLoadRegistrationStaging>,
        owner_package: streamlib_idents::PackageRef,
    ) -> Self {
        ACTIVE_CDYLIB_REGISTRATION_SINK.with(|slot| {
            *slot.borrow_mut() = Some(ActiveCdylibRegistrationScope {
                staging,
                owner_package,
            });
        });
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for CdylibRegistrationSinkGuard {
    fn drop(&mut self) {
        ACTIVE_CDYLIB_REGISTRATION_SINK.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

/// Stage a cdylib schema registration into the active sink, if one is
/// installed on this thread. Returns `false` when no sink is active (the
/// caller falls through to the direct-to-global path).
pub(crate) fn stage_schema_via_active_cdylib_sink(canonical_id: &str, body: &str) -> bool {
    ACTIVE_CDYLIB_REGISTRATION_SINK.with(|slot| {
        let borrow = slot.borrow();
        let Some(scope) = borrow.as_ref() else {
            return false;
        };
        scope.staging.stage_schema(
            canonical_id.to_string(),
            Arc::from(body),
            scope.owner_package.clone(),
        );
        true
    })
}

/// Stage a cdylib processor registration into the active sink, if one is
/// installed on this thread. Returns the descriptor back when no sink is
/// active (the caller falls through to the direct-to-global path).
pub(crate) fn stage_processor_via_active_cdylib_sink(
    descriptor: ProcessorDescriptor,
    vtable: &'static ProcessorVTable,
) -> std::result::Result<(), ProcessorDescriptor> {
    ACTIVE_CDYLIB_REGISTRATION_SINK.with(|slot| {
        let borrow = slot.borrow();
        let Some(scope) = borrow.as_ref() else {
            return Err(descriptor);
        };
        scope.staging.stage_processor(
            descriptor,
            StagedProcessorRegistrationKind::VTable {
                vtable,
                cdylib_resident: true,
            },
            scope.owner_package.clone(),
        );
        Ok(())
    })
}

/// Look up a schema body in the active sink's staging buffer, if a sink
/// is installed on this thread. Overlay for `host_schema_lookup` so a
/// cdylib registration prologue sees schemas its own load staged.
pub(crate) fn lookup_schema_via_active_cdylib_sink(canonical_id: &str) -> Option<Arc<str>> {
    ACTIVE_CDYLIB_REGISTRATION_SINK.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|scope| scope.staging.staged_schema_body(canonical_id))
    })
}

// =============================================================================
// Commit
// =============================================================================

/// Apply a successful load's staged registrations to the global
/// registries, write the ledger, flip the memo placeholders, and publish
/// the `Committed` signals. Runs under [`MODULE_REGISTRY_COMMIT_LOCK`].
///
/// Commit is infallible by construction — everything fallible (manifest
/// parsing, dlopen, ABI validation, schema reads) happened during the
/// walk. An application error here is bug-grade: it is logged at error
/// level and the remaining items still commit (visible ⇒ permanently
/// committed; there is no partial-rollback of a commit).
pub(super) fn commit_module_load_registrations(
    staging: &ModuleLoadRegistrationStaging,
    armed_placeholder_guards: Vec<super::recursive_walker::InFlightPlaceholderGuard<'_>>,
    committed_dependency_requirer_edges: Vec<(
        streamlib_idents::PackageRef,
        streamlib_idents::PackageRef,
    )>,
    resolution_memo: &super::recursive_walker::ResolutionMemo,
) {
    use super::ledger;

    let _commit_guard = MODULE_REGISTRY_COMMIT_LOCK.lock();

    // (1) Staged plugin images → process-lifetime retention. First, so
    // vtable pointers are backed by retained images before any processor
    // registration referencing them becomes globally visible.
    let staged_libraries: Vec<StagedPluginLibrary> = staging.libraries.lock().drain(..).collect();
    let mut dylib_paths_by_owner: std::collections::HashMap<
        streamlib_idents::PackageRef,
        Vec<std::path::PathBuf>,
    > = std::collections::HashMap::new();
    {
        let mut retained = LOADED_PLUGIN_LIBRARIES.lock();
        for staged in staged_libraries {
            dylib_paths_by_owner
                .entry(staged.owner_package.clone())
                .or_default()
                .push(staged.dylib_path.clone());
            retained.push(RetainedPluginLibrary {
                library: staged.library,
                dylib_path: staged.dylib_path,
                first_loaded_for: staged.owner_package,
            });
        }
    }

    // (2) Staged schemas → global schema registry (schemas BEFORE
    // processors; a racing reader that sees processors without schemas
    // would resolve port payloads against missing schemas, whereas
    // schemas-without-processors reads as module-not-loaded-yet).
    let staged_schemas: Vec<StagedSchemaRegistration> = staging.schemas.lock().drain(..).collect();
    let mut schema_ids_by_owner: std::collections::HashMap<
        streamlib_idents::PackageRef,
        Vec<String>,
    > = std::collections::HashMap::new();
    for staged in staged_schemas {
        schema_ids_by_owner
            .entry(staged.owner_package.clone())
            .or_default()
            .push(staged.canonical_id.clone());
        crate::core::embedded_schemas::register_schema(staged.canonical_id, staged.body);
    }

    // (3) Staged processors → global processor registry. The duplicate
    // branches inside `register_via_vtable` / `register_dynamic` are
    // defensive dead code on this path — the single-version gate skipped
    // already-registered packages during the walk.
    let staged_processors: Vec<StagedProcessorRegistration> =
        staging.processors.lock().drain(..).collect();
    let mut processor_idents_by_owner: std::collections::HashMap<
        streamlib_idents::PackageRef,
        Vec<SchemaIdent>,
    > = std::collections::HashMap::new();
    for staged in staged_processors {
        let ident = staged.descriptor.name.clone();
        let apply_result = match staged.kind {
            StagedProcessorRegistrationKind::VTable {
                vtable,
                cdylib_resident,
            } => crate::core::processors::PROCESSOR_REGISTRY.register_via_vtable(
                staged.descriptor,
                vtable,
                cdylib_resident,
            ),
            StagedProcessorRegistrationKind::Dynamic { constructor } => {
                crate::core::processors::PROCESSOR_REGISTRY
                    .register_dynamic(staged.descriptor, constructor)
            }
        };
        match apply_result {
            Ok(()) => {
                processor_idents_by_owner
                    .entry(staged.owner_package.clone())
                    .or_default()
                    .push(ident);
            }
            Err(e) => {
                // Bug-grade: the walk validated everything fallible.
                tracing::error!(
                    processor = %ident,
                    package = %staged.owner_package,
                    "module-load commit failed to apply a staged processor \
                     registration (bug-grade — the walk should have caught \
                     this): {e}",
                );
            }
        }
    }

    // (4) Ledger records + memo flips, per package resolved by this load.
    // The flip runs under the memo lock and returns the record's final
    // requirer list, so a concurrent gate's requirer push can never be
    // lost between the ledger write and the flip. Completion signals are
    // published LAST so a waiter that proceeds observes complete state.
    let mut completion_signals: Vec<
        Arc<super::recursive_walker::PackageResolutionCompletionSignal>,
    > = Vec::new();
    for guard in armed_placeholder_guards {
        let package = guard.package_ref().clone();
        let Some((record, signal)) =
            resolution_memo.flip_in_flight_placeholder_to_committed(&package)
        else {
            // Defensive: only the owning load holds an armed guard.
            tracing::error!(
                package = %package,
                "module-load commit found no in-flight placeholder to flip \
                 (bug-grade)",
            );
            guard.disarm();
            continue;
        };
        guard.disarm();

        let required_by: Vec<streamlib_idents::PackageRef> = record
            .required_by
            .iter()
            .filter_map(|requirer| requirer.requirer.clone())
            .collect();
        ledger::insert_loaded_module_registration_record(
            package.clone(),
            ledger::LoadedModuleRegistrationRecord {
                version: record.version,
                schema_ids: schema_ids_by_owner.remove(&package).unwrap_or_default(),
                processor_idents: processor_idents_by_owner
                    .remove(&package)
                    .unwrap_or_default(),
                dylib_paths: dylib_paths_by_owner.remove(&package).unwrap_or_default(),
                required_by,
            },
        );
        completion_signals.push(signal);
    }

    // Requirer edges into packages some EARLIER load committed (this
    // walk skipped them at the gate) — append onto their ledger records
    // so `remove_module`'s RequiredByLoadedModules check stays accurate.
    for (dependency_package, requirer_package) in committed_dependency_requirer_edges {
        ledger::append_requirer_to_loaded_module_record(&dependency_package, requirer_package);
    }

    // (5) Publish `Committed` to concurrent loads that skipped these
    // packages as in-flight.
    for signal in completion_signals {
        signal.publish_committed();
    }
}

/// Register a single already-compiled session-local processor (the
/// `add_local` path) through the SAME staging → commit → ledger seam a
/// disk-backed module load uses, minus the manifest walk a runtime-authored
/// host type has no need of. Stages the one processor, then — under
/// [`MODULE_REGISTRY_COMMIT_LOCK`] — registers its host-address-space vtable
/// (`cdylib_resident: false`, identical to [`crate::core::processors::ProcessorInstanceFactory::register`])
/// and writes a ledger record keyed by `@session/<name>`, so
/// [`super::super::Runner::remove_module`] unregisters it symmetrically.
///
/// Fail-loud on a live name collision: a `@session/<name>` already in the
/// ledger is an unremoved live registration of the same name, refused with
/// [`AddModuleError::DuplicateSessionProcessorName`] rather than silently
/// deduped by the vtable registry's ident key. The short-type-name shadow of
/// an installed processor is a warning (both stay addressable), never a
/// refusal — that check runs before the ident-key registration.
pub(super) fn commit_session_processor_registration(
    staging: &ModuleLoadRegistrationStaging,
    module: &streamlib_idents::ModuleIdent,
    version: streamlib_idents::SemVer,
    descriptor: ProcessorDescriptor,
    vtable: &'static ProcessorVTable,
) -> std::result::Result<SchemaIdent, super::errors::AddModuleError> {
    use super::ledger;

    let package_ref = module.package_ref();
    let processor_ident = descriptor.name.clone();

    let _commit_guard = MODULE_REGISTRY_COMMIT_LOCK.lock();

    // Live-name collision: an unremoved `@session/<name>` registration.
    if ledger::with_loaded_module_registration_record(&package_ref, |_| ()).is_some() {
        return Err(super::errors::AddModuleError::DuplicateSessionProcessorName {
            module: module.clone(),
        });
    }

    // Short-type-name shadow of an installed processor: warn, keep both.
    crate::core::processors::PROCESSOR_REGISTRY.warn_on_short_name_shadow(&processor_ident);

    staging.stage_processor(
        descriptor,
        StagedProcessorRegistrationKind::VTable {
            vtable,
            cdylib_resident: false,
        },
        package_ref.clone(),
    );

    let staged: Vec<StagedProcessorRegistration> = staging.processors.lock().drain(..).collect();
    let mut committed_idents: Vec<SchemaIdent> = Vec::new();
    for staged_processor in staged {
        let ident = staged_processor.descriptor.name.clone();
        match staged_processor.kind {
            StagedProcessorRegistrationKind::VTable {
                vtable,
                cdylib_resident,
            } => {
                match crate::core::processors::PROCESSOR_REGISTRY.register_via_vtable(
                    staged_processor.descriptor,
                    vtable,
                    cdylib_resident,
                ) {
                    Ok(()) => committed_idents.push(ident),
                    Err(e) => tracing::error!(
                        processor = %ident,
                        "add_local commit failed to apply the staged session \
                         processor registration (bug-grade): {e}",
                    ),
                }
            }
            StagedProcessorRegistrationKind::Dynamic { .. } => {
                tracing::error!(
                    processor = %ident,
                    "add_local staged a Dynamic-kind processor (bug-grade — \
                     session registration stages only host vtables)",
                );
            }
        }
    }

    ledger::insert_loaded_module_registration_record(
        package_ref,
        ledger::LoadedModuleRegistrationRecord {
            version,
            schema_ids: Vec::new(),
            processor_idents: committed_idents,
            dylib_paths: Vec::new(),
            required_by: Vec::new(),
        },
    );

    Ok(processor_ident)
}
