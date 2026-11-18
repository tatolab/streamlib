// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameDescriptorBody,
    DataChannelPayloadDescriptor, GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody,
    ImageFrameH273Colour, array_frame_attribute_table_of,
};
use streamlib::sdk::rhi::TextureFormat;

use super::texture_to_tensor::*;

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
