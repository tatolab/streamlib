// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value};

use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    StreamGraphDocument,
};

/// The import path a graph names `SetVariable` by, in the built-in node module.
pub const SET_VARIABLE_NODE_TYPE: &str = "tatolab.stream:SetVariable";

/// The import path a graph names `GetVariable` by, in the built-in node module.
pub const GET_VARIABLE_NODE_TYPE: &str = "tatolab.stream:GetVariable";

/// The config key both variable nodes name their variable under.
pub const GRAPH_VARIABLE_NAME_CONFIG_KEY: &str = "variable";

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
            let Some(variable_of_the_last_setter) = the_names_nodes_of_the_type_hold(
                level,
                SET_VARIABLE_NODE_TYPE,
                GRAPH_VARIABLE_NAME_CONFIG_KEY,
            )
            .pop() else {
                return config;
            };
            (GRAPH_VARIABLE_NAME_CONFIG_KEY, variable_of_the_last_setter)
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
            node.config
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
            the_default_named(GRAPH_INPUT_NODE_TYPE, json!({"family": "channels"}), &level),
            json!({"port": "input1", "family": "channels"})
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
             "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "input1", "family": "image"}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "graphinput-2",
             "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "Input3", "family": "image"}},
            {"id": "00000000-0000-0000-0000-000000000003", "name": "graphoutput",
             "type": GRAPH_OUTPUT_NODE_TYPE, "config": {"port": "input2", "family": "image"}},
        ]));
        assert_eq!(
            the_default_named(GRAPH_INPUT_NODE_TYPE, json!({}), &level),
            json!({"port": "input2"})
        );
        let level = a_level_holding(json!([
            {"id": "00000000-0000-0000-0000-000000000001", "name": "graphinput",
             "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "input1", "family": "image"}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "graphinput-2",
             "type": GRAPH_INPUT_NODE_TYPE, "config": {"port": "input2", "family": "image"}},
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
             "type": SET_VARIABLE_NODE_TYPE, "config": {"variable": "var1"}},
            {"id": "00000000-0000-0000-0000-000000000002", "name": "setvariable-2",
             "type": SET_VARIABLE_NODE_TYPE, "config": {"variable": "blur"}},
        ]));
        assert_eq!(
            the_default_named(SET_VARIABLE_NODE_TYPE, json!({}), &level),
            json!({"variable": "var2"})
        );
        assert_eq!(
            the_default_named(GET_VARIABLE_NODE_TYPE, json!({}), &level),
            json!({"variable": "blur"})
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
