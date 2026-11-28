// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// Allow `::streamlib::*` paths emitted by the procedural macro to
// resolve back to this crate when invoked from external consumer
// crates (e.g. domain packages, customer apps).
extern crate self as streamlib;

// =============================================================================
// Tier 1 — public SDK API
// =============================================================================

pub mod sdk {
    //! Public SDK API surface. Default for all consumer code.

    // ---- Engine `core::*` sub-modules that are SDK-public ----
    //
    // Engine internals (`compiler`, `config`,
    // `logging`, `signals`)
    // are `pub(crate)` in the engine crate (see
    // `core/mod.rs`) — those module paths are not reachable here OR
    // via `engine_internal::*` (Tier 3) by construction. Items inside
    // that ARE customer-facing are re-exported by the engine at its
    // crate root and travel into the SDK through the Tier-3
    // `engine_internal` namespace.

    pub use streamlib_engine::core::child_process_lifetime;
    pub use streamlib_engine::core::child_process_restart_backoff;
    pub use streamlib_engine::core::color;
    pub use streamlib_engine::core::context;
    pub use streamlib_engine::core::descriptors;
    pub use streamlib_engine::core::error;
    pub use streamlib_engine::core::execution;
    pub use streamlib_engine::core::graph;
    pub use streamlib_engine::core::graph::graph_snapshot;
    pub use streamlib_engine::core::helper_process_transport;
    pub use streamlib_engine::core::json_schema;
    pub use streamlib_engine::core::media_clock;
    pub use streamlib_engine::core::node_interpreter;
    pub use streamlib_engine::core::pin_value_packing;
    pub use streamlib_engine::core::prelude;
    pub use streamlib_engine::core::pubsub;
    pub use streamlib_engine::core::rhi;
    pub use streamlib_engine::core::runtime;
    pub use streamlib_engine::core::shared_native_services;
    pub use streamlib_engine::core::supervised_child_process;
    pub use streamlib_engine::core::unix_socket_path_cleared_for_bind;

    pub use streamlib_engine::core::texture;

    // ---- App authoring sugar ----

    pub mod app;

    pub use app::{AddedNode, App, AppPortEndpoint, THE_STREAM_ID_AN_APP_LOADS};

    pub use streamlib_engine::sdk::nodes;

    // ---- Runtime-control requests ----
    /// `request_the_shutdown_of_every_loaded_stream` — ask whoever owns the
    /// machine's shutdown signals to shut every loaded stream down.
    pub mod runtime_control {
        pub use streamlib_engine::core::runtime::request_the_shutdown_of_every_loaded_stream;
    }

    // ---- Cross-cutting modules from engine top-level ----

    /// `iceoryx2` Rust wrapper module — required by macro-emitted paths.
    pub use streamlib_engine::iceoryx2;

    /// Logging pipeline.
    pub use streamlib_engine::logging;

    /// `serde_json` re-export — required by macro-emitted paths.
    pub use streamlib_engine::serde_json;

    pub use streamlib_engine::schemars;

    /// `crossbeam_channel` re-export — required by macro-emitted paths.
    pub use streamlib_engine::crossbeam_channel;

    // ---- Procedural macros ----

    pub use streamlib_engine::node;

    // ---- Permission helpers ----

    pub mod permissions {
        pub use streamlib_engine::{request_audio_permission, request_camera_permission};
    }

    // ---- Platform info ----

    pub use streamlib_engine::platform;

    // =========================================================================
    // Tier 2 — SDK's curated engine-bridge surface
    // =========================================================================

    /// Curated engine-bridge surface. Adapter crates and HOST-RHI
    /// examples that legitimately need raw GPU primitives reach
    /// through this namespace.
    ///
    /// The `engine` segment signals "you're touching engine-bridge
    /// primitives via the SDK's official extension surface" — distinct
    /// from `sdk::*` (regular SDK API) and `streamlib::engine_internal::*`
    /// (direct passthrough).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub mod engine {
        /// Host-side Vulkan RHI types (HostVulkanDevice,
        /// HostVulkanTexture, HostVulkanBuffer,
        /// HostVulkanTimelineSemaphore, VulkanComputeKernel,
        /// VulkanGraphicsKernel, VulkanRayTracingKernel,
        /// VulkanTextureReadback, VulkanAccelerationStructure, etc.).
        pub use streamlib_engine::host_rhi;

