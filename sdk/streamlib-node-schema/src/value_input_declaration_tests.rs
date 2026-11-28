// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use crate::pin_type::{IntegerSize, NodeInputMode, PinType};
use crate::value_input_declaration::the_input_taking_no_wire;

#[test]
fn a_value_input_is_read_each_run_holding_its_default() {
    let pin_type = PinType::Integer {
        exact_size: Some(IntegerSize::UInt32),
    };
    let input =
        the_input_taking_no_wire("sample_rate", "The rate.", pin_type.clone(), json!(48_000));
    assert_eq!(input.name, "sample_rate");
    assert_eq!(input.input_mode, Some(NodeInputMode::HandedToItsNode));
    assert_eq!(input.pin_default, Some(json!(48_000)));
    assert_eq!(input.delivery_profile, None);
    let pin_declaration = input
        .pin_declaration
        .expect("an input taking no wire declares its pin");
    assert!(!pin_declaration.may_be_unset);
    let pin = pin_declaration
        .into_resolved()
        .expect("an input taking no wire states its pin type");
    assert_eq!(pin.pin_type, pin_type);
}

#[test]
fn a_value_input_whose_default_is_null_may_be_unset() {
    let input = the_input_taking_no_wire("pin", "A pin.", PinType::String {}, Value::Null);
    assert_eq!(input.pin_default, None);
    assert!(
        input
            .pin_declaration
            .expect("an input taking no wire declares its pin")
            .may_be_unset
    );
}
