// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/camera` — camera capture processor carved out of the streamlib engine.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// Cross-platform shim that re-exports the per-platform impl under a unified name.
#[cfg(target_os = "linux")]
pub mod camera;

/// Camera → CUDA host-pipeline copy processor (Linux only — CUDA is
/// Linux-only on the in-tree adapter set; macOS / iOS compile a
/// no-op stub that returns a configuration error from `setup()`).
/// Lives here rather than in a downstream example so any consumer
/// that wants the camera frame as a GPU-resident CUDA tensor picks
/// it up for free without re-deriving the VkBuffer / timeline /
/// surface-share wiring.
pub mod camera_to_cuda_copy;

#[cfg(target_os = "linux")]
pub mod linux;

// The Apple (AVFoundation) camera arm reaches the `streamlib` engine facade
// plus the facade-only `sdk::display_info` refresh-rate query and
// `sdk::rhi::PixelBufferRef`, which the engine-free plugin SDK exposes only
// under `cfg(linux)` today. Parked under `_apple_impl_pending_` and gated so
// it never compiles on any target; to unpark — restore the `camera.rs` Apple
// re-export + this crate's Apple entry in `export_plugin!` below and re-add
// the objc2 / dispatch2 interop deps in `Cargo.toml` — once the plugin SDK
// ships an engine-free Apple RHI surface.
#[cfg(any())]
mod _apple_impl_pending_;

#[cfg(target_os = "linux")]
pub use camera::{CameraDevice, CameraProcessor};
pub use camera_to_cuda_copy::{CameraToCudaCopyProcessor, CUDA_CAMERA_SURFACE_ID};
// Re-exported from `_generated_` (codegen'd from
// `schemas/camera_to_cuda_copy_config.yaml`) so callers can construct
// the config without reaching into the `_generated_` path themselves.
pub use _generated_::CameraToCudaCopyConfig;

#[cfg(target_os = "linux")]
streamlib_plugin_abi::export_plugin!(
    crate::CameraProcessor::Processor,
    crate::CameraToCudaCopyProcessor::Processor,
);
