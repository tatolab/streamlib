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
        let settings = TensorToTextureSettings {
            format: format.to_string(),
            ..TensorToTextureSettings::default()
        };
        assert!(image_format_of_the_settings(&settings).is_err(), "{format}");
    }
    assert_eq!(
        image_format_of_the_settings(&TensorToTextureSettings::default()).ok(),
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

/// A `TensorToTexture` handed the values its inputs hold before it is set up writes the format
/// they name, each setting they name no value for keeping its default; a value its setting does
/// not take is refused, naming the node.
///
/// Mental-revert: settings left at their defaults keep writing `rgba8_unorm` after the inputs
/// hold `bgra8_unorm`.
#[test]
fn the_settings_held_before_setup_are_the_ones_it_writes_with() {
    use streamlib::sdk::nodes::GeneratedNode;
    let mut node = <TensorToTexture::Node as GeneratedNode>::from_config(Default::default())
        .expect("a TensorToTexture");
    assert_eq!(
        node.image_format_it_writes().unwrap(),
        TextureFormat::Rgba8Unorm
    );

    let held = |values: streamlib::sdk::serde_json::Value| {
        values.as_object().cloned().expect("an object of values")
    };
    node.set_unlinked_input_values(&held(streamlib::sdk::serde_json::json!({
        "format": "bgra8_unorm",
        "array": null,
    })))
    .expect("the settings are taken");

    assert_eq!(
        node.image_format_it_writes().unwrap(),
        TextureFormat::Bgra8Unorm
    );
    assert_eq!(node.settings.attribute, "image");

    let refusal = node
        .set_unlinked_input_values(&held(streamlib::sdk::serde_json::json!({ "format": 3 })))
        .expect_err("a number is no format");
    assert!(refusal.to_string().contains("TensorToTexture"), "{refusal}");
}

/// The node catalog lists `TensorToTexture`'s settings after its `array` port, `format` a Menu
/// of the formats one tensor holds, each read at setup, and no config.
///
/// Mental-revert: a `format` declared as a String lists no options to choose from.
#[test]
fn the_catalog_lists_its_format_as_a_menu_read_at_setup() {
    crate::register_media_builtin_node_types();
    let descriptor = streamlib::sdk::nodes::NODE_REGISTRY
        .descriptor(&TensorToTexture::node_class_import_path())
        .expect("the built-in registers");
    let rendered = serde_json::to_value(streamlib::sdk::json_schema::NodeDescriptorOutput::from(
        &descriptor,
    ))
    .unwrap();
    let inputs = rendered["inputs"].as_array().unwrap();
    assert_eq!(
        inputs
            .iter()
            .map(|input| input["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["array", "attribute", "format"],
        "{rendered}"
    );
    let format = &inputs[2];
    assert_eq!(
        format["pin_type"]["pin_type"],
        serde_json::json!({
            "type": "menu",
            "option_names": [
                "rgba8_unorm",
                "rgba8_unorm_srgb",
                "bgra8_unorm",
                "bgra8_unorm_srgb",
                "rgba16_float",
                "rgba32_float"
            ]
        }),
        "{rendered}"
    );
    assert_eq!(format["default_value"], serde_json::json!("rgba8_unorm"));
    assert_eq!(
        format["input_mode"],
        serde_json::json!({"mode": "setting_only", "read": "at_setup"})
    );
    assert!(descriptor.config_schema.is_none(), "{rendered}");
}

/// The formats `image` states are those one `[h, w, c]` tensor holds, each a `format` the node
/// writes, so a link to it is judged by what it writes.
///
/// Mental-revert: a format the kernel learns to write and `image` leaves out refuses a link the
/// node would serve.
#[test]
fn its_image_output_states_each_format_one_tensor_holds() {
    use streamlib::sdk::descriptors::{PinType, TexturePixelFormat};
    use streamlib::sdk::nodes::GeneratedNode;

    let conversion = <TensorToTexture::Node as GeneratedNode>::descriptor()
        .expect("a descriptor")
        .pin_conversion()
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .expect("it states a conversion");
    let the_formats_one_tensor_holds: Vec<TexturePixelFormat> = TexturePixelFormat::EVERY_FORMAT
        .into_iter()
        .filter(|format| {
            TextureFormat::from_wire_name(format.wire_name())
                .and_then(super::texture_to_tensor::tensor_element_and_channels_of_one_pixel)
                .is_some()
        })
        .collect();
    assert_eq!(
        conversion.output_pin_type,
        PinType::Texture {
            pixel_formats: the_formats_one_tensor_holds
        }
    );
}
