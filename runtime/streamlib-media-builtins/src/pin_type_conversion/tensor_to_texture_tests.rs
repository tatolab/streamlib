// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::context::NativeNodeOutputPixelBufferFrame;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameCpuAttributeToPack,
    ArrayFrameDescriptorBody, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
    array_frame_payload_of_cpu_attributes,
};
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

use super::tensor_to_texture::*;

fn gpu_attribute_slot() -> GpuPoolSlotFrameDescriptorBody {
    GpuPoolSlotFrameDescriptorBody {
        pool: *b"0123456789abcdef",
        slot: 5,
        generation: 7,
        timeline_value: 0,
        image_layout: 0,
    }
}

fn array_frame_of_one_gpu_attribute(
    dtype: &str,
    shape: Vec<u64>,
) -> (DataChannelPayloadDescriptor, Vec<u8>) {
    let row = ArrayFrameAttributeRow {
        name: "image".to_string(),
        dtype: dtype.to_string(),
        shape,
        residency: ArrayFrameAttributeResidency::GpuPoolSlot(gpu_attribute_slot()),
    };
    (
        DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody { attribute_count: 1 }),
        row.to_row_bytes().expect("the row packs").to_vec(),
    )
}

fn no_pixel_buffer_is_acquired(
    _width: u32,
    _height: u32,
    _pixel_format: PixelFormat,
) -> Result<NativeNodeOutputPixelBufferFrame> {
    panic!("a GPU attribute is passed on, never copied into a pixel buffer")
}

#[test]
fn a_gpu_attribute_becomes_the_image_frames_own_slot() {
    let (descriptor, payload) = array_frame_of_one_gpu_attribute("uint8", vec![16, 64, 4]);
    let image_descriptor = image_frame_of_the_array_frame(
        "image",
        TextureFormat::Bgra8Unorm,
        descriptor,
        &payload,
        no_pixel_buffer_is_acquired,
    )
    .expect("a [h, w, 4] uint8 attribute is a bgra8 image");
    assert_eq!(
        DataChannelPayloadDescriptorKind::from_wire(image_descriptor.kind),
        Some(DataChannelPayloadDescriptorKind::ImageFrame)
    );
    let image = ImageFrameDescriptorBody::from_descriptor_body(&image_descriptor.body);
    assert_eq!(image.slot, gpu_attribute_slot());
    assert_eq!((image.width, image.height), (64, 16));
    assert_eq!(image.format, TextureFormat::Bgra8Unorm as u32);
    assert_eq!(image.colour, ImageFrameH273Colour::UNSPECIFIED);
}

#[test]
fn an_attribute_of_another_dtype_or_shape_is_refused() {
    for (dtype, shape) in [
        ("float32", vec![16, 64, 4]),
        ("uint8", vec![16, 64, 3]),
        ("uint8", vec![16, 64]),
        ("uint8", vec![0, 64, 4]),
    ] {
        let (descriptor, payload) = array_frame_of_one_gpu_attribute(dtype, shape.clone());
        let refusal = image_frame_of_the_array_frame(
            "image",
            TextureFormat::Rgba8Unorm,
            descriptor,
            &payload,
            no_pixel_buffer_is_acquired,
        )
        .expect_err("the attribute is no rgba8 image");
        assert!(
            refusal.to_string().contains("attribute \"image\""),
            "{dtype} {shape:?}: {refusal}"
        );
    }
}

#[test]
fn a_frame_without_the_attribute_is_refused_naming_what_it_has() {
    let data = vec![0_u8; 16];
    let (body, payload) = array_frame_payload_of_cpu_attributes(&[ArrayFrameCpuAttributeToPack {
        name: "mask",
        dtype: "uint8",
        shape: &[2, 2, 4],
        data: &data,
    }])
    .expect("the payload packs");
    let refusal = image_frame_of_the_array_frame(
        "image",
        TextureFormat::Rgba8Unorm,
        DataChannelPayloadDescriptor::array_frame(body),
        &payload,
        no_pixel_buffer_is_acquired,
    )
    .expect_err("no attribute is named image");
    assert!(refusal.to_string().contains("[\"mask\"]"), "{refusal}");
}

