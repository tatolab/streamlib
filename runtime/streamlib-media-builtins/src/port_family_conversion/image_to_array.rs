// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `ImageToArray`: each Image frame as one Array frame whose one GPU
//! attribute is the frame's own pool slot, borrowed as an `[h, w, c]` tensor.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameDescriptorBody,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::rhi::TextureFormat;

use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const IMAGE_INPUT_PORT: &str = "image";
const ARRAY_OUTPUT_PORT: &str = "array";

/// The one attribute each Array frame `ImageToArray` writes holds.
pub const IMAGE_TO_ARRAY_IMAGE_ATTRIBUTE_NAME: &str = "image";

#[streamlib::sdk::processor(
    built_in_node,
    execution = reactive,
    input(
        "image",
        description = "The Image frames to borrow as tensors",
        delivery_profile = "newest",
        family = Image
    ),
    output(
        "array",
        description = "One Array frame per Image frame, its slot as the `image` attribute",
        family = Array,
        names = ["image"],
        residency = "gpu"
    ),
)]
#[meta(
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
pub struct ImageToArray;

impl ReactiveProcessor for ImageToArray::Processor {
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
                    tracing::warn!(%refusal, "ImageToArray wrote nothing for a message")
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
        "ImageToArray",
        IMAGE_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ImageFrame,
    )?;
    let image = ImageFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body);
    let format = TextureFormat::from_repr_value(image.format).ok_or_else(|| {
        Error::PortError(format!(
            "ImageToArray: an Image frame on `image` states texture format {}, which this \
             runtime does not know",
            image.format
        ))
    })?;
    let (dtype, channels) = tensor_element_and_channels_of_one_pixel(format).ok_or_else(|| {
        Error::PortError(format!(
            "ImageToArray: an Image frame on `image` is {}, whose planes no one `[h, w, c]` \
             tensor holds",
            format.wire_name()
        ))
    })?;
    let row = ArrayFrameAttributeRow {
        name: IMAGE_TO_ARRAY_IMAGE_ATTRIBUTE_NAME.to_string(),
        dtype: dtype.to_string(),
        shape: vec![u64::from(image.height), u64::from(image.width), channels],
        residency: ArrayFrameAttributeResidency::GpuPoolSlot(image.slot),
    };
    let payload = row
        .to_row_bytes()
        .map_err(|refusal| Error::PortError(format!("ImageToArray: {refusal}")))?
        .to_vec();
    Ok((
        DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody { attribute_count: 1 }),
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{
        GpuPoolSlotFrameDescriptorBody, ImageFrameH273Colour, array_frame_attribute_table_of,
    };

    use super::*;

    fn image_frame_of(
        format: TextureFormat,
    ) -> (DataChannelPayloadDescriptor, GpuPoolSlotFrameDescriptorBody) {
        let slot = GpuPoolSlotFrameDescriptorBody {
            pool: *b"0123456789abcdef",
            slot: 3,
            generation: 9,
            timeline_value: 41,
            image_layout: 1,
        };
        (
            DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
                slot,
                width: 640,
                height: 360,
                format: format as u32,
                colour: ImageFrameH273Colour {
                    primaries: 1,
                    transfer: 13,
                    matrix: 1,
                    range: 1,
                },
            }),
            slot,
        )
    }

    #[test]
    fn an_image_frame_becomes_one_attribute_borrowing_its_own_slot() {
        let (image_descriptor, slot) = image_frame_of(TextureFormat::Rgba8Unorm);

        let (array_descriptor, array_payload) =
            array_frame_of_the_image_frame(image_descriptor).unwrap();

        let rows = array_frame_attribute_table_of(
            ArrayFrameDescriptorBody::from_descriptor_body(&array_descriptor.body),
            &array_payload,
        )
        .unwrap();
        assert_eq!(
            rows,
            vec![ArrayFrameAttributeRow {
                name: "image".into(),
                dtype: "uint8".into(),
                shape: vec![360, 640, 4],
                residency: ArrayFrameAttributeResidency::GpuPoolSlot(slot),
            }]
        );
    }

    #[test]
    fn each_single_plane_format_is_a_tensor_of_its_element_type() {
        for (format, dtype) in [
            (TextureFormat::Rgba8UnormSrgb, "uint8"),
            (TextureFormat::Bgra8Unorm, "uint8"),
            (TextureFormat::Rgba16Float, "float16"),
            (TextureFormat::Rgba32Float, "float32"),
        ] {
            let (image_descriptor, _) = image_frame_of(format);
            let (array_descriptor, array_payload) =
                array_frame_of_the_image_frame(image_descriptor).unwrap();
            let rows = array_frame_attribute_table_of(
                ArrayFrameDescriptorBody::from_descriptor_body(&array_descriptor.body),
                &array_payload,
            )
            .unwrap();
            assert_eq!(rows[0].dtype, dtype, "{format:?}");
        }
    }

    #[test]
    fn an_nv12_frame_is_refused_naming_its_format() {
        let (image_descriptor, _) = image_frame_of(TextureFormat::Nv12);

        let refusal = array_frame_of_the_image_frame(image_descriptor)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("nv12"), "{refusal}");
    }

    #[test]
    fn a_message_that_is_not_an_image_frame_is_refused() {
        let refusal = array_frame_of_the_image_frame(DataChannelPayloadDescriptor::bytes())
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("ImageFrame"), "{refusal}");
    }
}
