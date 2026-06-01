// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/moq` — MoQ (Media over QUIC) publish/subscribe track processors.
//! Built on the `streamlib-moq` transport library (publish/subscribe sessions
//! + broadcast catalog).

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod moq_publish_track;
pub mod moq_subscribe_track;

pub use _generated_::{MoqPublishTrackConfig, MoqSubscribeTrackConfig};
pub use moq_publish_track::MoqPublishTrackProcessor;
pub use moq_subscribe_track::MoqSubscribeTrackProcessor;

streamlib_plugin_abi::export_plugin!(
    crate::MoqPublishTrackProcessor::Processor,
    crate::MoqSubscribeTrackProcessor::Processor,
);
