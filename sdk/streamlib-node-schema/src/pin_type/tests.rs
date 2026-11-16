// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::IMAGE_TEXTURE_FORMAT_WIRE_NAMES;

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
        BuiltInPinTypeName::NodeReference => PinType::NodeReference {
            referenced_node_type: None,
            one_referrer_per_referenced_node: true,
        },
        BuiltInPinTypeName::PinTypeChoice => PinType::PinTypeChoice {
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
        assert!(!row.python_form.is_empty());
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
    assert_eq!(
        IMAGE_TEXTURE_FORMAT_WIRE_NAMES,
        TexturePixelFormat::EVERY_FORMAT.map(TexturePixelFormat::wire_name)
    );
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

#[test]
fn an_input_mode_is_tagged_and_a_setting_is_read_at_setup_unless_it_says_otherwise() {
    let setting: NodeInputMode =
        serde_json::from_value(json!({ "mode": "setting_only" })).expect("reads");
    assert_eq!(
        setting,
        NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup
        }
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

#[test]
fn a_pin_annotation_reuses_the_port_stream_fact_forms() {
    let annotations: PinAnnotations = serde_json::from_value(json!({
        "frame_rate": { "stated": { "numerator": 30000, "denominator": 1001 } },
        "sample_rate": { "from_config": { "config_key": "rate" } },
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
        Some(crate::ChannelsOutputPortSampleRate::FromConfig {
            config_key: "rate".into()
        })
    );
}
