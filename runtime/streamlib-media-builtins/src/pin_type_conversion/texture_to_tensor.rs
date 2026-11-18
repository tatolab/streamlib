// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-in `TextureToTensor`: each Image frame as one Array frame whose one GPU
//! attribute is the frame's own pool slot, borrowed as an `[h, w, c]` tensor.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameDescriptorBody,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};
use streamlib::sdk::nodes::ReactiveNode;
use streamlib::sdk::rhi::TextureFormat;

use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const IMAGE_INPUT_PORT: &str = "image";
const ARRAY_OUTPUT_PORT: &str = "array";

/// The one attribute each Array frame `TextureToTensor` writes holds.
pub const TEXTURE_TO_TENSOR_IMAGE_ATTRIBUTE_NAME: &str = "image";

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "image",
        description = "The Image frames to borrow as tensors",
        delivery_profile = "newest",
        pin_type = Texture
    ),
    output(
        "array",
        description = "One Array frame per Image frame, its slot as the `image` attribute",
        pin_type = Tensor,
        names = ["image"],
        residency = "gpu"
    ),
)]
#[meta(
    conversion = true,
    description = "Converts each Image frame on `image` into one Array frame on `array`, whose \
                   one GPU attribute `image` is the frame's own pool slot borrowed as an `[h, w, \
                   4]` tensor: `uint8` for an `rgba8` or `bgra8` frame, `float16` for \
                   `rgba16_float` and `float32` for `rgba32_float`, its channels in the frame's own \
                   order. This node copies nothing. A \
                   reader whose device maps the slot linearly reads the tensor in place, at the \
                   slot's own address; one that cannot reads it through one GPU copy into a \
                   linear staging buffer, as a read of the Image frame itself does. The tensor's \
                   rows are the slot's rows, so a padded row pitch is a stride, never a copy. An \
                   `nv12` frame has two planes and no one-tensor layout, and is refused and writes \
                   nothing. A reader claims the slot when it reads the Array frame, so a \
                   producer that has recycled the slot by then makes the read a counted loss, \
                   never another frame's pixels."
)]
pub struct TextureToTensor;

impl ReactiveNode for TextureToTensor::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw(IMAGE_INPUT_PORT)? {
            match array_frame_of_the_image_frame(message.payload_descriptor) {
                Ok((payload_descriptor, payload)) => self.outputs.write_bytes_described_by(
                    ARRAY_OUTPUT_PORT,
                    &payload,
                    message.timestamp_ns,
                    message.group_boundary,
                    payload_descriptor,
                )?,
                Err(refusal) => {
                    tracing::warn!(%refusal, "TextureToTensor wrote nothing for a message")
                }
            }
        }
        Ok(())
    }
}

/// The tensor element type and channel count one pixel of `format` holds, or
/// `None` for a format of more than one plane.
pub fn tensor_element_and_channels_of_one_pixel(
    format: TextureFormat,
) -> Option<(&'static str, u64)> {
    match format {
        TextureFormat::Rgba8Unorm
        | TextureFormat::Rgba8UnormSrgb
        | TextureFormat::Bgra8Unorm
        | TextureFormat::Bgra8UnormSrgb => Some(("uint8", 4)),
        TextureFormat::Rgba16Float => Some(("float16", 4)),
        TextureFormat::Rgba32Float => Some(("float32", 4)),
        TextureFormat::Nv12 => None,
    }
}

/// The Array frame whose `image` attribute borrows the pool slot of the Image
/// frame `payload_descriptor` names, as an `[h, w, c]` tensor of its format.
pub fn array_frame_of_the_image_frame(
    payload_descriptor: DataChannelPayloadDescriptor,
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>)> {
    refuse_unless_the_descriptor_is(
        "TextureToTensor",
        IMAGE_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ImageFrame,
    )?;
    let image = ImageFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body);
    let format = TextureFormat::from_repr_value(image.format).ok_or_else(|| {
        Error::PortError(format!(
            "TextureToTensor: an Image frame on `image` states texture format {}, which this \
             runtime does not know",
            image.format
        ))
    })?;
    let (dtype, channels) = tensor_element_and_channels_of_one_pixel(format).ok_or_else(|| {
        Error::PortError(format!(
            "TextureToTensor: an Image frame on `image` is {}, whose planes no one `[h, w, c]` \
             tensor holds",
            format.wire_name()
        ))
    })?;
    let row = ArrayFrameAttributeRow {
        name: TEXTURE_TO_TENSOR_IMAGE_ATTRIBUTE_NAME.to_string(),
        dtype: dtype.to_string(),
        shape: vec![u64::from(image.height), u64::from(image.width), channels],
        residency: ArrayFrameAttributeResidency::GpuPoolSlot(image.slot),
    };
    let payload = row
        .to_row_bytes()
        .map_err(|refusal| Error::PortError(format!("TextureToTensor: {refusal}")))?
        .to_vec();
    Ok((
        DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody { attribute_count: 1 }),
        payload,
    ))
}
