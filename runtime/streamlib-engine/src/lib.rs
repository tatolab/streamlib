// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    clippy::too_many_arguments,
    reason = "some engine APIs take many independent parameters, such as a video encoding config"
)]
#![allow(
    clippy::type_complexity,
    reason = "the engine's nested generic types read clearly where they are used"
)]
#![allow(
    clippy::missing_safety_doc,
    reason = "safety contracts are documented in the implementation comments"
)]
#![allow(
    clippy::wrong_self_convention,
    reason = "to_* methods on Copy types take self by value on purpose"
)]
#![allow(
    clippy::collapsible_match,
    reason = "nested matches read more clearly in some places"
)]
#![allow(
    clippy::manual_clamp,
    reason = "a written-out clamp reads more clearly in some places"
)]
#![allow(
    clippy::should_implement_trait,
    reason = "some method names match a std trait's without implementing it"
)]

// Allow `::streamlib::` paths to work inside this crate (for proc macro generated code)
extern crate self as streamlib;

// Re-export crossbeam_channel for macro-generated code
pub use crossbeam_channel;
pub use schemars;
pub use serde_json;

pub mod core;
pub mod iceoryx2;

/// Unified logging pathway. Wraps the engine-internal
/// [`core::logging`] module so customer code can reach the
/// pathway via `streamlib_engine::logging::*` while the
/// `streamlib_engine::core::logging` module path stays
/// engine-private.
pub mod logging {
    pub use crate::core::logging::{
        ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES, EngineLogRecordForTheParentProcess,
        EngineLogRecordsDrainedForTheParentProcess, HelperProcessEngineLogRecordRing,
        LoggingTunables, PrettyLogMirrorStandardStream, ProcessLoggingPathwayHold, ProjectLogRoute,
        ProjectLogRouteEntry, ProjectLogRouteEntryEnteredOnThisThread,
        RUNTIME_OWN_LOG_INSTANCE_NAME, StreamlibLoggingConfig, StreamlibLoggingGuard,
        capture_this_helper_processes_engine_log_records,
        carrying_this_threads_project_log_route_entry, hold_the_process_logging_pathway,
        init_for_tests, listen_for_each_project_log_written,
        run_in_the_project_log_route_entry_when_there_is_one,
        the_project_log_route_entry_of_this_thread,
    };
}

// Re-export the attribute macro for processor syntax:
// #[streamlib::processor(execution = …, …)] - execution + ports in code; a
// processor's identity is the import path of its type, captured by the macro
pub use streamlib_macros::{processor, processor_config};

/// The `#[processor]` attribute accepts no authored identity, in any spelling.
///
/// The message is asserted at the parse seam in `streamlib-macros`; these
/// assert the refusal survives a real expansion, which a unit test on the
/// parser cannot.
///
/// The first block is the control, and it is what makes the other two mean
/// anything: it is the same fixture with the identity removed, and it must
/// COMPILE. A `compile_fail` block passes for any reason at all — an
/// unresolved path, a missing trait impl — so without a positive twin proving
/// the fixture is otherwise sound, the two refusals below would pass on a
/// typo.
///
/// ```
/// use streamlib::sdk::context::RuntimeContextFullAccess;
/// use streamlib::sdk::error::Result;
///
/// #[streamlib::sdk::processor(execution = manual)]
/// pub struct AcceptedWithoutIdentity;
///
/// impl streamlib::sdk::processors::ManualProcessor for AcceptedWithoutIdentity::Processor {
///     fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
///         Ok(())
///     }
/// }
/// ```
///
/// The leading positional `@org/package/Type` the grammar used to take:
///
/// ```compile_fail
/// use streamlib::sdk::context::RuntimeContextFullAccess;
/// use streamlib::sdk::error::Result;
///
/// #[streamlib::sdk::processor("@tatolab/camera/Camera", execution = manual)]
/// pub struct RefusedPositionalIdentity;
///
/// impl streamlib::sdk::processors::ManualProcessor for RefusedPositionalIdentity::Processor {
///     fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
///         Ok(())
///     }
/// }
/// ```
///
/// The `type = "..."` override that used to name the synthesized segment:
///
/// ```compile_fail
/// use streamlib::sdk::context::RuntimeContextFullAccess;
/// use streamlib::sdk::error::Result;
///
/// #[streamlib::sdk::processor(execution = manual, type = "CustomName")]
/// pub struct RefusedTypeOverride;
///
/// impl streamlib::sdk::processors::ManualProcessor for RefusedTypeOverride::Processor {
///     fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
///         Ok(())
///     }
/// }
/// ```
#[cfg(doctest)]
pub struct ProcessorAttributeAcceptsNoIdentity;

