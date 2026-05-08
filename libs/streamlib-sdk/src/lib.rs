// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! streamlib — public authoring API surface.
//!
//! Customer apps depend on this crate (`streamlib`). Engine internals
//! (RHI, IPC plumbing, surface-share, runtime executor) live in a
//! private `streamlib-engine` dep that the SDK pulls in transitively
//! but does NOT publicly re-export. The boundary is enforced by what
//! this crate chooses to surface — items not listed here are
//! unreachable from a `streamlib`-only dep graph.
//!
//! In particular this crate does NOT re-export `host_rhi`,
//! `linux_surface_share`, the `vulkan` / `metal` / `linux` / `apple`
//! backend modules, or the engine-internal `compiler` / runtime
//! executor / embedded-schemas surface. Engine-side adapters and
//! integration tests that need those reach for `streamlib-engine`
//! directly.

// Allow `::streamlib::` paths emitted by the procedural macro to
// resolve back to this crate when invoked from external consumer
// crates (e.g. domain packages, customer apps).
extern crate self as streamlib;

// Re-export crates the procedural macro emits paths into.
pub use streamlib_engine::crossbeam_channel;
pub use streamlib_engine::inventory;
pub use streamlib_engine::serde_json;

// Public authoring API — re-exported from the engine. Each item below
// is intentional; the engine has additional public items
// (`host_rhi::*`, `linux_surface_share::*`, `vulkan::*`, etc.) that
// customer apps must NOT see.
pub use streamlib_engine::{
    are_synchronized,
    convert_audio_to_sample,
    convert_video_to_samples,
    gl_constants,
    input,
    output,
    timestamp_delta_ms,
    video_audio_delta_ms,
    video_audio_synchronized,
    video_audio_synchronized_with_tolerance,
    ApiServerProcessor,
    AudioCodec,
    ConnectionDefinition,
    ContinuousProcessor,
    GlContext,
    GlTextureBinding,
    GpuContext,
    GraphFileDefinition,
    H264Profile,
    InputPortMarker,
    LfoWaveform,
    ManualProcessor,
    MediaClock,
    Mp4Muxer,
    Mp4MuxerConfig,
    NativeTextureHandle,
    OpusDecoderProcessor,
    OpusEncoderProcessor,
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
    StreamError,
    StreamRuntime,
    StreamTexture,
    TextureDescriptor,
    TextureFormat,
    TexturePool,
    TexturePoolDescriptor,
    TextureUsages,
    TimeContext,
    VideoCodec,
    WebRtcSession,
    WebRtcWhepProcessor,
    WebRtcWhipProcessor,
    WhepClient,
    WhepConfig,
    WhipClient,
    WhipConfig,
    DEFAULT_SYNC_TOLERANCE_MS,
    FOURCC_H264,
    PROCESSOR_REGISTRY,
};

// Re-export attribute macros for processor syntax:
// - #[streamlib::processor("Name")]
// - #[derive(ConfigDescriptor)]
pub use streamlib_engine::{processor, ConfigDescriptor};

// Generated config types from JTD schemas.
pub use streamlib_engine::{
    ApiServerConfig, EncodedAudioFrame, EncodedVideoFrame, OpusDecoderConfig, OpusEncoderConfig,
    VideoFrame,
};

// Linux-only generated config types and processors.
#[cfg(target_os = "linux")]
pub use streamlib_engine::{
    BgraFileSourceConfig, BgraFileSourceProcessor, H264DecoderConfig, H264DecoderProcessor,
    H264EncoderConfig, H264EncoderProcessor, H265DecoderConfig, H265DecoderProcessor,
    H265EncoderConfig, H265EncoderProcessor, LinuxMp4WriterConfig, LinuxMp4WriterProcessor,
};

// macOS / iOS — CLAP plugin support and Apple processors.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use streamlib_engine::{ClapEffectProcessor, ClapPluginInfo, ClapScanner};

// Cross-platform processor re-exports use the engine's platform-aliased names.
pub use streamlib_engine::{CameraProcessor, DisplayProcessor};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use streamlib_engine::{Mp4WriterProcessor, ScreenCaptureProcessor};

// MoQ streaming (cross-platform, behind `moq` feature).
#[cfg(feature = "moq")]
pub use streamlib_engine::{
    MoqPublishTrackConfig, MoqPublishTrackProcessor, MoqSubscribeTrackConfig,
    MoqSubscribeTrackProcessor,
};

// Permission helpers.
pub use streamlib_engine::{
    request_audio_permission, request_camera_permission, request_display_permission,
};

// `iceoryx2` Rust wrapper module — required by macro-emitted paths
// (`::streamlib::iceoryx2::OutputWriter` / `InputMailboxes` / `ReadMode`).
pub use streamlib_engine::iceoryx2;

// Generated schema types — required so macro-emitted code can resolve
// `::streamlib::_generated_::FrameType` etc. when consumer crates
// generate their own typed bindings.
pub use streamlib_engine::_generated_;

// Selectively re-export public items from the engine's `core` module
// — the modules customer code is allowed to traverse. Engine internals
// like `compiler`, `embedded_schemas`, `runtime_hooks`, `signals`,
// `streamlib_home`, `pubsub`, `observability`, `streaming` (impls),
// `clap` (engine-internal helpers), `codec` (engine-internal helpers)
// stay engine-private.
pub mod core {
    // Broad re-export of the engine's `core` items so macro-emitted
    // paths (`::streamlib::core::ProcessorSpec`, `::streamlib::core::SchemaIdent`,
    // `::streamlib::core::context::*`, etc.) resolve transparently.
    //
    // TODO(#735): downgrade engine-internal `core::*` modules from
    // `pub` to `pub(crate)` at the engine source-of-truth. Today this
    // glob re-exports `compiler`, `embedded_schemas`, `runtime_hooks`,
    // `observability`, `streamlib_home`, `pubsub`, `signals` — engine
    // internals that customer code can reach as `streamlib::core::*`.
    // The fix is type-system-enforced visibility at the engine, not
    // SDK-side curation discipline. Soft-blocker for #681.
    pub use streamlib_engine::core::*;
}

// `platform` info — small, cross-platform, customer-visible.
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
