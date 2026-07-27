// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any(target_os = "macos", target_os = "ios"))]

pub mod audio_capture;
pub mod audio_output;

pub use audio_capture::{AppleAudioCaptureProcessor, AppleAudioInputDevice};
pub use audio_output::{AppleAudioDevice, AppleAudioOutputProcessor};
