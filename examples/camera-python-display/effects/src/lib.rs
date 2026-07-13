// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera-python-display effects package — Rust-backed cdylib processors
//! (`BlendingCompositor`, `CrtFilmGrain`). The package is linked into the
//! consuming app's `streamlib_modules/` (via `streamlib link ./effects`) and
//! the runtime lazily discovers + loads this cdylib on the first
//! `processor_type_ref!` reference to one of its processors.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
mod blending_compositor;
#[cfg(target_os = "linux")]
mod blending_compositor_kernel;
#[cfg(target_os = "linux")]
mod crt_film_grain;
#[cfg(target_os = "linux")]
mod crt_film_grain_kernel;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::export_plugin;

#[cfg(target_os = "linux")]
export_plugin!(
    blending_compositor::BlendingCompositorProcessor::Processor,
    crt_film_grain::CrtFilmGrainProcessor::Processor,
);
