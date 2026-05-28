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
pub(crate) mod config;
pub(crate) mod embedded_schemas;
pub(crate) mod logging;
pub(crate) mod observability;
pub mod plugin;
pub(crate) mod runtime_hooks;
pub(crate) mod signals;
pub(crate) mod streamlib_home;
#[cfg(test)]
pub(crate) mod test_support;
// Apple-flavored codec wrappers + VideoToolbox + AppleMp4Muxer live in
// their domain packages' `_apple_impl_pending_/` directories (#786):
// h264 (videotoolbox + video_codec/encoder/decoder + their configs),
// mp4 (apple muxer + mp4_muxer + config), audio (audio_codec).

// Customer-facing modules. Module-path stays `pub` so consumers
// can reach `streamlib::sdk::<name>` via the SDK's per-module
// re-exports.
pub mod color;
pub mod context;
pub mod descriptors;
pub mod display_info;
pub mod error;
pub mod execution;
pub mod graph;
pub mod graph_snapshot;
pub mod json_schema;
pub mod media_clock;
pub mod prelude;
pub mod processors;
pub mod pubsub;
pub mod rhi;
pub mod runtime;
pub mod sync;
pub mod texture;
pub mod utils;

// Customer-facing modules (wildcard re-exports stay).
pub use context::*;
pub use descriptors::*;
pub use error::*;
pub use rhi::{gl_constants, GlContext, GlTextureBinding, NativeTextureHandle, RhiBackend};
pub use graph::*;
pub use graph_snapshot::*;
pub use processors::*;
pub use utils::*;
pub use execution::*;
pub use runtime::*;
pub use sync::*;
pub use texture::*;

// Narrow re-exports of engine-internal items that have sanctioned
// external consumers. Each line below is a deliberate boundary
// crossing — items not listed here stay engine-internal.
//
// CLI tooling (`streamlib-cli`):
pub use compiler::compiler_ops::ensure_processor_venv;
pub use config::{InstalledPackageEntry, InstalledPackageManifest, ProjectConfig};
pub use streamlib_home::{get_cached_package_dir, get_streamlib_home};
