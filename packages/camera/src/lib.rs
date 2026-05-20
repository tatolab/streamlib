// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/camera` — camera capture processor carved out of the streamlib engine.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// Cross-platform shim that re-exports the per-platform impl under a unified name.
pub mod camera;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

pub use camera::{CameraDevice, CameraProcessor};

#[cfg(all(
    feature = "plugin",
    any(target_os = "linux", target_os = "macos", target_os = "ios")
))]
streamlib_plugin_abi::export_plugin!(crate::CameraProcessor::Processor);
