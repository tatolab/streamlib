// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free grayscale-compute plugin — a Rust-backed processor loaded as
//! a cdylib via `runtime.add_module_with(..., Strategy::Path)` against this
//! crate's `streamlib.yaml`. The cdylib's `STREAMLIB_PLUGIN` callback
//! registers the `GrayscaleCompute` processor with the host registry.
//!
//! Unlike `camera-rust-plugin` (which links the `streamlib` engine facade),
//! this plugin depends ONLY on the engine-free `streamlib-plugin-sdk`. It
//! resolves the incoming camera `VideoFrame` surface to a GPU `Texture` via
//! [`GpuContextLimitedAccess::resolve_texture_registration_by_surface_id`],
//! runs a BT.601-luma grayscale SPIR-V compute kernel built through
//! [`GpuContextFullAccess::create_compute_kernel`], and writes the result
//! into a ring of output textures — proving the engine-free surface-consumer
//! path end-to-end.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
mod grayscale_compute;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::export_plugin;

#[cfg(target_os = "linux")]
export_plugin!(grayscale_compute::GrayscaleComputeProcessor::Processor);
