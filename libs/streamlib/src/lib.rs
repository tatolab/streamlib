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

/// Unified logging pathway — public re-export of [`core::logging`].
pub use core::logging;

/// Generated types from JTD schemas.
/// Run `cargo xtask generate-schemas` to regenerate.
pub mod _generated_;

// Re-export commonly used generated config types
pub use _generated_::{ApiServerConfig, Encodedaudioframe, Encodedvideoframe, Videoframe};

// Re-export attribute macros for processor syntax:
// - #[streamlib::processor("com.tatolab.camera")] - Processor definition by name lookup in streamlib.yaml
// - #[derive(ConfigDescriptor)] - Config field metadata derive macro
pub use streamlib_macros::{processor, ConfigDescriptor};

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
    // TODO: Migrate to iceoryx2 API
    // AudioCaptureConfig,
    AudioChannelConverterProcessor,
    AudioCodec,
    // TODO: Migrate to iceoryx2 API
    // AudioDevice,
    // TODO: Migrate to iceoryx2 API
    // AudioInputDevice,
    AudioMixerProcessor,
    // TODO: Migrate to iceoryx2 API
    // AudioOutputConfig,
    AudioResamplerProcessor,
    BufferRechunkerProcessor,
    // TODO: Migrate to iceoryx2 API
    // CameraConfig,
    // CameraDevice,
    ChordGeneratorProcessor,
    ConnectionDefinition,
    // Processor traits (mode-specific)
    ContinuousProcessor,
    // TODO: Migrate to iceoryx2 API
    // DisplayConfig,
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
    // TODO: Migrate to iceoryx2 API
    // Mp4WriterConfig,
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
    StreamError,
    StreamTexture,
    TextureDescriptor,
    TextureFormat,
    TexturePool,
    TexturePoolDescriptor,
    TextureUsages,
    TimeContext,
    VideoCodec,
    // TODO: Migrate to iceoryx2 API
    // WindowId,
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

// Apple processor re-exports (migrated to iceoryx2 API)
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{
    AppleAudioCaptureProcessor as AudioCaptureProcessor,
    AppleAudioOutputProcessor as AudioOutputProcessor,
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,
    AppleMp4WriterProcessor as Mp4WriterProcessor,
    AppleScreenCaptureProcessor as ScreenCaptureProcessor,
    // MetalDevice,
    // VideoToolbox encoder (config types are in core::codec):
    // VideoToolboxEncoder,
};

// Linux processor re-exports
#[cfg(target_os = "linux")]
pub use linux::{
    LinuxAudioCaptureProcessor as AudioCaptureProcessor,
    LinuxAudioOutputProcessor as AudioOutputProcessor,
    LinuxCameraProcessor as CameraProcessor,
    LinuxDisplayProcessor as DisplayProcessor,
};

// Vulkan compositor re-exports for in-tree examples that drive the kernel
// directly. Linux-only because the Vulkan module itself is Linux-gated.
#[cfg(target_os = "linux")]
pub use vulkan::rhi::{
    blending_compositor_flags, BlendingCompositorInputs, BlendingCompositorPushConstants,
    CrtFilmGrainInputs, CrtFilmGrainPushConstants, VulkanBlendingCompositor, VulkanCrtFilmGrain,
};

/// Per-runtime surface-share service primitives. Exposed for adapter
/// integration tests and 3rd-party tooling that needs to drive the
/// service in isolation; production callers go through [`StreamRuntime`].
#[cfg(target_os = "linux")]
pub mod linux_surface_share {
    pub use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};
}

/// Public surface for the host-side Vulkan RHI types that
/// `streamlib-adapter-cpu-readback` (legitimately host-side per
/// `docs/architecture/adapter-runtime-integration.md`), the in-tree
/// surface adapter tests, and host-side application code need to
/// name. The module is intentionally narrow: it exposes the `Host*`
/// flavor of each RHI primitive plus [`HostMarker`], nothing more.
///
/// Subprocess cdylibs MUST NOT depend on `streamlib` at runtime —
/// they get the trait machinery + `Consumer*` flavor from
/// `streamlib-consumer-rhi`, and the FullAccess capability boundary
/// is enforced by Cargo (this module is unreachable from a dep graph
/// that excludes `streamlib`).
///
/// The previous `streamlib::adapter_support` module which re-exported
/// both `Host*` and `Consumer*` was deleted as part of #560 — its
/// transitional shape collapsed both flavors into one place; the
/// type-system-enforced boundary needs the consumer flavor in a
/// separate crate.
#[cfg(target_os = "linux")]
pub mod host_rhi {
    pub use crate::vulkan::rhi::{
        HostMarker, HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTexture,
        HostVulkanTimelineSemaphore, VulkanComputeKernel, VulkanTextureReadback,
    };

    /// EGL DRM-modifier probe — exposed so adapter conformance tests
    /// can pick a sampler-only modifier (`external_only=TRUE`) that
    /// would otherwise be discarded by the higher-level
    /// `acquire_render_target_dma_buf_image` path.
    pub use crate::vulkan::rhi::drm_modifier_probe;
}

// WebRTC streaming (cross-platform)
pub use core::streaming::{WebRtcSession, WhepClient, WhepConfig, WhipClient, WhipConfig};

// WebRTC WHIP/WHEP processors (cross-platform)
pub use core::processors::{WebRtcWhepProcessor, WebRtcWhipProcessor};

// Codec processors (cross-platform)
pub use core::processors::{OpusEncoderProcessor, OpusDecoderProcessor};
pub use _generated_::{OpusEncoderConfig, OpusDecoderConfig};

// Vulkan Video codec processors (Linux)
#[cfg(target_os = "linux")]
pub use linux::processors::h264_encoder::H264EncoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::h265_encoder::H265EncoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::h264_decoder::H264DecoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::h265_decoder::H265DecoderProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::mp4_writer::LinuxMp4WriterProcessor;
#[cfg(target_os = "linux")]
pub use linux::processors::bgra_file_source::BgraFileSourceProcessor;
#[cfg(target_os = "linux")]
pub use _generated_::{
    H264EncoderConfig, H264DecoderConfig,
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

pub use core::StreamRuntime;

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
