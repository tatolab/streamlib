// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! One access unit decoded on an owned Vulkan Video decode device on the first GPU the loader
//! enumerates, its display window converted to RGBA on the GPU and read back.

use std::sync::Arc;

use streamlib_asset_media_facts::{PosterFrameVideoCodec, RgbaPictureFromThePosterFrameHelper};
use streamlib_consumer_rhi::{
    OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
    vulkan_device_uuid_of_the_first_device_the_loader_enumerates,
};
use streamlib_vulkan_video::{
    Codec, DecodedPictureDelivery, SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
    VulkanVideoCodecDevice,
};

/// An end-of-stream NAL unit, which makes the decoder finish the picture it holds.
fn end_of_stream_nal_unit_of(codec: PosterFrameVideoCodec) -> &'static [u8] {
    match codec {
        PosterFrameVideoCodec::H264 => &[0, 0, 0, 1, 0x0B],
        PosterFrameVideoCodec::H265 => &[0, 0, 0, 1, 0x4A, 0x01],
    }
}

fn rgba_picture_of(
    decoded: SimpleDecodedFrame,
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    if !decoded.is_rgba {
        return Err("the decoder handed back NV12 where RGBA was asked for".to_owned());
    }
    Ok(RgbaPictureFromThePosterFrameHelper {
        pixel_width: decoded.width,
        pixel_height: decoded.height,
        rgba_bytes: decoded.data,
    })
}

/// The first picture `annex_b_access_unit` decodes to, as RGBA.
pub(crate) fn decode_the_first_picture_on_the_first_vulkan_video_device(
    codec: PosterFrameVideoCodec,
    annex_b_access_unit: &[u8],
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    let device_uuid = vulkan_device_uuid_of_the_first_device_the_loader_enumerates()
        .map_err(|refusal| format!("no Vulkan device answered: {refusal}"))?;
    let decode_device: Arc<dyn VulkanVideoCodecDevice> = Arc::new(
        OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
            device_uuid,
            VulkanVideoCodecDirection::Decode,
        )
        .map_err(|refusal| format!("the Vulkan Video decode device was refused: {refusal}"))?,
    );
    let mut decoder = SimpleDecoder::new(
        decode_device,
        SimpleDecoderConfig {
            codec: match codec {
                PosterFrameVideoCodec::H264 => Codec::H264,
                PosterFrameVideoCodec::H265 => Codec::H265,
            },
            decoded_picture_delivery: DecodedPictureDelivery::ReadBackRgba,
            ..SimpleDecoderConfig::default()
        },
    )
    .map_err(|refusal| format!("the decoder could not be made: {refusal}"))?;
    let mut decoded = decoder
        .feed(annex_b_access_unit)
        .map_err(|refusal| format!("the access unit did not decode: {refusal}"))?;
    if decoded.is_empty() {
        decoded = decoder
            .feed(end_of_stream_nal_unit_of(codec))
            .map_err(|refusal| format!("the access unit did not decode: {refusal}"))?;
    }
    decoded
        .into_iter()
        .next()
        .ok_or_else(|| "the access unit decoded to no picture".to_owned())
        .and_then(rgba_picture_of)
}
