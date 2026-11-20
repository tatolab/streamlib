// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value a wire carries into a setting waits for its node's rebuild, a burst of them as its
//! last, and a value the node is built with, or did not build with, waits for nothing.

use serde_json::json;
use uuid::Uuid;

use super::{ValueAWireCarriedIntoASetting, ValuesWiresCarryIntoSettings};

#[test]
fn a_burst_of_values_on_one_setting_waits_as_its_last() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    for kernel_size in 4..=9 {
        carried.carried(node, "kernel_size", json!(kernel_size));
    }

    assert_eq!(
        carried.take_each_value_waiting(),
        vec![ValueAWireCarriedIntoASetting {
            node,
            setting: "kernel_size".to_owned(),
            value: json!(9),
            built_with_before: None,
        }]
    );
    assert_eq!(
        carried.the_value_its_node_is_built_with(node, "kernel_size"),
        Some(json!(9))
    );
    assert!(carried.take_each_value_waiting().is_empty());
}

#[test]
fn a_value_its_node_is_built_with_waits_for_nothing() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.built_with_unless_its_wire_carried_one(node, "kernel_size", json!(3));
    carried.carried(node, "kernel_size", json!(3));
    assert!(carried.take_each_value_waiting().is_empty());

    carried.carried(node, "kernel_size", json!(5));
    carried.carried(node, "kernel_size", json!(3));
    let waiting = carried.take_each_value_waiting();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0].value, json!(3));
    assert_eq!(waiting[0].built_with_before, Some(json!(3)));
}

#[test]
fn a_value_put_back_keeps_the_earlier_one_and_waits_for_nothing_until_another_arrives() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.built_with_unless_its_wire_carried_one(node, "kernel_size", json!(3));
    carried.carried(node, "kernel_size", json!(0));
    let refused = carried.take_each_value_waiting();
    carried.put_back(&refused);

    assert_eq!(
        carried.the_value_its_node_is_built_with(node, "kernel_size"),
        Some(json!(3))
    );
    carried.carried(node, "kernel_size", json!(0));
    assert!(
        carried.take_each_value_waiting().is_empty(),
        "a value its node did not build with is not tried again"
    );
    carried.carried(node, "kernel_size", json!(7));
    assert_eq!(carried.take_each_value_waiting()[0].value, json!(7));
}

#[test]
fn a_node_or_a_setting_forgotten_holds_nothing() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.carried(node, "kernel_size", json!(5));
    carried.carried(node, "width", json!(640));
    carried.forget_the_setting(node, "width");
    assert_eq!(carried.take_each_value_waiting().len(), 1);

    carried.forget_the_node(node);
    assert_eq!(
        carried.the_value_its_node_is_built_with(node, "kernel_size"),
        None
    );
}
