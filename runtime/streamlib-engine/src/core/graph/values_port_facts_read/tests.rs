// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::descriptors::{
    NodeInputMode, PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime,
};

fn a_setting_read_at_setup(name: &str, default: Value) -> PortInfo {
    PortInfo::from(
        &PortDescriptor::new(
            name,
            name,
            false,
            PortFamilyDeclaration::of_family(PortFamily::Records),
        )
        .with_pin_declaration(PinDeclaration::exactly(PinType::Integer {
            exact_size: None,
        }))
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup,
        })
        .with_pin_default(default),
    )
}

#[test]
fn each_setting_read_at_setup_lays_its_value_or_default_over_the_config() {
    let inputs = [
        a_setting_read_at_setup("sample_rate", json!(48_000)),
        a_setting_read_at_setup("channel_count", json!(1)),
        PortInfo::declaring_only_a_family(
            "array",
            "",
            PortFamilyDeclaration::of_family(PortFamily::Array),
        ),
    ];
    let values_set = Map::from_iter([
        ("sample_rate".to_string(), json!(44_100)),
        ("array".to_string(), json!("not a setting")),
    ]);
    assert_eq!(
        the_values_port_facts_read(Some(&json!({"kept": true})), &inputs, &values_set),
        Some(json!({"kept": true, "sample_rate": 44_100, "channel_count": 1}))
    );
}

#[test]
fn a_node_with_no_setting_read_at_setup_reads_its_config_alone() {
    let inputs = [PortInfo::declaring_only_a_family(
        "array",
        "",
        PortFamilyDeclaration::of_family(PortFamily::Array),
    )];
    assert_eq!(the_values_port_facts_read(None, &inputs, &Map::new()), None);
    assert_eq!(
        the_values_port_facts_read(Some(&json!({"rate": 2})), &inputs, &Map::new()),
        Some(json!({"rate": 2}))
    );
}
