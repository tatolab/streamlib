
pub mod core;

// Re-export derive macro with same name as trait (standard Rust pattern)
// The macro and trait occupy different namespaces, so no collision occurs
pub use streamlib_macros::StreamProcessor;

pub use core::{
    media_clock::MediaClock,
    StreamError, Result, GpuContext, AudioContext, RuntimeContext,
    VideoFrame, AudioFrame, DataFrame, MetadataValue,
    StreamElement, ElementType, DynStreamElement,
    StreamOutput, StreamInput, PortType, PortMessage, ProcessorConnection,
    CameraDevice, CameraConfig,
    WindowId, DisplayConfig,
    AudioDevice, AudioOutputConfig,
    AudioInputDevice, AudioCaptureConfig,
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
    ParameterInfo, PluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation,
    ChordGeneratorProcessor, ChordGeneratorConfig,
    AudioMixerProcessor, MixingStrategy, AudioMixerConfig,
    Schema, Field, FieldType, SemanticVersion, SerializationFormat,
    ProcessorDescriptor, PortDescriptor, ProcessorExample,
    AudioRequirements,
    SCHEMA_VIDEO_FRAME, SCHEMA_AUDIO_FRAME, SCHEMA_DATA_MESSAGE,
    SCHEMA_BOUNDING_BOX, SCHEMA_OBJECT_DETECTIONS,
    timestamp_delta_ms, video_audio_delta_ms,
    are_synchronized, video_audio_synchronized, video_audio_synchronized_with_tolerance,
    DEFAULT_SYNC_TOLERANCE_MS,
    ProcessorRegistry, ProcessorRegistration,
    DescriptorProvider, global_registry,
    register_processor,
    list_processors, list_processors_by_tag,
    is_processor_registered, unregister_processor,
    Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView,
    ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,
    AppleAudioOutputProcessor as AudioOutputProcessor,
    AppleAudioCaptureProcessor as AudioCaptureProcessor,
    WgpuBridge,
    MetalDevice,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::permissions::{request_camera_permission, request_display_permission, request_audio_permission};

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
pub use permissions::{request_camera_permission, request_display_permission, request_audio_permission};

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(any(feature = "python", feature = "python-embed"))]
pub mod python;

mod runtime;
pub use runtime::StreamRuntime;

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

#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
#[pymodule]
fn streamlib(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    python::register_python_module(m)
}
