// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/screen-capture` — screen capture processor for streamlib.
//!
//! Apple-only today (macOS / iOS via ScreenCaptureKit). The sole processor —
//! the ScreenCaptureKit capture — is parked under `_apple_impl_pending_`, so
//! this package currently ships no live processor on any target. A Linux
//! screen-capture processor is tracked separately.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// The Apple ScreenCaptureKit processor references an engine-free Apple RHI /
// surface-pool surface (`streamlib_plugin_sdk::sdk::rhi::{PixelBuffer,
// PixelBufferRef, PixelFormat}` plus the surface-pool acquire/check-in APIs)
// that the plugin SDK exposes only under `cfg(linux)` today. Gated so it never
// compiles on any target; unpark this module — restore its
// `export_plugin!(AppleScreenCaptureProcessor::Processor)` registration below
// and re-add its Apple interop deps in Cargo.toml — once the plugin SDK ships
// an engine-free Apple RHI / surface-pool surface.
#[cfg(any())]
mod _apple_impl_pending_;

pub use _generated_::ScreenCaptureConfig;
pub use _generated_::tatolab__screen_capture::screen_capture_config::TargetType;
