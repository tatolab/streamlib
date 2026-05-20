// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/h264` — H.264 encoder + decoder via Vulkan Video, carved
//! out of the streamlib engine substrate.
//!
//! Linux-only today; macOS VideoToolbox H.264 is a future addition.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::{H264DecoderProcessor, H264EncoderProcessor};

pub use _generated_::{H264DecoderConfig, H264EncoderConfig};

#[cfg(all(feature = "plugin", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(
    crate::H264DecoderProcessor::Processor,
    crate::H264EncoderProcessor::Processor,
);

// `_apple_impl_pending_` holds the VideoToolbox encoder/decoder + the
// cross-platform Apple-flavored codec wrappers parked out of the engine
// in #786. Gated so it never compiles; re-enable + rewire imports to
// the public SDK surface once Apple support is activated.
#[cfg(any())]
mod _apple_impl_pending_;
