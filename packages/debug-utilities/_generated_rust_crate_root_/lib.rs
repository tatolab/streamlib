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
#[path = "../processors/bgra_file_source.rs"]
pub mod bgra_file_source;
#[cfg(target_os = "linux")]
#[path = "../processors/jpeg_bytes_source.rs"]
pub mod jpeg_bytes_source;
#[path = "../processors/live_video_frame_forwarder.rs"]
pub mod live_video_frame_forwarder;
#[path = "../processors/simple_passthrough.rs"]
pub mod simple_passthrough;
#[path = "../processors/video_frame_counter.rs"]
pub mod video_frame_counter;

streamlib_plugin_abi::export_plugin!(
    #[cfg(target_os = "linux")]
    crate::bgra_file_source::BgraFileSourceProcessor::Processor,
    #[cfg(target_os = "linux")]
    crate::jpeg_bytes_source::JpegBytesSourceProcessor::Processor,
    crate::live_video_frame_forwarder::LiveVideoFrameForwarderProcessor::Processor,
    crate::simple_passthrough::SimplePassthroughProcessor::Processor,
    crate::video_frame_counter::VideoFrameCounterProcessor::Processor,
);
