// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value a wire carries into a setting is what the setting reads, and leaves its node to be
//! handed it once, however many values a burst carries; a value the wire carried already leaves
//! nothing to hand; a wire taken off a setting it carried a value into leaves its node to be
//! handed the setting's own value.

use serde_json::json;
use uuid::Uuid;

use super::ValuesWiresCarryIntoSettings;

/// Mental-revert: leaving the node to be handed each value of a burst hands it six times.
#[test]
fn a_burst_of_values_on_one_setting_reads_as_its_last_and_leaves_its_node_to_hand_once() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    for kernel_size in 4..=9 {
        carried.carried(node, "kernel_size", json!(kernel_size));
    }

    assert_eq!(
        carried.the_value_its_wire_carried(node, "kernel_size"),
        Some(json!(9))
    );
    assert_eq!(
        carried.take_each_node_waiting_for_its_values(),
        std::collections::BTreeSet::from([node])
    );
    assert!(carried.take_each_node_waiting_for_its_values().is_empty());
}

/// Mental-revert: leaving the node to be handed every value, equal or not, hands it 5 again.
#[test]
fn a_value_its_wire_carried_already_leaves_nothing_to_hand() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.carried(node, "kernel_size", json!(5));
    carried.take_each_node_waiting_for_its_values();

    carried.carried(node, "kernel_size", json!(5));
    assert!(carried.take_each_node_waiting_for_its_values().is_empty());
    carried.carried(node, "kernel_size", json!(3));
    assert_eq!(
        carried.take_each_node_waiting_for_its_values(),
        std::collections::BTreeSet::from([node])
    );
}

#[test]
fn a_node_forgotten_holds_nothing() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    let another = Uuid::new_v4();
    carried.carried(node, "kernel_size", json!(5));
    carried.carried(another, "kernel_size", json!(6));
    carried.forget_the_node(node);

    assert_eq!(
        carried.take_each_node_waiting_for_its_values(),
        std::collections::BTreeSet::from([another])
    );
    assert_eq!(
        carried.the_value_its_wire_carried(node, "kernel_size"),
        None
    );
}

/// Mental-revert: forgetting the value alone as its wire leaves leaves its node holding the
/// wire's last value.
#[test]
fn a_wire_leaving_a_setting_it_carried_a_value_into_leaves_its_node_to_hand_its_own() {
    let carried = ValuesWiresCarryIntoSettings::default();
    let node = Uuid::new_v4();
    carried.carried(node, "kernel_size", json!(5));
    carried.take_each_node_waiting_for_its_values();

    carried.its_wire_left(node, "kernel_size");
    assert_eq!(
        carried.the_value_its_wire_carried(node, "kernel_size"),
        None
    );
    assert_eq!(
        carried.take_each_node_waiting_for_its_values(),
        std::collections::BTreeSet::from([node])
    );

    carried.its_wire_left(node, "kernel_size");
    assert!(
        carried.take_each_node_waiting_for_its_values().is_empty(),
        "a wire that carried nothing leaves nothing to hand"
    );
}