#[test]
fn a_format_with_no_one_tensor_layout_is_refused_at_setup() {
    for format in ["nv12", "rgb8", ""] {
        let config = TensorToTextureConfig {
            format: format.to_string(),
            ..TensorToTextureConfig::default()
        };
        assert!(image_format_of_the_config(&config).is_err(), "{format}");
    }
    assert_eq!(
        image_format_of_the_config(&TensorToTextureConfig::default()).ok(),
        Some(TextureFormat::Rgba8Unorm)
    );
}

/// Placed to feed a Texture input, the node holds the first format that input takes which one
/// tensor holds, `rgba8_unorm` for an input that takes any, and writes a Texture of it; an input
/// taking only formats no tensor holds, or no Texture at all, is fed by no `TensorToTexture`.
///
/// Mental-revert: picking `rgba8_unorm` whatever the input takes writes a format a
/// `bgra8_unorm` input does not read.
#[test]
fn the_format_picked_for_an_input_is_the_first_it_takes_that_one_tensor_holds() {
    use streamlib::sdk::descriptors::{PinType, TexturePixelFormat};
    let texture_of = |pixel_formats: &[TexturePixelFormat]| PinType::Texture {
        pixel_formats: pixel_formats.to_vec(),
    };
    let picked_for = |input: &PinType| {
        the_format_picked_for_the_input(input).map(|picked| {
            (
                picked.input_values["format"].clone(),
                picked.output_pin_type,
            )
        })
    };

    assert_eq!(
        picked_for(&texture_of(&[
            TexturePixelFormat::Nv12,
            TexturePixelFormat::Bgra8Unorm,
            TexturePixelFormat::Rgba8Unorm,
        ])),
        Some((
            "bgra8_unorm".into(),
            texture_of(&[TexturePixelFormat::Bgra8Unorm])
        ))
    );
    assert_eq!(
        picked_for(&texture_of(&[])),
        Some((
            "rgba8_unorm".into(),
            texture_of(&[TexturePixelFormat::Rgba8Unorm])
        ))
    );
    assert_eq!(picked_for(&texture_of(&[TexturePixelFormat::Nv12])), None);
    assert_eq!(picked_for(&PinType::Tensor {}), None);
}

/// A `TensorToTexture` writes a Texture of the `format` it holds, or of `rgba8_unorm` when it
/// holds none; a `format` no one tensor holds, or one that is no name, gives no type.
///
/// Mental-revert: reading the output as it states itself, a Texture of no format, gives that
/// whatever the node holds.
#[test]
fn the_texture_written_is_of_the_format_the_node_holds() {
    use streamlib::sdk::descriptors::{PinType, TexturePixelFormat};
    use streamlib::sdk::serde_json::{Map, Value, json};
    let held =
        |value: Value| -> Map<String, Value> { value.as_object().cloned().expect("an object") };
    let texture_of = |format: TexturePixelFormat| PinType::Texture {
        pixel_formats: vec![format],
    };

    assert_eq!(
        the_texture_written_under_the_settings(&Map::new()),
        Some(texture_of(TexturePixelFormat::Rgba8Unorm))
    );
    assert_eq!(
        the_texture_written_under_the_settings(&held(json!({ "format": "bgra8_unorm" }))),
        Some(texture_of(TexturePixelFormat::Bgra8Unorm))
    );
    assert_eq!(
        the_texture_written_under_the_settings(&held(json!({ "format": "nv12" }))),
        None
    );
    assert_eq!(
        the_texture_written_under_the_settings(&held(json!({ "format": 3 }))),
        None
    );
}

/// A `TensorToTexture` that takes a new config live writes the format that config names from the
/// next frame, the format a link's verdict reads from the config it holds.
///
/// Mental-revert: a format read once at setup keeps writing `rgba8_unorm` after the config names
/// `bgra8_unorm`.
#[test]
fn a_config_taken_live_changes_the_format_written() {
    use streamlib::sdk::nodes::GeneratedNode;
    let mut node =
        <TensorToTexture::Node as GeneratedNode>::from_config(TensorToTextureConfig::default())
            .expect("a TensorToTexture of the default config");
    assert_eq!(
        node.image_format_it_writes().unwrap(),
        TextureFormat::Rgba8Unorm
    );

    node.apply_config_json(&streamlib::sdk::serde_json::json!({ "format": "bgra8_unorm" }))
        .expect("the config is taken live");

    assert_eq!(
        node.image_format_it_writes().unwrap(),
        TextureFormat::Bgra8Unorm
    );
}
