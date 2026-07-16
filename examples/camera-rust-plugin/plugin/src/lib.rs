// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grayscale video-effect package — a Rust-backed cdylib processor. The
//! package is linked into the consuming app's `streamlib_modules/` (via
//! `streamlib link ./plugin`) and the runtime lazily discovers + loads this
//! cdylib on the first `processor_type_ref!` reference to it; its
//! `STREAMLIB_PLUGIN` callback then registers the `GrayscaleRust` processor
//! with the host registry.
//!
//! The Linux implementation ([`grayscale_linux`]) samples the input
//! camera texture through a sandboxed graphics kernel and writes a
//! BT.601-luma grayscale frame into a ring of output render-target
//! textures.
//!
//! The macOS/iOS grayscale arm (`grayscale_apple.rs`) reads pixels from
//! `CVPixelBuffer` on the CPU. It is not compiled here: it needs the
//! `GpuContextLimitedAccess` CPU-surface accessors (`check_out_surface` /
//! `acquire_pixel_buffer`) that the engine-free `streamlib-plugin-sdk`
//! carries on Linux but not yet on macOS. The source stays in-tree; the
//! rewrite onto the SDK's macOS surface is a separate follow-up (Apple
//! paths are parked until the Linux path is stable). Compiling it here
//! would require the `streamlib` facade, reintroducing the double-engine
//! hazard this cdylib avoids by linking only the SDK.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
mod grayscale_kernel;
#[cfg(target_os = "linux")]
mod grayscale_linux;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::export_plugin;

#[cfg(target_os = "linux")]
export_plugin!(grayscale_linux::GrayscaleProcessor::Processor);
