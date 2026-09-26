// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video encoder/decoder primitives — the codec layer of the
//! engine RHI. Lives above the core GPU plumbing (`vulkan::rhi`); the
//! public constructors take engine RHI handles (`GpuContextFullAccess`),
//! not raw Vulkan device / queue / allocator handles.
//!
//! Origin: ported from NVIDIA nvpro-samples
//! (<https://github.com/nvpro-samples/vk_video_samples>). Sessions are
//! minted on the host device only, behind the video codec seam.

// --- Public API ---
pub mod decode;
pub mod encode;
pub mod nv12_to_rgb;
pub mod rgb_to_nv12;
pub mod video_context;
pub(crate) mod vulkan_video_codec_backend;

// Public codec types — re-exported at the engine `crate::vulkan::video::*`
// surface and pulled through to `streamlib::sdk::engine::video::*`.
pub use decode::{DecodedFrame, SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig};
pub use encode::{Codec, EncodePacket, Preset, SimpleEncoder, SimpleEncoderConfig};
pub use encode::{EncodedOutput, FrameType};
pub use nv12_to_rgb::Nv12ToRgbConverter;
pub use rgb_to_nv12::RgbToNv12Converter;
pub use video_context::{VideoContext, VideoError, VideoResult};

// --- Internal modules (ported 1-to-1 from nvpro C++) ---
pub mod codec_utils;
pub mod frame_buffer;
pub mod nv_video_parser;
pub mod vk_video_decoder;
pub mod vk_video_encoder;
pub mod vk_video_parser;
