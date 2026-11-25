// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{
    BundleMember, DeclaredPinType, FloatSize, IntegerSize, PinAnnotations, PinDeclaration, PinFlow,
    PinNumberRange, PinType, VectorComponentCount,
};

fn a_declaration_of(pin_type: PinType, range: Option<PinNumberRange>) -> PinDeclaration {
    PinDeclaration {
        annotations: PinAnnotations {
            range,
            ..PinAnnotations::default()
        },
        ..PinDeclaration::exactly(pin_type)
    }
}

#[test]
fn an_integer_is_held_within_both_its_exact_size_and_its_range() {
    let declaration = a_declaration_of(
        PinType::Integer {
            exact_size: Some(IntegerSize::UInt8),
        },
        Some(PinNumberRange {
            minimum: Some(-10.0),
            maximum: Some(100.0),
        }),
    );

    assert_eq!(
        declaration.value_json_schema(),
        Some(json!({ "type": "integer", "minimum": 0, "maximum": 100 }))
    );
}

/// An Int64's bounds are its exact limits, which a float rounds past: `i64::MAX` as a float is
/// 2^63, one more than the largest Int64.
#[test]
fn an_int64_is_held_within_its_exact_limits() {
    let declaration = a_declaration_of(
        PinType::Integer {
            exact_size: Some(IntegerSize::Int64),
        },
        None,
    );

    assert_eq!(
        declaration.value_json_schema(),
        Some(json!({ "type": "integer", "minimum": i64::MIN, "maximum": i64::MAX }))
    );
    assert_eq!(
        a_declaration_of(
            PinType::Integer {
                exact_size: Some(IntegerSize::UInt64),
            },
            None,
        )
        .value_json_schema(),
        Some(json!({ "type": "integer", "minimum": 0, "maximum": u64::MAX }))
    );
}

#[test]
fn a_float_folds_in_its_range_and_a_32_bit_float_its_size() {
    let ranged = a_declaration_of(
        PinType::Float { exact_size: None },
        Some(PinNumberRange {
            minimum: Some(0.0),
            maximum: Some(1.5),
        }),
    );
    let unranged_float32 = a_declaration_of(
        PinType::Float {
            exact_size: Some(FloatSize::Float32),
        },
        None,
    );

    assert_eq!(
        ranged.value_json_schema(),
        Some(json!({ "type": "number", "minimum": 0, "maximum": 1.5 }))
    );
    let float32 = unranged_float32
        .value_json_schema()
        .expect("a Float holds a value");
    assert_eq!(float32["maximum"], json!(f64::from(f32::MAX)));
    assert_eq!(float32["minimum"], json!(-f64::from(f32::MAX)));
}

#[test]
fn a_menu_takes_one_of_its_options_and_a_vector_exactly_its_keys() {
    assert_eq!(
        PinType::Menu {
            option_names: vec!["soft".to_owned(), "hard".to_owned()],
        }
        .value_json_schema(None),
        Some(json!({ "type": "string", "enum": ["soft", "hard"] }))
    );
    assert_eq!(
        PinType::Vector {
            component_count: VectorComponentCount::Three,
        }
        .value_json_schema(None),
        Some(json!({
            "type": "object",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" },
                "z": { "type": "number" },
                "w": { "type": "null" },
            },
            "required": ["x", "y", "z"],
            "additionalProperties": false,
        }))
    );
}

#[test]
fn a_list_states_its_element_and_a_bundle_requires_each_member_with_no_default() {
    assert_eq!(
        PinType::ListOf {
            element_type: Box::new(PinType::Boolean {}),
        }
        .value_json_schema(None),
        Some(json!({ "type": "array", "items": { "type": "boolean" } }))
    );
    assert_eq!(
        PinType::Bundle {
            name: None,
            members: vec![
                BundleMember {
                    name: "gain".to_owned(),
                    member_type: PinType::Float { exact_size: None },
                    meaning: None,
                    default_value: Some(json!(1.0)),
                },
                BundleMember {
                    name: "label".to_owned(),
                    member_type: PinType::String {},
                    meaning: None,
                    default_value: None,
                },
            ],
        }
        .value_json_schema(None),
        Some(json!({
            "type": "object",
            "properties": {
                "gain": {
                    "type": "number",
                    "minimum": -f64::MAX,
                    "maximum": f64::MAX,
                    "default": 1.0,
                },
                "label": { "type": "string" },
            },
            "required": ["label"],
            "additionalProperties": false,
        }))
    );
}

#[test]
fn an_input_that_may_be_unset_takes_null_beside_its_type() {
    let declaration = PinDeclaration {
        may_be_unset: true,
        ..PinDeclaration::exactly(PinType::String {})
    };

    assert_eq!(
        declaration.value_json_schema(),
        Some(json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }))
    );
}

#[test]
fn a_type_that_holds_no_value_and_a_type_found_with_the_graph_state_no_schema() {
    let texture = PinDeclaration::exactly(PinType::Texture {
        pixel_formats: Vec::new(),
    });
    let found_with_the_graph = PinDeclaration {
        pin_type: DeclaredPinType::Any,
        flow: PinFlow::Value,
        annotations: PinAnnotations::default(),
        former_names: Vec::new(),
        may_be_unset: false,
    };

    assert_eq!(texture.value_json_schema(), None);
    assert_eq!(found_with_the_graph.value_json_schema(), None);
    assert_eq!(PinType::Trigger {}.value_json_schema(None), None);
}

