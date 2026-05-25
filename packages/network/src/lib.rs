// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/network` — generic UDP source/sink processors. Byte-level
//! transport only. Framing, reassembly, and protocol-specific concerns
//! belong in consumer packages.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod udp_sink;
pub mod udp_source;

pub use udp_sink::UdpSinkProcessor;
pub use udp_source::UdpSourceProcessor;

streamlib_plugin_abi::export_plugin!(
    crate::UdpSinkProcessor::Processor,
    crate::UdpSourceProcessor::Processor,
);
