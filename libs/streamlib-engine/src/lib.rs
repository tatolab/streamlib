// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Suppress pedantic clippy warnings that are intentional design choices
#![allow(clippy::too_many_arguments)] // Some APIs need many parameters (e.g., video encoding config)
#![allow(clippy::type_complexity)] // Complex types are clear in context
#![allow(clippy::missing_safety_doc)] // Safety documented in implementation comments
#![allow(clippy::arc_with_non_send_sync)] // Used intentionally for specific threading patterns
#![allow(clippy::wrong_self_convention)] // to_* methods with Copy types are intentional
#![allow(clippy::collapsible_match)] // Nested matches are clearer in some cases
#![allow(clippy::manual_clamp)] // Manual clamp is sometimes clearer
#![allow(clippy::should_implement_trait)] // Method names like `default` are contextually clear

// Allow `::streamlib::` paths to work inside this crate (for proc macro generated code)
extern crate self as streamlib;

// Re-export crossbeam_channel for macro-generated code
pub use crossbeam_channel;
pub use inventory;
pub use serde_json;

pub mod core;
pub mod iceoryx2;

/// Unified logging pathway. Wraps the engine-internal
/// [`core::logging`] module so customer code can reach the
/// pathway via `streamlib_engine::logging::*` while the
/// `streamlib_engine::core::logging` module path stays
/// engine-private.
pub mod logging {
    pub use crate::core::logging::*;
}

/// Generated types from JTD schemas.
/// Run `cargo xtask generate-schemas` to regenerate.
pub mod _generated_;

// Re-export commonly used generated config types
pub use _generated_::{ApiServerConfig, EncodedAudioFrame, EncodedVideoFrame, VideoFrame};

// Re-export attribute macros for processor syntax:
// - #[streamlib::processor("Camera")] - Processor definition by name lookup in streamlib.yaml
// - #[derive(ConfigDescriptor)] - Config field metadata derive macro
pub use streamlib_macros::{processor, schema_ident, schema_ident_any_version, ConfigDescriptor};

pub use core::{
    are_synchronized,
    gl_constants,
    // Port marker traits and helpers for compile-time safe connections
    input,
    media_clock::MediaClock,
    output,
    timestamp_delta_ms,
    video_audio_delta_ms,
    video_audio_synchronized,
    video_audio_synchronized_with_tolerance,
    AudioCodec,
    ConnectionDefinition,
    // Processor traits (mode-specific)
    ContinuousProcessor,
    GlContext,
    GlTextureBinding,
    GpuContext,
    GraphFileDefinition,
    H264Profile,
    InputPortMarker,
    LfoWaveform,
    ManualProcessor,
    Mp4Muxer,
    Mp4MuxerConfig,
    NativeTextureHandle,
    OutputPortMarker,
    ParameterAutomation,
    ParameterInfo,
    ParameterModulator,
    PluginInfo,
    PooledTextureHandle,
    ProcessorDefinition,
    ProcessorSpec,
    ReactiveProcessor,
    ResamplingQuality,
    Result,
    RtpTimestampCalculator,
    RuntimeContext,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    Error,
    Texture,
    TextureDescriptor,
    TextureFormat,
    TexturePool,
    TexturePoolDescriptor,
    TextureUsages,
    TimeContext,
    VideoCodec,
    DEFAULT_SYNC_TOLERANCE_MS,
    FOURCC_H264,
    PROCESSOR_REGISTRY,
};

pub use core::ApiServerProcessor;

pub use core::{convert_audio_to_sample, convert_video_to_samples};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use core::{ClapEffectProcessor, ClapPluginInfo, ClapScanner};

// GPU Backends - Metal and Vulkan
// Metal module is always available on macOS/iOS since Apple platform services need Metal types
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod metal;

// Vulkan module: explicit feature OR Linux default
#[cfg(any(
    feature = "backend-vulkan",
    all(target_os = "linux", not(feature = "backend-metal"))
))]
pub(crate) mod vulkan;

// Linux platform services
#[cfg(target_os = "linux")]
pub(crate) mod linux;

// Platform services (Apple)
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

// Display processor lives in `@tatolab/display` (#674).

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{
    AppleMp4WriterProcessor as Mp4WriterProcessor,
    AppleScreenCaptureProcessor as ScreenCaptureProcessor,
};

/// Per-runtime surface-share service primitives. Exposed for adapter
/// integration tests and 3rd-party tooling that needs to drive the
/// service in isolation; production callers go through [`Runner`].
#[cfg(target_os = "linux")]
pub mod linux_surface_share {
    pub use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};
}

#[cfg(any(
    feature = "backend-vulkan",
    all(target_os = "linux", not(feature = "backend-metal"))
))]
pub mod host_rhi;

#[cfg(any(
    feature = "backend-vulkan",
    all(target_os = "linux", not(feature = "backend-metal"))
))]
pub use host_rhi::{HostGpuDeviceExt, HostPixelBufferRefExt, HostTextureExt};

// WebRTC streaming (cross-platform)
pub use core::streaming::{WebRtcSession, WhepClient, WhepConfig, WhipClient, WhipConfig};