#[test]
fn each_type_that_holds_a_stored_value_states_a_schema_and_each_other_states_none() {
    let every_kind = [
        PinType::Boolean {},
        PinType::Integer { exact_size: None },
        PinType::Float { exact_size: None },
        PinType::String {},
        PinType::Menu {
            option_names: vec!["one".to_owned()],
        },
        PinType::Color {},
        PinType::Vector {
            component_count: VectorComponentCount::Two,
        },
        PinType::Rectangle {},
        PinType::Transform {},
        PinType::Texture {
            pixel_formats: Vec::new(),
        },
        PinType::Audio {},
        PinType::EncodedVideo { codecs: Vec::new() },
        PinType::EncodedAudio { codecs: Vec::new() },
        PinType::Tensor {},
        PinType::Asset { media: Vec::new() },
        PinType::Trigger {},
        PinType::ListOf {
            element_type: Box::new(PinType::String {}),
        },
        PinType::Bundle {
            name: None,
            members: Vec::new(),
        },
        PinType::NodeReference {
            referenced_node_type: None,
            one_referrer_per_referenced_node: false,
        },
        PinType::a_choice_of_any(),
    ];

    for pin_type in every_kind {
        assert_eq!(
            pin_type.value_json_schema(None).is_some(),
            pin_type.holds_a_stored_value(),
            "{pin_type:?}"
        );
    }
}

#[test]
fn each_built_in_types_value_schema_reads_on_its_own_with_every_reference_defined_in_it() {
    use super::BuiltInPinTypeName;

    fn every_reference_in(schema: &serde_json::Value, references: &mut Vec<String>) {
        match schema {
            serde_json::Value::Object(keywords) => {
                for (keyword, stated) in keywords {
                    match (keyword.as_str(), stated) {
                        ("$ref", serde_json::Value::String(reference)) => {
                            references.push(reference.clone())
                        }
                        _ => every_reference_in(stated, references),
                    }
                }
            }
            serde_json::Value::Array(schemas) => {
                for schema in schemas {
                    every_reference_in(schema, references);
                }
            }
            _ => {}
        }
    }

    for name in BuiltInPinTypeName::EVERY_NAME {
        let Some(schema) = name.value_json_schema_on_its_own() else {
            continue;
        };
        let mut references = Vec::new();
        every_reference_in(&schema, &mut references);
        for reference in references {
            let defined = reference
                .strip_prefix("#/$defs/")
                .is_some_and(|definition| schema["$defs"].get(definition).is_some());
            assert!(
                defined,
                "{name:?}'s schema names {reference}, which it does not define"
            );
        }
    }
}

/// A Vector of two components takes `z` and `w` as null, as its stored value check reads them,
/// and a Vector of three takes `w` as null.
#[test]
fn a_vectors_unused_components_may_be_null_as_its_check_reads_them() {
    let two = PinType::Vector {
        component_count: VectorComponentCount::Two,
    };
    let schema = two.value_json_schema(None).expect("a Vector holds a value");

    assert!(
        two.check_value(&json!({ "x": 1, "y": 2, "z": null, "w": null }))
            .is_ok()
    );
    assert_eq!(schema["properties"]["z"], json!({ "type": "null" }));
    assert_eq!(schema["properties"]["w"], json!({ "type": "null" }));
    assert_eq!(schema["required"], json!(["x", "y"]));
}

/// A pin type choice among types takes each whole, as the contract writes it, not its tag alone;
/// one among any takes a built-in tag alone, and one among the bundles its stream declares takes
/// the bundle tag alone, listed or not, since the runtime matches a bundle to one it lists.
#[test]
fn a_pin_type_choice_takes_each_type_it_names_whole() {
    let among = PinType::PinTypeChoice {
        among_the_bundles_its_stream_declares: false,
        among: vec![
            PinType::Integer {
                exact_size: Some(IntegerSize::UInt8),
            },
            PinType::Boolean {},
        ],
    };
    let any = PinType::a_choice_of_any();

    assert_eq!(
        among.value_json_schema(None),
        Some(json!({ "enum": [
            { "type": "integer", "exact_size": "uint8" },
            { "type": "boolean" },
        ] }))
    );
    let any_schema = any.value_json_schema(None).expect("a choice holds a value");
    let tags = any_schema["properties"]["type"]["enum"]
        .as_array()
        .expect("a choice among any names each built-in tag");
    assert!(tags.contains(&json!("float")));
    assert!(!tags.contains(&json!("bogus")));

    let listed = PinType::PinTypeChoice {
        among: vec![PinType::Bundle {
            name: Some("Reading".into()),
            members: Vec::new(),
        }],
        among_the_bundles_its_stream_declares: true,
    };
    for among_the_bundles in [
        PinType::a_choice_among_the_bundles_its_stream_declares(),
        listed,
    ] {
        assert_eq!(
            among_the_bundles.value_json_schema(None).expect("a choice")["properties"]["type"]["enum"],
            json!(["bundle"]),
            "{among_the_bundles:?}"
        );
    }
}
