// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/frame-tap` — a sink processor that fans out from any video
//! output port and samples frames to disk (JPEG) on a configurable
//! strategy, off the hot path. The seed for streamlib's general tap
//! behavior: attach it to a camera, decoder, or effect output to inspect
//! that output and capture images without rerouting the real pipeline.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// GPU texture readback is Linux-only (the host RHI's `VulkanTextureReadback`
// lives behind `#[cfg(target_os = "linux")]`). The tap follows the same
// platform split as camera/display.
#[cfg(target_os = "linux")]
pub mod frame_tap;

#[cfg(target_os = "linux")]
pub use frame_tap::FrameTapProcessor;

#[cfg(target_os = "linux")]
streamlib_plugin_abi::export_plugin!(crate::FrameTapProcessor::Processor,);
