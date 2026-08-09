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
#[path = "../processors/color_resolved_to_core_linux.rs"]
pub mod color_resolved_to_core_linux;
#[cfg(target_os = "linux")]
#[path = "../processors/decoder_linux.rs"]
pub mod decoder_linux;

#[cfg(target_os = "linux")]
streamlib_plugin_abi::export_plugin!(
    #[cfg(target_os = "linux")]
    crate::decoder_linux::JpegDecoderProcessor::Processor,
);
