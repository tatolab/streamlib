// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn a_pin_type_named(name: BuiltInPinTypeName) -> PinType {
    match name {
        BuiltInPinTypeName::Boolean => PinType::Boolean {},
        BuiltInPinTypeName::Integer => PinType::Integer {
            exact_size: Some(IntegerSize::UInt8),
        },
        BuiltInPinTypeName::Float => PinType::Float { exact_size: None },
        BuiltInPinTypeName::String => PinType::String {},
        BuiltInPinTypeName::Menu => PinType::Menu {
            option_names: vec!["sine".into(), "square".into()],
        },
        BuiltInPinTypeName::Color => PinType::Color {},
        BuiltInPinTypeName::Vector => PinType::Vector {
            component_count: VectorComponentCount::Three,
        },
        BuiltInPinTypeName::Rectangle => PinType::Rectangle {},
        BuiltInPinTypeName::Transform => PinType::Transform {},
        BuiltInPinTypeName::Texture => PinType::Texture {
            pixel_formats: vec![TexturePixelFormat::Rgba8Unorm],
        },
        BuiltInPinTypeName::Audio => PinType::Audio {},
        BuiltInPinTypeName::EncodedVideo => PinType::EncodedVideo {
            codecs: vec!["h264".into()],
        },
        BuiltInPinTypeName::EncodedAudio => PinType::EncodedAudio { codecs: vec![] },
        BuiltInPinTypeName::Tensor => PinType::Tensor {},
        BuiltInPinTypeName::Asset => PinType::Asset {
            media: vec![ProjectAssetMedia::Stream],
        },
        BuiltInPinTypeName::Trigger => PinType::Trigger {},
        BuiltInPinTypeName::ListOf => PinType::ListOf {
            element_type: Box::new(PinType::Float { exact_size: None }),
        },
        BuiltInPinTypeName::Bundle => PinType::Bundle {
            name: None,
            members: vec![BundleMember {
                name: "opacity".into(),
                member_type: PinType::Float { exact_size: None },
                meaning: None,
                default_value: Some(json!(1.0)),
            }],
        },
        BuiltInPinTypeName::Named => PinType::Named {
            named_pin_type: NamedPinTypeImportPath::new("faces:FaceBox").expect("names a class"),
        },
        BuiltInPinTypeName::Connection => PinType::Connection {
            connection_type: NamedPinTypeImportPath::new("relays:RelayConnection")
                .expect("names a class"),
            one_referrer_per_referenced_node: true,
        },
        BuiltInPinTypeName::NodeReference => PinType::NodeReference {
            referenced_node_type: None,
            one_referrer_per_referenced_node: true,
        },
        BuiltInPinTypeName::PinTypeChoice => PinType::PinTypeChoice {
            among_the_bundles_its_stream_declares: false,
            among: vec![PinType::Audio {}],
        },
    }
}

fn wire_name_of(value: impl serde::Serialize + std::fmt::Debug) -> String {
    serde_json::to_value(&value)
        .ok()
        .and_then(|written| written.as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("{value:?} serializes as a string"))
}

#[test]
fn each_name_is_the_type_tag_of_its_pin_type_and_round_trips() {
    for name in BuiltInPinTypeName::EVERY_NAME {
        let pin_type = a_pin_type_named(name);
        assert_eq!(pin_type.built_in_name(), name);
        let written = serde_json::to_value(&pin_type).expect("a pin type serializes");
        assert_eq!(written["type"], json!(wire_name_of(name)));
        let read: PinType = serde_json::from_value(written).expect("a pin type reads back");
        assert_eq!(read, pin_type);
    }
}

#[test]
fn there_is_one_row_per_built_in_name_in_order_with_its_typescript_literal() {
    let rows = built_in_pin_types();
    let names: Vec<BuiltInPinTypeName> = rows.iter().map(|row| row.name).collect();
    assert_eq!(names, BuiltInPinTypeName::EVERY_NAME.to_vec());
    for row in rows {
        assert_eq!(
            row.typescript_form,
            format!("\"{}\"", wire_name_of(row.name))
        );
        assert!(!row.flows.is_empty(), "{:?} allows no flow", row.name);
        assert!(!row.label.is_empty() && !row.documentation.is_empty());
        assert!(
            !row.python_forms.is_empty(),
            "{:?} has no Python form",
            row.name
        );
        assert!(row.python_forms.iter().all(|form| !form.is_empty()));
    }
}

