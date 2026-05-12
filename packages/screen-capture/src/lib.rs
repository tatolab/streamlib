// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/screen-capture` — screen capture processor for streamlib.
//!
//! Apple-only today (macOS / iOS via ScreenCaptureKit). A Linux screen-
//! capture processor is tracked separately.

pub mod _generated_;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleScreenCaptureProcessor;

pub use _generated_::ScreenCaptureConfig;
pub use _generated_::tatolab__screen_capture::screen_capture_config::TargetType;
