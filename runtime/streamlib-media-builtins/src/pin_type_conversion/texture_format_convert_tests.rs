// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::context::TEXTURE_FORMATS_CONVERTED_INTO_RGBA8_UNORM;
use streamlib::sdk::descriptors::{PinType, TexturePixelFormat};
use streamlib::sdk::iceoryx2::{
    ArrayFrameDescriptorBody, DataChannelPayloadDescriptor, GpuPoolSlotFrameDescriptorBody,
    ImageFrameDescriptorBody, ImageFrameH273Colour,
};
use streamlib::sdk::nodes::GeneratedNode;
use streamlib::sdk::rhi::TextureFormat;

use super::texture_format_convert::*;

/// The conversion `TextureFormatConvert` states: what the engine's stage converts in, and
/// `rgba8_unorm` out, so the verdict's coverage is what the kernel does.
///
/// Mental-revert: stating `bgra8_unorm_srgb` on `image` makes the verdict place the node on an
/// sRGB link the kernel would decode.
#[test]
fn its_pins_state_what_the_kernel_converts_in_and_rgba8_unorm_out() {
    let conversion = TextureFormatConvert::Node::descriptor()
        .expect("a descriptor")
        .pin_conversion()
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .expect("it states a conversion");

    let the_kernels_formats: Vec<TexturePixelFormat> = TEXTURE_FORMATS_CONVERTED_INTO_RGBA8_UNORM
        .iter()
        .map(|format| {
            TexturePixelFormat::from_wire_name(format.wire_name()).expect("a pixel format")
        })
        .collect();
    assert_eq!(
        conversion.input_pin_type,
        PinType::Texture {
            pixel_formats: the_kernels_formats
        }
    );
    assert_eq!(
        conversion.output_pin_type,
        PinType::Texture {
            pixel_formats: vec![TexturePixelFormat::Rgba8Unorm]
        }
    );
    assert_eq!(
        (
            conversion.input_port_name.as_str(),
            conversion.output_port_name.as_str()
        ),
        ("image", "rgba_image")
    );
}

#[test]
fn an_image_frame_message_names_its_frame() {
    let image = ImageFrameDescriptorBody {
        slot: GpuPoolSlotFrameDescriptorBody {
            pool: *b"0123456789abcdef",
            slot: 3,
            generation: 9,
            timeline_value: 41,
            image_layout: 1,
        },
        width: 640,
        height: 360,
        format: TextureFormat::Bgra8Unorm as u32,
        colour: ImageFrameH273Colour::UNSPECIFIED,
    };

    let read = the_image_frame_a_message_carries(DataChannelPayloadDescriptor::image_frame(image))
        .expect("an Image frame");

    assert_eq!(read, image);
}

#[test]
fn a_message_of_another_kind_is_refused_naming_the_input() {
    let refusal = the_image_frame_a_message_carries(DataChannelPayloadDescriptor::array_frame(
        ArrayFrameDescriptorBody { attribute_count: 1 },
    ))
    .expect_err("an Array frame is no Image frame")
    .to_string();

    assert!(refusal.contains("TextureFormatConvert"), "{refusal}");
    assert!(refusal.contains("`image`"), "{refusal}");
}
