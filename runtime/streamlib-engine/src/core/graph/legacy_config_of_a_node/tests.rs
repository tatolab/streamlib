// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn of_a_type(config_fields: &[&str], input_ports: &[&str]) -> LegacyConfigOfANodeType {
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    LegacyConfigOfANodeType {
        fields_its_config_schema_lists: names(config_fields),
        its_input_ports: names(input_ports),
    }
}

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(object) = value else {
        panic!("an object");
    };
    object
}

#[test]
fn a_name_the_config_schema_lists_is_config_and_an_input_port_name_a_pin_value() {
    let split = of_a_type(&["waveform", "gain"], &["pitch", "gain"]);
    let input_values = object(json!({"waveform": "sine", "gain": 0.5, "pitch": 440.0}));
    assert_eq!(
        split.split(&input_values),
        ConfigAndPinValues {
            config: json!({"waveform": "sine", "gain": 0.5}),
            pin_values: object(json!({"pitch": 440.0})),
        }
    );
    assert_eq!(
        split.config_of(&input_values),
        json!({"waveform": "sine", "gain": 0.5})
    );
}

#[test]
fn a_name_neither_listed_nor_an_input_port_is_config_for_the_schema_to_refuse() {
    let split = of_a_type(&["frame_width"], &["video"]);
    let input_values = object(json!({"frame_widht": 640}));
    assert_eq!(
        split.split(&input_values),
        ConfigAndPinValues {
            config: json!({"frame_widht": 640}),
            pin_values: Map::new(),
        }
    );
}

#[test]
fn a_type_listing_no_config_field_reads_an_input_port_name_as_a_pin_value() {
    let split = of_a_type(&[], &["x"]);
    let input_values = object(json!({"x": 1.0, "label": "a"}));
    assert_eq!(
        split.split(&input_values),
        ConfigAndPinValues {
            config: json!({"label": "a"}),
            pin_values: object(json!({"x": 1.0})),
        }
    );
}

#[test]
fn a_config_and_pin_values_fold_back_into_one_map_with_a_pin_value_winning() {
    assert_eq!(
        input_values_of_a_config_and_pin_values(
            Some(&json!({"gain": 0.5, "pitch": 220.0})),
            &object(json!({"pitch": 440.0}))
        ),
        object(json!({"gain": 0.5, "pitch": 440.0}))
    );
    assert_eq!(
        input_values_of_a_config_and_pin_values(None, &Map::new()),
        Map::new()
    );
}

#[test]
fn a_node_known_by_its_input_ports_alone_holds_a_value_on_each_and_config_besides() {
    let split = LegacyConfigOfANodeType::of_the_input_ports(["video".to_owned()]);
    assert_eq!(
        split.split(&object(json!({"video": 1, "strength": 2}))),
        ConfigAndPinValues {
            config: json!({"strength": 2}),
            pin_values: object(json!({"video": 1})),
        }
    );
}