#[test]
fn a_setting_only_type_flows_only_as_a_value_and_a_trigger_only_as_a_trigger() {
    for name in [
        BuiltInPinTypeName::NodeReference,
        BuiltInPinTypeName::PinTypeChoice,
    ] {
        assert_eq!(built_in_pin_type(name).flows, &[PinFlow::Value]);
    }
    assert_eq!(
        built_in_pin_type(BuiltInPinTypeName::Trigger).flows,
        &[PinFlow::Trigger]
    );
}

#[test]
fn each_texture_pixel_format_serializes_as_its_wire_name() {
    for format in TexturePixelFormat::EVERY_FORMAT {
        assert_eq!(wire_name_of(format), format.wire_name());
    }
}

/// Annotations stating only the field `name` names.
fn annotations_stating(name: PinAnnotationName) -> PinAnnotations {
    let none = PinAnnotations::default();
    match name {
        PinAnnotationName::Range => PinAnnotations {
            range: Some(PinNumberRange::default()),
            ..none
        },
        PinAnnotationName::Step => PinAnnotations {
            step: Some(0.1),
            ..none
        },
        PinAnnotationName::Unit => PinAnnotations {
            unit: Some("Hz".into()),
            ..none
        },
        PinAnnotationName::Meaning => PinAnnotations {
            meaning: Some("frequency".into()),
            ..none
        },
        PinAnnotationName::Control => PinAnnotations {
            control: Some("slider".into()),
            ..none
        },
        PinAnnotationName::FrameRate => PinAnnotations {
            frame_rate: Some(crate::ImageOutputPortFrameRate::FromSetting {
                setting: "fps".into(),
            }),
            ..none
        },
        PinAnnotationName::SampleRate => PinAnnotations {
            sample_rate: Some(crate::ChannelsOutputPortSampleRate::Stated(48_000)),
            ..none
        },
        PinAnnotationName::SampleType => PinAnnotations {
            sample_type: Some(crate::ChannelsOutputPortSampleType::F32),
            ..none
        },
        PinAnnotationName::Residency => PinAnnotations {
            residency: Some(crate::ArrayOutputPortResidency::Gpu),
            ..none
        },
        PinAnnotationName::ProjectAsset => PinAnnotations {
            project_asset: Some(PinProjectAssetAnnotation::default()),
            ..none
        },
    }
}

#[test]
fn each_annotation_name_is_the_key_of_the_one_field_it_names() {
    let every_field_stated = PinAnnotations {
        range: Some(PinNumberRange::default()),
        step: Some(0.1),
        unit: Some("Hz".into()),
        meaning: Some("frequency".into()),
        control: Some("slider".into()),
        frame_rate: Some(crate::ImageOutputPortFrameRate::FromSetting {
            setting: "fps".into(),
        }),
        sample_rate: Some(crate::ChannelsOutputPortSampleRate::Stated(48_000)),
        sample_type: Some(crate::ChannelsOutputPortSampleType::F32),
        residency: Some(crate::ArrayOutputPortResidency::Gpu),
        project_asset: Some(PinProjectAssetAnnotation::default()),
    };
    let every_key: Vec<String> = serde_json::to_value(every_field_stated)
        .expect("writes")
        .as_object()
        .unwrap_or_else(|| panic!("annotations serialize as an object"))
        .keys()
        .cloned()
        .collect();
    let every_name: Vec<PinAnnotationName> = every_key
        .iter()
        .map(|key| {
            serde_json::from_value(json!(key)).unwrap_or_else(|_| panic!("`{key}` has no name"))
        })
        .collect();
    let distinct_names: std::collections::HashSet<PinAnnotationName> =
        every_name.iter().copied().collect();
    assert_eq!(distinct_names.len(), every_key.len());
    for name in every_name {
        let written = serde_json::to_value(annotations_stating(name)).expect("writes");
        let keys: Vec<&String> = written
            .as_object()
            .unwrap_or_else(|| panic!("annotations serialize as an object"))
            .keys()
            .collect();
        assert_eq!(keys, [&wire_name_of(name)], "{name:?}");
    }
}

