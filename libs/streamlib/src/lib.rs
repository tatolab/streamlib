//! # streamlib - Real-Time Streaming Infrastructure for AI Agents
//!
//! A unified crate providing platform-agnostic streaming with GPU acceleration.
//!
//! ## Architecture
//!
//! ```text
//! streamlib (unified crate)
//!   ├─ core/     - Always included (runtime, processors, GPU)
//!   ├─ apple/    - Conditional (cfg(target_os = "macos"))
//!   ├─ mcp/      - Optional (feature = "mcp")
//!   └─ python/   - Optional (feature = "python")
//! ```
//!
//! ## Features
//!
//! - `default`: Core functionality only
//! - `mcp`: Enable MCP server for AI agents
//! - `python`: Enable Python bindings
//! - `debug-overlay`: Enable FPS/GPU overlay
//!
//! ## Examples
//!
//! ### Basic Rust
//!
//! ```ignore
//! use streamlib::{StreamRuntime, CameraProcessor, DisplayProcessor};
//!
//! let mut runtime = StreamRuntime::new();
//! runtime.add_processor(Box::new(CameraProcessor::new(0)));
//! runtime.start().await?;
//! ```
//!
//! ### MCP Server (feature = "mcp")
//!
//! ```ignore
//! use streamlib::mcp::McpServer;
//! let server = McpServer::new(streamlib::global_registry());
//! server.run_stdio().await?;
//! ```
//!
//! ### Python (feature = "python")
//!
//! ```python
//! from streamlib import camera_processor, StreamRuntime
//! @camera_processor(device_id=0)
//! def camera(): pass
//! ```

// Core module (always included)
pub mod core;

// Re-export procedural macros
pub use streamlib_macros::StreamProcessor as DeriveStreamProcessor;

// Re-export core types at crate root (but not the runtime module itself)
pub use core::{
    Clock, SoftwareClock, AudioClock, VideoClock, PTPClock, GenlockClock,
    media_clock::MediaClock,
    StreamError, Result, GpuContext, AudioContext, RuntimeContext,
    VideoFrame, AudioFrame, DataFrame, MetadataValue,
    MonoSignal, StereoSignal, QuadSignal, FiveOneSignal,
    // v2.0 traits - GStreamer-inspired hierarchy
    StreamElement, ElementType, DynStreamElement,
    StreamOutput, StreamInput, PortType, PortMessage,
    // Note: CameraProcessor, DisplayProcessor, and AudioProcessor traits are in core,
    // but we'll re-export platform implementations below
    CameraDevice, CameraOutputPorts, CameraConfig,
    WindowId, DisplayInputPorts, DisplayConfig,
    AudioDevice, AudioOutputInputPorts, AudioOutputConfig,
    AudioInputDevice, AudioCaptureOutputPorts, AudioCaptureConfig,
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
    ParameterInfo, PluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation,
    ChordGeneratorProcessor, ChordGeneratorOutputPorts, ChordGeneratorConfig,
    AudioMixerProcessor, MixingStrategy,
    AudioMixerOutputPorts, AudioMixerConfig,
    Schema, Field, FieldType, SemanticVersion, SerializationFormat,
    ProcessorDescriptor, PortDescriptor, ProcessorExample,
    AudioRequirements,
    SCHEMA_VIDEO_FRAME, SCHEMA_AUDIO_FRAME, SCHEMA_DATA_MESSAGE,
    SCHEMA_BOUNDING_BOX, SCHEMA_OBJECT_DETECTIONS,
    // Sync utilities
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

// Platform-specific module (conditional compilation)
// Internal use only - external users should use the platform-agnostic aliases below
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

// Platform permission functions
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::permissions::{request_camera_permission, request_display_permission, request_audio_permission};

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod permissions {
    //! Platform permission stubs for non-Apple platforms
    //!
    //! On Linux/Windows, permission handling may differ or not be required.
    //! These stubs allow the same API to be used across platforms.

    use crate::core::Result;

    /// Request camera permission (stub for non-Apple platforms)
    ///
    /// On Linux/Windows, camera permissions are typically handled at the OS level
    /// or don't require explicit runtime requests. This stub always returns true.
    ///
    /// Platform-specific implementations should be added as needed.
    pub fn request_camera_permission() -> Result<bool> {
        tracing::info!("Camera permission granted (no system prompt on this platform)");
        Ok(true)
    }

    /// Request display permission (stub for non-Apple platforms)
    ///
    /// On most platforms, creating windows doesn't require special permissions.
    /// This stub always returns true.
    pub fn request_display_permission() -> Result<bool> {
        tracing::info!("Display permission granted (no system prompt on this platform)");
        Ok(true)
    }

    /// Request audio input permission (stub for non-Apple platforms)
    ///
    /// On Linux/Windows, audio permissions are typically handled at the OS level
    /// or don't require explicit runtime requests. This stub always returns true.
    ///
    /// Platform-specific implementations should be added as needed.
    pub fn request_audio_permission() -> Result<bool> {
        tracing::info!("Audio permission granted (no system prompt on this platform)");
        Ok(true)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub use permissions::{request_camera_permission, request_display_permission, request_audio_permission};

// Optional MCP module (feature-gated)
#[cfg(feature = "mcp")]
pub mod mcp;

// Optional Python module (feature-gated for both python and python-embed)
#[cfg(any(feature = "python", feature = "python-embed"))]
pub mod python;

// Platform-configured runtime wrapper
mod runtime;
pub use runtime::{StreamRuntime, AudioConfig};

// Platform information
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

// PyO3 module definition (when python feature enabled)
#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
#[pymodule]
fn streamlib(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    python::register_python_module(m)
}
