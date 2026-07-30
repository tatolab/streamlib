// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod graph_change_listener;
mod install;
mod module_loader;
mod operations;
mod operations_runtime;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod status;
mod tap;

pub use install::{InstallError, InstallOptions, InstallReport, install};
pub use module_loader::{
    AcquireConfirmationHandler, AcquireOnReferencePolicy, AddModuleError, AddedModule,
    ArtifactChecksum, BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy,
    BuildRequest, BuildSource, BuildStream, LoadedModule, ModuleLoadEvent, PackageSourceProvenance,
    RemoveModuleError, SemVerRange, StagedArtifact, Strategy,
    extract_package_archive_to_installed_slot, host_target_triple, loaded_plugin_library_count,
};
pub(crate) use module_loader::{
    lookup_schema_via_active_cdylib_sink, stage_processor_via_active_cdylib_sink,
    stage_schema_via_active_cdylib_sink,
};
pub use operations::{
    BoxFuture, ProcessorLanguage, RegisterProcessorReceipt, RegisteredPortReceipt,
    RegisteredProcessorReceipt, ReplaceProcessorFromSource, RuntimeOperations,
    SubmittedProcessorSource,
};
pub use runtime::Runner;
#[cfg(test)]
pub(crate) use runtime_shutdown_request::RuntimeShutdownRequestLatchClearedOnDrop;
pub use runtime_shutdown_request::{
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, is_runtime_shutdown_requested,
    request_runtime_shutdown, take_runtime_shutdown_request_latch,
};
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;
pub use streamlib_idents::app_modules::{
    APP_MODULES_DIR_NAME, ActiveLinkSlotPolicy, AddPackageOptions, AddPackageReport,
    AddPackageSource, AppModulesDir, AppModulesError, InstallFromLockfileReport,
    InstalledFromLockKind, InstalledFromLockPackage, LinkPackageReport, LockfileRecordingPolicy,
    RemovePackageReport, ReplacedSlotBackup, UnlinkPackageReport, parse_lockfile_package_key,
};
pub use tap::TapSubscription;
