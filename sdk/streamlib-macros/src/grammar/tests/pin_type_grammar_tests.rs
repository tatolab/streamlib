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
        output("frames", pin_type = Texture, formats = ["rgba8_unorm"]),
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
            pixel_formats: vec![TexturePixelFormat::Rgba8Unorm]
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

/// Mental-revert: a Texture output stating no format is taken, and a link to it is judged by
/// formats it never stated.
#[test]
fn a_texture_output_stating_no_format_is_refused_and_an_input_may_state_none() {
    let msg = parse_err(quote! {
        execution = manual,
        output("frames", pin_type = Texture),
    });
    assert!(
        msg.contains("Texture output `frames` states no pixel format")
            && msg.contains("`formats = [\"…\"]`"),
        "got: {msg}"
    );

    let schema = parse_ok(quote! {
        execution = manual,
        input("frames", pin_type = Texture),
    })
    .to_node_schema();
    assert_eq!(
        schema.inputs[0].pin_type,
        Some(PinType::Texture {
            pixel_formats: vec![]
        })
    );
}

#[test]
fn a_pin_type_beside_the_family_it_is_carried_in_is_taken_with_its_family_keys() {
    let schema = parse_ok(quote! {
        execution = manual,
        output("audio", pin_type = Audio, dtype = "f32"),
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

/// Mental-revert: a port declares its family again beside, or in place of, its pin type.
#[test]
fn the_retired_family_key_is_refused_by_name() {
    let msg = parse_err(quote! {
        execution = manual,
        output("gain", family = Channels, pin_type = Float),
    });
    assert!(
        msg.contains("port `gain` declares `family = …`, which is retired"),
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
fn a_port_declaring_no_pin_type_names_both_ways_to_declare_one() {
    let msg = parse_err(quote! {
        execution = manual,
        output("video"),
    });
    assert!(
        msg.contains("port `video` declares no pin type")
            && msg.contains("`pin_type = …`")
            && msg.contains("`pin_type = from_setting(\"<setting>\")`"),
        "got: {msg}"
    );
}

#[test]
fn an_input_with_a_pin_may_state_no_delivery_profile_and_one_without_still_must() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("gain", pin_type = Float),
        input("frames", pin_type = Texture),
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
        input("records", pin_type = from_setting("bundle_type"), flow = Stream),
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
        input("still", pin_type = Texture, formats = ["rgba8_unorm"], flow = Value),
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
fn an_unknown_flow_is_refused_naming_the_flows() {
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
