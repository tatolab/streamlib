// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

const STREAM: &str = "860fdbd6-8fe2-592b-8ebc-faae672abefa";
const NODE: &str = "6b6b6b6b-6b6b-4b6b-8b6b-6b6b6b6b6b6b";

/// `transaction` upgraded, `None` when it already holds only current forms.
fn upgraded(transaction: Value) -> Option<Value> {
    let mut upgraded = transaction;
    upgrade_recorded_transaction_in_place(&mut upgraded).then_some(upgraded)
}

fn set_node_input_value(input: &str, from: Value, to: Value) -> Value {
    json!({ "stream": STREAM, "operation": "set_node_input_value", "node": NODE,
            "input": input, "from": from, "to": to })
}

#[test]
fn a_set_pin_value_reads_as_a_set_of_the_one_input() {
    let transaction = json!({
        "operations": [{ "stream": STREAM, "operation": "set_pin_value", "node": NODE,
                         "input": "gain", "from": 0.75, "to": 0.9 }],
        "inverse_operations": [{ "stream": STREAM, "operation": "set_pin_value", "node": NODE,
                                 "input": "gain", "from": 0.9, "to": 0.75 }],
        "actions": [{ "operation": "set_pin_value", "stream": STREAM, "node": NODE,
                      "input": "gain", "value": 0.9 }],
    });
    assert_eq!(
        upgraded(transaction),
        Some(json!({
            "operations": [set_node_input_value("gain", json!(0.75), json!(0.9))],
            "inverse_operations": [set_node_input_value("gain", json!(0.9), json!(0.75))],
            "actions": [{ "operation": "set_node_input_value", "stream": STREAM, "node": NODE,
                          "input": "gain", "value": 0.9 }],
        }))
    );
}

#[test]
fn a_set_node_config_reads_as_one_set_for_each_name_that_differs() {
    let transaction = json!({
        "operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                         "from": { "frame_width": 640, "gain": 0.25, "mode": "soft" },
                         "to": { "frame_width": 1280, "gain": 0.25, "level": 3 } }],
        "inverse_operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                                 "from": { "frame_width": 1280, "gain": 0.25, "level": 3 },
                                 "to": { "frame_width": 640, "gain": 0.25, "mode": "soft" } }],
    });
    assert_eq!(
        upgraded(transaction),
        Some(json!({
            "operations": [
                set_node_input_value("frame_width", json!(640), json!(1280)),
                set_node_input_value("level", Value::Null, json!(3)),
                set_node_input_value("mode", json!("soft"), Value::Null),
            ],
            "inverse_operations": [
                set_node_input_value("frame_width", json!(1280), json!(640)),
                set_node_input_value("mode", Value::Null, json!("soft")),
                set_node_input_value("level", json!(3), Value::Null),
            ],
        }))
    );
}

#[test]
fn a_set_node_config_that_changed_nothing_reads_as_sets_to_the_values_held() {
    let transaction = json!({
        "operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                         "from": { "gain": 0.25 }, "to": { "gain": 0.25 } }],
    });
    assert_eq!(
        upgraded(transaction),
        Some(json!({
            "operations": [set_node_input_value("gain", json!(0.25), json!(0.25))],
        }))
    );
}

#[test]
fn a_set_node_config_that_names_no_value_stays_as_written() {
    let transaction = json!({
        "operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                         "from": {}, "to": {} }],
        "inverse_operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                                 "from": null, "to": {} }],
    });
    assert_eq!(upgraded(transaction), None);
}

#[test]
fn a_set_node_config_action_and_a_config_that_is_no_object_stay_as_written() {
    let transaction = json!({
        "operations": [{ "stream": STREAM, "operation": "set_node_config", "node": NODE,
                         "from": 3, "to": { "gain": 0.25 } }],
        "actions": [{ "operation": "set_node_config", "stream": STREAM, "node": NODE,
                      "config": { "gain": 0.25 } }],
    });
    assert_eq!(upgraded(transaction), None);
}

#[test]
fn a_transaction_in_current_forms_needs_no_upgrade() {
    let transaction = json!({
        "operations": [
            set_node_input_value("gain", json!(0.75), json!(0.9)),
            { "stream": STREAM, "operation": "rename_node", "node": NODE,
              "from": "mixer", "to": "mix" },
            { "operation": "set_project_name", "from": "a", "to": "b" },
        ],
        "inverse_operations": [],
    });
    assert_eq!(upgraded(transaction), None);
    assert_eq!(upgraded(json!("no object")), None);
}
