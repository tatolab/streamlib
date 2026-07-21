// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod graph_change_listener;
mod install;
mod module_loader;
mod operations;
mod operations_runtime;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_unique_id;
mod status;
mod tap;

pub use install::{InstallError, InstallOptions, InstallReport, install};
pub use streamlib_idents::app_modules::{
    APP_MODULES_DIR_NAME, AddPackageOptions, AddPackageReport, AddPackageSource, AppModulesDir,
    AppModulesError, InstallFromLockfileReport, InstalledFromLockKind, InstalledFromLockPackage,
    LinkPackageReport, RemovePackageReport, UnlinkPackageReport,
};
pub use module_loader::{
    AcquireConfirmationHandler, AcquireOnReferencePolicy, AddModuleError, AddedModule,
    ArtifactChecksum, BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy,
    BuildRequest, BuildSource, BuildStream, LoadedModule, ModuleLoadEvent, RemoveModuleError,
    SemVerRange, StagedArtifact, Strategy, extract_slpkg_to_cache, host_target_triple,
    loaded_plugin_library_count,
};
pub(crate) use module_loader::{
    lookup_schema_via_active_cdylib_sink, stage_processor_via_active_cdylib_sink,
    stage_schema_via_active_cdylib_sink,
};
pub use operations::{
    BoxFuture, ConnectOptions, ProcessorLanguage, RegisterProcessorReceipt, RegisteredPortReceipt,
    RegisteredProcessorReceipt, ReplaceProcessorFromSource, RuntimeOperations,
    SchemaValidationPosture, SubmittedProcessorSource,
};
pub use runtime::Runner;
pub use tap::TapSubscription;
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;

use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};

/// Map a cross-plugin-ABI shutdown request onto the engine's internal
/// runtime-shutdown event. This is the single mapping point every
/// engine-free boundary funnels through — today the plugin-ABI
/// `pubsub_publish` control topic
/// ([`streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`]),
/// tomorrow a subprocess escalate op or an api-server endpoint — so the
/// engine owns the `Event` encoding in exactly one place rather than
/// letting each boundary freeze it into its own wire form. The request
/// is idempotent (the shutdown listeners are flag-setting) and
/// fire-and-forget, matching every other `RuntimeShutdown` publisher
/// including the engine's own signal handler. `reason` is logged at
/// `info` so operators can attribute who stopped the runtime.
#[tracing::instrument]
pub fn request_runtime_shutdown_from_plugin_abi_boundary(reason: &str) {
    tracing::info!(reason, "runtime shutdown requested across the plugin ABI");
    let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
}
