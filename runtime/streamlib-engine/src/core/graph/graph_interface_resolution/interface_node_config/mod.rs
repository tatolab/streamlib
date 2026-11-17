// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an interface node's config declares: the port it names, the family it names, and the
//! keys it takes at its level.

use super::{
    GRAPH_INPUT_DEFAULT_CONFIG_KEY, GRAPH_INPUT_LIFETIME_CONFIG_KEY, GRAPH_INPUT_RANGE_CONFIG_KEY,
    GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    GraphInterfaceLevel, a_top_level_graph_input_of_the_family_runs,
    the_value_lifetime_a_graph_input_config_declares,
};
use crate::core::descriptors::PortFamily;
use crate::core::graph::graph_snapshot::GraphSnapshotNode;
use crate::core::{Error, Result};

fn interface_node_kind(node: &GraphSnapshotNode) -> &'static str {
    if node.node_type.as_str() == GRAPH_OUTPUT_NODE_TYPE {
        "GraphOutput"
    } else {
        "GraphInput"
    }
}

/// The port an interface node's config names, and the family it names, `None` when it leaves
/// `family` out or `null`.
pub(super) fn port_and_family_an_interface_nodes_config_declares(
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
                PortFamily::ALL
                    .into_iter()
                    .find(|family| Some(family.wire_name()) == family_wire_name)
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

/// Refuse a config key `node` does not take at `level` with `family`, and on a top-level
/// `GraphInput` a client writes, a `default`, `range` or `lifetime` its family does not take.
/// A top-level `GraphInput` whose family is not resolved yet takes all three, checked once a
/// link resolves it.
pub(super) fn refuse_a_config_key_the_interface_node_does_not_take(
    node: &GraphSnapshotNode,
    level: GraphInterfaceLevel,
    family: Option<PortFamily>,
) -> Result<()> {
    let kind = interface_node_kind(node);
    let may_run_as_a_source_node = kind == "GraphInput"
        && level == GraphInterfaceLevel::TheStreamsTopLevel
        && family.is_none_or(a_top_level_graph_input_of_the_family_runs);
    let keys_it_takes: &[&str] = if may_run_as_a_source_node {
        &[
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
            GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
            GRAPH_INPUT_DEFAULT_CONFIG_KEY,
            GRAPH_INPUT_RANGE_CONFIG_KEY,
            GRAPH_INPUT_LIFETIME_CONFIG_KEY,
        ]
    } else {
        &[
            GRAPH_INTERFACE_PORT_CONFIG_KEY,
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
                GRAPH_INPUT_DEFAULT_CONFIG_KEY,
                GRAPH_INPUT_RANGE_CONFIG_KEY,
                GRAPH_INPUT_LIFETIME_CONFIG_KEY,
            ]
            .contains(&unread_key.as_str())
        {
            " A `GraphInput` takes it on a stream's top level, of a family a client writes: \
             array, channels or records"
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
        refuse_a_default_or_range_its_family_does_not_take(node, family)?;
    }
    Ok(())
}

fn refuse_a_default_or_range_its_family_does_not_take(
    node: &GraphSnapshotNode,
    family: PortFamily,
) -> Result<()> {
    the_value_lifetime_a_graph_input_config_declares(Some(&serde_json::Value::Object(
        node.input_values.clone(),
    )))
    .map_err(|why| Error::GraphError(format!("`GraphInput` node `{}`: {why}", node.name)))?;
    if let Some(default) = node.input_values.get(GRAPH_INPUT_DEFAULT_CONFIG_KEY)
        && let Err(why) =
            crate::core::nodes::the_message_a_value_written_onto_an_output_is(family, default, 0)
    {
        return Err(Error::GraphError(format!(
            "`GraphInput` node `{}` declares its `{GRAPH_INPUT_DEFAULT_CONFIG_KEY}` as \
             {default}, and it carries {}: {why}",
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
