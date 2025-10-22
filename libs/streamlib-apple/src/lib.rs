//! streamlib-apple: Metal/IOSurface GPU backend for macOS and iOS
//!
//! This crate provides zero-copy GPU texture management for Apple platforms.
//! It implements the streamlib runtime primitives using:
//! - Metal for GPU operations
//! - IOSurface for zero-copy texture sharing
//! - AVFoundation for camera capture
//! - ARKit for AR capabilities (iOS full, macOS limited)

// Platform-specific modules (flat structure)
pub mod metal;
pub mod iosurface;
pub mod camera;
pub mod arkit;
pub mod texture;
pub mod display;
pub mod processors;
pub mod runtime_ext;
pub mod text;

// Re-export core types
pub use streamlib_core::{Result, StreamError};

// Re-export Metal-specific types
pub use texture::MetalTextureGpuData;

// Re-export runtime constructor (auto-configured for macOS)
pub use runtime_ext::new_runtime;

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
