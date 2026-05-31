// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grayscale video-effect package — a Rust-backed processor loaded as a
//! cdylib via `runtime.add_module_with(..., Strategy::Path)` against this
//! crate's `streamlib.yaml`. The cdylib's `STREAMLIB_PLUGIN` callback
//! registers the `GrayscaleRust` processor with the host registry.
//!
//! The Linux implementation ([`grayscale_linux`]) samples the input
//! camera texture through a sandboxed graphics kernel and writes a
//! BT.601-luma grayscale frame into a ring of output render-target
//! textures. The macOS implementation ([`grayscale_apple`]) reads pixels
//! directly from `CVPixelBuffer` memory via CoreVideo.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
mod grayscale_kernel;
#[cfg(target_os = "linux")]
mod grayscale_linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod grayscale_apple;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
use streamlib_plugin_abi::export_plugin;

#[cfg(target_os = "linux")]
export_plugin!(grayscale_linux::GrayscaleProcessor::Processor);

#[cfg(any(target_os = "macos", target_os = "ios"))]
export_plugin!(grayscale_apple::GrayscaleProcessor::Processor);
