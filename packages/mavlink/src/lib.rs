// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/mavlink` — MAVLink2 encoder + decoder processors wrapping the
//! `rust-mavlink` crate's common-dialect message set. Pairs with
//! `@tatolab/network` for MAVLink-over-UDP.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod mavlink_decoder;
pub mod mavlink_encoder;

pub use mavlink_decoder::MavlinkDecoderProcessor;
pub use mavlink_encoder::MavlinkEncoderProcessor;

#[cfg(feature = "plugin")]
streamlib_plugin_abi::export_plugin!(
    crate::MavlinkDecoderProcessor::Processor,
    crate::MavlinkEncoderProcessor::Processor,
);
