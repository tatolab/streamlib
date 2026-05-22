// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video encoder/decoder primitives — the codec layer of the
//! engine RHI. Lives above the core GPU plumbing (`vulkan::rhi`); the
//! public constructors take engine RHI handles (`GpuContextFullAccess`),
//! not raw Vulkan device / queue / allocator handles.
//!
//! Origin: ported from NVIDIA nvpro-samples
//! (<https://github.com/nvpro-samples/vk_video_samples>). The standalone
//! `SimpleEncoder::new` / `SimpleDecoder::new` self-owned-device paths
//! that originated with the port are scheduled for removal in favor of
//! the engine RHI-integrated `from_full_access` constructors.

// --- Public API ---
pub mod video_context;
pub mod decode;
pub mod encode;
pub mod rgb_to_nv12;
pub mod nv12_to_rgb;
pub mod rhi;

// Public codec types — re-exported at the engine `crate::vulkan::video::*`
// surface and pulled through to `streamlib::sdk::engine::video::*`.
// The `rhi` submodule (which holds `RhiQueueSubmitter`) stays `pub` because
// several codec-interior `pub fn` items hold `Arc<dyn RhiQueueSubmitter>`
// fields and Rust's privacy rules require the trait to be at least as
// visible as those items. No consumer-facing API references the trait
// directly: `from_full_access` constructs the submitter internally. The
// trait's eventual removal — codec calling `HostVulkanDevice` methods
// directly instead of going through the trait — is follow-up interior
// re-plumbing under the Vulkan Video RHI Coupling milestone.
pub use video_context::{VideoContext, VideoError, VideoResult};
pub use decode::{DecodedFrame, SimpleDecoder, SimpleDecoderConfig, SimpleDecodedFrame};
pub use encode::{EncodedOutput, FrameType};
pub use encode::{SimpleEncoder, SimpleEncoderConfig, EncodePacket, Codec, Preset};
pub use encode::{color_vui, H273ColorVui};
pub use rgb_to_nv12::RgbToNv12Converter;
pub use nv12_to_rgb::Nv12ToRgbConverter;

// --- Internal modules (ported 1-to-1 from nvpro C++) ---
pub mod codec_utils;
pub mod nv_video_parser;
pub mod vk_video_decoder;
pub mod vk_video_encoder;
pub mod vk_video_parser;
pub mod frame_buffer;
