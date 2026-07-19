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
//! use streamlib::sdk::rhi::Texture;
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
//! to the `pub` surface of the `streamlib-engine` crate. For very
//! rare cases where the SDK's curated `sdk::engine` surface doesn't
//! expose what's needed AND extending the SDK isn't right. Reads as
//! "I'm reaching past the curated boundary; I know what I'm doing."
//!
//! Engine-internal `core::*` modules (`compiler`, `embedded_schemas`,
//! `runtime_hooks`, etc.) are `pub(crate)` in the engine, so
//! `streamlib::engine_internal::core::<internal>` does not compile
//! by construction — even Tier 3 cannot reach module paths the
//! engine has marked private. Items inside those modules that
//! genuinely need cross-crate access are explicitly re-exported
//! item-by-item at `streamlib_engine::core::*` or at the engine
//! crate root. The set of those re-exports IS the engine's external
//! surface; if one is missing, file an issue rather than reaching
//! around the boundary.
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
    // Engine internals (`compiler`, `config`, `embedded_schemas`,
    // `logging`, `observability`, `runtime_hooks`, `signals`,
    // `streamlib_home`) are `pub(crate)` in the engine crate (see
    // `core/mod.rs`) — those module paths are not reachable here OR
    // via `engine_internal::*` (Tier 3) by construction. Items inside
    // that ARE customer-facing are re-exported by the engine at its
    // crate root and travel into the SDK via the top-level
    // `pub use streamlib_engine::*` items below and the Tier-3
    // `engine_internal` namespace.

    pub use streamlib_engine::core::color;
    pub use streamlib_engine::core::context;
    pub use streamlib_engine::core::descriptors;
    pub use streamlib_engine::core::display_info;
    pub use streamlib_engine::core::error;
    pub use streamlib_engine::core::execution;
    pub use streamlib_engine::core::graph;
    pub use streamlib_engine::core::graph_snapshot;
    pub use streamlib_engine::core::json_schema;
    pub use streamlib_engine::core::media_clock;
    /// Plugin-loading host-services payload + cdylib install helper
    /// — referenced from `streamlib_plugin_abi::export_plugin!`
    /// macro expansion.
    pub use streamlib_engine::core::plugin;
    pub use streamlib_engine::core::prelude;
    pub use streamlib_engine::core::pubsub;
    pub use streamlib_engine::core::rhi;
    pub use streamlib_engine::core::runtime;

    /// The default polyglot build orchestrator — available behind the
    /// `auto-build` feature (on by default). For most callers prefer the
    /// [`RunnerAutoBuild::with_auto_build`] convenience over wiring this
    /// by hand. A `--no-default-features` (frozen `.slpkg`-only) build
    /// excludes this (and the trait below) entirely.
    #[cfg(feature = "auto-build")]
    pub use streamlib_build_orchestrator::PolyglotBuildOrchestrator;

    /// Extension constructor (behind the `auto-build` feature) that builds
    /// a [`runtime::Runner`] with the default [`PolyglotBuildOrchestrator`]
    /// already wired — the common dev / runtime-authoring path. The engine
    /// crate can't provide this itself (the orchestrator lives downstream
    /// of the engine), so it's an SDK extension trait. Import it to call
    /// `Runner::with_auto_build()`.
    ///
    /// `Runner::new()` stays orchestrator-free for frozen `.slpkg`-only /
    /// custom-orchestrator deployments.
    #[cfg(feature = "auto-build")]
    pub trait RunnerAutoBuild {
        /// A `Runner` with the default polyglot build orchestrator wired,
        /// so build-requiring module loads (`Strategy::Path`/`Git` +
        /// `IfStale`/`AlwaysBuild`) materialize from source on demand.
        fn with_auto_build()
        -> streamlib_engine::core::error::Result<std::sync::Arc<runtime::Runner>>;
    }

    #[cfg(feature = "auto-build")]
    impl RunnerAutoBuild for runtime::Runner {
        fn with_auto_build()
        -> streamlib_engine::core::error::Result<std::sync::Arc<runtime::Runner>> {
            runtime::Runner::new_with_orchestrator(PolyglotBuildOrchestrator::default())
        }
    }

    pub use streamlib_engine::core::sync;
    pub use streamlib_engine::core::texture;

    // ---- App authoring sugar ----

    /// [`App`] — thin authoring sugar over [`runtime::Runner`]. Construct, add
    /// processors, connect ports, run; every method forwards to an existing
    /// `Runner` op.
    #[cfg(feature = "auto-build")]
    pub mod app;

    #[cfg(feature = "auto-build")]
    pub use app::{App, AppPortEndpoint};

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
        pub use streamlib_engine::core::graph::{InputPortMarker, OutputPortMarker, input, output};

        // Port schema spec — semantically processor-related; lives in
        // `core::descriptors` in engine source (re-exported from
        // `streamlib-processor-schema`).
        pub use streamlib_engine::core::descriptors::PortSchemaSpec;
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

    /// Schemas currently registered with the runtime.
    pub use streamlib_engine::schemas;

    // ---- Procedural macros ----

    /// `#[streamlib::sdk::processor("@org/pkg/Type@version", …)]` attribute
    /// macro — declares processor identity, execution, and ports.
    pub use streamlib_engine::processor;

    /// `#[derive(ConfigDescriptor)]` derive macro.
    pub use streamlib_engine::ConfigDescriptor;

    /// `streamlib::sdk::schema_ident_any_version!("org", "package", "Type")` —
    /// resolve a `SchemaIdent` **now** against the **already-registered**
    /// processor types (the post-`add_module` / power-caller form).
    /// Validates `(org, package, type)` at compile time; resolves the
    /// version at runtime against the global processor registry,
    /// picking the highest registered `SemVer` (Cargo / npm convention).
    /// Returns `Result<SchemaIdent, streamlib::sdk::error::Error>`. For a
    /// version-free reference that lazily loads its provider from
    /// `streamlib_modules/`, use [`processor_type_ref!`] instead.
    pub use streamlib_engine::schema_ident_any_version;

    /// `streamlib::sdk::schema_ident!("org", "package", "Type", "1.0.0")` —
    /// strict version-pinning form. Short form of
    /// [`SchemaIdent::new`](descriptors::SchemaIdent::new). Reach for
    /// this only when you have a deliberate reason to refuse
    /// newer-but-compatible registered versions; otherwise prefer
    /// [`schema_ident_any_version!`].
    pub use streamlib_engine::schema_ident;

    /// `streamlib::sdk::processor_type_ref!("org", "package", "Type")` — a
    /// **version-free** processor-type reference for the lazy-discovery world
    /// (app code that never calls `add_module`). Validates `(org, package,
    /// type)` at compile time and expands to a
    /// [`ProcessorTypeReference::ResolveToInstalled`](processors::ProcessorTypeReference)
    /// with no version and **no registry lookup at the call site**, so the
    /// reference reaches `add_processor`'s lazy hook and resolves to the
    /// single installed provider — loading its package from
    /// `streamlib_modules/` on first reference. This is the canonical form for
    /// referencing a processor by `@org/package/Type` with no version; prefer
    /// it over [`schema_ident_any_version!`] when you want lazy loading.
    pub use streamlib_engine::processor_type_ref;

    /// `streamlib::sdk::module_ident!("org", "name", "^1.0.0")` —
    /// imperative-API module identifier with a pinned semver range.
    /// Validates org / name / semver range at compile time; expands to
    /// a [`ModuleIdent::new`](descriptors::ModuleIdent::new)
    /// expression. Pair with [`crate::sdk::runtime::Runner::add_module`].
    pub use streamlib_engine::module_ident;

    /// `streamlib::sdk::module_ident_any_version!("org", "name")` —
    /// any-installed-version variant of [`module_ident!`]. Equivalent
    /// to `module_ident!("org", "name", "*")`.
    pub use streamlib_engine::module_ident_any_version;

    /// `streamlib::sdk::module_ident_joined!("@org/name", "^1.0.0")` —
    /// joined-org/name variant of [`module_ident!`]. Same identifier;
    /// different call-site ergonomics for callers that already have
    /// the canonical `"@org/name"` string in hand.
    pub use streamlib_engine::module_ident_joined;

    /// `streamlib::sdk::module_ident_joined_any_version!("@org/name")` —
    /// joined-org/name + any-version variant. Equivalent to
    /// `module_ident_joined!("@org/name", "*")`.
    pub use streamlib_engine::module_ident_joined_any_version;

    // ---- Permission helpers ----

    pub mod permissions {
        pub use streamlib_engine::{
            request_audio_permission, request_camera_permission, request_display_permission,
        };
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
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
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

        /// Vulkan Video codec layer — engine-tier H.264/H.265
        /// encode/decode primitives (`SimpleEncoder`, `SimpleDecoder`,
        /// `Codec`, `Preset`, `EncodePacket`, codec configuration
        /// types, `H273ColorVui`). Construction goes through
        /// `SimpleEncoder::from_full_access` /
        /// `SimpleDecoder::from_full_access`, which take the host
        /// `&GpuContextFullAccess` and wire the host RHI's queue
        /// mutex, allocator, and per-queue families internally.
        /// Domain codec packages (`@tatolab/h264`, `@tatolab/h265`)
        /// reach for the codec types here.
        #[cfg(target_os = "linux")]
        pub use streamlib_engine::video;
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
/// // `core::observability` is `pub(crate)` in streamlib-engine.
/// // Importing the module path itself fails because the module is
/// // crate-private — locks the visibility downgrade independent of
/// // which items the module currently re-exports.
/// use streamlib::engine_internal::core::observability;
/// ```
///
/// ```compile_fail
/// // `core::runtime_hooks` is `pub(crate)` in streamlib-engine.
/// use streamlib::engine_internal::core::runtime_hooks::RuntimeInitHook;
/// ```
///
/// Items inside engine-internal modules that genuinely need
/// cross-crate access are explicitly re-exported item-by-item at
/// `streamlib_engine::core::*` (e.g. `ProjectConfig`,
/// `get_cached_package_dir`) or at the engine crate root. The set of
/// those re-exports IS the engine's external surface.
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
