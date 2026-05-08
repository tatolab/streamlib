// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! streamlib — public authoring API surface.
//!
//! # Three-tier path system
//!
//! Every consumer import path falls into exactly one of three tiers,
//! and the path itself tells the reader which tier they're in.
//!
//! **Tier 1 — `streamlib::sdk::*`** — public SDK API. Default for all
//! consumer code. Examples / apps / domain packages should never need
//! anything outside this tier.
//!
//! ```ignore
//! use streamlib::sdk::runtime::Runner;
//! use streamlib::sdk::processors::ProcessorSpec;
//! use streamlib::sdk::rhi::StreamTexture;
//! use streamlib::sdk::iceoryx2::OutputWriter;
//! ```
//!
//! **Tier 2 — `streamlib::sdk::engine::*`** — the SDK's curated
//! engine-bridge surface. Adapter crates and HOST-RHI examples that
//! legitimately need raw GPU primitives reach through this namespace.
//! It's part of the SDK's public API; the `engine` segment signals
//! "you're touching engine-bridge primitives via the SDK's official
//! extension surface."
//!
//! ```ignore
//! use streamlib::sdk::engine::HostGpuDeviceExt;
//! use streamlib::sdk::engine::host_rhi::HostVulkanDevice;
//! ```
//!
//! **Tier 3 — `streamlib::engine_internal::*`** — direct passthrough
//! to the entire `streamlib-engine` crate. For very rare cases where
//! the SDK's curated `sdk::engine` surface doesn't expose what's
//! needed AND extending the SDK isn't right. Reads as "I'm reaching
//! past the curated boundary; I know what I'm doing."
//!
//! ```ignore
//! // Rare — should only be used in very specific circumstances.
//! use streamlib::engine_internal::core::compiler::SomeInternalThing;
//! ```
//!
//! If you find yourself importing from `engine_internal` for an item
//! that would benefit other consumers, that's a signal: either extend
//! the SDK's curated surface (`sdk::engine::*` or one of the topical
//! sub-namespaces) or open an issue.
//!
//! Direct `streamlib_engine::*` imports outside the engine itself and
//! this facade source are forbidden by `cargo xtask check-boundaries`
//! Check 6.

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
    // Engine internals (`compiler`, `embedded_schemas`, `runtime_hooks`,
    // `observability`, `streamlib_home`, `pubsub`, `signals`, `display_info`)
    // are deliberately NOT re-exported here. Consumers needing them
    // reach via `streamlib::engine_internal::core::<name>::*` (rare).

    pub use streamlib_engine::core::context;
    pub use streamlib_engine::core::descriptors;
    pub use streamlib_engine::core::display_info;
    pub use streamlib_engine::core::error;
    pub use streamlib_engine::core::execution;
    pub use streamlib_engine::core::graph;
    pub use streamlib_engine::core::graph_file;
    pub use streamlib_engine::core::json_schema;
    pub use streamlib_engine::core::media_clock;
    pub use streamlib_engine::core::prelude;
    pub use streamlib_engine::core::rhi;
    pub use streamlib_engine::core::runtime;
    pub use streamlib_engine::core::streaming;
    pub use streamlib_engine::core::sync;
    pub use streamlib_engine::core::texture;
    pub use streamlib_engine::core::utils;

    // ---- Processors namespace ----
    //
    // Combines engine's `core::processors::*` with the platform-
    // aliased processor types that engine exposes at its crate root
    // (e.g., `CameraProcessor` is `LinuxCameraProcessor` on Linux,
    // `AppleCameraProcessor` on macOS).
    pub mod processors {
        pub use streamlib_engine::core::processors::*;

        // Port markers + input/output helpers — semantically processor-
        // related; physically live in `core::graph::edges::link_port_markers`
        // in engine source.
        pub use streamlib_engine::core::graph::{
            input, output, InputPortMarker, OutputPortMarker,
        };

        // Port schema spec — semantically processor-related; lives in
        // `core::descriptors` in engine source (re-exported from
        // `streamlib-processor-schema`).
        pub use streamlib_engine::core::descriptors::PortSchemaSpec;

        // Platform-aliased camera + display processors.
        pub use streamlib_engine::{CameraProcessor, DisplayProcessor};

        // Apple-only processors.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        pub use streamlib_engine::{
            ClapEffectProcessor, Mp4WriterProcessor, ScreenCaptureProcessor,
        };

        // Linux-only processors (codec + bgra + mp4 writer).
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::{
            BgraFileSourceProcessor, H264DecoderProcessor, H264EncoderProcessor,
            H265DecoderProcessor, H265EncoderProcessor, LinuxMp4WriterProcessor,
        };

        // MoQ feature-gated processors.
        #[cfg(feature = "moq")]
        pub use streamlib_engine::{MoqPublishTrackProcessor, MoqSubscribeTrackProcessor};
    }

    // ---- Cross-cutting modules from engine top-level ----

    /// `iceoryx2` Rust wrapper module — required by macro-emitted paths.
    pub use streamlib_engine::iceoryx2;

    /// Logging pipeline.
    pub use streamlib_engine::logging;

    /// `inventory` re-export — required by macro-emitted paths.
    pub use streamlib_engine::inventory;

    /// `serde_json` re-export — required by macro-emitted paths.
    pub use streamlib_engine::serde_json;

    /// `crossbeam_channel` re-export — required by macro-emitted paths.
    pub use streamlib_engine::crossbeam_channel;

    /// Generated schema types (config types, wire vocabulary types).
    pub use streamlib_engine::_generated_;

    // ---- Procedural macros ----

    /// `#[streamlib::sdk::processor("...")]` attribute macro.
    pub use streamlib_engine::processor;

    /// `#[derive(ConfigDescriptor)]` derive macro.
    pub use streamlib_engine::ConfigDescriptor;

    // ---- Permission helpers ----

    pub mod permissions {
        pub use streamlib_engine::{
            request_audio_permission, request_camera_permission, request_display_permission,
        };
    }

    // ---- Platform info ----

    pub use streamlib_engine::platform;

    // ---- CLAP plugin support (Apple-only) ----

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub use streamlib_engine::{ClapPluginInfo, ClapScanner};

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
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub mod engine {
        /// Host-side Vulkan RHI types (HostVulkanDevice,
        /// HostVulkanTexture, HostVulkanPixelBuffer,
        /// HostVulkanTimelineSemaphore, VulkanComputeKernel,
        /// VulkanGraphicsKernel, VulkanRayTracingKernel,
        /// VulkanTextureReadback, VulkanAccelerationStructure, etc.).
        pub use streamlib_engine::host_rhi;

        /// Privileged extension traits surfacing raw `Host*` handles
        /// on SDK-bucket types. Importing one unlocks `vulkan_inner()`
        /// / `from_vulkan()` / `vulkan_device()` on
        /// `StreamTexture` / `RhiPixelBufferRef` / `GpuDevice`.
        pub use streamlib_engine::{
            HostGpuDeviceExt, HostRhiPixelBufferRefExt, HostStreamTextureExt,
        };

        /// Per-runtime surface-share service primitives. For adapter
        /// integration tests and 3rd-party tooling that needs to drive
        /// the service in isolation; production callers go through
        /// [`crate::sdk::runtime::Runner`].
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::linux_surface_share;
    }
}

// =============================================================================
// Tier 3 — direct passthrough to streamlib-engine
// =============================================================================

/// Direct passthrough to the entire `streamlib-engine` crate.
///
/// **Use sparingly.** This exists for the rare case where the SDK's
/// curated [`sdk::engine`] surface doesn't expose what's needed AND
/// extending the SDK isn't right. The path itself signals "I'm
/// reaching past the curated boundary."
///
/// If you find yourself importing from this namespace for an item
/// that would benefit other consumers, that's a signal: either extend
/// the SDK's curated surface or open an issue.
///
/// Direct `streamlib_engine::*` imports outside the engine itself and
/// this facade source are forbidden by
/// `cargo xtask check-boundaries` Check 6 — `engine_internal` is the
/// allowed escape hatch for the very rare cases that need it.
pub mod engine_internal {
    pub use streamlib_engine::*;
}
