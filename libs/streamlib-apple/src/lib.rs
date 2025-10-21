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

// Re-export core types
pub use streamlib_core::{Result, StreamError};

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
