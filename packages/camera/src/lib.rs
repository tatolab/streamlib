// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/camera` — camera capture processor carved out of the streamlib engine.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// Cross-platform shim that re-exports the per-platform impl under a unified name.
pub mod camera;

/// Camera → CUDA host-pipeline copy processor (Linux only — CUDA is
/// Linux-only on the in-tree adapter set; macOS / iOS compile a
/// no-op stub that returns a configuration error from `setup()`).
/// Lives here rather than in a downstream example so any consumer
/// that wants the camera frame as a GPU-resident CUDA tensor picks
/// it up for free without re-deriving the VkBuffer / timeline /
/// surface-share / adapter wiring.
pub mod camera_to_cuda_copy;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

pub use camera::{CameraDevice, CameraProcessor};
pub use camera_to_cuda_copy::{
    CameraToCudaCopyConfig, CameraToCudaCopyProcessor, CUDA_CAMERA_SURFACE_ID,
};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
streamlib_plugin_abi::export_plugin!(
    crate::CameraProcessor::Processor,
    crate::CameraToCudaCopyProcessor::Processor,
);
