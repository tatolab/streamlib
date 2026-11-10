// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

/// The JSON Schema the view SDK writes from its Zod schema, recorded from tatolab-app's
/// `packages/tato-app-view-sdk/tatolab_view.schema.json`; a change there is recorded here again.
const VIEW_SDK_TATOLAB_VIEW_JSON_SCHEMA: &str =
    include_str!("../view-contract/tatolab_view.schema.json");

/// What a JSON Schema says of a value's shape: its fields, which are required, whether others
/// are refused, and the names an enum allows. Patterns, bounds and defaults are left out.
#[derive(Debug, PartialEq)]
enum JsonSchemaShape {
    AnyValue,
    Scalar(&'static str),
    Enumeration(Vec<String>),
    Array(Box<JsonSchemaShape>),
    Object {
        properties: BTreeMap<String, JsonSchemaShape>,
        required: Vec<String>,
        closed: bool,
    },
}

fn the_schema_a_reference_names<'root>(root: &'root Value, reference: &str) -> &'root Value {
    let pointer = reference
        .strip_prefix('#')
        .expect("a reference inside the document");
    root.pointer(pointer)
        .unwrap_or_else(|| panic!("the schema holds {reference}"))
}

fn the_shape_of(schema: &Value, root: &Value) -> JsonSchemaShape {
    let Some(schema_object) = schema.as_object() else {
        return JsonSchemaShape::AnyValue;
    };
    if let Some(reference) = schema_object.get("$ref").and_then(Value::as_str) {
        return the_shape_of(the_schema_a_reference_names(root, reference), root);
    }
    for alternatives_key in ["allOf", "anyOf", "oneOf"] {
        if let Some(alternatives) = schema_object
            .get(alternatives_key)
            .and_then(Value::as_array)
        {
            let not_null: Vec<&Value> = alternatives
                .iter()
                .filter(|alternative| alternative.get("type") != Some(&json!("null")))
                .collect();
            if let [only] = not_null.as_slice() {
                return the_shape_of(only, root);
            }
            let alternative_shapes: Vec<JsonSchemaShape> = not_null
                .iter()
                .map(|alternative| the_shape_of(alternative, root))
                .collect();
            if alternative_shapes
                .iter()
                .all(|shape| matches!(shape, JsonSchemaShape::Enumeration(_)))
            {
                let mut names: Vec<String> = alternative_shapes
                    .into_iter()
                    .flat_map(|shape| match shape {
                        JsonSchemaShape::Enumeration(names) => names,
                        _ => Vec::new(),
                    })
                    .collect();
                names.sort();
                return JsonSchemaShape::Enumeration(names);
            }
        }
    }
    if let Some(allowed) = schema_object.get("enum").and_then(Value::as_array) {
        let mut names: Vec<String> = allowed
            .iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect();
        names.sort();
        return JsonSchemaShape::Enumeration(names);
    }
    if let Some(constant) = schema_object.get("const").and_then(Value::as_str) {
        return JsonSchemaShape::Enumeration(vec![constant.to_owned()]);
    }
    let type_name = match schema_object.get("type") {
        Some(Value::String(type_name)) => type_name.as_str(),
        Some(Value::Array(type_names)) => type_names
            .iter()
            .filter_map(Value::as_str)
            .find(|type_name| *type_name != "null")
            .unwrap_or("null"),
        _ => return JsonSchemaShape::AnyValue,
    };
    match type_name {
        "object" => {
            let Some(properties) = schema_object.get("properties").and_then(Value::as_object)
            else {
                return JsonSchemaShape::Scalar("object");
            };
            let mut required: Vec<String> = schema_object
                .get("required")
                .and_then(Value::as_array)
                .map(|required| {
                    required
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            required.sort();
            JsonSchemaShape::Object {
                properties: properties
                    .iter()
                    .map(|(name, property)| (name.clone(), the_shape_of(property, root)))
                    .collect(),
                required,
                closed: schema_object.get("additionalProperties") == Some(&json!(false)),
            }
        }
        "array" => {
            let items = schema_object
                .get("prefixItems")
                .and_then(|prefix_items| prefix_items.get(0))
                .or_else(|| schema_object.get("items"))
                .unwrap_or(&Value::Bool(true));
            JsonSchemaShape::Array(Box::new(the_shape_of(items, root)))
        }
        "string" => JsonSchemaShape::Scalar("string"),
        "number" | "integer" => JsonSchemaShape::Scalar("number"),
        "boolean" => JsonSchemaShape::Scalar("boolean"),
        _ => JsonSchemaShape::AnyValue,
    }
}

/// The fields the runtime reads as any JSON value and checks by hand.
const FIELDS_THE_RUNTIME_READS_AS_ANY_JSON_VALUE: [&str; 1] = ["tatolab_view.json.settings"];

/// Every place the runtime's shape differs from the view SDK's, by field path.
fn every_drift_between(
    runtime_shape: &JsonSchemaShape,
    view_sdk_shape: &JsonSchemaShape,
    field_path: &str,
    drifts: &mut Vec<String>,
) {
    match (runtime_shape, view_sdk_shape) {
        (JsonSchemaShape::AnyValue, _)
            if FIELDS_THE_RUNTIME_READS_AS_ANY_JSON_VALUE.contains(&field_path) => {}
        (JsonSchemaShape::Array(runtime_items), JsonSchemaShape::Array(view_sdk_items)) => {
            every_drift_between(
                runtime_items,
                view_sdk_items,
                &format!("{field_path}[]"),
                drifts,
            )
        }
        (
            JsonSchemaShape::Object {
                properties: runtime_properties,
                required: runtime_required,
                closed: runtime_closed,
            },
            JsonSchemaShape::Object {
                properties: view_sdk_properties,
                required: view_sdk_required,
                closed: view_sdk_closed,
            },
        ) => {
            if runtime_required != view_sdk_required {
                drifts.push(format!(
                    "{field_path}: the runtime requires {runtime_required:?}, the view SDK {view_sdk_required:?}"
                ));
            }
            if runtime_closed != view_sdk_closed {
                drifts.push(format!(
                    "{field_path}: the runtime refuses other fields: {runtime_closed}, the view SDK: {view_sdk_closed}"
                ));
            }
            for name in runtime_properties
                .keys()
                .chain(view_sdk_properties.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let property_path = format!("{field_path}.{name}");
                match (runtime_properties.get(name), view_sdk_properties.get(name)) {
                    (Some(runtime_property), Some(view_sdk_property)) => every_drift_between(
                        runtime_property,
                        view_sdk_property,
                        &property_path,
                        drifts,
                    ),
                    (Some(_), None) => drifts.push(format!(
                        "{property_path}: the runtime reads it, the view SDK has no such field"
                    )),
                    (None, _) => drifts.push(format!(
                        "{property_path}: the view SDK declares it, the runtime does not read it"
                    )),
                }
            }
        }
        _ if runtime_shape == view_sdk_shape => {}
        _ => drifts.push(format!(
            "{field_path}: the runtime reads {runtime_shape:?}, the view SDK declares {view_sdk_shape:?}"
        )),
    }
}

#[test]
fn the_runtimes_manifest_type_has_the_shape_the_view_sdk_declares() {
    let view_sdk_schema: Value =
        serde_json::from_str(VIEW_SDK_TATOLAB_VIEW_JSON_SCHEMA).expect("the recorded schema");
    let runtime_schema =
        serde_json::to_value(schemars::schema_for!(TatolabViewManifest)).expect("its schema");
    let mut drifts = Vec::new();
    every_drift_between(
        &the_shape_of(&runtime_schema, &runtime_schema),
        &the_shape_of(&view_sdk_schema, &view_sdk_schema),
        "tatolab_view.json",
        &mut drifts,
    );
    assert!(drifts.is_empty(), "{drifts:#?}");
}

fn a_node_view_manifest() -> Value {
    json!({
        "contract": "1.0",
        "title": "DJ Deck",
        "icon": "icon.svg",
        "entry": "index.html",
        "placement": "node",
        "inputs": [{ "name": "waveform", "family": "channels" }],
        "outputs": [{ "name": "fader", "family": "array", "range": [0, 1] }],
        "capabilities": ["gpu", "gpu"],
    })
}

#[test]
fn a_node_view_manifest_reads_with_its_defaults_filled_in() {
    let manifest = read_tatolab_view_manifest(&a_node_view_manifest()).expect("it reads");

    assert_eq!(manifest.title, "DJ Deck");
    assert_eq!(manifest.placement, TatolabViewPlacement::Node);
    assert_eq!(manifest.inputs[0].family, PortFamily::Channels);
    assert_eq!(manifest.outputs[0].range, Some([0.0, 1.0]));
    assert_eq!(manifest.capabilities, vec![TatolabViewCapability::Gpu]);
    assert!(manifest.accepts.files.is_empty());
    assert!(!manifest.sandbox);
}

#[test]
fn a_field_the_contract_does_not_know_is_refused_naming_it() {
    let mut manifest = a_node_view_manifest();
    manifest["id"] = json!("acme-dj:deck");

    let refusal = read_tatolab_view_manifest(&manifest).expect_err("an id is never written");

    assert!(refusal.contains("unknown field `id`"), "{refusal}");
}

#[test]
fn each_way_a_manifest_falls_short_is_named_by_its_field() {
    let refusal = read_tatolab_view_manifest(&json!({
        "contract": "2.0",
        "title": "  ",
        "entry": "../escape.html",
        "placement": "area",
        "outputs": [
            { "name": "Fader", "family": "records", "range": [1, 0] },
            { "name": "Fader", "family": "image" },
        ],
        "network": ["https://example.com"],
    }))
    .expect_err("nothing about it opens");

    for expected in [
        "`contract`: this runtime reads view contract 1.0.",
        "`title`: it is the name the view-type menu lists.",
        "`entry`: it is a path inside the view's folder",
        "`outputs[0].name`: a port name is lower-case letters, digits and underscores.",
        "`outputs[1].name`: `Fader` is named twice.",
        "`outputs[0].range`: a range is [lowest, highest], lowest first.",
        "`outputs[0].range`: only an Array port carries a range.",
        "`network[0]`: a host is a lower-case name",
        "`placement`: an `area` view has no node",
    ] {
        assert!(refusal.contains(expected), "{expected} in {refusal}");
    }
}

#[test]
fn a_settings_declaration_whose_setting_gives_no_default_does_not_open() {
    let mut manifest = a_node_view_manifest();
    manifest["settings"] = json!({
        "type": "object",
        "properties": { "gain": { "type": "number" } },
    });

    let refusal = read_tatolab_view_manifest(&manifest).expect_err("a setting needs a default");

    assert_eq!(
        refusal,
        "`settings.properties.gain.default`: every setting gives its `default`."
    );
}

#[test]
fn a_path_inside_the_views_folder_has_no_scheme_root_or_parent_step() {
    for inside in ["index.html", "assets/icon.svg", "a..b/c"] {
        assert!(is_a_path_inside_the_views_folder(inside), "{inside}");
    }
    for outside in [
        "",
        "/index.html",
        "../index.html",
        "a/./b",
        "a//b",
        "a\\b",
        "https://example.com/x",
        "data:text/html,x",
    ] {
        assert!(!is_a_path_inside_the_views_folder(outside), "{outside}");
    }
}

#[test]
fn a_network_host_is_a_lower_case_name_with_an_optional_port() {
    for host in ["example.com", "localhost:8080", "a-b.c0"] {
        assert!(is_a_network_host(host), "{host}");
    }
    for not_a_host in [
        "Example.com",
        "*.example.com",
        "https://x",
        "-a.com",
        "a.com:",
        "a.com:123456",
        "",
    ] {
        assert!(!is_a_network_host(not_a_host), "{not_a_host}");
    }
}
