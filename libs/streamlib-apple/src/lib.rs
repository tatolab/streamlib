//! streamlib-apple: Metal → WebGPU bridge for macOS and iOS
//!
//! This crate provides thin wrappers around Apple platform features,
//! exposing them as WebGPU resources for use with streamlib-core.
//!
//! ## Architecture
//!
//! streamlib-apple is a **wrapper layer only** - it doesn't implement
//! any runtime logic or processing. Instead, it:
//!
//! 1. Wraps native Metal GPU resources
//! 2. Bridges them to WebGPU (wgpu)
//! 3. Wraps platform features (Camera, ARKit, IOSurface)
//! 4. Exposes everything as WebGPU-compatible types
//!
//! ## Core Modules
//!
//! - `wgpu_bridge` - Metal ↔ WebGPU zero-copy bridging
//! - `metal` - Metal device creation and management
//! - `iosurface` - Zero-copy texture sharing via IOSurface
//! - `camera` - AVFoundation camera capture → WebGPU textures
//! - `arkit` - ARKit AR frames → WebGPU textures
//! - `texture` - Metal texture utilities
//!
//! ## Optional Features
//!
//! - `display` - Window/display support (disabled by default for headless use)
//!
//! ## Example: Creating a WebGPU-enabled runtime
//!
//! ```ignore
//! use streamlib_apple::{WgpuBridge, metal::MetalDevice};
//! use streamlib_core::StreamRuntime;
//!
//! // Create Metal device
//! let metal_device = MetalDevice::system_default()?;
//!
//! // Create WebGPU bridge (wraps Metal)
//! let bridge = WgpuBridge::new(metal_device.device().clone()).await?;
//!
//! // Create runtime with WebGPU
//! let mut runtime = StreamRuntime::new(60.0);
//! let (device, queue) = bridge.into_wgpu();
//! runtime.set_wgpu(device, queue);
//! ```

// Core wrapper modules
pub mod arkit;
pub mod iosurface;
pub mod metal;
pub mod texture;
pub mod wgpu_bridge;

// StreamProcessor implementations
pub mod processors;

// Runtime configuration (used by streamlib facade)
pub mod runtime_ext;

// Internal helpers (not part of public API)
mod runtime_helpers;

// Re-export core types
pub use streamlib_core::{Result, StreamError};

// Re-export wrapper types
pub use metal::MetalDevice;
pub use wgpu_bridge::WgpuBridge;

// Re-export processor implementations
pub use processors::{AppleCameraProcessor, AppleDisplayProcessor};

#[cfg(test)]
mod tests {
    #[test]
    fn test_platform_detection() {
        #[cfg(target_os = "macos")]
        println!("Running on macOS");

        #[cfg(target_os = "ios")]
        println!("Running on iOS");
    }
}
