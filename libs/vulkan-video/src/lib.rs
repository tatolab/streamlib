// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Rust port of NVIDIA nvpro-samples Vulkan Video encoder/decoder libraries.
//!
//! # Usage
//!
//! ```ignore
//! use vulkan_video::{SimpleDecoder, SimpleDecoderConfig, Codec};
//!
//! // Decode: bitstream -> NV12 pixels
//! let config = SimpleDecoderConfig { codec: Codec::H264, ..Default::default() };
//! let mut decoder = SimpleDecoder::new(config)?;
//! let frames = decoder.feed(&h264_bitstream)?;
//! ```
//!
//! Reference: <https://github.com/nvpro-samples/vk_video_samples>

// --- Public API ---
pub mod video_context;
pub mod decode;
pub mod encode;
pub mod rgb_to_nv12;
pub mod nv12_to_rgb;

// Re-export key types at crate root for convenience
pub use video_context::{
    VideoContext, VideoError, VideoResult,
    REQUIRED_VULKAN_API_VERSION, reject_software_renderer,
};
pub use decode::{DecodedFrame, SimpleDecoder, SimpleDecoderConfig, SimpleDecodedFrame};
pub use encode::{EncodedOutput, FrameType};
pub use encode::{SimpleEncoder, SimpleEncoderConfig, EncodePacket, Codec, Preset};
pub use rgb_to_nv12::RgbToNv12Converter;
pub use nv12_to_rgb::Nv12ToRgbConverter;

// --- Internal modules (ported 1-to-1 from nvpro C++) ---
pub mod codec_utils;
pub mod nv_video_parser;
pub mod vk_video_decoder;
pub mod vk_video_encoder;
pub mod vk_video_parser;
pub mod frame_buffer;