// WebRTC WHIP/WHEP processors (cross-platform)
pub use core::processors::{WebRtcWhepProcessor, WebRtcWhipProcessor};

// Codec processors (cross-platform)
pub use core::processors::{OpusEncoderProcessor, OpusDecoderProcessor};
pub use _generated_::{OpusEncoderConfig, OpusDecoderConfig};

// Vulkan Video codec processors (Linux)
// H.264 encoder/decoder live in `@tatolab/h264` (`streamlib-h264`) (#675).
#[cfg(target_os = "linux")]
pub use linux::processors::h265_encoder::H265EncoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::h265_decoder::H265DecoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::mp4_writer::LinuxMp4WriterProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::bgra_file_source::BgraFileSourceProcessor;
#[cfg(target_os = "linux")]
pub use _generated_::{
    H265EncoderConfig, H265DecoderConfig,
    LinuxMp4WriterConfig, BgraFileSourceConfig,
};

// MoQ streaming (cross-platform)
#[cfg(feature = "moq")]
pub use core::processors::{MoqPublishTrackProcessor, MoqSubscribeTrackProcessor};
#[cfg(feature = "moq")]
pub use _generated_::{MoqPublishTrackConfig, MoqSubscribeTrackConfig};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::permissions::{
    request_audio_permission, request_camera_permission, request_display_permission,
};

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod permissions {

    use crate::core::Result;

    pub fn request_camera_permission() -> Result<bool> {
        tracing::info!("Camera permission granted (no system prompt on this platform)");
        Ok(true)
    }

    pub fn request_display_permission() -> Result<bool> {
        tracing::info!("Display permission granted (no system prompt on this platform)");
        Ok(true)
    }

    pub fn request_audio_permission() -> Result<bool> {
        tracing::info!("Audio permission granted (no system prompt on this platform)");
        Ok(true)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub use permissions::{
    request_audio_permission, request_camera_permission, request_display_permission,
};

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

    pub fn gpu_backend() -> &'static str {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        return "Metal";
        #[cfg(target_os = "linux")]
        return "Vulkan";
        #[cfg(target_os = "windows")]
        return "Direct3D 12";
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
// `streamlib::sdk::*` structure (defined in `libs/streamlib-sdk/src/lib.rs`)
// so the same emit-paths work in both compilation contexts.
//
// External consumers see this crate's items as `streamlib::engine_internal::*`
// (passthrough re-export from the SDK). They never reach for `streamlib::sdk::*`
// in this crate directly — that namespace exists only for in-engine macro
// path resolution.

pub mod sdk {
    pub use crate::core::context;
    pub use crate::core::descriptors;
    pub use crate::core::display_info;
    pub use crate::core::error;
    pub use crate::core::execution;
    pub use crate::core::graph;
    pub use crate::core::graph_file;
    pub use crate::core::json_schema;
    pub use crate::core::media_clock;
    pub use crate::core::prelude;
    pub use crate::core::rhi;
    pub use crate::core::runtime;
    pub use crate::core::streaming;
    pub use crate::core::sync;
    pub use crate::core::texture;
    pub use crate::core::utils;

    /// Processors namespace: combines `core::processors::*` with
    /// platform-aliased processor types from the engine root.
    pub mod processors {
        pub use crate::core::processors::*;

        // Port markers + input/output helpers — semantically processor-
        // related; live in `core::graph::edges::link_port_markers`.
        pub use crate::core::graph::{
            input, output, InputPortMarker, OutputPortMarker,
        };

        // Port schema spec — semantically processor-related; lives in
        // `core::descriptors`.
        pub use crate::core::descriptors::PortSchemaSpec;

        // Camera processor lives in `streamlib-camera` (#673).
        // Display processor lives in `streamlib-display` (#674).

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        pub use crate::{ClapEffectProcessor, Mp4WriterProcessor, ScreenCaptureProcessor};

        #[cfg(target_os = "linux")]
        pub use crate::{
            BgraFileSourceProcessor,
            H265DecoderProcessor, H265EncoderProcessor, LinuxMp4WriterProcessor,
        };

        #[cfg(feature = "moq")]
        pub use crate::{MoqPublishTrackProcessor, MoqSubscribeTrackProcessor};
    }

    pub use crate::iceoryx2;
    pub use crate::logging;
    pub use crate::inventory;
    pub use crate::serde_json;
    pub use crate::crossbeam_channel;
    pub use crate::_generated_;

    pub use streamlib_macros::{processor, schema_ident, schema_ident_any_version, ConfigDescriptor};

    pub mod permissions {
        pub use crate::{
            request_audio_permission, request_camera_permission, request_display_permission,
        };
    }

    pub use crate::platform;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub use crate::{ClapPluginInfo, ClapScanner};

    /// Engine-bridge surface mirror — same shape the SDK exposes via
    /// [`streamlib::sdk::engine`](../../streamlib-sdk/src/lib.rs).
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub mod engine {
        pub use crate::host_rhi;
        pub use crate::{HostGpuDeviceExt, HostPixelBufferRefExt, HostTextureExt};
        #[cfg(target_os = "linux")]
        pub use crate::linux_surface_share;
    }
}