pub use core::{
    // Processor traits (mode-specific)
    ContinuousProcessor,
    Error,
    GpuContext,
    GraphSnapshot,
    InputPortMarker,
    ManualProcessor,
    NativeTextureHandle,
    OutputPortMarker,
    PROCESSOR_REGISTRY,
    PooledTextureHandle,
    ProcessorSpec,
    ReactiveProcessor,
    Result,
    RuntimeContext,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamClock,
    StreamClockSpec,
    Texture,
    TextureDescriptor,
    TextureFormat,
    TexturePool,
    TexturePoolDescriptor,
    TextureUsages,
    // Port marker traits and helpers for compile-time safe connections
    input,
    media_clock::MediaClock,
    output,
};

// The one RHI, on every supported platform. MoltenVK is the macOS driver;
// there is no second backend and no per-platform RHI.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod vulkan;

// Linux platform services
#[cfg(target_os = "linux")]
pub(crate) mod linux;

// Platform services (Apple)
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

/// Per-runtime surface-share service primitives. Exposed for adapter
/// integration tests and 3rd-party tooling that needs to drive the
/// service in isolation; production callers go through [`Runner`].
#[cfg(target_os = "linux")]
pub mod linux_surface_share {
    pub use crate::linux::surface_share::UnixSocketSurfaceService;
    pub use crate::linux::surface_share::state::SurfaceShareState;
}

/// Per-runtime surface-share service primitives over raw Mach, the macOS
/// peer of `linux_surface_share`. Exposed for the service's multi-process
/// tests and tooling that drives it in isolation; production callers go
/// through [`Runner`].
#[cfg(target_os = "macos")]
pub mod apple_surface_share {
    pub use crate::apple::iosurface::{
        RetainedIOSurfaceSharedAcrossThreads, create_iosurface_mach_send_right,
        create_private_iosurface_for_a_gpu_image, create_private_iosurface_with_packed_rows,
    };
    pub use crate::apple::surface_share::{
        CROSS_PROCESS_TIMELINE_WAIT_BOUND, ConsumerReleaseOutcome, CrossProcessTimelinePair,
        CrossProcessTimelinePairsBySurface, IOSurfaceShareState, MachSurfaceShareService,
        MachSurfaceShareServiceRendezvous, SurfaceShareHelperProcessAdmission,
    };
}

/// The ALSA audio arm's own entry point, for the audio tier only.
///
/// The chain's probe takes the first arm that opens and no dial overrides it,
/// so on a machine carrying an audio daemon the tier could never reach this
/// arm through [`core::context::probe_audio_device_backend`] — which is what
/// every production caller uses, and naming this type selects nothing: the
/// runtime still asks the chain. `doc(hidden)` rather than feature-gated so
/// the tier file is typechecked in an ordinary build and its tests report as
/// `ignored` the way every other tier-2 test does.
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub mod linux_alsa_audio_device_backend {
    pub use crate::linux::alsa_audio_device_backend::AlsaAudioDeviceBackend;
}

/// The CoreAudio arm's device lookups, the microphone gate's preflight and the
/// application macOS asks on this process's behalf, for the audio tier only: a
/// test pins the devices it measures and names the setting a person must
/// change, and for which application, rather than timing out. `doc(hidden)`
/// for the same reason as the ALSA arm's entry point.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod apple_coreaudio_audio_tier {
    pub use crate::apple::coreaudio_audio_device_backend::{
        CoreAudioStreamDirection, built_in_audio_device_uid, default_audio_device_uid,
    };
    pub use crate::apple::permissions::microphone_access_for_a_capture_hardware_test;
    pub use crate::apple::responsible_gui_application::responsible_gui_application_name;
}

/// The resolved colour → V4L2 direction of the engine's V4L2 colour map, for
/// the virtual camera's loopback door, which signals a V4L2 output device at
/// `S_FMT` with it.
#[cfg(target_os = "linux")]
pub mod linux_v4l2_color {
    pub use crate::linux::v4l2_color::{V4l2PixFormatColorFields, resolved_color_to_v4l2_color};
}

