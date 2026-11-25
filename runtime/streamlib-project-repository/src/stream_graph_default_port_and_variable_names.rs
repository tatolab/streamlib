// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value};

use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    StreamGraphDocument,
};

pub use streamlib_node_schema::{GET_VARIABLE_NODE_TYPE, SET_VARIABLE_NODE_TYPE};

/// The config key both variable nodes name their variable under.
pub const GRAPH_VARIABLE_NAME_CONFIG_KEY: &str = "variable";

/// The config key a `GetVariable` names the `SetVariable` node it reads under, by that node's
/// UUID, so renaming the variable keeps every get reading it.
pub const GET_VARIABLE_SET_VARIABLE_CONFIG_KEY: &str = "set_variable";

/// The name a `GraphInput` added with none takes: `input` and the first number from 1 no other
/// `GraphInput` of its level declares.
const DEFAULT_GRAPH_INPUT_PORT_NAME_PREFIX: &str = "input";

/// The name a `GraphOutput` added with none takes, numbered as a `GraphInput`'s is.
const DEFAULT_GRAPH_OUTPUT_PORT_NAME_PREFIX: &str = "output";

/// The name a `SetVariable` added with none takes, numbered as a `GraphInput`'s is.
const DEFAULT_GRAPH_VARIABLE_NAME_PREFIX: &str = "var";

/// `config`, of a node of `node_type` being added to `level`, holding the name the node takes
/// when `config` names none: a `GraphInput` the next free `input1`, `input2` …, a `GraphOutput`
/// the next free `output1` …, a `SetVariable` the next free `var1` …, each unique among its
/// kind on the level, and a `GetVariable` the variable of the level's last `SetVariable`. Any
/// other node's config, and a config that names one, is returned as it is.
pub fn config_holding_the_default_name_of_a_graph_port_or_variable(
    node_type: &str,
    mut config: Map<String, Value>,
    level: &StreamGraphDocument,
) -> Map<String, Value> {
    let (name_key, default_name) = match node_type {
        GRAPH_INPUT_NODE_TYPE => (
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
            the_first_name_no_node_of_the_type_holds(
                level,
                GRAPH_INPUT_NODE_TYPE,
                GRAPH_INTERFACE_PORT_CONFIG_KEY,
                DEFAULT_GRAPH_INPUT_PORT_NAME_PREFIX,
            ),
        ),
        GRAPH_OUTPUT_NODE_TYPE => (
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
            the_first_name_no_node_of_the_type_holds(
                level,
                GRAPH_OUTPUT_NODE_TYPE,
                GRAPH_INTERFACE_PORT_CONFIG_KEY,
                DEFAULT_GRAPH_OUTPUT_PORT_NAME_PREFIX,
            ),
        ),
        SET_VARIABLE_NODE_TYPE => (
            GRAPH_VARIABLE_NAME_CONFIG_KEY,
            the_first_name_no_node_of_the_type_holds(
                level,
                SET_VARIABLE_NODE_TYPE,
                GRAPH_VARIABLE_NAME_CONFIG_KEY,
                DEFAULT_GRAPH_VARIABLE_NAME_PREFIX,
            ),
        ),
        GET_VARIABLE_NODE_TYPE => {
            if config.contains_key(GET_VARIABLE_SET_VARIABLE_CONFIG_KEY) {
                return config;
            }
            let chosen = config
                .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
                .and_then(Value::as_str)
                .filter(|variable| !variable.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    the_names_nodes_of_the_type_hold(
                        level,
                        SET_VARIABLE_NODE_TYPE,
                        GRAPH_VARIABLE_NAME_CONFIG_KEY,
                    )
                    .pop()
                });
            let Some(variable) = chosen else {
                return config;
            };
            config.remove(GRAPH_VARIABLE_NAME_CONFIG_KEY);
            config.extend(the_input_values_of_a_get_reading(&variable, level));
            return config;
        }
        _ => return config,
    };
    let names_none = match config.get(name_key) {
        None | Some(Value::Null) => true,
        Some(Value::String(name)) => name.is_empty(),
        Some(_) => false,
    };
    if names_none {
        config.insert(name_key.to_owned(), Value::String(default_name));
    }
    config
}

/// The input values of a `GetVariable` on `level` reading `variable`: the UUID of the level's
/// last `SetVariable` of it, so renaming the variable keeps the get reading it, or, while no
/// setter on `level` sets it, as one of an enclosing level may, the variable by its name.
pub fn the_input_values_of_a_get_reading(
    variable: &str,
    level: &StreamGraphDocument,
) -> Map<String, Value> {
    let setter = level.nodes().into_iter().rev().find(|node| {
        node.node_type == SET_VARIABLE_NODE_TYPE
            && node.input_values.get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
                == Some(&Value::String(variable.to_owned()))
    });
    match setter {
        Some(setter) => Map::from_iter([(
            GET_VARIABLE_SET_VARIABLE_CONFIG_KEY.to_owned(),
            Value::String(setter.id.to_string()),
        )]),
        None => Map::from_iter([(
            GRAPH_VARIABLE_NAME_CONFIG_KEY.to_owned(),
            Value::String(variable.to_owned()),
        )]),
    }
}

