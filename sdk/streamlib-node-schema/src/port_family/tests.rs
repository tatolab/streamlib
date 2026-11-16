// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// `tatolab.stream` declares each family with the description the catalog
/// serves, so its reference never says something the runtime does not.
#[test]
fn the_python_sdk_declares_each_family_with_the_description_the_runtime_serves() {
    let python_port_families = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tatolab-stream/tatolab/stream/_port_family_declaration.py"
    ))
    .expect("the Python SDK's port family declarations are in this repository");
    for family in PortFamily::ALL {
        let declaration = format!(
            "PortFamily(\n    \"{}\",\n    description=\"{}\",\n)",
            family.declaration_name(),
            family.description()
        );
        assert!(
            python_port_families.contains(&declaration),
            "`tatolab.stream.{}` must be declared as `{declaration}`",
            family.declaration_name()
        );
    }
}

fn endpoint<'a>(
    node_name: &'a str,
    port_name: &'a str,
    declaration: &'a PortFamilyDeclaration,
) -> PortFamilyLinkEndpoint<'a> {
    PortFamilyLinkEndpoint {
        node_name,
        port_name,
        declaration,
    }
}

fn channels_output_stating(names: &[&str]) -> PortFamilyDeclaration {
    PortFamilyDeclaration {
        names: Some(OutputPortDeclaredNames::Stated(
            names.iter().map(|name| name.to_string()).collect(),
        )),
        ..PortFamilyDeclaration::of_family(PortFamily::Channels)
    }
}

fn channels_input_requiring(names: &[&str]) -> PortFamilyDeclaration {
    PortFamilyDeclaration {
        requires: names.iter().map(|name| name.to_string()).collect(),
        ..PortFamilyDeclaration::of_family(PortFamily::Channels)
    }
}

#[test]
fn a_family_mismatch_is_refused_naming_both_endpoints_and_the_conversion() {
    let image = PortFamilyDeclaration::of_family(PortFamily::Image);
    let array = PortFamilyDeclaration::of_family(PortFamily::Array);
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("camera", "video", &image),
        None,
        endpoint("model", "tensor", &array),
    )
    .expect("an Image output never feeds an Array input");
    assert!(refusal.contains("camera.video (Image)"), "{refusal}");
    assert!(refusal.contains("model.tensor (Array)"), "{refusal}");
    assert!(refusal.contains("ImageToArray"), "{refusal}");
}

#[test]
fn a_link_of_one_family_with_nothing_required_is_taken() {
    let image = PortFamilyDeclaration::of_family(PortFamily::Image);
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint("camera", "video", &image),
            None,
            endpoint("display", "video", &image),
        ),
        None
    );
}

#[test]
fn a_required_name_the_output_does_not_state_is_refused_naming_it() {
    let output = channels_output_stating(&["left", "right"]);
    let input = channels_input_requiring(&["left", "rpm"]);
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("mic", "audio", &output),
        None,
        endpoint("meter", "audio", &input),
    )
    .expect("rpm is not stated");
    assert!(
        refusal.contains("requires [\"rpm\"], which"),
        "only the missing name: {refusal}"
    );
    assert!(refusal.contains("mic.audio") && refusal.contains("meter.audio"));
}

#[test]
fn a_requirement_on_an_output_stating_no_names_is_refused() {
    let output = PortFamilyDeclaration::of_family(PortFamily::Channels);
    let input = channels_input_requiring(&["left"]);
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("mic", "audio", &output),
        None,
        endpoint("meter", "audio", &input),
    )
    .expect("an output with no names satisfies no requirement");
    assert!(refusal.contains("states no names"), "{refusal}");
}

#[test]
fn names_from_config_resolve_against_the_output_nodes_config() {
    let output = PortFamilyDeclaration {
        names: Some(OutputPortDeclaredNames::FromConfig {
            config_key: "channel_names".into(),
        }),
        ..PortFamilyDeclaration::of_family(PortFamily::Channels)
    };
    let input = channels_input_requiring(&["rpm"]);
    let config = serde_json::json!({ "channel_names": ["rpm", "torque"] });
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint("engine", "signals", &output),
            Some(&config),
            endpoint("gauge", "signals", &input),
        ),
        None
    );
    let config_without_rpm = serde_json::json!({ "channel_names": ["torque"] });
    assert!(
        refusal_of_a_link_between_port_families(
            endpoint("engine", "signals", &output),
            Some(&config_without_rpm),
            endpoint("gauge", "signals", &input),
        )
        .is_some()
    );
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("engine", "signals", &output),
        None,
        endpoint("gauge", "signals", &input),
    )
    .expect("unset config key cannot satisfy rpm");
    assert!(refusal.contains("channel_names"), "{refusal}");
}

