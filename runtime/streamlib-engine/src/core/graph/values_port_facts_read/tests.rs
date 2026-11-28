// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::descriptors::{NodeInputMode, PinDeclaration, PinType, PortDescriptor};

fn an_input_handed_to_its_node(name: &str, default: Value) -> PortInfo {
    PortInfo::from(
        &PortDescriptor::new(
            name,
            name,
            false,
            PinDeclaration::exactly(PinType::Integer { exact_size: None }),
        )
        .with_input_mode(NodeInputMode::HandedToItsNode)
        .with_pin_default(default),
    )
}

#[test]
fn each_input_handed_to_its_node_reads_its_value_or_else_its_default() {
    let inputs = [
        an_input_handed_to_its_node("sample_rate", json!(48_000)),
        an_input_handed_to_its_node("channel_count", json!(1)),
        PortInfo::declared_by_no_node_type("array", None),
    ];
    let input_values = Map::from_iter([
        ("kept".to_string(), json!(true)),
        ("sample_rate".to_string(), json!(44_100)),
    ]);
    assert_eq!(
        the_values_port_facts_read(&inputs, &input_values),
        json!({"kept": true, "sample_rate": 44_100, "channel_count": 1})
    );
}

#[test]
fn a_node_with_no_input_handed_to_it_reads_the_values_it_holds() {
    let inputs = [PortInfo::declared_by_no_node_type("array", None)];
    assert_eq!(the_values_port_facts_read(&inputs, &Map::new()), json!({}));
    let input_values = Map::from_iter([("rate".to_string(), json!(2))]);
    assert_eq!(
        the_values_port_facts_read(&inputs, &input_values),
        json!({"rate": 2})
    );
}

/// Mental-revert: filling defaults only for an input read at setup, as before, leaves out a node
/// function's value input, which was read each run, so an output whose facts follow it states
/// none while it holds no value.
#[test]
fn a_node_functions_value_input_holding_no_value_reads_its_default() {
    let inputs = [PortInfo::from(
        &crate::core::descriptors::the_input_taking_no_wire(
            "frame_rate",
            "",
            PinType::Integer { exact_size: None },
            json!(30),
        ),
    )];
    assert_eq!(
        the_values_port_facts_read(&inputs, &Map::new()),
        json!({"frame_rate": 30})
    );
}
