// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
#[path = "../processors/blending_compositor.rs"]
pub mod blending_compositor;
#[cfg(target_os = "linux")]
#[path = "../processors/blending_compositor_kernel.rs"]
pub mod blending_compositor_kernel;
#[cfg(target_os = "linux")]
#[path = "../processors/crt_film_grain.rs"]
pub mod crt_film_grain;
#[cfg(target_os = "linux")]
#[path = "../processors/crt_film_grain_kernel.rs"]
pub mod crt_film_grain_kernel;
#[cfg(target_os = "linux")]
#[path = "../processors/tone_mapper.rs"]
pub mod tone_mapper;

#[cfg(target_os = "linux")]
streamlib_plugin_abi::export_plugin!(
    #[cfg(target_os = "linux")]
    crate::blending_compositor::BlendingCompositorProcessor::Processor,
    #[cfg(target_os = "linux")]
    crate::crt_film_grain::CrtFilmGrainProcessor::Processor,
);
