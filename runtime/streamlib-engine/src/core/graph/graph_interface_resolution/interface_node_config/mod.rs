// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an interface node's settings declare: the port it names, the pin type it chooses, and
//! the settings it takes at its level.

use super::graph_input_annotations::{
    is_an_annotation_setting_of_a_graph_input, refuse_an_annotation_the_chosen_type_does_not_take,
    the_annotation_settings_of_a_graph_input,
};
use super::{
    GRAPH_INPUT_LIFETIME_CONFIG_KEY, GRAPH_INPUT_RANGE_CONFIG_KEY, GRAPH_INPUT_VALUE_FORMER_NAME,
    GRAPH_INTERFACE_PIN_TYPE_SETTING, GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE, GraphInterfaceLevel,
    a_top_level_graph_input_runs, an_interface_node_states_what_its_value_port_carries,
    the_value_lifetime_a_graph_input_config_declares,
    what_the_value_port_of_an_interface_node_carries,
};
use crate::core::descriptors::PinType;
use crate::core::graph::graph_snapshot::GraphSnapshotNode;
use crate::core::pin_value_packing::{message_kind_of, pack_pin_value};
use crate::core::{Error, Result};
use streamlib_ipc_types::DataChannelPayloadDescriptorKind;

fn interface_node_kind(node: &GraphSnapshotNode) -> &'static str {
    if node.node_type.as_str() == GRAPH_OUTPUT_NODE_TYPE {
        "GraphOutput"
    } else {
        "GraphInput"
    }
}

/// What an interface node's settings declare of the port it offers.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WhatAnInterfaceNodeDeclares {
    /// The port's name, as its `port` setting wrote it.
    pub(super) port: String,
    /// The pin type its `pin_type` setting chooses; `None` while it holds none, so the first
    /// port linked to it decides it.
    pub(super) pin_type: Option<PinType>,
}

impl WhatAnInterfaceNodeDeclares {
    /// Whether it states what the port carries, a pin type, so no link decides it.
    pub(super) fn states_what_the_port_carries(&self) -> bool {
        self.pin_type.is_some()
    }
}

/// What an interface node's settings declare: the port it names and the pin type it chooses.
/// Refused, naming the node, for a pin type no link carries, as
/// [`what_the_value_port_of_an_interface_node_carries`] refuses it.
pub(super) fn what_an_interface_nodes_settings_declare(
    node: &GraphSnapshotNode,
) -> Result<WhatAnInterfaceNodeDeclares> {
    let kind = interface_node_kind(node);
    let port = node
        .input_values
        .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)
        .and_then(serde_json::Value::as_str)
        .filter(|port| !port.is_empty())
        .ok_or_else(|| {
            Error::GraphError(format!(
                "`{kind}` node `{}` names no port: its config takes \
                 `{GRAPH_INTERFACE_PORT_CONFIG_KEY}`, a non-empty string",
                node.name
            ))
        })?;
    let pin_type = what_the_value_port_of_an_interface_node_carries(Some(
        &serde_json::Value::Object(node.input_values.clone()),
    ))
    .map_err(|why| Error::GraphError(format!("`{kind}` node `{}`: {why}", node.name)))?;
    debug_assert_eq!(
        pin_type.is_some(),
        an_interface_node_states_what_its_value_port_carries(&node.input_values)
    );
    Ok(WhatAnInterfaceNodeDeclares {
        port: port.to_string(),
        pin_type,
    })
}

/// The settings a top-level `GraphInput` a client writes takes besides those every interface
/// node takes and the annotations of its `value`: its `value` (saved as `default` before it was
/// an input) and its `lifetime`.
const THE_VALUE_SETTINGS_ONLY_A_GRAPH_INPUT_A_CLIENT_WRITES_TAKES: [&str; 3] = [
    GRAPH_INTERFACE_VALUE_PORT_NAME,
    GRAPH_INPUT_VALUE_FORMER_NAME,
    GRAPH_INPUT_LIFETIME_CONFIG_KEY,
];

/// Whether a top-level `GraphInput` a client writes is the only interface node that takes
/// `setting`: its `value`, an annotation of it, or its `lifetime`.
fn only_a_graph_input_a_client_writes_takes(setting: &str) -> bool {
    THE_VALUE_SETTINGS_ONLY_A_GRAPH_INPUT_A_CLIENT_WRITES_TAKES.contains(&setting)
        || is_an_annotation_setting_of_a_graph_input(setting)
}

/// The settings every interface node takes.
const THE_SETTINGS_EVERY_INTERFACE_NODE_TAKES: [&str; 2] = [
    GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_PIN_TYPE_SETTING,
];

