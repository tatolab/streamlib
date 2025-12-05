// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Platform-specific re-exports with unified names
// Users import these common names and get the appropriate platform implementation
#[cfg(target_os = "macos")]
pub use crate::apple::processors::camera::{
    AppleCameraConfig as CameraConfig, AppleCameraDevice as CameraDevice,
    AppleCameraProcessor as CameraProcessor,
};

// Future platform implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::camera::{
//     LinuxCameraProcessor as CameraProcessor,
//     LinuxCameraConfig as CameraConfig,
//     LinuxCameraDevice as CameraDevice,
// };

// #[cfg(target_os = "windows")]
// pub use crate::windows::processors::camera::{
//     WindowsCameraProcessor as CameraProcessor,
//     WindowsCameraConfig as CameraConfig,
//     WindowsCameraDevice as CameraDevice,
// };
