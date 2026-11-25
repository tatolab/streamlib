// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::descriptors::{NodeInputMode, PinDeclaration, PinType, PortDescriptor, SettingReadTime};

fn a_setting_read_at_setup(name: &str, default: Value) -> PortInfo {
    PortInfo::from(
        &PortDescriptor::new(name, name, false, PinDeclaration::exactly(PinType::Integer {
            exact_size: None,
        }))
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup,
        })
        .with_pin_default(default),
    )
}

#[test]
fn each_setting_read_at_setup_reads_its_value_or_else_its_default() {
    let inputs = [
        a_setting_read_at_setup("sample_rate", json!(48_000)),
        a_setting_read_at_setup("channel_count", json!(1)),
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
fn a_node_with_no_setting_read_at_setup_reads_the_values_it_holds() {
    let inputs = [PortInfo::declared_by_no_node_type("array", None)];
    assert_eq!(the_values_port_facts_read(&inputs, &Map::new()), json!({}));
    let input_values = Map::from_iter([("rate".to_string(), json!(2))]);
    assert_eq!(
        the_values_port_facts_read(&inputs, &input_values),
        json!({"rate": 2})
    );
}
