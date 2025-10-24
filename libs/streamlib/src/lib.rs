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
//! let mut runtime = StreamRuntime::new(60.0);
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

// Re-export core types at crate root (but not the runtime module itself)
pub use core::{
    RingBuffer, Clock, TimedTick, SoftwareClock, PTPClock, GenlockClock,
    StreamError, Result, TickBroadcaster, GpuContext,
    VideoFrame, AudioBuffer, DataMessage, MetadataValue,
    StreamProcessor, StreamOutput, StreamInput, PortType, PortMessage,
    // Note: CameraProcessor and DisplayProcessor traits are in core,
    // but we'll re-export platform implementations below
    CameraDevice, CameraOutputPorts,
    WindowId, DisplayInputPorts,
    ShaderId, // from runtime, but we'll override StreamRuntime below
    Schema, Field, FieldType, SemanticVersion, SerializationFormat,
    ProcessorDescriptor, PortDescriptor, ProcessorExample,
    SCHEMA_VIDEO_FRAME, SCHEMA_AUDIO_BUFFER, SCHEMA_DATA_MESSAGE,
    SCHEMA_BOUNDING_BOX, SCHEMA_OBJECT_DETECTIONS,
    ProcessorRegistry, ProcessorRegistration, ProcessorFactory,
    DescriptorProvider, global_registry,
    register_processor, register_processor_descriptor,
    list_processors, list_processors_by_tag,
    create_processor, is_processor_registered, unregister_processor,
    Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView,
    ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge,
};

// Platform-specific module (conditional compilation)
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,
    WgpuBridge,
    MetalDevice,
};

// Optional MCP module (feature-gated)
#[cfg(feature = "mcp")]
pub mod mcp;

// Optional Python module (feature-gated)
#[cfg(feature = "python")]
pub mod python;

// Platform-configured runtime wrapper
mod runtime;
pub use runtime::StreamRuntime;

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
