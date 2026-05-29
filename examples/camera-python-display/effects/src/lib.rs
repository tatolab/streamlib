// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera-python-display effects package — Rust-backed processors
//! loaded as a cdylib via `runtime.add_module_with_blocking(...,
//! Strategy::Path)` against this crate's
//! `streamlib.yaml`.

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
