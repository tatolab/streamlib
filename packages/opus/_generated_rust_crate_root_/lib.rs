// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[path = "../processors/opus_decoder.rs"]
pub mod opus_decoder;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[path = "../processors/opus_encoder.rs"]
pub mod opus_encoder;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    crate::opus_decoder::OpusDecoderProcessor::Processor,
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    crate::opus_encoder::OpusEncoderProcessor::Processor,
);
