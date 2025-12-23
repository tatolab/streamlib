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

// Re-export attribute macros for processor syntax:
// - #[streamlib::processor(execution = Reactive)] - Main processor definition
// - #[streamlib::input] - Input port marker
// - #[streamlib::output] - Output port marker
// - #[streamlib::config] - Config field marker
pub use streamlib_macros::{config, input, output, processor};

pub use core::{
    are_synchronized,
    convert_audio_to_sample,
    // Port marker traits and helpers for compile-time safe connections
    input,
    media_clock::MediaClock,
    output,
    timestamp_delta_ms,
    video_audio_delta_ms,
    video_audio_synchronized,
    video_audio_synchronized_with_tolerance,
    AudioCaptureConfig,
    AudioChannelConverterConfig,
    AudioChannelConverterProcessor,
    AudioDevice,
    AudioEncoderConfig,
    AudioEncoderOpus,
    AudioFrame,
    AudioInputDevice,
    AudioMixerConfig,
    AudioMixerProcessor,
    AudioOutputConfig,
    AudioRequirements,
    AudioResamplerConfig,
    AudioResamplerProcessor,
    BufferRechunkerConfig,
    BufferRechunkerProcessor,
    CameraConfig,
    CameraDevice,
    ChannelConversionMode,
    ChordGeneratorConfig,
    ChordGeneratorProcessor,
    ClapEffectConfig,
    ClapEffectProcessor,
    ClapPluginInfo,
    ClapScanner,
    // Processor traits (mode-specific)
    ContinuousProcessor,
    DataFrame,
    DisplayConfig,
    EncodedAudioFrame,
    Field,
    FieldType,
    GpuContext,
    InputPortMarker,
    LfoWaveform,
    LinkCapacity,
    LinkInput,
    LinkInputDataReader,
    LinkInstance,
    LinkOutput,
    LinkOutputDataWriter,
    LinkPortMessage,
    LinkPortType,
    ManualProcessor,
    MetadataValue,
    MixingStrategy,
    Mp4WriterConfig,
    // Streaming utilities:
    OpusEncoder,
    OutputPortMarker,
    ParameterAutomation,
    ParameterInfo,
    ParameterModulator,
    PluginInfo,
    PortDescriptor,
    ProcessorDescriptor,
    ProcessorExample,
    ProcessorSpec,
    ReactiveProcessor,
    ResamplingQuality,
    Result,
    RtpTimestampCalculator,
    RuntimeContext,
    Schema,
    SemanticVersion,
    SerializationFormat,
    StreamError,
    Texture,
    TextureDescriptor,
    TextureFormat,
    TextureUsages,
    TextureView,
    VideoFrame,
    WindowId,
    DEFAULT_SYNC_TOLERANCE_MS,
    PROCESSOR_REGISTRY,
    SCHEMA_AUDIO_FRAME,
    SCHEMA_BOUNDING_BOX,
    SCHEMA_DATA_MESSAGE,
    SCHEMA_OBJECT_DETECTIONS,
    SCHEMA_VIDEO_FRAME,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use core::convert_video_to_samples;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{
    AppleAudioCaptureProcessor as AudioCaptureProcessor,
    AppleAudioOutputProcessor as AudioOutputProcessor,
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,
    AppleMp4WriterProcessor as Mp4WriterProcessor,
    H264Profile,
    MetalDevice,
    VideoCodec,
    VideoEncoderConfig,
    // VideoToolbox encoder and config types:
    VideoToolboxEncoder,
    WebRtcSession,
    WebRtcWhepConfig,
    // WebRTC WHEP processor and config types:
    WebRtcWhepProcessor,
    WebRtcWhipConfig,
    // WebRTC WHIP processor and config types:
    WebRtcWhipProcessor,
    // Metal/wgpu utilities:
    WgpuBridge,
    WhepClient,
    WhepConfig,
    WhipClient,
    WhipConfig,
};

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

// MCP server removed - needs complete redesign for Phase 1 architecture
// TODO: Reimplement MCP server with new graph-based API

// Python bindings removed - needs complete redesign for Phase 1 architecture
// TODO: Reimplement Python bindings with new graph-based API

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
