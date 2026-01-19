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
pub mod schemas;

// Re-export attribute macros for processor syntax:
// - #[streamlib::processor(execution = Reactive)] - Main processor definition
// - #[streamlib::input] - Input port marker
// - #[streamlib::output] - Output port marker
// - #[streamlib::config] - Config field marker
pub use streamlib_macros::{config, input, output, processor, ConfigDescriptor};

pub use core::{
    are_synchronized,
    convert_audio_to_sample,
    gl_constants,
    // Port marker traits and helpers for compile-time safe connections
    input,
    media_clock::MediaClock,
    output,
    timestamp_delta_ms,
    video_audio_delta_ms,
    video_audio_synchronized,
    video_audio_synchronized_with_tolerance,
    ApiServerConfig,
    ApiServerProcessor,
    // TODO: Migrate to iceoryx2 API
    // AudioCaptureConfig,
    AudioChannelConverterConfig,
    AudioChannelConverterProcessor,
    AudioCodec,
    // TODO: Migrate to iceoryx2 API
    // AudioDevice,
    AudioEncoderConfig,
    AudioEncoderOpus,
    AudioFrame,
    // TODO: Migrate to iceoryx2 API
    // AudioInputDevice,
    AudioMixerConfig,
    AudioMixerProcessor,
    // TODO: Migrate to iceoryx2 API
    // AudioOutputConfig,
    AudioResampler1chProcessor,
    AudioResampler2chProcessor,
    AudioResamplerConfig,
    BufferRechunker1chProcessor,
    BufferRechunker2chProcessor,
    BufferRechunkerConfig,
    // TODO: Migrate to iceoryx2 API
    // CameraConfig,
    // CameraDevice,
    ChannelConversionMode,
    ChordGeneratorConfig,
    ChordGeneratorProcessor,
    // TODO: Migrate to iceoryx2 API
    // ClapEffectConfig,
    // ClapEffectProcessor,
    // ClapPluginInfo,
    // ClapScanner,
    ConnectionDefinition,
    // Processor traits (mode-specific)
    ContinuousProcessor,
    // TODO: Migrate to iceoryx2 API
    // DisplayConfig,
    EncodedAudioFrame,
    EncodedVideoFrame,
    GlContext,
    GlTextureBinding,
    GpuContext,
    GraphFileDefinition,
    H264Profile,
    InputPortMarker,
    LfoWaveform,
    ManualProcessor,
    MixingStrategy,
    Mp4Muxer,
    Mp4MuxerConfig,
    // TODO: Migrate to iceoryx2 API
    // Mp4WriterConfig,
    NativeTextureHandle,
    // Streaming utilities:
    OpusEncoder,
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
    VideoDecoder,
    VideoDecoderConfig,
    VideoEncoder,
    VideoEncoderConfig,
    VideoFrame,
    // TODO: Migrate to iceoryx2 API
    // WindowId,
    DEFAULT_SYNC_TOLERANCE_MS,
    FOURCC_H264,
    PROCESSOR_REGISTRY,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use core::convert_video_to_samples;

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

// Linux platform services (FFmpeg-based encoding)
#[cfg(target_os = "linux")]
pub(crate) mod linux;

// Platform services (Apple)
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

// TODO: Migrate to iceoryx2 API
// #[cfg(any(target_os = "macos", target_os = "ios"))]
// pub use apple::{
//     AppleAudioCaptureProcessor as AudioCaptureProcessor,
//     AppleAudioOutputProcessor as AudioOutputProcessor,
//     AppleCameraProcessor as CameraProcessor,
//     AppleDisplayProcessor as DisplayProcessor,
//     AppleMp4WriterProcessor as Mp4WriterProcessor,
//     MetalDevice,
//     // VideoToolbox encoder (config types are in core::codec):
//     VideoToolboxEncoder,
// };

// WebRTC streaming (cross-platform)
pub use core::streaming::{WebRtcSession, WhepClient, WhepConfig, WhipClient, WhipConfig};

// TODO: Migrate to iceoryx2 API
// WebRTC WHIP/WHEP processors (cross-platform)
// pub use core::processors::{
//     WebRtcWhepConfig, WebRtcWhepProcessor, WebRtcWhipConfig, WebRtcWhipProcessor,
// };

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
