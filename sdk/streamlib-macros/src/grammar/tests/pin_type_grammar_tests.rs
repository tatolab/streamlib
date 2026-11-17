// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use quote::quote;
use streamlib_node_schema::pin_type::{PinType, TexturePixelFormat};
use streamlib_node_schema::{ChannelsOutputPortSampleType, PortFamily};

use super::*;

#[test]
fn a_port_declared_by_pin_type_serves_the_family_its_messages_belong_to() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("gain", pin_type = Float, delivery_profile = "newest"),
        output("enabled", pin_type = Boolean),
        output("frames", pin_type = Texture),
    });
    let schema = parsed.to_node_schema();

    assert_eq!(
        schema.inputs[0].pin_type,
        Some(PinType::Float { exact_size: None })
    );
    assert_eq!(schema.inputs[0].family.family, PortFamily::Channels);
    assert_eq!(schema.outputs[0].pin_type, Some(PinType::Boolean {}));
    assert_eq!(schema.outputs[0].family.family, PortFamily::Channels);
    assert_eq!(
        schema.outputs[1].pin_type,
        Some(PinType::Texture {
            pixel_formats: vec![]
        })
    );
    assert_eq!(schema.outputs[1].family.family, PortFamily::Image);
}

#[test]
fn a_port_declaring_only_a_family_declares_no_pin_type() {
    let schema = parse_ok(quote! {
        execution = manual,
        output("frames", family = Image),
    })
    .to_node_schema();

    assert_eq!(schema.outputs[0].pin_type, None);
}

#[test]
fn a_pin_type_beside_the_family_it_is_carried_in_is_taken_with_its_family_keys() {
    let schema = parse_ok(quote! {
        execution = manual,
        output("audio", family = Channels, pin_type = Audio, dtype = "f32"),
        output("frames", pin_type = Texture, formats = ["bgra8_unorm"]),
    })
    .to_node_schema();

    assert_eq!(schema.outputs[0].pin_type, Some(PinType::Audio {}));
    assert_eq!(
        schema.outputs[0].family.dtype,
        Some(ChannelsOutputPortSampleType::F32)
    );
    assert_eq!(
        schema.outputs[1].pin_type,
        Some(PinType::Texture {
            pixel_formats: vec![TexturePixelFormat::Bgra8Unorm]
        })
    );
    assert_eq!(schema.outputs[1].family.formats, vec!["bgra8_unorm"]);
}

#[test]
fn a_pin_type_beside_another_family_is_an_error_naming_both() {
    let msg = parse_err(quote! {
        execution = manual,
        output("gain", family = Image, pin_type = Float),
    });
    assert!(
        msg.contains(
            "port `gain` declares `family = Image` and a pin type whose messages are \
                      Channels"
        ),
        "got: {msg}"
    );
}

#[test]
fn a_pin_type_a_rust_port_cannot_name_alone_is_an_error_listing_those_it_can() {
    let msg = parse_err(quote! {
        execution = manual,
        output("choice", pin_type = Menu),
    });
    assert!(
        msg.contains("port `choice` declares `pin_type = Menu`") && msg.contains("`Float`"),
        "got: {msg}"
    );
}

#[test]
fn a_port_with_neither_a_family_nor_a_pin_type_names_both_ways_to_declare_one() {
    let msg = parse_err(quote! {
        execution = manual,
        output("video"),
    });
    assert!(
        msg.contains("port `video` declares no family") && msg.contains("`pin_type = …`"),
        "got: {msg}"
    );
}
