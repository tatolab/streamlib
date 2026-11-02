// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's Vulkan Video arm: the encode and decode paths
//! `streamlib-vulkan-video` provides, minted on the engine's device.

pub use streamlib_vulkan_video::{decode, encode, nv12_to_rgb, rgb_to_nv12};
pub(crate) mod vulkan_video_codec_backend;

pub use decode::{
    DecodedFrame, DecodedPictureDelivery, SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
};
pub use encode::{Codec, EncodePacket, Preset, SimpleEncoder, SimpleEncoderConfig};
pub use encode::{EncodedOutput, FrameType};
pub use nv12_to_rgb::Nv12ToRgbConverter;
pub use rgb_to_nv12::RgbToNv12Converter;
pub use streamlib_vulkan_video::{VideoError, VideoResult};
