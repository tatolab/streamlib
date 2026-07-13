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

pub use install::{InstallError, InstallOptions, InstallReport, install};
pub use streamlib_idents::app_modules::{
    APP_MODULES_DIR_NAME, AddPackageOptions, AddPackageReport, AddPackageSource, AppModulesDir,
    AppModulesError, RemovePackageReport,
};
pub use module_loader::{
    AddModuleError, AddedModule, ArtifactChecksum, BuildError, BuildEvent, BuildEventSink,
    BuildOrchestrator, BuildPolicy, BuildRequest, BuildSource, BuildStream, LoadedModule,
    ModuleLoadEvent, RemoveModuleError, SemVerRange, StagedArtifact, Strategy,
    extract_slpkg_to_cache, host_target_triple,
};
pub use operations::{BoxFuture, RuntimeOperations};
pub use runtime::Runner;
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;
