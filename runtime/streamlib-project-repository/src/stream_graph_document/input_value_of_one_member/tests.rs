// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn held(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

#[test]
fn a_member_set_changes_that_member_alone() {
    let before = held(json!({ "reading": { "level": 0.5, "label": "kitchen" }, "gain": 2 }));
    let after = input_values_after_the_set(&before, "reading", Some("level"), Some(&json!(1)));
    assert_eq!(
        Value::Object(after),
        json!({ "reading": { "level": 1, "label": "kitchen" }, "gain": 2 })
    );
}

#[test]
fn the_last_member_taken_off_takes_the_inputs_value_off() {
    let before = held(json!({ "reading": { "level": 0.5 } }));
    let after = input_values_after_the_set(&before, "reading", Some("level"), None);
    assert_eq!(Value::Object(after), json!({}));
}

#[test]
fn a_member_set_on_an_input_holding_no_value_starts_its_object() {
    let after =
        input_values_after_the_set(&Map::new(), "reading", Some("label"), Some(&json!("hall")));
    assert_eq!(
        Value::Object(after.clone()),
        json!({ "reading": { "label": "hall" } })
    );
    assert_eq!(
        the_input_value_held_on(&after, "reading", Some("label")),
        Some(&json!("hall"))
    );
    assert_eq!(
        the_input_value_held_on(&after, "reading", Some("level")),
        None
    );
}
