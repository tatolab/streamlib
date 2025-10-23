//! # streamlib - Platform-Agnostic Real-Time Video Streaming
//!
//! streamlib provides infrastructure for building real-time video processing pipelines
//! that work across macOS, iOS, Linux, and Windows. It uses a WebGPU-first architecture
//! for zero-copy GPU operations and platform abstraction.
//!
//! ## Architecture
//!
//! streamlib follows the React Native model:
//!
//! - **Platform-agnostic core** (`streamlib-core`): Runtime, processors, ports, tick system
//! - **Platform-specific implementations**: Camera, Display, AR features (auto-selected at compile time)
//! - **WebGPU bridging**: Zero-copy texture sharing across platforms
//!
//! ## User Experience
//!
//! Users write platform-agnostic code that "just works":
//!
//! ```ignore
//! use streamlib::{CameraProcessor, DisplayProcessor, StreamRuntime};
//!
//! #[tokio::main]
//! async fn main() -> streamlib::Result<()> {
//!     let mut runtime = StreamRuntime::new(60.0);
//!
//!     let mut camera = CameraProcessor::new()?;
//!     let mut display = DisplayProcessor::new()?;
//!
//!     runtime.connect(&mut camera.ports.output.video, &mut display.ports.input.video)?;
//!
//!     runtime.add_processor(Box::new(camera));
//!     runtime.add_processor(Box::new(display));
//!
//!     runtime.start().await?;
//!     Ok(())
//! }
//! ```
//!
//! The same code compiles and runs on macOS, Linux, and Windows - the correct
//! platform implementation is selected automatically at compile time.
//!
//! ## Platform Support
//!
//! | Platform | Status | Backend |
//! |----------|--------|---------|
//! | macOS    | ✅ Supported | Metal via wgpu |
//! | iOS      | ✅ Supported | Metal via wgpu |
//! | Linux    | 🚧 Planned | Vulkan via wgpu |
//! | Windows  | 🚧 Planned | D3D12 via wgpu |
//!
//! ## Zero-Copy Architecture
//!
//! streamlib uses WebGPU (wgpu) as a unified GPU abstraction layer:
//!
//! - **macOS/iOS**: Native Metal textures → WebGPU (zero-copy via wgpu-hal)
//! - **Linux**: Native Vulkan textures → WebGPU (zero-copy via wgpu-hal)
//! - **Windows**: Native D3D12 textures → WebGPU (zero-copy via wgpu-hal)
//!
//! All platform-specific details are hidden from the user.

// Re-export all core types (always available, platform-agnostic)
pub use streamlib_core::{
    // Processors and Ports
    StreamProcessor,
    StreamInput, StreamOutput,
    PortType, PortMessage,

    // Processor Traits (platform implementations provided below)
    CameraProcessor as CameraProcessorTrait,
    DisplayProcessor as DisplayProcessorTrait,
    CameraDevice, CameraOutputPorts,
    WindowId, DisplayInputPorts,

    // Messages
    VideoFrame, AudioBuffer, DataMessage, MetadataValue,

    // Textures (WebGPU types)
    Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView,

    // GPU Context
    GpuContext,

    // Clock system
    Clock, TimedTick, SoftwareClock, PTPClock, GenlockClock,

    // Buffers and Events
    RingBuffer, TickBroadcaster,

    // Topology
    ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge,

    // Error handling
    StreamError, Result,

    // Other types
    ShaderId,
};

// Platform-configured runtime wrapper
mod runtime;
pub use runtime::StreamRuntime;

//
// Platform-Specific Processors
//
// These are conditionally compiled based on the target platform.
// Users import from `streamlib::CameraProcessor` - Rust automatically
// pulls in the correct platform implementation.
//

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg_attr(docsrs, doc(cfg(any(target_os = "macos", target_os = "ios"))))]
pub use streamlib_apple::{
    // Core types
    WgpuBridge,
    MetalDevice,

    // Processor implementations (automatically selected on Apple platforms)
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,

    // TODO: ARKitProcessor,
};

#[cfg(target_os = "linux")]
#[cfg_attr(docsrs, doc(cfg(target_os = "linux")))]
pub use streamlib_linux::{
    // TODO: Create streamlib-linux
    // CameraProcessor,
    // DisplayProcessor,
};

#[cfg(target_os = "windows")]
#[cfg_attr(docsrs, doc(cfg(target_os = "windows")))]
pub use streamlib_windows::{
    // TODO: Create streamlib-windows
    // CameraProcessor,
    // DisplayProcessor,
};

// Compile-time platform check
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "windows"
)))]
compile_error!(
    "streamlib is not yet supported on this platform. \
     Supported platforms: macOS, iOS, Linux, Windows. \
     Contributions welcome: https://github.com/tato123/streamlib"
);

/// Platform information
pub mod platform {
    /// Returns the current platform name
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

    /// Returns the GPU backend being used
    pub fn gpu_backend() -> &'static str {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        return "Metal";

        #[cfg(target_os = "linux")]
        return "Vulkan";

        #[cfg(target_os = "windows")]
        return "Direct3D 12";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = platform::name();
        let backend = platform::gpu_backend();

        println!("Running on: {}", platform);
        println!("GPU backend: {}", backend);

        // Verify platform is detected
        assert!(!platform.is_empty());
        assert!(!backend.is_empty());
    }
}
