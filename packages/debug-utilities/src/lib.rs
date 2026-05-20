// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/debug-utilities` — utility processors for development,
//! demos, and rigorous-input testing (BgraFileSource, SimplePassthrough).

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod simple_passthrough;
pub mod video_frame_counter;

#[cfg(target_os = "linux")]
pub mod bgra_file_source;

#[cfg(target_os = "linux")]
pub mod jpeg_bytes_source;

pub use simple_passthrough::SimplePassthroughProcessor;
pub use video_frame_counter::VideoFrameCounterProcessor;

#[cfg(target_os = "linux")]
pub use bgra_file_source::BgraFileSourceProcessor;

#[cfg(target_os = "linux")]
pub use jpeg_bytes_source::JpegBytesSourceProcessor;

#[cfg(all(feature = "plugin", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(
    crate::SimplePassthroughProcessor::Processor,
    crate::VideoFrameCounterProcessor::Processor,
    crate::BgraFileSourceProcessor::Processor,
    crate::JpegBytesSourceProcessor::Processor,
);

#[cfg(all(feature = "plugin", not(target_os = "linux")))]
streamlib_plugin_abi::export_plugin!(
    crate::SimplePassthroughProcessor::Processor,
    crate::VideoFrameCounterProcessor::Processor,
);
