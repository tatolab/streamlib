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
pub mod app_directory;
pub mod color;
pub mod context;
pub mod descriptors;
pub mod display_info;
pub mod error;
pub mod execution;
pub mod graph;
pub mod graph_snapshot;
pub mod json_schema;
pub mod machine_global_unique_name;
pub mod media_clock;
pub mod prelude;
pub mod processors;
pub mod pubsub;
pub mod rhi;
pub mod runtime;
pub mod stable_short_id;
pub mod texture;
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

/// The framed-IPC transport a helper process is driven over.
///
/// Public because the spawn host that owns a Python helper lives in the wheel,
/// outside this crate; the transport itself is language-agnostic and is shared
/// with the engine's own subprocess hosts.
pub mod helper_process_transport {
    pub use super::compiler::compiler_ops::subprocess_bridge::{
        ENGINE_BUILD_ID, ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE, EscalateTransport,
        HelperProcessShutdownCommand, SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS, SubprocessBridge,
        SubprocessBridgeLinkDelivery, refusal_of_a_link_into_a_helper_process_that_failed,
        spawn_fd_line_reader,
    };
}
