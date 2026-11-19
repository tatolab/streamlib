// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A described port declaring its type another way than exactly: one the node cannot find is
//! refused at declaration, naming the port, as is a 64-bit Integer on a single pin.

use super::*;

fn requested() -> NodeClassImportPath {
    NodeClassImportPath::new("my_app.pass:Pass").expect("the test path names a class")
}

/// A described process node with `inputs` and `outputs`, each `(name, declared pin type)`.
fn described_with(
    inputs: &[(&str, serde_json::Value)],
    outputs: &[(&str, serde_json::Value)],
) -> serde_json::Value {
    let port = |(name, pin_type): &(&str, serde_json::Value)| {
        serde_json::json!({
            "name": name,
            "family": {"family": "records"},
            "description": "",
            "delivery_profile": "newest",
            "pin_declaration": {"pin_type": pin_type, "flow": "value"},
        })
    };
    serde_json::json!({
        "import_path": "my_app.pass:Pass",
        "short_name": "Pass",
        "description": "passes a value on",
        "kind": "process",
        "execution": {"mode": "reactive"},
        "scheduling_priority": "normal",
        "paced_by_real_time": false,
        "config_schema": {"type": "object"},
        "input_ports": inputs.iter().map(port).collect::<Vec<_>>(),
        "output_ports": outputs.iter().map(port).collect::<Vec<_>>(),
    })
}

fn read(described: &serde_json::Value) -> Result<PythonNodeDeclaration, String> {
    PythonNodeDeclaration::read_from_described_node_type(described, &requested())
}

fn refusal_of(described: &serde_json::Value) -> String {
    match read(described) {
        Ok(_) => panic!("the described node type was accepted; a refusal was expected"),
        Err(refusal) => refusal,
    }
}

#[test]
fn an_any_input_and_an_output_of_its_type_read() {
    let declaration = read(&described_with(
        &[("value", serde_json::json!({"kind": "any"}))],
        &[(
            "value",
            serde_json::json!({"kind": "same_as_input", "input_port_name": "value"}),
        )],
    ))
    .expect("an output takes the type of an input of any type");

    assert_eq!(
        declaration.descriptor.outputs[0]
            .declared_pin()
            .map(|declared| declared.pin_type),
        Some(DeclaredPinType::SameAsInput {
            input_port_name: "value".to_owned()
        })
    );
}

#[test]
fn an_output_following_an_input_the_node_does_not_declare_is_refused() {
    let refusal = refusal_of(&described_with(
        &[],
        &[(
            "value",
            serde_json::json!({"kind": "same_as_input", "input_port_name": "missing"}),
        )],
    ));

    assert!(
        refusal.contains("\"value\"") && refusal.contains("\"missing\""),
        "got {refusal}"
    );
}

#[test]
fn a_type_chosen_by_an_input_that_is_no_pin_type_choice_is_refused() {
    let refusal = refusal_of(&described_with(
        &[(
            "kind",
            serde_json::json!({"kind": "is", "pin_type": {"type": "string"}}),
        )],
        &[(
            "value",
            serde_json::json!({"kind": "set_by_input", "input_name": "kind"}),
        )],
    ));

    assert!(
        refusal.contains("\"value\"") && refusal.contains("pin type choice"),
        "got {refusal}"
    );
}

#[test]
fn an_output_of_any_type_is_refused() {
    let refusal = refusal_of(&described_with(
        &[],
        &[("value", serde_json::json!({"kind": "any"}))],
    ));

    assert!(
        refusal.contains("output port \"value\" declares any type"),
        "got {refusal}"
    );
}

#[test]
fn a_64_bit_integer_on_a_single_pin_is_refused_with_the_reason() {
    let refusal = refusal_of(&described_with(
        &[(
            "count",
            serde_json::json!({"kind": "is", "pin_type": {"type": "integer", "exact_size": "int64"}}),
        )],
        &[],
    ));

    assert!(
        refusal.contains("\"count\"") && refusal.contains("exact to 2^53"),
        "got {refusal}"
    );
}