#[test]
fn a_pin_type_with_its_narrowing_left_out_reads_as_taking_any() {
    let texture: PinType = serde_json::from_value(json!({ "type": "texture" })).expect("reads");
    assert_eq!(
        texture,
        PinType::Texture {
            pixel_formats: vec![]
        }
    );
    assert_eq!(
        serde_json::to_value(&texture).expect("writes"),
        json!({ "type": "texture" })
    );
    let chosen: PinType =
        serde_json::from_value(json!({ "type": "encoded_video", "codecs": ["h264"] }))
            .expect("reads");
    assert_eq!(
        chosen,
        PinType::EncodedVideo {
            codecs: vec!["h264".into()]
        }
    );
}

#[test]
fn a_pin_type_with_a_key_its_variant_does_not_name_is_refused() {
    assert!(serde_json::from_value::<PinType>(json!({ "type": "texture", "codecs": [] })).is_err());
    assert!(serde_json::from_value::<PinType>(json!({ "type": "channels" })).is_err());
}

#[test]
fn a_type_with_nothing_to_state_refuses_any_key_but_its_tag() {
    assert_eq!(
        serde_json::from_value::<PinType>(json!({ "type": "boolean" })).expect("reads"),
        PinType::Boolean {}
    );
    assert_eq!(
        serde_json::to_value(PinType::Boolean {}).expect("writes"),
        json!({ "type": "boolean" })
    );
    for name in BuiltInPinTypeName::EVERY_NAME {
        let mut written = serde_json::to_value(a_pin_type_named(name)).expect("writes");
        written["not_a_key_of_any_type"] = json!([]);
        assert!(
            serde_json::from_value::<PinType>(written).is_err(),
            "{name:?} took a key it does not name"
        );
    }
}

#[test]
fn a_named_pin_type_names_a_module_and_a_class() {
    for refused in ["", "  ", "faces.FaceBox", ":FaceBox", "faces:", " : "] {
        assert!(
            NamedPinTypeImportPath::new(refused).is_err(),
            "`{refused}` was taken"
        );
        assert!(serde_json::from_value::<NamedPinTypeImportPath>(json!(refused)).is_err());
    }
    let read: NamedPinTypeImportPath =
        serde_json::from_value(json!("faces.boxes:FaceBox.Inner")).expect("names a class");
    assert_eq!(read.as_str(), "faces.boxes:FaceBox.Inner");
}

#[test]
fn a_declaration_writes_its_defaults_as_nothing() {
    let declaration = PinDeclaration {
        pin_type: DeclaredPinType::Is {
            pin_type: PinType::Float { exact_size: None },
        },
        flow: PinFlow::Value,
        annotations: PinAnnotations::default(),
        former_names: vec![],
        may_be_unset: false,
    };
    let written = serde_json::to_value(&declaration).expect("writes");
    assert_eq!(
        written,
        json!({ "pin_type": { "kind": "is", "pin_type": { "type": "float" } }, "flow": "value" })
    );
    let read: PinDeclaration = serde_json::from_value(written).expect("reads back");
    assert_eq!(read, declaration);
}

/// An input the engine hands its node is written as contract 4's `setting_only`, and the `read`
/// a `tatolab-stream` released before every input was live stamps beside it is ignored.
///
/// Mental-revert: refusing a field the mode does not name refuses every node a released SDK
/// declares with a setting.
#[test]
fn an_input_mode_is_tagged_and_a_read_time_beside_it_is_ignored() {
    for stamped in [
        json!({ "mode": "setting_only" }),
        json!({ "mode": "setting_only", "read": "at_setup" }),
        json!({ "mode": "setting_only", "read": "each_run" }),
    ] {
        let handed: NodeInputMode = serde_json::from_value(stamped).expect("reads");
        assert_eq!(handed, NodeInputMode::HandedToItsNode);
    }
    assert_eq!(
        serde_json::to_value(NodeInputMode::HandedToItsNode).expect("writes"),
        json!({ "mode": "setting_only" })
    );
    assert_eq!(
        serde_json::to_value(NodeInputMode::WireOnly).expect("writes"),
        json!({ "mode": "wire_only" })
    );
}