#[test]
fn a_codec_outside_the_required_set_is_refused() {
    let output = PortFamilyDeclaration {
        codec: Some("h265".into()),
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    let input = PortFamilyDeclaration {
        requires_codec: vec!["h264".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("encoder", "packets", &output),
        None,
        endpoint("publisher", "video", &input),
    )
    .expect("h265 is not h264");
    assert!(refusal.contains("\"h265\"") && refusal.contains("\"h264\""));
    assert!(refusal.contains("encoder.packets") && refusal.contains("publisher.video"));
}

#[test]
fn image_formats_that_do_not_overlap_are_refused() {
    let output = PortFamilyDeclaration {
        formats: vec!["rgba16_float".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let input = PortFamilyDeclaration {
        formats: vec!["rgba8_unorm".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("render", "video", &output),
        None,
        endpoint("compositor", "video", &input),
    )
    .expect("rgba16_float never reaches an rgba8_unorm-only input");
    assert!(refusal.contains("rgba16_float") && refusal.contains("rgba8_unorm"));
    let input_reading_both = PortFamilyDeclaration {
        formats: vec!["rgba8_unorm".into(), "rgba16_float".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint("render", "video", &output),
            None,
            endpoint("compositor", "video", &input_reading_both),
        ),
        None
    );
}

#[test]
fn an_image_outputs_frame_rate_resolves_from_its_class_or_its_nodes_config() {
    let stated = PortFamilyDeclaration {
        frame_rate: Some(ImageOutputPortFrameRate::Stated(
            crate::ImageStreamFrameRate {
                numerator: 30_000,
                denominator: 1_001,
            },
        )),
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    assert_eq!(
        output_port_frame_rate_resolved_against_config(&stated, None),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 30_000,
            denominator: 1_001
        }))
    );
    let from_config = PortFamilyDeclaration {
        frame_rate: Some(ImageOutputPortFrameRate::FromConfig {
            config_key: "frames_per_second".into(),
        }),
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let resolved = |config: serde_json::Value| {
        output_port_frame_rate_resolved_against_config(&from_config, Some(&config))
    };
    assert_eq!(
        resolved(serde_json::json!({"frames_per_second": 60})),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 60,
            denominator: 1
        }))
    );
    assert_eq!(
        resolved(
            serde_json::json!({"frames_per_second": {"numerator": 24000, "denominator": 1001}})
        ),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 24_000,
            denominator: 1_001
        }))
    );
    assert_eq!(resolved(serde_json::json!({})), Ok(None));
    assert!(resolved(serde_json::json!({"frames_per_second": 0})).is_err());
    assert!(resolved(serde_json::json!({"frames_per_second": "fast"})).is_err());
    assert!(
        PortFamilyDeclaration {
            frame_rate: from_config.frame_rate.clone(),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        }
        .refusal_of_its_keys("audio", PortFamilyDeclarationDirection::Output)
        .is_some_and(|refusal| refusal.contains("frame_rate"))
    );
}

