// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Engine-internal modules. Module-path is `pub(crate)` so consumers
// cannot reach `streamlib_engine::core::<name>` (or
// `streamlib::engine_internal::core::<name>`) — the boundary is
// type-system-enforced at the engine source-of-truth. Items that
// genuinely need to cross the boundary are re-exported below as
// narrow `pub use` selections; items not re-exported stay
// engine-internal.
pub(crate) mod compiler;
#[cfg(test)]
mod engine_build_id_composition;
#[cfg(test)]
mod engine_build_id_composition_tests;
pub(crate) mod logging;
pub(crate) mod signals;
#[cfg(test)]
pub(crate) mod test_support;

// Customer-facing modules. Module-path stays `pub` so consumers
// can reach `streamlib::sdk::<name>` via the SDK's per-module
// re-exports.
pub mod child_process_lifetime;
pub mod child_process_restart_backoff;
pub mod color;
pub mod context;
pub mod descriptors;
pub mod error;
pub mod execution;
pub mod graph;
pub mod graph_fragment_flattening;
pub mod graph_interface_resolution;
pub mod graph_snapshot;
pub mod graph_variable_resolution;
pub mod json_schema;
pub mod machine_global_unique_name;
pub mod media_clock;
pub(crate) mod output_port_stream_facts;
pub mod prelude;
pub mod processors;
pub mod pubsub;
pub mod rhi;
pub mod runtime;
pub mod shared_native_services;
pub mod stable_short_id;
pub mod supervised_child_process;
pub mod texture;
pub mod unix_socket_path_cleared_for_bind;
pub mod utils;

// Customer-facing modules (wildcard re-exports stay).
pub use context::*;
pub use descriptors::*;
pub use error::*;
pub use execution::*;
pub use graph::*;
pub use graph_snapshot::*;
pub use processors::*;
pub use rhi::NativeTextureHandle;
pub use runtime::*;
pub use texture::*;
pub use utils::*;

// Narrow re-exports of engine-internal items that have sanctioned
// external consumers. Each line below is a deliberate boundary
// crossing — items not listed here stay engine-internal.

/// The framed-IPC transport a processor interpreter is driven over.
///
/// Public for the wheel's processor-interpreter side, which checks the engine
/// build it imported against its parent's, and for the engine's own
/// integration tests, which drive a bridge across real processes.
pub mod helper_process_transport {
    pub use super::compiler::compiler_ops::subprocess_bridge::{
        ENGINE_BUILD_ID, SubprocessBridge, SubprocessBridgeLinkDelivery,
    };
}

/// What a processor interpreter's own side shares with the engine that starts
/// it: where its bootstrap sits in a lend, where a runtime unit or the macOS
/// app bundle lays out the lend and the Vulkan driver it carries, the
/// variables it is named by, the shutdown ladder's budgets, and the reader of
/// a described node type.
pub mod processor_interpreter {
    pub use super::compiler::compiler_ops::processor_interpreter_shutdown_ladder::{
        CALLBACK_RETURN_BUDGET, CHILD_SELF_EXIT_GRACE, TEARDOWN_BUDGET,
    };
    pub use super::compiler::compiler_ops::processor_interpreter_spawn_host::{
        PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY,
        PROCESSOR_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
        PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
        PROCESSOR_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
        PROCESSOR_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
        SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, processor_interpreter_bootstrap_path,
    };
    pub use super::compiler::compiler_ops::python_processor_declaration::{
        AudioWindowFieldRefusal, HowAPythonNodeOfTheTypeRuns, PythonProcessorDeclaration,
        read_a_channel_count_or_the_source_spelling,
    };
    pub use streamlib_consumer_rhi::runtime_unit_layout::{
        APP_BUNDLE_CONTENTS_DIRECTORY_NAME, APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME,
        BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, BROWSER_HOST_DIRECTORY_NAME,
        BROWSER_HOST_EXECUTABLE_FILE_NAME, BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME,
        BUNDLED_ICD_MANIFEST_FILE_NAME, FIRST_PARTY_MODULES_DIRECTORY_NAME,
        LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS,
        LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME,
        app_bundle_contents_directory_holding_the_executable_directory,
        browser_host_executable_beside_the_lend, bundled_vulkan_driver_directory_in_the_lend,
        first_party_modules_directory_beside_the_lend, lend_directory_in_the_app_bundle_contents,
        lend_directory_in_the_runtime_unit,
    };
}
