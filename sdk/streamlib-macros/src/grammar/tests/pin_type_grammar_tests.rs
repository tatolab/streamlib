// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use quote::quote;
use streamlib_node_schema::pin_type::{PinFlow, PinType, TexturePixelFormat};
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
fn a_string_port_serves_the_records_family_and_states_its_flow() {
    let schema = parse_ok(quote! {
        execution = manual,
        input("cue", pin_type = String, flow = Stream, delivery_profile = "newest"),
        output("label", pin_type = String),
    })
    .to_node_schema();

    assert_eq!(schema.inputs[0].pin_type, Some(PinType::String {}));
    assert_eq!(schema.inputs[0].family.family, PortFamily::Records);
    assert_eq!(schema.inputs[0].flow, Some(PinFlow::Stream));
    assert_eq!(schema.outputs[0].pin_type, Some(PinType::String {}));
    assert_eq!(schema.outputs[0].flow, None);
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

#[test]
fn an_input_with_a_pin_may_state_no_delivery_profile_and_one_without_still_must() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("gain", pin_type = Float),
        input("frames", family = Image),
        input("every_frame", pin_type = Texture, delivery_profile = "ordered"),
    });
    let deliveries: Vec<Option<&str>> = parsed
        .inputs
        .iter()
        .map(|port| port.delivery_profile.as_deref())
        .collect();
    assert_eq!(deliveries, [None, None, Some("ordered")]);

    let msg = parse_err(quote! {
        execution = manual,
        input("records", family = Records),
    });
    assert!(
        msg.contains("input port `records` must declare a `delivery_profile`")
            && msg.contains("`pin_type = …` whose default it reads by"),
        "got: {msg}"
    );
}

#[test]
fn a_stated_flow_reaches_the_schema_and_a_picture_port_states_its_texture_with_it() {
    let schema = parse_ok(quote! {
        execution = manual,
        input("levels", pin_type = Float, flow = Stream),
        input("still", family = Image, formats = ["rgba8_unorm"], flow = Value),
        output("gain", pin_type = Float),
    })
    .to_node_schema();

    assert_eq!(schema.inputs[0].flow, Some(PinFlow::Stream));
    assert_eq!(schema.inputs[1].flow, Some(PinFlow::Value));
    assert_eq!(
        schema.inputs[1].pin_type,
        Some(PinType::Texture {
            pixel_formats: vec![TexturePixelFormat::Rgba8Unorm]
        })
    );
    assert_eq!(schema.outputs[0].flow, None);
}

#[test]
fn a_flow_the_type_does_not_allow_is_refused_naming_the_port_and_the_flows_it_has() {
    let msg = parse_err(quote! {
        execution = manual,
        input("audio", pin_type = Audio, flow = Value),
    });
    assert!(
        msg.contains(
            "port `audio` states the flow `value`, which the Audio type does not allow; it \
             flows as `stream`"
        ),
        "got: {msg}"
    );
}

#[test]
fn a_flow_on_a_port_with_no_pin_or_an_unknown_flow_is_refused() {
    let msg = parse_err(quote! {
        execution = manual,
        output("records", family = Records, flow = Value),
    });
    assert!(
        msg.contains("port `records` states `flow = …` and has no pin type to flow by"),
        "got: {msg}"
    );

    let msg = parse_err(quote! {
        execution = manual,
        output("gain", pin_type = Float, flow = Sometimes),
    });
    assert!(
        msg.contains("port `gain` declares `flow = Sometimes` — a flow is one of"),
        "got: {msg}"
    );
}

#[test]
fn a_windowed_input_whose_pins_default_skips_messages_is_refused() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("level", pin_type = Float, audio_window = match_device),
    });
    assert!(
        msg.contains("port `level`: a port declaring `audio_window` must declare"),
        "got: {msg}"
    );
}

#[test]
fn a_trigger_port_carries_records_flows_as_a_trigger_and_reads_ordered_alone() {
    let schema = parse_ok(quote! {
        execution = reactive,
        input("go", pin_type = Trigger),
        input("stated", pin_type = Trigger, delivery_profile = "ordered"),
        output("fired", pin_type = Trigger),
    })
    .to_node_schema();
    for port in schema.inputs.iter().chain(&schema.outputs) {
        assert_eq!(port.pin_type, Some(PinType::Trigger {}), "{}", port.name);
        assert_eq!(port.family.family, PortFamily::Records, "{}", port.name);
    }

    let msg = parse_err(quote! {
        execution = reactive,
        input("go", pin_type = Trigger, delivery_profile = "newest"),
    });
    assert!(
        msg.contains("input port `go` is a Trigger and states the delivery profile `newest`"),
        "got: {msg}"
    );
    let msg = parse_err(quote! {
        execution = reactive,
        input("go", pin_type = Trigger, flow = Value),
    });
    assert!(
        msg.contains("states the flow `value`, which the Trigger type does not allow"),
        "got: {msg}"
    );
}