/// The virtual camera's PipeWire door: a `Video/Source` camera node fed YUY2
/// rows over shared memory, opened by the node that uses it.
#[cfg(target_os = "linux")]
pub mod linux_pipewire_camera {
    pub use crate::linux::pipewire_video_source::{
        PipeWireCameraFramePresentation, PipeWireCameraNode,
    };
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod host_rhi;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use host_rhi::{HostGpuDeviceExt, HostPixelBufferRefExt, HostTextureExt};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use host_rhi::HostSurfaceStoreExt;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::permissions::{request_audio_permission, request_camera_permission};

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod permissions {

    use crate::core::Result;

    pub fn request_camera_permission() -> Result<bool> {
        tracing::info!("Camera permission granted (no system prompt on this platform)");
        Ok(true)
    }

    pub fn request_audio_permission() -> Result<bool> {
        tracing::info!("Audio permission granted (no system prompt on this platform)");
        Ok(true)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub use permissions::{request_audio_permission, request_camera_permission};

pub use core::Runner;

pub mod platform {
    pub fn name() -> &'static str {
        #[cfg(target_os = "macos")]
        return "macOS";
        #[cfg(target_os = "ios")]
        return "iOS";
        #[cfg(target_os = "linux")]
        return "Linux";
        #[cfg(target_os = "windows")]
        return "Windows";
    }
}

// =============================================================================
// SDK self-mirror — for macro path resolution inside the engine
// =============================================================================
//
// Engine-internal processors use the same `#[streamlib::sdk::processor]`
// attribute syntax as external consumers. With `extern crate self as
// streamlib;` above, those macro paths (`::streamlib::sdk::*`) resolve
// inside this crate via this self-mirror. Mirrors the public SDK's
// `streamlib::sdk::*` structure (defined in `sdk/streamlib-sdk/src/lib.rs`)
// so the same emit-paths work in both compilation contexts.
//
// External consumers see this crate's items as `streamlib::engine_internal::*`
// (passthrough re-export from the SDK). The SDK re-exports this mirror's
// `processors` module as its public `streamlib::sdk::processors`; the rest of
// this namespace exists only for in-engine macro path resolution.

pub mod sdk {
    pub use crate::core::context;
    pub use crate::core::descriptors;
    pub use crate::core::error;
    pub use crate::core::execution;
    pub use crate::core::graph;
    pub use crate::core::graph::graph_snapshot;
    pub use crate::core::json_schema;
    pub use crate::core::media_clock;
    pub use crate::core::prelude;
    pub use crate::core::rhi;
    pub use crate::core::runtime;
    pub use crate::core::texture;
    pub use crate::core::utils;

    /// Processor traits and specs from `core::processors`, with the port
    /// markers and the `input`/`output` helpers from `core::graph`.
    pub mod processors {
        pub use crate::core::processors::{
            __generated_private, Config, ConfigValidationError, ContinuousProcessor,
            DynGeneratedProcessor, DynamicProcessorConstructorFn, EmptyConfig, GeneratedProcessor,
            InstalledModuleOfANodeType, ManualProcessor, NodeTypesOneStreamResolves,
            OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWireOutcome,
            OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope, PROCESSOR_REGISTRY,
            ProcessorInstance, ProcessorInstanceFactory, ProcessorSpec, ProcessorState,
            ProcessorStateComponent, ReactiveProcessor, VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME,
            traits,
        };

        // Port markers + input/output helpers — semantically processor-
        // related; live in `core::graph::edges::link_port_markers`.
        pub use crate::core::graph::{InputPortMarker, OutputPortMarker, input, output};
    }

    pub use crate::crossbeam_channel;
    pub use crate::iceoryx2;
    pub use crate::logging;
    pub use crate::schemars;
    pub use crate::serde_json;

    pub use streamlib_macros::{processor, processor_config};

    pub mod permissions {
        pub use crate::{request_audio_permission, request_camera_permission};
    }

    pub use crate::platform;

    /// Engine-bridge surface mirror — same shape the SDK exposes via
    /// [`streamlib::sdk::engine`](../../streamlib-sdk/src/lib.rs).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub mod engine {
        pub use crate::HostSurfaceStoreExt;
        pub use crate::host_rhi;
        #[cfg(target_os = "linux")]
        pub use crate::linux_surface_share;
        pub use crate::{HostGpuDeviceExt, HostPixelBufferRefExt, HostTextureExt};
    }
}
