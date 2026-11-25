// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-in `TextureFormatConvert`: each 8-bit RGBA or BGRA Image frame as one `rgba8_unorm`
//! Image frame, through the colour converter's image-to-image kernel.

use streamlib::sdk::context::{
    RgbaOrBgraImageFrameToPooledRgbaConversionStage, RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};
use streamlib::sdk::nodes::ReactiveNode;

use super::messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const IMAGE_INPUT_PORT: &str = "image";
const RGBA_IMAGE_OUTPUT_PORT: &str = "rgba_image";

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "image",
        description = "The 8-bit RGBA or BGRA Image frames to convert",
        delivery_profile = "newest",
        pin_type = Texture,
        formats = ["rgba8_unorm", "bgra8_unorm"]
    ),
    output(
        "rgba_image",
        description = "One rgba8_unorm Image frame per Image frame",
        pin_type = Texture,
        formats = ["rgba8_unorm"]
    ),
)]
#[meta(
    conversion = true,
    description = "Converts each `rgba8_unorm` or `bgra8_unorm` Image frame on `image` into one \
                   `rgba8_unorm` Image frame on `rgba_image`, of the same size and colour, its \
                   channels in RGBA order. One GPU dispatch samples the frame into a texture, \
                   which one copy writes into a pixel buffer from this node's own output pool; \
                   a frame held in a pixel buffer is first copied into a texture. Each frame is \
                   written by the time it is sent. An sRGB frame would be decoded by the \
                   sampling, and any other format has no such kernel, so `image` takes neither, \
                   and one that arrives is refused and writes nothing."
)]
pub struct TextureFormatConvert {
    conversion_stage: Option<RgbaOrBgraImageFrameToPooledRgbaConversionStage>,
}

impl ReactiveNode for TextureFormatConvert::Node {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let node_run_id_of_this_node = ctx
            .node_run_id()
            .unwrap_or_else(|| "TextureFormatConvert".to_string());
        self.conversion_stage = Some(RgbaOrBgraImageFrameToPooledRgbaConversionStage::create(
            ctx.gpu_full_access(),
            &node_run_id_of_this_node,
        )?);
        Ok(())
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some(conversion_stage) = self.conversion_stage.as_mut() else {
            return Err(Error::Runtime(
                "TextureFormatConvert: process ran before setup".to_string(),
            ));
        };
        let gpu = ctx.gpu_limited_access();
        while let Some(message) = self.inputs.read_raw(IMAGE_INPUT_PORT)? {
            let converted =
                the_image_frame_a_message_carries(message.payload_descriptor).and_then(|image| {
                    conversion_stage.convert_into_the_next_pooled_pixel_buffer(gpu, &image)
                });
            match converted {
                Ok(rgba_image) => self.outputs.write_bytes_described_by(
                    RGBA_IMAGE_OUTPUT_PORT,
                    &[],
                    message.timestamp_ns,
                    message.group_boundary,
                    DataChannelPayloadDescriptor::image_frame(rgba_image),
                )?,
                Err(refusal) => {
                    tracing::warn!(%refusal, "TextureFormatConvert wrote nothing for a message")
                }
            }
        }
        Ok(())
    }
}

/// The Image frame `payload_descriptor` names, refused when it names no Image frame.
pub fn the_image_frame_a_message_carries(
    payload_descriptor: DataChannelPayloadDescriptor,
) -> Result<ImageFrameDescriptorBody> {
    refuse_unless_the_descriptor_is(
        "TextureFormatConvert",
        IMAGE_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ImageFrame,
    )?;
    Ok(ImageFrameDescriptorBody::from_descriptor_body(
        &payload_descriptor.body,
    ))
}