#[test]
fn each_stored_value_reads_the_json_form_the_code_saves() {
    serde_json::from_value::<ColorPinValue>(
        json!({ "red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0 }),
    )
    .expect("a colour");
    serde_json::from_value::<VectorPinValue>(json!({ "x": 0.5, "y": 0.5 })).expect("two");
    serde_json::from_value::<VectorPinValue>(json!({ "x": 0, "y": 0, "z": 0, "w": 1 }))
        .expect("four");
    serde_json::from_value::<RectanglePinValue>(
        json!({ "x": 80, "y": 800, "width": 640, "height": 360 }),
    )
    .expect("a rectangle");
    serde_json::from_value::<TransformPinValue>(json!([
        1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1
    ]))
    .expect("the identity");
    serde_json::from_value::<AssetPinValue>(
        json!({ "asset": "0eaf9ee2-0f73-4154-a3f6-a99c261b850d" }),
    )
    .expect("an asset");
    serde_json::from_value::<crate::NodeReferenceInConfig>(json!({ "node": "gateway" }))
        .expect("a node reference");
}

#[test]
fn a_stored_value_with_a_key_its_form_does_not_name_or_a_bare_asset_string_is_refused() {
    assert!(serde_json::from_value::<VectorPinValue>(json!({ "x": 0, "y": 0, "v": 0 })).is_err());
    assert!(
        serde_json::from_value::<AssetPinValue>(json!("0eaf9ee2-0f73-4154-a3f6-a99c261b850d"))
            .is_err()
    );
    assert!(serde_json::from_value::<TransformPinValue>(json!([1, 0, 0])).is_err());
}

#[test]
fn an_asset_that_is_not_a_uuid_is_refused() {
    assert!(serde_json::from_value::<AssetPinValue>(json!({ "asset": "logo.png" })).is_err());
    assert!(
        serde_json::from_value::<AssetPinValue>(json!({
            "asset": "0eaf9ee2-0f73-4154-a3f6-a99c261b850d",
            "from": "the-other-project"
        }))
        .is_err()
    );
}

/// A rate read from a setting is written `from_setting`, and the `from_config` an older
/// declaration wrote still reads.
#[test]
fn a_rate_read_from_a_setting_is_written_from_setting_and_reads_its_older_spelling() {
    let read_from_rate = crate::ChannelsOutputPortSampleRate::FromSetting {
        setting: "rate".into(),
    };
    assert_eq!(
        serde_json::to_value(&read_from_rate).unwrap(),
        json!({ "from_setting": { "setting": "rate" } })
    );
    assert_eq!(
        serde_json::from_value::<crate::ChannelsOutputPortSampleRate>(
            json!({ "from_config": { "config_key": "rate" } })
        )
        .unwrap(),
        read_from_rate
    );
}

#[test]
fn a_pin_annotation_reuses_the_port_stream_fact_forms() {
    let annotations: PinAnnotations = serde_json::from_value(json!({
        "frame_rate": { "stated": { "numerator": 30000, "denominator": 1001 } },
        "sample_rate": { "from_setting": { "setting": "rate" } },
        "sample_type": "f32",
        "residency": "gpu",
    }))
    .expect("reads");
    assert_eq!(
        annotations.frame_rate,
        Some(crate::ImageOutputPortFrameRate::Stated(
            crate::ImageStreamFrameRate {
                numerator: 30000,
                denominator: 1001
            }
        ))
    );
    assert_eq!(
        annotations.sample_rate,
        Some(crate::ChannelsOutputPortSampleRate::FromSetting {
            setting: "rate".into()
        })
    );
}

#[test]
fn a_type_flows_by_its_rows_first_flow_and_a_list_or_bundle_streams_when_a_member_does() {
    let float = PinType::Float { exact_size: None };
    let texture = PinType::Texture {
        pixel_formats: vec![],
    };
    let member = |name: &str, member_type: PinType| BundleMember {
        name: name.into(),
        member_type,
        meaning: None,
        default_value: None,
    };

    assert_eq!(PinType::Boolean {}.default_flow(), PinFlow::Value);
    assert_eq!(
        PinType::Integer { exact_size: None }.default_flow(),
        PinFlow::Value
    );
    assert_eq!(float.default_flow(), PinFlow::Value);
    assert_eq!(texture.default_flow(), PinFlow::Stream);
    assert_eq!(PinType::Audio {}.default_flow(), PinFlow::Stream);
    assert_eq!(PinType::Trigger {}.default_flow(), PinFlow::Trigger);
    assert_eq!(
        PinType::ListOf {
            element_type: Box::new(float.clone())
        }
        .default_flow(),
        PinFlow::Value
    );
    assert_eq!(
        PinType::ListOf {
            element_type: Box::new(texture.clone())
        }
        .default_flow(),
        PinFlow::Stream
    );
    assert_eq!(
        PinType::Bundle {
            name: None,
            members: vec![member("opacity", float.clone())]
        }
        .default_flow(),
        PinFlow::Value
    );
    assert_eq!(
        PinType::Bundle {
            name: None,
            members: vec![member("opacity", float), member("frame", texture)]
        }
        .default_flow(),
        PinFlow::Stream
    );
}

#[test]
fn a_port_of_exactly_one_type_is_read_in_its_default_flow_and_resolves_to_it_alone() {
    let declaration = PinDeclaration::exactly(PinType::Float { exact_size: None });

    assert_eq!(
        serde_json::to_value(&declaration).unwrap(),
        json!({"pin_type": {"kind": "is", "pin_type": {"type": "float"}}, "flow": "value"})
    );
    let followed = PinDeclaration {
        pin_type: DeclaredPinType::SameAsInput {
            input_port_name: "value".into(),
        },
        ..declaration.clone()
    };
    assert_eq!(
        declaration.into_resolved(),
        Some(ResolvedPin {
            pin_type: PinType::Float { exact_size: None },
            flow: PinFlow::Value
        })
    );
    assert_eq!(followed.into_resolved(), None);
}

#[test]
fn a_texture_pixel_format_is_found_by_its_wire_name_and_no_other() {
    for format in TexturePixelFormat::EVERY_FORMAT {
        assert_eq!(
            TexturePixelFormat::from_wire_name(format.wire_name()),
            Some(format)
        );
    }
    assert_eq!(TexturePixelFormat::from_wire_name("yuv420"), None);
}

#[test]
fn a_type_holds_a_stored_value_exactly_when_it_states_a_value_form() {
    for name in BuiltInPinTypeName::EVERY_NAME {
        let pin_type = a_pin_type_named(name);
        let holds_none = matches!(
            name,
            BuiltInPinTypeName::Texture
                | BuiltInPinTypeName::Audio
                | BuiltInPinTypeName::EncodedVideo
                | BuiltInPinTypeName::EncodedAudio
                | BuiltInPinTypeName::Tensor
                | BuiltInPinTypeName::Trigger
                | BuiltInPinTypeName::Named
                | BuiltInPinTypeName::Connection
        );
        assert_eq!(pin_type.holds_a_stored_value(), !holds_none, "{name:?}");
    }
}

#[test]
fn each_type_takes_the_example_it_gives_and_refuses_a_value_of_another_form() {
    for name in BuiltInPinTypeName::EVERY_NAME {
        let pin_type = a_pin_type_named(name);
        if !pin_type.holds_a_stored_value() {
            let refused = pin_type
                .check_value(&json!(1.0))
                .expect_err("holds no value");
            if name != BuiltInPinTypeName::Trigger {
                assert_eq!(refused.takes, "no value, only a link", "{name:?}");
            }
            continue;
        }
        let refused = pin_type
            .check_value(&json!({ "neither": "form" }))
            .expect_err("an object of no member is no type's value here");
        pin_type
            .check_value(&refused.example)
            .unwrap_or_else(|not_taken| panic!("{name:?} refuses its own example: {not_taken:?}"));
    }
}

#[test]
fn a_trigger_takes_only_null_which_fires_it() {
    let trigger = PinDeclaration::exactly(PinType::Trigger {});

    assert!(trigger.check_value(&json!(null)).is_ok());
    for value in [json!(true), json!(1), json!({}), json!("now")] {
        let refused = trigger
            .check_value(&value)
            .expect_err("a Trigger holds no value");
        assert_eq!(
            refused.takes,
            "no value: a live set with no value, or `null`, fires it once"
        );
        assert_eq!(refused.example, json!(null));
    }
}

#[test]
fn a_number_is_taken_within_its_size_and_never_rounded_or_clamped() {
    let byte = PinType::Integer {
        exact_size: Some(IntegerSize::UInt8),
    };
    assert!(byte.check_value(&json!(255)).is_ok());
    assert_eq!(
        byte.check_value(&json!(256))
            .expect_err("past a byte")
            .takes,
        "a whole number from 0 to 255"
    );
    assert!(byte.check_value(&json!(-1)).is_err());
    assert!(byte.check_value(&json!(1.5)).is_err(), "no rounding");
    assert!(byte.check_value(&json!(true)).is_err());

    let float = PinType::Float { exact_size: None };
    assert!(float.check_value(&json!(1)).is_ok(), "`1` is taken as 1.0");
    assert!(float.check_value(&json!(1.7)).is_ok());
    assert!(float.check_value(&json!("1.7")).is_err());
    let float32 = PinType::Float {
        exact_size: Some(FloatSize::Float32),
    };
    assert!(float32.check_value(&json!(1e300)).is_err());

    assert!(PinType::Boolean {}.check_value(&json!(false)).is_ok());
    assert!(PinType::Boolean {}.check_value(&json!(0)).is_err());
}

#[test]
fn a_shaped_value_is_taken_only_whole_and_of_its_declared_size() {
    let three = PinType::Vector {
        component_count: VectorComponentCount::Three,
    };
    assert!(
        three
            .check_value(&json!({ "x": 1, "y": 2, "z": 3 }))
            .is_ok()
    );
    assert!(three.check_value(&json!({ "x": 1, "y": 2 })).is_err());
    assert!(
        three
            .check_value(&json!({ "x": 1, "y": 2, "z": 3, "w": 4 }))
            .is_err()
    );
    assert!(
        PinType::Rectangle {}
            .check_value(&json!({ "x": 0, "y": 0, "width": -1, "height": 1 }))
            .is_err()
    );
    let menu = PinType::Menu {
        option_names: vec!["sine".into(), "square".into()],
    };
    assert!(menu.check_value(&json!("square")).is_ok());
    assert_eq!(
        menu.check_value(&json!("Square"))
            .expect_err("spelled as declared")
            .takes,
        "one of `sine`, `square`"
    );
    let floats = PinType::ListOf {
        element_type: Box::new(PinType::Float { exact_size: None }),
    };
    assert!(floats.check_value(&json!([0.5, 1])).is_ok());
    assert!(floats.check_value(&json!([0.5, "one"])).is_err());
}

#[test]
fn a_bundle_takes_each_member_in_its_form_and_leaves_out_only_one_with_a_default() {
    let bundle = PinType::Bundle {
        name: None,
        members: vec![
            BundleMember {
                name: "gain".into(),
                member_type: PinType::Float { exact_size: None },
                meaning: None,
                default_value: Some(json!(1.0)),
            },
            BundleMember {
                name: "enabled".into(),
                member_type: PinType::Boolean {},
                meaning: None,
                default_value: None,
            },
        ],
    };
    assert!(bundle.check_value(&json!({ "enabled": true })).is_ok());
    assert!(bundle.check_value(&json!({ "gain": 0.5 })).is_err());
    assert!(
        bundle
            .check_value(&json!({ "enabled": true, "other": 1 }))
            .is_err()
    );
    assert!(bundle.check_value(&json!({ "enabled": 1 })).is_err());
}

/// A bundle `name` declares, of the one Float member `member`.
fn a_bundle_named(name: &str, member: &str) -> PinType {
    PinType::Bundle {
        name: Some(name.into()),
        members: vec![BundleMember {
            name: member.into(),
            member_type: PinType::Float { exact_size: None },
            meaning: None,
            default_value: None,
        }],
    }
}

/// A bundle writes the name its class gives it beside its members, and one no class declares
/// writes none.
///
/// Mental-revert: a bundle that drops its name reads back without it.
#[test]
fn a_bundle_writes_and_reads_back_the_name_its_class_gives_it() {
    let reading = a_bundle_named("Reading", "level");
    let written = serde_json::to_value(&reading).expect("a bundle serializes");
    assert_eq!(written["name"], json!("Reading"));
    assert_eq!(
        serde_json::from_value::<PinType>(written).expect("a bundle reads back"),
        reading
    );
    let unnamed = PinType::Bundle {
        name: None,
        members: Vec::new(),
    };
    assert_eq!(
        serde_json::to_value(&unnamed).expect("a bundle serializes"),
        json!({ "type": "bundle", "members": [] })
    );
}

/// A choice among the bundles its stream declares takes exactly a bundle it lists, refusing any
/// other naming each it lists, and refuses every type while it lists none; a choice among any
/// type takes any.
///
/// Mental-revert: reading an empty list as any type takes a bundle of a stream that declares
/// none.
#[test]
fn a_choice_among_the_bundles_its_stream_declares_takes_only_a_bundle_it_lists() {
    let reading = a_bundle_named("Reading", "level");
    let tone = a_bundle_named("Tone", "pitch");
    let offering = |among: Vec<PinType>| PinType::PinTypeChoice {
        among,
        among_the_bundles_its_stream_declares: true,
    };
    let as_written = |pin_type: &PinType| serde_json::to_value(pin_type).expect("serializes");

    let both = offering(vec![reading.clone(), tone.clone()]);
    assert!(both.check_value(&as_written(&tone)).is_ok());
    let elsewhere = a_bundle_named("Elsewhere", "level");
    let refused = both
        .check_value(&as_written(&elsewhere))
        .expect_err("a bundle it does not list is refused");
    assert!(refused.takes.contains("`Reading`"), "{}", refused.takes);
    assert!(refused.takes.contains("`Tone`"), "{}", refused.takes);
    assert_eq!(refused.example, as_written(&reading));
    assert!(both.check_value(&json!({ "type": "float" })).is_err());

    let none = offering(Vec::new());
    let refused = none
        .check_value(&as_written(&reading))
        .expect_err("a stream declaring no bundle offers none");
    assert!(refused.takes.contains("declare none"), "{}", refused.takes);

    let any = PinType::a_choice_of_any();
    assert!(any.check_value(&as_written(&reading)).is_ok());
}

/// A choice among the bundles its stream declares takes a bundle of a listed one's member names
/// and types, unnamed as a value saved before bundles were named, or of the listed name; a
/// member's default and meaning do not count, and another name or member type is refused.
///
/// Mental-revert: matching a chosen bundle whole refuses the unnamed `Reading` an earlier graph
/// holds, which undoing a set puts back.
#[test]
fn a_choice_among_the_bundles_its_stream_declares_takes_a_listed_bundle_unnamed_or_by_its_name() {
    let reading = a_bundle_named("Reading", "level");
    let offering = PinType::PinTypeChoice {
        among: vec![reading.clone()],
        among_the_bundles_its_stream_declares: true,
    };
    let with = |name: Option<&str>, member_type: PinType, meaning: Option<&str>| {
        json!(PinType::Bundle {
            name: name.map(str::to_owned),
            members: vec![BundleMember {
                name: "level".into(),
                member_type,
                meaning: meaning.map(str::to_owned),
                default_value: meaning.map(|_| json!(0.5)),
            }],
        })
    };
    let float = || PinType::Float { exact_size: None };

    assert!(offering.check_value(&with(None, float(), None)).is_ok());
    assert!(
        offering
            .check_value(&with(Some("Reading"), float(), Some("loudness")))
            .is_ok()
    );
    assert!(
        offering
            .check_value(&with(Some("Tone"), float(), None))
            .is_err()
    );
    assert!(
        offering
            .check_value(&with(None, PinType::String {}, None))
            .is_err()
    );
}

#[test]
fn an_input_that_may_be_unset_takes_null_and_one_that_may_not_refuses_it() {
    let declared = PinDeclaration::exactly(PinType::Float { exact_size: None });
    let refused = declared
        .check_value(&serde_json::Value::Null)
        .expect_err("an input that must hold a value refuses null");
    assert_eq!(refused.takes, "a number");

    let may_be_unset = PinDeclaration {
        may_be_unset: true,
        ..declared
    };
    assert!(may_be_unset.check_value(&serde_json::Value::Null).is_ok());
    assert!(may_be_unset.check_value(&json!(0.25)).is_ok());
    assert!(may_be_unset.check_value(&json!("loud")).is_err());
}

#[test]
fn a_declaration_of_several_types_takes_a_value_any_of_them_takes() {
    let declared = PinDeclaration {
        pin_type: DeclaredPinType::OneOf {
            pin_types: vec![PinType::Boolean {}, PinType::String {}],
        },
        ..PinDeclaration::exactly(PinType::Boolean {})
    };
    assert!(declared.check_value(&json!(true)).is_ok());
    assert!(declared.check_value(&json!("on")).is_ok());
    let refused = declared
        .check_value(&json!(1))
        .expect_err("neither type takes a number");
    assert_eq!(refused.takes, "true or false, or text");
    assert_eq!(refused.example, json!(true));
}

/// A picture with a level and a label beside it: a Texture member beside two small ones, the
/// label holding a default.
fn a_labelled_metered_picture() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![
            a_member("picture", a_pin_type_named(BuiltInPinTypeName::Texture)),
            a_member("level", PinType::Float { exact_size: None }),
            BundleMember {
                default_value: Some(json!("unnamed")),
                ..a_member("label", PinType::String {})
            },
        ],
    }
}

