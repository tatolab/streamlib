// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/jpeg` — GPU-backed JPEG decoder processor, thin wrapper
//! around `libs/vulkan-jpeg::SimpleJpegDecoder`.
//!
//! Linux-only today; the underlying primitive is Linux-only
//! (VulkanComputeBackend + optional NvJpegBackend, both bound to
//! `streamlib::sdk::context::GpuContextFullAccess`).

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::JpegDecoderProcessor;

pub use _generated_::{EncodedJpegFrame, JpegDecoderConfig};
