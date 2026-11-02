// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video encode on any device that names its queues and allocator
//! through [`VulkanVideoCodecDevice`]: the engine's own device, or the
//! [`streamlib_consumer_rhi::OwnedVulkanVideoEncodeDevice`] a codec module
//! owns in its helper process. Linux only; elsewhere the crate is empty.
//!
//! Ported from NVIDIA nvpro-samples
//! (<https://github.com/nvpro-samples/vk_video_samples>).

#[cfg(target_os = "linux")]
mod annex_b_nal_units;
#[cfg(target_os = "linux")]
pub mod encode;
#[cfg(all(test, target_os = "linux"))]
mod hardware_test_codec_device;
#[cfg(target_os = "linux")]
pub mod rgb_to_nv12;
#[cfg(target_os = "linux")]
mod rgba_pixel_buffer_video_encoder;
#[cfg(target_os = "linux")]
mod video_error;
#[cfg(target_os = "linux")]
pub mod vk_video_encoder;
#[cfg(target_os = "linux")]
mod vulkan_query_pool;
#[cfg(target_os = "linux")]
mod vulkan_video_bitstream_buffer;
#[cfg(target_os = "linux")]
mod vulkan_video_codec_device;
#[cfg(target_os = "linux")]
mod vulkan_video_dpb_image;
#[cfg(target_os = "linux")]
mod vulkan_video_encode_context;
#[cfg(target_os = "linux")]
mod vulkan_video_session;

#[cfg(target_os = "linux")]
pub use annex_b_nal_units::{
    split_annex_b_nal_units, why_no_decoder_could_enter_on_these_parameter_sets,
};
#[cfg(target_os = "linux")]
pub use encode::{
    Codec, EncodePacket, EncodedOutput, FrameType, Preset, SimpleEncoder, SimpleEncoderConfig,
};
#[cfg(target_os = "linux")]
pub use rgb_to_nv12::RgbToNv12Converter;
#[cfg(target_os = "linux")]
pub use rgba_pixel_buffer_video_encoder::RgbaPixelBufferVideoEncoder;
#[cfg(target_os = "linux")]
pub use video_error::{VideoError, VideoResult};
#[cfg(target_os = "linux")]
pub use vulkan_query_pool::{QueryPoolDescriptor, VideoEncodeFeedbackResult, VulkanQueryPool};
#[cfg(target_os = "linux")]
pub use vulkan_video_bitstream_buffer::{
    VideoBitstreamBufferDescriptor, VideoBitstreamDirection, VulkanVideoBitstreamBuffer,
};
#[cfg(target_os = "linux")]
pub use vulkan_video_codec_device::{VulkanVideoCodecDevice, VulkanVideoCodecQueue};
#[cfg(target_os = "linux")]
pub use vulkan_video_dpb_image::{VideoDpbDirection, VideoDpbImageDescriptor, VulkanVideoDpbImage};
#[cfg(target_os = "linux")]
pub use vulkan_video_encode_context::{
    VulkanVideoEncodeContext, create_nv12_ycbcr_conversion_for_video_image_views,
};
#[cfg(target_os = "linux")]
pub use vulkan_video_session::{
    VIDEO_DECODE_CODEC_OPERATIONS, VIDEO_ENCODE_CODEC_OPERATIONS, VideoCodecOperationDirection,
    VideoSessionDescriptor, VideoSessionParametersAddInfo, VideoSessionParametersDescriptor,
    VulkanVideoSession, VulkanVideoSessionParameters,
};