        /// Privileged extension traits surfacing raw `Host*` handles
        /// on SDK-bucket types. Importing one unlocks `vulkan_inner()`
        /// / `from_vulkan()` / `vulkan_device()` on
        /// `Texture` / `PixelBufferRef` / `GpuDevice`, or the
        /// engine-only timeline-semaphore registration paths on
        /// `SurfaceStore` (`register_texture` /
        /// `register_pixel_buffer_with_timeline`).
        pub use streamlib_engine::{HostGpuDeviceExt, HostPixelBufferRefExt, HostTextureExt};

        #[cfg(target_os = "linux")]
        pub use streamlib_engine::HostSurfaceStoreExt;

        /// Per-runtime surface-share service primitives. For adapter
        /// integration tests and 3rd-party tooling that needs to drive
        /// the service in isolation; production callers go through
        /// [`crate::sdk::runtime::Runner`].
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::linux_surface_share;

        /// The macOS peer of `linux_surface_share`: the raw-Mach service, its
        /// rendezvous, and the helper-process admission a spawner holds.
        #[cfg(target_os = "macos")]
        pub use streamlib_engine::apple_surface_share;

        /// The resolved colour → V4L2 map, for a built-in that signals a V4L2
        /// output device at `S_FMT`.
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::linux_v4l2_color;

        /// The virtual camera's PipeWire door, for the host table's virtual
        /// camera output stream.
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::linux_pipewire_camera;
    }
}

// =============================================================================
// Tier 3 — direct passthrough to streamlib-engine
// =============================================================================

/// Direct passthrough to the `pub` surface of the `streamlib-engine`
/// crate.
///
/// **Use sparingly.** This exists for the rare case where the SDK's
/// curated [`sdk::engine`] surface doesn't expose what's needed AND
/// extending the SDK isn't right. The path itself signals "I'm
/// reaching past the curated boundary."
///
/// Engine-internal `core::*` modules are `pub(crate)` in the engine
/// — those module paths cannot be reached from here:
///
/// ```compile_fail
/// // `core::compiler` is `pub(crate)` in streamlib-engine.
/// use streamlib::engine_internal::core::compiler::Compiler;
/// ```
///
/// ```compile_fail
/// // `core::signals` is `pub(crate)` in streamlib-engine.
/// use streamlib::engine_internal::core::signals;
/// ```
///
/// Items inside engine-internal modules that genuinely need
/// cross-crate access are explicitly re-exported item-by-item at
/// `streamlib_engine::core::*` (e.g. `helper_process_transport::ENGINE_BUILD_ID`) or at the
/// engine crate root. The set of those re-exports IS the engine's
/// external surface.
///
/// If you find yourself importing from this namespace for an item
/// that would benefit other consumers, that's a signal: either extend
/// the SDK's curated surface or open an issue.
///
/// Direct `streamlib_engine::*` imports outside the engine itself and
/// this facade source are refused by the ast-grep rule
/// `no-streamlib-engine-outside-the-engine` — `engine_internal` is the
/// allowed escape hatch for the very rare cases that need it.
pub mod engine_internal {
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    pub use streamlib_engine::permissions;
    pub use streamlib_engine::{
        Error, GpuContext, GraphSnapshot, InputPortMarker, MediaClock, NODE_REGISTRY,
        NativeTextureHandle, NodeSpec, OutputPortMarker, PooledTextureHandle, Result, Runner,
        RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamClock,
        StreamClockSpec, Texture, TextureDescriptor, TextureFormat, TexturePool,
        TexturePoolDescriptor, TextureUsages, core, crossbeam_channel, iceoryx2, input, logging,
        node, output, platform, schemars, sdk, serde_json,
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub use streamlib_engine::{
        HostGpuDeviceExt, HostPixelBufferRefExt, HostSurfaceStoreExt, HostTextureExt, host_rhi,
    };
    #[cfg(target_os = "macos")]
    pub use streamlib_engine::{apple_coreaudio_audio_tier, apple_surface_share};
    #[cfg(target_os = "linux")]
    pub use streamlib_engine::{
        linux_alsa_audio_device_backend, linux_pipewire_camera, linux_surface_share,
        linux_v4l2_color,
    };
    pub use streamlib_engine::{request_audio_permission, request_camera_permission};
}
