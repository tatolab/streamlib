// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/screen-capture` — screen capture processor for streamlib.
//!
//! Apple-only today (macOS / iOS via ScreenCaptureKit). A Linux screen-
//! capture processor is tracked separately.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleScreenCaptureProcessor;

pub use _generated_::ScreenCaptureConfig;
pub use _generated_::tatolab__screen_capture::screen_capture_config::TargetType;

#[cfg(all(feature = "plugin", any(target_os = "macos", target_os = "ios")))]
streamlib_plugin_abi::export_plugin!(crate::AppleScreenCaptureProcessor::Processor);