/// A member `name` of `member_type`, meaning nothing and declaring no default.
fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.into(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

/// A bundle holding a Texture holds the values of its small members, its own example among
/// them: the picture, which only a link carries, is left out and refused when set, and a small
/// member declaring no default is still needed. A bundle of only heavy members holds none.
///
/// Mental-revert: judging the bundle wire-only as a whole refuses `{"level": 0.5}`, which a
/// Make Bundle beside a wired picture is typed.
#[test]
fn a_bundle_holding_a_texture_holds_the_values_of_its_small_members() {
    let bundle = a_labelled_metered_picture();

    assert!(bundle.holds_a_stored_value());
    assert!(bundle.check_value(&json!({ "level": 0.5 })).is_ok());
    assert!(
        bundle
            .check_value(&json!({ "level": 0.5, "label": "kitchen" }))
            .is_ok()
    );
    let setting_the_picture = bundle
        .check_value(&json!({ "level": 0.5, "picture": { "surface_id": 1 } }))
        .expect_err("the picture takes only a link");
    assert_eq!(
        setting_the_picture.takes,
        "an object of the members `level`, `label`, as `picture` takes only a link"
    );
    assert!(bundle.check_value(&json!({ "label": "kitchen" })).is_err());
    let refused = bundle
        .check_value(&json!({}))
        .expect_err("`level` declares no default");
    assert!(bundle.check_value(&refused.example).is_ok(), "{refused:?}");
    assert_eq!(
        bundle.value_json_schema(None).unwrap()["required"],
        json!(["level"])
    );

    let only_a_picture = PinType::Bundle {
        name: None,
        members: vec![a_member(
            "picture",
            a_pin_type_named(BuiltInPinTypeName::Texture),
        )],
    };
    assert!(!only_a_picture.holds_a_stored_value());
    let refused = only_a_picture
        .check_value(&json!({}))
        .expect_err("a bundle of only a picture holds no value");
    assert_eq!(refused.takes, "no value, only a link");
}
