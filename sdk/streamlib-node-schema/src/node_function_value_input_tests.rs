// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use crate::node_function_value_input::the_value_input_of_a_node_function;
use crate::pin_type::{IntegerSize, NodeInputMode, PinType, SettingReadTime};

#[test]
fn a_value_input_is_read_each_run_holding_its_default() {
    let input = the_value_input_of_a_node_function::<u32>("sample_rate", "The rate.", &48_000);
    assert_eq!(input.name, "sample_rate");
    assert_eq!(
        input.input_mode,
        Some(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun
        })
    );
    assert_eq!(input.pin_default, Some(json!(48_000)));
    assert_eq!(input.delivery_profile, None);
    let pin_declaration = input
        .pin_declaration
        .expect("a value input declares its pin");
    assert!(!pin_declaration.may_be_unset);
    let pin = pin_declaration
        .into_resolved()
        .expect("a value input states its pin type");
    assert_eq!(
        pin.pin_type,
        PinType::Integer {
            exact_size: Some(IntegerSize::UInt32)
        }
    );
}

#[test]
fn a_value_input_whose_default_is_none_may_be_unset() {
    let input = the_value_input_of_a_node_function::<Option<PinType>>("pin", "A pin.", &None);
    assert_eq!(input.pin_default, None);
    assert!(
        input
            .pin_declaration
            .expect("a value input declares its pin")
            .may_be_unset
    );
}