/// Refuse a setting `node` does not take at `level` carrying `pin_type`, and on a top-level
/// `GraphInput` a client writes, a `value` (saved as `default` before it was an input), an
/// annotation of its `value` or a `lifetime` its type does not take. A top-level `GraphInput`
/// whose type is not resolved yet takes them all, checked once a link resolves it.
pub(super) fn refuse_a_config_key_the_interface_node_does_not_take(
    node: &GraphSnapshotNode,
    level: GraphInterfaceLevel,
    pin_type: Option<&PinType>,
) -> Result<()> {
    let kind = interface_node_kind(node);
    let may_run_as_a_source_node = kind == "GraphInput"
        && level == GraphInterfaceLevel::TheStreamsTopLevel
        && pin_type.is_none_or(a_top_level_graph_input_runs);
    let takes = |key: &str| {
        THE_SETTINGS_EVERY_INTERFACE_NODE_TAKES.contains(&key)
            || (may_run_as_a_source_node && only_a_graph_input_a_client_writes_takes(key))
    };
    if let Some(unread_key) = node.input_values.keys().find(|key| !takes(key)) {
        let a_key_a_client_writes =
            kind == "GraphInput" && only_a_graph_input_a_client_writes_takes(unread_key);
        if a_key_a_client_writes && level == GraphInterfaceLevel::TheStreamsTopLevel {
            let what_it_carries =
                pin_type.map_or_else(|| "nothing yet".to_string(), ToString::to_string);
            return Err(Error::GraphError(format!(
                "`GraphInput` node `{}` carries {what_it_carries}, which a client does not \
                 write, and holds `{unread_key}`: a stream's top-level Input takes a \
                 `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, the annotations of it and a \
                 `{GRAPH_INPUT_LIFETIME_CONFIG_KEY}` only of a type a client writes: a type \
                 that holds a value, a Tensor or Audio. To fix: choose such a type, or clear \
                 `{unread_key}`",
                node.name
            )));
        }
        let where_it_would_be_taken = if a_key_a_client_writes {
            " A `GraphInput` takes it on a stream's top level, of a type a client writes: a \
             type that holds a value, a Tensor or Audio"
        } else {
            ""
        };
        let mut settings_it_takes = THE_SETTINGS_EVERY_INTERFACE_NODE_TAKES.to_vec();
        if may_run_as_a_source_node {
            settings_it_takes.extend(THE_VALUE_SETTINGS_ONLY_A_GRAPH_INPUT_A_CLIENT_WRITES_TAKES);
            settings_it_takes.extend(the_annotation_settings_of_a_graph_input());
        }
        return Err(Error::GraphError(format!(
            "`{kind}` node `{}` carries the config key `{unread_key}`, which it does not take. \
             It takes: {}.{where_it_would_be_taken}",
            node.name,
            settings_it_takes
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if may_run_as_a_source_node {
        refuse_an_annotation_the_chosen_type_does_not_take(node, pin_type)?;
    }
    if let Some(pin_type) = pin_type
        && may_run_as_a_source_node
    {
        refuse_a_default_or_range_its_type_does_not_take(node, pin_type)?;
    }
    Ok(())
}

/// Refuse the `lifetime`, the saved `value` and the `range` of `node`, a top-level `GraphInput`
/// carrying `pin_type`, when its type does not take them: a value it does not hold, or a client
/// would not write onto it, and a range on a type that carries no number.
fn refuse_a_default_or_range_its_type_does_not_take(
    node: &GraphSnapshotNode,
    pin_type: &PinType,
) -> Result<()> {
    the_value_lifetime_a_graph_input_config_declares(Some(&serde_json::Value::Object(
        node.input_values.clone(),
    )))
    .map_err(|why| Error::GraphError(format!("`GraphInput` node `{}`: {why}", node.name)))?;
    let saved_value = node
        .input_values
        .get(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .or_else(|| node.input_values.get(GRAPH_INPUT_VALUE_FORMER_NAME))
        .filter(|value| !value.is_null());
    if let Some(value) = saved_value
        && pin_type.holds_a_stored_value()
        && let Err(not_taken) = pin_type.check_value(value)
    {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` holds its `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, its \
             `{GRAPH_INPUT_VALUE_FORMER_NAME}` before it was an input, as {value}, and it \
             carries {pin_type}, which takes {}",
            node.name, not_taken.takes
        )));
    }
    if let Some(value) = saved_value.filter(|_| !pin_type.holds_a_stored_value())
        && let Err(why) = pack_pin_value(pin_type, value, 0)
    {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` holds its `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, its \
             `{GRAPH_INPUT_VALUE_FORMER_NAME}` before it was an input, as {value}, and it \
             carries {pin_type}: {why}",
            node.name
        )));
    }
    let Some(range) = node.input_values.get(GRAPH_INPUT_RANGE_CONFIG_KEY) else {
        return Ok(());
    };
    if !matches!(
        message_kind_of(pin_type),
        Some(DataChannelPayloadDescriptorKind::ChannelsBlock)
            | Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    ) {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` carries {pin_type}, and a `{GRAPH_INPUT_RANGE_CONFIG_KEY}` \
             spans a number, which only an Input of numbers carries",
            node.name
        )));
    }
    let lowest_and_highest = range
        .as_array()
        .filter(|bounds| bounds.len() == 2)
        .and_then(|bounds| Some((bounds[0].as_f64()?, bounds[1].as_f64()?)));
    match lowest_and_highest {
        Some((lowest, highest)) if lowest <= highest => Ok(()),
        _ => Err(Error::GraphError(format!(
            "`GraphInput` node `{}` declares its `{GRAPH_INPUT_RANGE_CONFIG_KEY}` as {range}, \
             and a range is `[lowest, highest]`, two numbers, lowest first",
            node.name
        ))),
    }
}
