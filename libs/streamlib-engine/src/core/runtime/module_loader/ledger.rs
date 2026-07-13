// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;

/// What one committed package registered — everything `remove_module`
/// needs to unregister it and to refuse an unsafe removal.
pub(crate) struct LoadedModuleRegistrationRecord {
    /// The concrete version the package committed at.
    pub version: streamlib_idents::SemVer,
    /// Canonical schema ids this package registered.
    pub schema_ids: Vec<String>,
    /// Structured processor idents this package registered.
    pub processor_idents: Vec<crate::core::descriptors::SchemaIdent>,
    /// Dylib images this package's load retained.
    pub dylib_paths: Vec<std::path::PathBuf>,
    /// Loaded packages that declared this package as a dependency.
    pub required_by: Vec<streamlib_idents::PackageRef>,
}

/// Process-global registry of committed module loads, keyed by
/// `@org/name`. Written at commit under the module-registry commit lock;
/// read + pruned by `remove_module` under the same lock.
static LOADED_MODULE_REGISTRATION_LEDGER: LazyLock<
    Mutex<HashMap<streamlib_idents::PackageRef, LoadedModuleRegistrationRecord>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Insert (or replace) a package's committed-load record.
pub(super) fn insert_loaded_module_registration_record(
    package: streamlib_idents::PackageRef,
    record: LoadedModuleRegistrationRecord,
) {
    LOADED_MODULE_REGISTRATION_LEDGER.lock().insert(package, record);
}

/// Append a requirer edge onto an already-committed package's record.
pub(super) fn append_requirer_to_loaded_module_record(
    dependency_package: &streamlib_idents::PackageRef,
    requirer_package: streamlib_idents::PackageRef,
) {
    let mut ledger = LOADED_MODULE_REGISTRATION_LEDGER.lock();
    match ledger.get_mut(dependency_package) {
        Some(record) => {
            if !record.required_by.contains(&requirer_package) {
                record.required_by.push(requirer_package);
            }
        }
        None => {
            // The dependency was removed between this load's gate skip and
            // its commit — the removal raced the walk. The requirer's graph
            // proceeds without the dependency's registrations; loud so the
            // operator sees the race rather than a later silent miss.
            tracing::warn!(
                dependency = %dependency_package,
                requirer = %requirer_package,
                "committed-dependency requirer edge targets a package that \
                 was removed mid-walk; the requirer may be missing the \
                 dependency's registrations — re-add the dependency",
            );
        }
    }
}

/// Run `f` over the package's record, if committed.
pub(super) fn with_loaded_module_registration_record<R>(
    package: &streamlib_idents::PackageRef,
    f: impl FnOnce(&LoadedModuleRegistrationRecord) -> R,
) -> Option<R> {
    LOADED_MODULE_REGISTRATION_LEDGER.lock().get(package).map(f)
}

/// Remove a package's record and prune it from every other record's
/// `required_by`. Returns the removed record.
pub(super) fn remove_loaded_module_registration_record(
    package: &streamlib_idents::PackageRef,
) -> Option<LoadedModuleRegistrationRecord> {
    let mut ledger = LOADED_MODULE_REGISTRATION_LEDGER.lock();
    let removed = ledger.remove(package);
    if removed.is_some() {
        for record in ledger.values_mut() {
            record.required_by.retain(|requirer| requirer != package);
        }
    }
    removed
}

/// Test observability: the set of committed package refs.
#[cfg(test)]
pub(crate) fn loaded_module_registration_ledger_packages() -> Vec<streamlib_idents::PackageRef> {
    LOADED_MODULE_REGISTRATION_LEDGER.lock().keys().cloned().collect()
}