/// The non-empty string each node of `node_type` on `level` holds at `name_key`, in the order
/// the level lists its nodes.
fn the_names_nodes_of_the_type_hold(
    level: &StreamGraphDocument,
    node_type: &str,
    name_key: &str,
) -> Vec<String> {
    level
        .nodes()
        .into_iter()
        .filter(|node| node.node_type == node_type)
        .filter_map(|node| {
            node.input_values
                .get(name_key)
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// `prefix` and the first number from 1 whose name, compared without case, no node of
/// `node_type` on `level` holds at `name_key`.
fn the_first_name_no_node_of_the_type_holds(
    level: &StreamGraphDocument,
    node_type: &str,
    name_key: &str,
    prefix: &str,
) -> String {
    let taken = the_names_nodes_of_the_type_hold(level, node_type, name_key);
    (1_u64..)
        .map(|number| format!("{prefix}{number}"))
        .find(|candidate| {
            !taken
                .iter()
                .any(|name| name.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or_else(|| prefix.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a_level_holding(nodes: Value) -> StreamGraphDocument {
        StreamGraphDocument::from_value(json!({ "nodes": nodes, "links": [] }))
            .expect("the level is a graph document")
    }

    fn the_default_named(node_type: &str, config: Value, level: &StreamGraphDocument) -> Value {
        let Value::Object(config) = config else {
            panic!("a config is an object");
        };
        Value::Object(config_holding_the_default_name_of_a_graph_port_or_variable(
            node_type, config, level,
        ))
    }

    #[test]
    fn the_first_graph_input_and_output_of_a_level_take_input1_and_output1() {
        let level = a_level_holding(json!([]));
        assert_eq!(
            the_default_named(
                GRAPH_INPUT_NODE_TYPE,
                json!({"pin_type": {"type": "float"}}),
                &level
            ),
            json!({"port": "input1", "pin_type": {"type": "float"}})
        );
        assert_eq!(
            the_default_named(GRAPH_OUTPUT_NODE_TYPE, json!({}), &level),
            json!({"port": "output1"})
        );
    }

    #[test]
    fn a_default_name_takes_the_first_number_no_node_of_its_kind_holds() {
        let level = a_level_holding(json!([
            {"id": "00000000-0000-0000-0000-000000000001", "name": "graphinput",
             "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "input1", "pin_type": {"type": "texture"}}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "graphinput-2",
             "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "Input3", "pin_type": {"type": "texture"}}},
            {"id": "00000000-0000-0000-0000-000000000003", "name": "graphoutput",
             "type": GRAPH_OUTPUT_NODE_TYPE, "input_values": {"port": "input2", "pin_type": {"type": "texture"}}},
        ]));
        assert_eq!(
            the_default_named(GRAPH_INPUT_NODE_TYPE, json!({}), &level),
            json!({"port": "input2"})
        );
        let level = a_level_holding(json!([
            {"id": "00000000-0000-0000-0000-000000000001", "name": "graphinput",
             "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "input1", "pin_type": {"type": "texture"}}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "graphinput-2",
             "type": GRAPH_INPUT_NODE_TYPE, "input_values": {"port": "input2", "pin_type": {"type": "texture"}}},
        ]));
        assert_eq!(
            the_default_named(GRAPH_INPUT_NODE_TYPE, json!({"port": ""}), &level),
            json!({"port": "input3"})
        );
    }

    #[test]
    fn a_name_the_config_gives_is_kept() {
        let level = a_level_holding(json!([]));
        assert_eq!(
            the_default_named(GRAPH_INPUT_NODE_TYPE, json!({"port": "brightness"}), &level),
            json!({"port": "brightness"})
        );
        assert_eq!(
            the_default_named(SET_VARIABLE_NODE_TYPE, json!({"variable": 3}), &level),
            json!({"variable": 3})
        );
    }

    #[test]
    fn a_set_variable_takes_the_next_free_var_and_a_get_variable_reads_the_last_setter() {
        let empty_level = a_level_holding(json!([]));
        assert_eq!(
            the_default_named(SET_VARIABLE_NODE_TYPE, json!({}), &empty_level),
            json!({"variable": "var1"})
        );
        assert_eq!(
            the_default_named(GET_VARIABLE_NODE_TYPE, json!({}), &empty_level),
            json!({})
        );
        let level = a_level_holding(json!([
            {"id": "00000000-0000-0000-0000-000000000001", "name": "setvariable",
             "type": SET_VARIABLE_NODE_TYPE, "input_values": {"variable": "var1"}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "setvariable-2",
             "type": SET_VARIABLE_NODE_TYPE, "input_values": {"variable": "blur"}},
        ]));
        assert_eq!(
            the_default_named(SET_VARIABLE_NODE_TYPE, json!({}), &level),
            json!({"variable": "var2"})
        );
        assert_eq!(
            the_default_named(GET_VARIABLE_NODE_TYPE, json!({}), &level),
            json!({"set_variable": "00000000-0000-0000-0000-000000000002"})
        );
    }

    #[test]
    fn a_get_variable_added_naming_a_variable_names_its_setter_on_the_level() {
        let level = a_level_holding(json!([
            {"id": "00000000-0000-0000-0000-000000000001", "name": "setvariable",
             "type": SET_VARIABLE_NODE_TYPE, "input_values": {"variable": "var1"}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "setvariable-2",
             "type": SET_VARIABLE_NODE_TYPE, "input_values": {"variable": "blur"}},
        ]));
        assert_eq!(
            the_default_named(GET_VARIABLE_NODE_TYPE, json!({"variable": "var1"}), &level),
            json!({"set_variable": "00000000-0000-0000-0000-000000000001"})
        );
        assert_eq!(
            the_default_named(GET_VARIABLE_NODE_TYPE, json!({"variable": "outer"}), &level),
            json!({"variable": "outer"}),
            "a variable no setter of the level sets may be an enclosing level's"
        );
    }

    #[test]
    fn a_node_of_any_other_type_is_left_as_it_is() {
        let level = a_level_holding(json!([]));
        assert_eq!(
            the_default_named("tatolab.ops:Constant", json!({}), &level),
            json!({})
        );
    }
}