#[test]
fn a_format_that_is_no_engine_texture_name_is_refused() {
    let image_in_shorthand = PortFamilyDeclaration {
        formats: vec!["rgba8".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let refusal = image_in_shorthand
        .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
        .expect("rgba8 is no engine texture name");
    assert!(refusal.contains("\"rgba8\"") && refusal.contains("rgba8_unorm"));
}

#[test]
fn a_key_the_family_or_direction_does_not_take_is_refused_by_name() {
    let image_with_names = PortFamilyDeclaration {
        names: Some(OutputPortDeclaredNames::Stated(vec!["x".into()])),
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let refusal = image_with_names
        .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
        .expect("Image has no names");
    assert!(refusal.contains("`names=`") && refusal.contains("Image"));
    let input_with_codec = PortFamilyDeclaration {
        codec: Some("h264".into()),
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    assert!(
        input_with_codec
            .refusal_of_its_keys("packets", PortFamilyDeclarationDirection::Input)
            .is_some()
    );
    assert_eq!(
        channels_output_stating(&["left"])
            .refusal_of_its_keys("audio", PortFamilyDeclarationDirection::Output),
        None
    );
}

#[test]
fn a_declaration_round_trips_through_its_wire_form() {
    let declaration = PortFamilyDeclaration {
        names: Some(OutputPortDeclaredNames::FromConfig {
            config_key: "channel_names".into(),
        }),
        rate: Some(ChannelsOutputPortSampleRate::Stated(48_000)),
        dtype: Some(ChannelsOutputPortSampleType::F32),
        ..PortFamilyDeclaration::of_family(PortFamily::Channels)
    };
    let wire = serde_json::to_value(&declaration).unwrap();
    assert_eq!(wire["family"], "channels");
    assert_eq!(wire["names"]["from_config"]["config_key"], "channel_names");
    assert_eq!(
        serde_json::from_value::<PortFamilyDeclaration>(wire).unwrap(),
        declaration
    );
}

fn an_output_whose_family_and_codec_come_from_config() -> PortFamilyDeclaration {
    PortFamilyDeclaration {
        family_from_config_key: Some("family".into()),
        codec_from_config_key: Some("codec".into()),
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    }
}

#[test]
fn an_output_family_and_codec_from_config_resolve_against_the_output_nodes_config() {
    let output = an_output_whose_family_and_codec_come_from_config();
    let h264_input = PortFamilyDeclaration {
        requires_codec: vec!["h264".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    let records_input = PortFamilyDeclaration::of_family(PortFamily::Records);
    let source = endpoint("receive", "content", &output);

    let h264_config = serde_json::json!({"family": "packets", "codec": "h264"});
    assert_eq!(
        refusal_of_a_link_between_port_families(
            source,
            Some(&h264_config),
            endpoint("decoder", "encoded_video", &h264_input),
        ),
        None
    );

    let records_config = serde_json::json!({"family": "records"});
    assert_eq!(
        refusal_of_a_link_between_port_families(
            source,
            Some(&records_config),
            endpoint("sink", "records", &records_input),
        ),
        None
    );
    let refusal = refusal_of_a_link_between_port_families(
        source,
        Some(&records_config),
        endpoint("decoder", "encoded_video", &h264_input),
    )
    .expect("a Records output into a Packets input is refused");
    assert!(
        refusal.contains("receive.content (Records)") && refusal.contains("(Packets)"),
        "got {refusal}"
    );

    let opus_config = serde_json::json!({"family": "packets", "codec": "opus"});
    let refusal = refusal_of_a_link_between_port_families(
        source,
        Some(&opus_config),
        endpoint("decoder", "encoded_video", &h264_input),
    )
    .expect("a codec outside the required set is refused");
    assert!(refusal.contains("\"opus\""), "got {refusal}");

    let unknown_family = serde_json::json!({"family": "video"});
    let refusal = refusal_of_a_link_between_port_families(
        source,
        Some(&unknown_family),
        endpoint("sink", "records", &records_input),
    )
    .expect("a family no port has is refused");
    assert!(
        refusal.contains("\"video\"") && refusal.contains("records"),
        "got {refusal}"
    );
}

#[test]
fn an_input_taking_several_families_takes_each_and_refuses_another_naming_the_conversion() {
    let input = PortFamilyDeclaration {
        also_takes_families: vec![PortFamily::Records],
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    let destination = endpoint("send", "content", &input);
    for family in [PortFamily::Packets, PortFamily::Records] {
        let output = PortFamilyDeclaration::of_family(family);
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("source", "out", &output),
                None,
                destination,
            ),
            None,
            "{family} is taken"
        );
    }
    let image = PortFamilyDeclaration::of_family(PortFamily::Image);
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("camera", "video", &image),
        None,
        destination,
    )
    .expect("an Image link is refused");
    assert!(
        refusal.contains("(Packets or Records)") && refusal.contains("H264Encoder"),
        "got {refusal}"
    );
}

#[test]
fn the_keys_resolved_from_config_are_taken_only_in_the_direction_that_has_them() {
    assert_eq!(
        an_output_whose_family_and_codec_come_from_config()
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output),
        None
    );
    assert!(
        an_output_whose_family_and_codec_come_from_config()
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Input)
            .is_some_and(|refusal| refusal.contains("family_from_config_key"))
    );
    let input_taking_records = PortFamilyDeclaration {
        also_takes_families: vec![PortFamily::Records],
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    assert!(
        input_taking_records
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output)
            .is_some_and(|refusal| refusal.contains("also_takes_families"))
    );
    let records_output_with_a_codec_key = PortFamilyDeclaration {
        codec_from_config_key: Some("codec".into()),
        ..PortFamilyDeclaration::of_family(PortFamily::Records)
    };
    assert!(
        records_output_with_a_codec_key
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output)
            .is_some_and(|refusal| refusal.contains("codec_from_config_key"))
    );
}

#[test]
fn a_records_output_whose_config_holds_an_empty_codec_links_to_a_records_input() {
    let records_input = PortFamilyDeclaration::of_family(PortFamily::Records);
    let config = serde_json::json!({"family": "records", "codec": ""});
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint(
                "receive",
                "content",
                &an_output_whose_family_and_codec_come_from_config()
            ),
            Some(&config),
            endpoint("sink", "records", &records_input),
        ),
        None
    );
}

#[test]
fn the_keys_of_every_family_a_port_may_carry_are_taken() {
    let input_taking_packets_and_records = PortFamilyDeclaration {
        also_takes_families: vec![PortFamily::Records],
        requires: vec!["pts".into()],
        requires_codec: vec!["h264".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    assert_eq!(
        input_taking_packets_and_records
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Input),
        None
    );
    let output_whose_family_comes_from_config_stating_names = PortFamilyDeclaration {
        names: Some(OutputPortDeclaredNames::Stated(vec!["pts".into()])),
        ..an_output_whose_family_and_codec_come_from_config()
    };
    assert_eq!(
        output_whose_family_comes_from_config_stating_names
            .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output),
        None
    );
}
