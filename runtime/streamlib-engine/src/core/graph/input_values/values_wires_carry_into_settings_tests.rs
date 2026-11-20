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

/// A value that arrived as its node's rebuild was being built, and which the node was built
/// with in place of the one taken, is the one held as refused when the rebuild is refused.
///
/// Mental-revert: holding the value taken as refused leaves 9, the value the node refused, to
/// be tried again, and the value taken, 4, never tried.
#[test]
fn a_rebuild_refused_holds_the_value_its_node_was_built_with_as_refused() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.built_with_unless_its_wire_carried_one(node, "kernel_size", json!(3));
    carried.carried(node, "kernel_size", json!(4));
    let taken = carried.take_each_value_waiting();
    carried.carried(node, "kernel_size", json!(9));
    carried.build_each_of_the_nodes_with_what_waits(&[node]);
    carried.put_back(&taken);

    assert_eq!(
        carried.the_value_its_node_is_built_with(node, "kernel_size"),
        Some(json!(3))
    );
    carried.carried(node, "kernel_size", json!(9));
    assert!(
        carried.take_each_value_waiting().is_empty(),
        "the value its node was refused with is not tried again"
    );
    carried.carried(node, "kernel_size", json!(4));
    assert_eq!(carried.take_each_value_waiting()[0].value, json!(4));
}

#[test]
fn a_node_forgotten_holds_nothing() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    let another = Uuid::new_v4();
    carried.carried(node, "kernel_size", json!(5));
    carried.carried(another, "kernel_size", json!(6));
    carried.forget_the_node(node);

    let waiting = carried.take_each_value_waiting();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].node, another);
    assert_eq!(
        carried.the_value_its_node_is_built_with(node, "kernel_size"),
        None
    );
}

/// Mental-revert: forgetting the setting alone as its wire leaves leaves its node built with
/// the wire's value.
#[test]
fn a_wire_leaving_a_setting_its_node_was_built_with_from_it_waits_with_the_settings_own_value() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.built_with_unless_its_wire_carried_one(node, "kernel_size", json!(3));
    carried.carried(node, "kernel_size", json!(5));
    carried.take_each_value_waiting();

    carried.its_wire_left(node, "kernel_size", Some(json!(3)));
    let waiting = carried.take_each_value_waiting();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0].value, json!(3));

    carried.its_wire_left(node, "kernel_size", Some(json!(3)));
    assert!(
        carried.take_each_value_waiting().is_empty(),
        "a node built with the setting's own value rebuilds for nothing"
    );
}

/// Mental-revert: leaving the values waiting rebuilds a node right after it is built with them.
#[test]
fn a_value_waiting_for_a_node_about_to_be_built_is_what_it_is_built_with() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let built = Uuid::new_v4();
    let running = Uuid::new_v4();
    carried.carried(built, "kernel_size", json!(5));
    carried.carried(running, "kernel_size", json!(6));

    carried.build_each_of_the_nodes_with_what_waits(&[built]);
    assert_eq!(
        carried.the_value_its_node_is_built_with(built, "kernel_size"),
        Some(json!(5))
    );
    let waiting = carried.take_each_value_waiting();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].node, running);
}
