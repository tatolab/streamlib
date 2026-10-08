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
pub(crate) mod observability;
pub(crate) mod runtime_hooks;
pub(crate) mod signals;
pub(crate) mod streamlib_home;
#[cfg(test)]
pub(crate) mod test_support;

// Customer-facing modules. Module-path stays `pub` so consumers
// can reach `streamlib::sdk::<name>` via the SDK's per-module
// re-exports.
pub mod annex_b_access_unit;
pub mod annex_b_start_code_finder;
pub mod app_directory;
pub mod color;
pub mod context;
pub mod descriptors;
pub mod directory_at_an_explicit_mode;
pub mod display_info;
pub mod error;
pub mod execution;
pub mod graph;
pub mod graph_snapshot;
pub mod h265_sequence_parameter_set;
pub mod json_schema;
pub mod machine_global_unique_name;
pub mod media_clock;
pub mod nal_unit_raw_byte_sequence_payload;
pub mod prelude;
pub mod processors;
pub mod pubsub;
pub mod rhi;
pub mod runtime;
pub mod stable_short_id;
pub mod texture;
pub mod unix_socket_path_cleared_for_bind;
pub mod utils;
// Wherever winit and the Vulkan present target compile.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod processor_owned_window;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod window_event_pump;

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
//
// Home / data-dir resolution:
pub use streamlib_home::{get_streamlib_data_dir, get_streamlib_home, get_uv_cache_dir};

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
/// it: where its bootstrap sits in a lend, the variables it is named by, the
/// shutdown ladder's budgets, and the reader of a described node type.
pub mod processor_interpreter {
    pub use super::compiler::compiler_ops::processor_interpreter_shutdown_ladder::{
        CALLBACK_RETURN_BUDGET, CHILD_SELF_EXIT_GRACE, TEARDOWN_BUDGET,
    };
    pub use super::compiler::compiler_ops::processor_interpreter_spawn_host::{
        PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY,
        PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
        SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, processor_interpreter_bootstrap_path,
    };
    pub use super::compiler::compiler_ops::python_processor_declaration::{
        AudioWindowFieldRefusal, PythonProcessorDeclaration,
        read_a_channel_count_or_the_source_spelling,
    };
}
