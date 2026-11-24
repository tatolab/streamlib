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

/// The words of [`refusal_of_a_link_between_port_families`]'s refusal, whatever kind it is.
fn refusal_words(
    output: PortFamilyLinkEndpoint<'_>,
    output_node_config: Option<&serde_json::Value>,
    input: PortFamilyLinkEndpoint<'_>,
) -> Option<String> {
    refusal_of_a_link_between_port_families(output, output_node_config, input)
        .map(|refusal| refusal.to_string())
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
    let refusal = refusal_words(
        endpoint("camera", "video", &image),
        None,
        endpoint("model", "tensor", &array),
    )
    .expect("an Image output never feeds an Array input");
    assert!(refusal.contains("camera.video (Image)"), "{refusal}");
    assert!(refusal.contains("model.tensor (Array)"), "{refusal}");
    assert!(refusal.contains("TextureToTensor"), "{refusal}");
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
    let refusal = refusal_words(
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
    let refusal = refusal_words(
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
    let refusal = refusal_words(
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
    let refusal = refusal_words(
        endpoint("encoder", "packets", &output),
        None,
        endpoint("publisher", "video", &input),
    )
    .expect("h265 is not h264");
    assert!(refusal.contains("\"h265\"") && refusal.contains("\"h264\""));
    assert!(refusal.contains("encoder.packets") && refusal.contains("publisher.video"));
}

#[test]
fn an_image_output_writing_only_formats_the_input_does_not_read_is_refused() {
    let output = PortFamilyDeclaration {
        formats: vec!["rgba16_float".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let input = PortFamilyDeclaration {
        formats: vec!["rgba8_unorm".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let refusal = refusal_words(
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
fn an_image_output_writing_a_format_the_input_does_not_read_is_refused_though_others_overlap() {
    let output = PortFamilyDeclaration {
        formats: vec!["rgba8_unorm".into(), "nv12".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let input = PortFamilyDeclaration {
        formats: vec!["rgba8_unorm".into()],
        ..PortFamilyDeclaration::of_family(PortFamily::Image)
    };
    let refusal = refusal_of_a_link_between_port_families(
        endpoint("camera", "video", &output),
        None,
        endpoint("compositor", "video", &input),
    )
    .expect("the camera may write nv12, which the compositor does not read");
    let PortFamilyLinkRefusal::ImageFormatsNotRead(refusal) = refusal else {
        panic!("a refusal by its formats alone: {refusal:?}");
    };
    assert!(
        refusal.ends_with(
            "camera.video may write [\"nv12\"], which compositor.video does not read (it reads \
             only [\"rgba8_unorm\"])"
        ),
        "{refusal}"
    );
    let input_reading_any = PortFamilyDeclaration::of_family(PortFamily::Image);
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint("camera", "video", &output),
            None,
            endpoint("compositor", "video", &input_reading_any),
        ),
        None
    );
    let output_stating_none = PortFamilyDeclaration::of_family(PortFamily::Image);
    assert_eq!(
        refusal_of_a_link_between_port_families(
            endpoint("camera", "video", &output_stating_none),
            None,
            endpoint("compositor", "video", &input),
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
    let refusal = refusal_words(
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
    let refusal = refusal_words(
        source,
        Some(&opus_config),
        endpoint("decoder", "encoded_video", &h264_input),
    )
    .expect("a codec outside the required set is refused");
    assert!(refusal.contains("\"opus\""), "got {refusal}");

    let unknown_family = serde_json::json!({"family": "video"});
    let refusal = refusal_words(
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
fn an_output_whose_type_a_choice_holds_travels_on_that_types_family_and_codec() {
    let output = PortFamilyDeclaration {
        family_from_config_key: Some("content_type".into()),
        codec_from_config_key: Some("content_type".into()),
        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
    };
    let chosen = |pin_type: serde_json::Value| serde_json::json!({ "content_type": pin_type });

    let h264 = chosen(serde_json::json!({"type": "encoded_video", "codecs": ["h264"]}));
    assert_eq!(
        output_port_family_resolved_against_config(&output, Some(&h264)),
        Ok(PortFamily::Packets)
    );
    assert_eq!(
        output_port_codec_resolved_against_config(&output, Some(&h264)),
        Ok(Some("h264".to_owned()))
    );

    let named = chosen(serde_json::json!({"type": "named", "named_pin_type": "notes:Note"}));
    assert_eq!(
        output_port_family_resolved_against_config(&output, Some(&named)),
        Ok(PortFamily::Records),
        "a named type's port is declared on Records"
    );
    assert_eq!(
        output_port_codec_resolved_against_config(&output, Some(&named)),
        Ok(None)
    );

    let any_codec = chosen(serde_json::json!({"type": "encoded_audio"}));
    assert_eq!(
        output_port_codec_resolved_against_config(&output, Some(&any_codec)),
        Ok(None),
        "a choice naming no one codec states none"
    );

    let a_list = chosen(serde_json::json!({"type": "list_of", "element_type": {"type": "float"}}));
    let refusal = output_port_family_resolved_against_config(&output, Some(&a_list))
        .expect_err("a List travels on no one family");
    assert!(
        refusal.contains("no stream's port carries"),
        "got {refusal}"
    );

    let no_pin_type = chosen(serde_json::json!({"type": "a picture"}));
    let refusal = output_port_family_resolved_against_config(&output, Some(&no_pin_type))
        .expect_err("an object naming no pin type is refused");
    assert!(refusal.contains("chooses no pin type"), "got {refusal}");
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
    let refusal = refusal_words(endpoint("camera", "video", &image), None, destination)
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

#[cfg(feature = "contract-json-schema")]
mod contract_json_schema {
    use std::collections::BTreeSet;

    use serde::Serialize;
    use serde_json::{Value, json};

    use schemars_1::{JsonSchema, SchemaGenerator};

    use crate::{
        ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
        ImageOutputPortFrameRate, ImageStreamFrameRate,
    };

    fn schema_of<T: JsonSchema>() -> Value {
        SchemaGenerator::default()
            .into_root_schema_for::<T>()
            .to_value()
    }

    fn keys_of(object: &Value) -> BTreeSet<String> {
        object
            .as_object()
            .unwrap_or_else(|| panic!("an object: {object}"))
            .keys()
            .cloned()
            .collect()
    }

    fn one_of(schema: &Value) -> &Vec<Value> {
        schema["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("a `oneOf`: {schema}"))
    }

    fn assert_each_serializes_as_a_const_of_the_schema<T: JsonSchema + Serialize>(values: &[T]) {
        let schema = schema_of::<T>();
        let consts: Vec<&Value> = one_of(&schema)
            .iter()
            .map(|variant| &variant["const"])
            .collect();
        assert_eq!(consts.len(), values.len());
        for value in values {
            let written = serde_json::to_value(value).expect("writes");
            assert!(consts.contains(&&written), "{written} is not in {schema}");
        }
    }

    /// Asserts `value` serializes with exactly the property names of the schema variant that
    /// requires its one key, and, for an object under that key, of that variant's inner object.
    fn assert_serializes_as_a_variant_of_the_schema<T: JsonSchema + Serialize>(value: T) {
        let schema = schema_of::<T>();
        let written = serde_json::to_value(value).expect("writes");
        let written_keys = keys_of(&written);
        let variant = one_of(&schema)
            .iter()
            .find(|variant| keys_of(&variant["properties"]) == written_keys)
            .unwrap_or_else(|| panic!("no variant of {schema} has the keys of {written}"));
        assert_eq!(
            variant["required"],
            json!(written_keys.iter().collect::<Vec<_>>())
        );
        for (key, inner) in written.as_object().into_iter().flatten() {
            let inner_schema = &variant["properties"][key];
            if inner.is_object() && inner_schema.get("properties").is_some() {
                assert_eq!(
                    keys_of(inner),
                    keys_of(&inner_schema["properties"]),
                    "{key}"
                );
            }
        }
    }

    #[test]
    fn each_unit_only_enum_serializes_as_one_of_its_schema_consts() {
        assert_each_serializes_as_a_const_of_the_schema(&[
            ChannelsOutputPortSampleType::F32,
            ChannelsOutputPortSampleType::F64,
        ]);
        assert_each_serializes_as_a_const_of_the_schema(&[
            ArrayOutputPortResidency::Gpu,
            ArrayOutputPortResidency::Cpu,
        ]);
    }

    #[test]
    fn each_stated_or_config_key_value_has_its_schema_variant_property_names() {
        let frame_rate = ImageStreamFrameRate {
            numerator: 30,
            denominator: 1,
        };
        assert_serializes_as_a_variant_of_the_schema(ImageOutputPortFrameRate::Stated(frame_rate));
        assert_serializes_as_a_variant_of_the_schema(ImageOutputPortFrameRate::FromConfig {
            config_key: "fps".into(),
        });
        assert_serializes_as_a_variant_of_the_schema(ChannelsOutputPortSampleRate::Stated(48_000));
        assert_serializes_as_a_variant_of_the_schema(ChannelsOutputPortSampleRate::FromConfig {
            config_key: "rate".into(),
        });
        let frame_rate_schema = schema_of::<ImageStreamFrameRate>();
        let written = serde_json::to_value(frame_rate).expect("writes");
        assert_eq!(keys_of(&written), keys_of(&frame_rate_schema["properties"]));
        assert_eq!(
            frame_rate_schema["required"],
            json!(["numerator", "denominator"])
        );
    }
}
