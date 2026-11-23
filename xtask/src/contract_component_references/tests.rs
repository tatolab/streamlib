// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn component_schemas(schemas: Value) -> Map<String, Value> {
    schemas
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the schemas are an object"))
}

#[test]
fn a_reference_inside_a_const_or_a_default_names_nothing() {
    let schema = json!({
        "properties": {
            "chosen": { "$ref": "#/components/schemas/PinType" },
            "kept": { "const": { "$ref": "#/components/schemas/NotNamed" } },
            "given": {
                "default": [{ "$ref": "#/components/schemas/NotNamedEither" }],
                "anyOf": [{ "$ref": "#/components/schemas/PinFlow" }, { "$ref": "#/$defs/Local" }]
            }
        }
    });
    assert_eq!(
        component_names_a_schema_names(&schema),
        vec!["PinType", "PinFlow"]
    );
}

#[test]
fn every_component_a_root_reaches_is_listed_once_in_the_contracts_order() {
    let schemas = component_schemas(json!({
        "Unreached": { "$ref": "#/components/schemas/Leaf" },
        "Leaf": { "type": "string" },
        "Middle": { "items": { "$ref": "#/components/schemas/Leaf" } },
        "Root": {
            "properties": {
                "middle": { "$ref": "#/components/schemas/Middle" },
                "again": { "$ref": "#/components/schemas/Root" }
            }
        },
        "OtherRoot": { "$ref": "#/components/schemas/Leaf" }
    }));
    let reached = component_names_reached_from(&schemas, &["Root", "OtherRoot"])
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(reached, vec!["Leaf", "Middle", "Root", "OtherRoot"]);
}

#[test]
fn a_root_or_a_reference_the_contract_has_no_schema_for_is_refused() {
    let schemas = component_schemas(json!({
        "Root": { "$ref": "#/components/schemas/Missing" }
    }));
    let refusal = component_names_reached_from(&schemas, &["Absent"])
        .err()
        .unwrap_or_else(|| panic!("an absent root is refused"))
        .to_string();
    assert!(refusal.contains("`Absent`"), "{refusal}");
    let refusal = component_names_reached_from(&schemas, &["Root"])
        .err()
        .unwrap_or_else(|| panic!("a reference to an absent schema is refused"))
        .to_string();
    assert!(refusal.contains("`Root` names `Missing`"), "{refusal}");
}

#[test]
fn a_closed_set_and_a_tagged_union_read_their_values_in_order() {
    let closed_set = json!({ "oneOf": [{ "const": "value" }, { "const": "stream" }] });
    assert_eq!(
        string_values_of_a_closed_set(&closed_set).unwrap(),
        ["value", "stream"]
    );
    let tagged_union = json!({ "oneOf": [
        { "properties": { "mode": { "const": "wire_only" } } },
        { "properties": { "mode": { "const": "setting_only" }, "read": {} } }
    ] });
    assert_eq!(
        tags_of_a_tagged_union(&tagged_union, "mode").unwrap(),
        ["wire_only", "setting_only"]
    );
    assert!(string_values_of_a_closed_set(&tagged_union).is_err());
    assert!(tags_of_a_tagged_union(&closed_set, "mode").is_err());
}

#[test]
fn a_reference_names_its_component_and_an_object_component_lists_its_keys() {
    assert_eq!(
        component_name_of_a_reference(&json!({ "$ref": "#/components/schemas/PinType" })),
        Some("PinType")
    );
    assert_eq!(
        component_name_of_a_reference(&json!({ "$ref": "#/$defs/PinType" })),
        None
    );
    let schemas = component_schemas(json!({
        "PinAnnotations": { "type": "object", "properties": { "range": {}, "unit": {} } },
        "PinFlow": { "type": "string" }
    }));
    assert_eq!(
        property_names_of(&schemas, "PinAnnotations").unwrap(),
        ["range", "unit"]
    );
    assert!(property_names_of(&schemas, "PinFlow").is_err());
}

#[test]
fn a_union_whose_alternatives_state_no_type_only_narrows_the_type_around_it() {
    let narrowing = json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/BundleMember" },
        "anyOf": [{ "items": { "properties": {} } }, { "items": { "properties": {} } }],
    });
    assert_eq!(the_union_a_schema_is(&narrowing), None);
    let tagged = json!({ "oneOf": [{ "type": "object" }, { "const": "a" }] });
    assert_eq!(
        the_union_a_schema_is(&tagged).map(Vec::len),
        Some(2),
        "a union of types is the schema's type"
    );
}
