// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[path = "../processors/moq_publish_track.rs"]
pub mod moq_publish_track;
#[path = "../processors/moq_subscribe_track.rs"]
pub mod moq_subscribe_track;

streamlib_plugin_abi::export_plugin!(
    crate::moq_publish_track::MoqPublishTrackProcessor::Processor,
    crate::moq_subscribe_track::MoqSubscribeTrackProcessor::Processor,
);
