// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/h265` — H.265 encoder + decoder processors via Vulkan Video.
//!
//! Linux-only today; macOS VideoToolbox H.265 is a future addition.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::{H265DecoderProcessor, H265EncoderProcessor};

pub use _generated_::{H265DecoderConfig, H265EncoderConfig};

#[cfg(all(feature = "plugin", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(
    crate::H265DecoderProcessor::Processor,
    crate::H265EncoderProcessor::Processor,
);
