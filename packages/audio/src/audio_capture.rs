// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-specific re-exports with unified names.

#[cfg(target_os = "linux")]
pub use crate::linux::{
    LinuxAudioCaptureProcessor as AudioCaptureProcessor,
    LinuxAudioInputDevice as AudioInputDevice,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use crate::apple::{
    AppleAudioCaptureProcessor as AudioCaptureProcessor,
    AppleAudioInputDevice as AudioInputDevice,
};
