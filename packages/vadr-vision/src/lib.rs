// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/vadr-vision` — Anduril AI Grand Prix VADR-TS-002 §4.6
//! vision-stream depayloader. Pairs with `@tatolab/network`'s `UdpSource`
//! and `@tatolab/jpeg`'s `JpegDecoder` for an end-to-end
//! `UdpSource(5600) → VadrVisionDepayloader → JpegDecoder → VideoFrame`
//! pipeline.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod depayloader;
pub mod header;
pub mod reassembly;

pub use depayloader::VadrVisionDepayloaderProcessor;

#[cfg(feature = "plugin")]
streamlib_plugin_abi::export_plugin!(crate::VadrVisionDepayloaderProcessor::Processor);
