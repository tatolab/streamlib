// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/camera` — camera capture processor carved out of the streamlib engine.

pub mod _generated_;

// Cross-platform shim that re-exports the per-platform impl under a unified name.
pub mod camera;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

pub use camera::{CameraDevice, CameraProcessor};
