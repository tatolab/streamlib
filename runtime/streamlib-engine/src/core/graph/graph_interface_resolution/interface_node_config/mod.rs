// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an interface node's settings declare: the port it names, the pin type it chooses, the
//! family it names, and the settings it takes at its level.

use super::{
    GRAPH_INPUT_LIFETIME_CONFIG_KEY, GRAPH_INPUT_RANGE_CONFIG_KEY, GRAPH_INPUT_VALUE_FORMER_NAME,
    GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE,
    GraphInterfaceLevel, a_top_level_graph_input_runs, the_pin_type_an_interface_node_chooses,
    the_value_lifetime_a_graph_input_config_declares,
};
use crate::core::descriptors::{PinType, PortFamily};
use crate::core::graph::graph_snapshot::GraphSnapshotNode;
use crate::core::{Error, Result};

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
    /// The family the port carries: the one its chosen pin type travels on, else the one its
    /// `family` setting names; `None` when it states neither, or chooses a type no family
    /// carries.
    pub(super) family: Option<PortFamily>,
    /// The pin type its `pin_type` setting chooses; `None` while it holds none.
    pub(super) pin_type: Option<PinType>,
}

impl WhatAnInterfaceNodeDeclares {
    /// Whether it states what the port carries, a pin type or a family, so no link decides it.
    pub(super) fn states_what_the_port_carries(&self) -> bool {
        self.pin_type.is_some() || self.family.is_some()
    }
}

/// What an interface node's settings declare: the port it names, the pin type it chooses, and
/// the family that type travels on, else the one its `family` setting names, `None` when it
/// leaves `family` out or `null`.
pub(super) fn what_an_interface_nodes_settings_declare(
    node: &GraphSnapshotNode,
) -> Result<WhatAnInterfaceNodeDeclares> {
    let (port, named_family) = port_and_family_an_interface_nodes_config_declares(node)?;
    let pin_type = the_pin_type_an_interface_node_chooses(Some(&node.input_values)).map_err(
        |why| {
            Error::GraphError(format!(
                "`{}` node `{}`: {why}",
                interface_node_kind(node),
                node.name
            ))
        },
    )?;
    let family = match &pin_type {
        Some(pin_type) => PortFamily::carrying(pin_type),
        None => named_family,
    };
    Ok(WhatAnInterfaceNodeDeclares {
        port,
        family,
        pin_type,
    })
}

/// The port an interface node's config names, and the family it names, `None` when it leaves
/// `family` out or `null`.
fn port_and_family_an_interface_nodes_config_declares(
    node: &GraphSnapshotNode,
) -> Result<(String, Option<PortFamily>)> {
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
    let family = match node.input_values.get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY) {
        None | Some(serde_json::Value::Null) => None,
        Some(named) => {
            let family_wire_name = named.as_str();
            Some(
                family_wire_name
                    .and_then(PortFamily::of_wire_name)
                    .ok_or_else(|| {
                        Error::GraphError(format!(
                            "`{kind}` node `{}` declares its port of family {named}, and its \
                             config's `{GRAPH_INTERFACE_FAMILY_CONFIG_KEY}` is one of {}, or \
                             left out for the family of the first port linked to it",
                            node.name,
                            PortFamily::ALL.map(|family| family.wire_name()).join(", ")
                        ))
                    })?,
            )
        }
    };
    Ok((port.to_string(), family))
}

/// Refuse a setting `node` does not take at `level` with `family` and `pin_type`, and on a
/// top-level `GraphInput` a client writes, a `value` (saved as `default` before it was an input),
/// `range` or `lifetime` its type or family does not take.
/// A top-level `GraphInput` whose family is not resolved yet takes all three, checked once a
/// link resolves it.
pub(super) fn refuse_a_config_key_the_interface_node_does_not_take(
    node: &GraphSnapshotNode,
    level: GraphInterfaceLevel,
    family: Option<PortFamily>,
    pin_type: Option<&PinType>,
) -> Result<()> {
    let kind = interface_node_kind(node);
    let may_run_as_a_source_node = kind == "GraphInput"
        && level == GraphInterfaceLevel::TheStreamsTopLevel
        && match pin_type {
            Some(_) => a_top_level_graph_input_runs(family, pin_type),
            None => family.is_none_or(|family| a_top_level_graph_input_runs(Some(family), None)),
        };
    let keys_it_takes: &[&str] = if may_run_as_a_source_node {
        &[
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
            GRAPH_INTERFACE_PIN_TYPE_SETTING,
            GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
            GRAPH_INTERFACE_VALUE_PORT_NAME,
            GRAPH_INPUT_VALUE_FORMER_NAME,
            GRAPH_INPUT_RANGE_CONFIG_KEY,
            GRAPH_INPUT_LIFETIME_CONFIG_KEY,
        ]
    } else {
        &[
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
            GRAPH_INTERFACE_PIN_TYPE_SETTING,
            GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
        ]
    };
    if let Some(unread_key) = node
        .input_values
        .keys()
        .find(|key| !keys_it_takes.contains(&key.as_str()))
    {
        let where_it_would_be_taken = if kind == "GraphInput"
            && [
                GRAPH_INTERFACE_VALUE_PORT_NAME,
                GRAPH_INPUT_VALUE_FORMER_NAME,
                GRAPH_INPUT_RANGE_CONFIG_KEY,
                GRAPH_INPUT_LIFETIME_CONFIG_KEY,
            ]
            .contains(&unread_key.as_str())
        {
            " A `GraphInput` takes it on a stream's top level, of a type or family a client \
             writes: a type that holds a value, or array, channels or records"
        } else {
            ""
        };
        return Err(Error::GraphError(format!(
            "`{kind}` node `{}` carries the config key `{unread_key}`, which it does not take. \
             It takes: {}.{where_it_would_be_taken}",
            node.name,
            keys_it_takes
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if let Some(family) = family
        && may_run_as_a_source_node
    {
        refuse_a_default_or_range_its_family_does_not_take(node, family, pin_type)?;
    }
    Ok(())
}

fn refuse_a_default_or_range_its_family_does_not_take(
    node: &GraphSnapshotNode,
    family: PortFamily,
    pin_type: Option<&PinType>,
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
    if let (Some(value), Some(pin_type)) = (saved_value, pin_type)
        && let Err(not_taken) = pin_type.check_value(value)
    {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` holds its `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, its \
             `{GRAPH_INPUT_VALUE_FORMER_NAME}` before it was an input, as {value}, and it \
             carries {pin_type}, which takes {}",
            node.name,
            not_taken.takes
        )));
    }
    if let Some(value) = saved_value.filter(|_| pin_type.is_none())
        && let Err(why) =
            crate::core::pin_value_packing::the_message_a_value_written_onto_an_output_is(
                family, value, 0,
            )
    {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` holds its `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, its \
             `{GRAPH_INPUT_VALUE_FORMER_NAME}` before it was an input, as {value}, and it \
             carries {}: {why}",
            node.name,
            family.declaration_name()
        )));
    }
    let Some(range) = node.input_values.get(GRAPH_INPUT_RANGE_CONFIG_KEY) else {
        return Ok(());
    };
    if family == PortFamily::Records {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` carries Records, and a `{GRAPH_INPUT_RANGE_CONFIG_KEY}` \
             spans a number, which only an Array or a Channels Input carries",
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
