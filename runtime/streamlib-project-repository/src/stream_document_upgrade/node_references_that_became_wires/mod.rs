// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Node types that gave themselves out by a node reference, `{"node": <name>}`, and give
//! themselves out on a connection output instead, and the inputs that held such a reference. A
//! saved document opens with each reference to one of them gone and a link into the input that
//! takes the wire: from the named node's connection output, or from the `value` of a
//! `GetVariable` it named, which hands the connection on. A `SetVariable` holding a reference to
//! one sets the variable from its connection the same way.

use serde_json::{Map, Value};
use streamlib_node_schema::{
    GET_VARIABLE_NODE_TYPE, NODE_REFERENCE_NODE_KEY, SET_VARIABLE_NODE_TYPE,
};

use super::{
    NODE_INPUT_VALUES_KEY, a_link, drop_the_input_values_when_empty, the_name_a_link_gives,
};

/// The output a `GetVariable` hands a variable's value on from.
const GET_VARIABLE_VALUE_OUTPUT: &str = "value";

/// A node type a reference named, which gives itself out on a connection output now.
#[derive(Debug, Clone, Copy)]
pub(super) struct NodeTypeGivingItselfOut {
    /// The node type's path, after any rename.
    pub(super) node_type: &'static str,
    /// The output it gives itself out on.
    pub(super) connection_output: &'static str,
}

/// An input that held a node reference, and the input a wire from the node it named feeds.
#[derive(Debug, Clone, Copy)]
pub(super) struct InputThatHeldANodeReference {
    /// The node type's path, after any rename.
    pub(super) node_type: &'static str,
    /// The input that held the reference.
    pub(super) input: &'static str,
    /// The input the wire feeds.
    pub(super) wired_input: &'static str,
    /// Whether a reference to a node of no type [`NODE_TYPES_GIVING_THEMSELVES_OUT`] names, or
    /// to a node the level does not hold, is kept as written, the input still taking one; else
    /// it is dropped.
    pub(super) keeps_any_other_reference: bool,
}

/// Each node type that gives itself out on a connection output in place of a reference.
pub(super) const NODE_TYPES_GIVING_THEMSELVES_OUT: &[NodeTypeGivingItselfOut] = &[
    NodeTypeGivingItselfOut {
        node_type: "tatolab.moq:MoqGateway",
        connection_output: "connection",
    },
    NodeTypeGivingItselfOut {
        node_type: "tatolab.moq:MoqTrack",
        connection_output: "connection",
    },
];

/// Each input that held a reference to a node of a type [`NODE_TYPES_GIVING_THEMSELVES_OUT`]
/// names.
pub(super) const INPUTS_THAT_HELD_A_NODE_REFERENCE: &[InputThatHeldANodeReference] = &[
    InputThatHeldANodeReference {
        node_type: "tatolab.moq:MoqTrack",
        input: "gateway",
        wired_input: "gateway",
        keeps_any_other_reference: false,
    },
    InputThatHeldANodeReference {
        node_type: "tatolab.moq:MoqTrackReceive",
        input: "gateway",
        wired_input: "gateway",
        keeps_any_other_reference: false,
    },
    InputThatHeldANodeReference {
        node_type: "tatolab.moq:MoqTrackSend",
        input: "track",
        wired_input: "track",
        keeps_any_other_reference: false,
    },
    InputThatHeldANodeReference {
        node_type: SET_VARIABLE_NODE_TYPE,
        input: "node_reference",
        wired_input: "value",
        keeps_any_other_reference: true,
    },
];

/// A node of a level, as a reference names it and as a link names it.
struct LevelNode {
    name: String,
    link_name: String,
    node_type: String,
}

/// Turn each reference a node of one level holds in an input `rows` name into a link in
/// `links`, in place; whether any was. A reference naming no node of the level is dropped, as
/// the node it named is gone.
pub(super) fn the_references_of_the_level_wired(
    nodes: &mut [Value],
    links: &mut Vec<Value>,
    rows: &[InputThatHeldANodeReference],
) -> bool {
    let is_a_row_type = |node: &Value| {
        rows.iter()
            .any(|row| node.get("type").and_then(Value::as_str) == Some(row.node_type))
    };
    if !nodes.iter().any(is_a_row_type) {
        return false;
    }
    let level_nodes: Vec<LevelNode> = nodes
        .iter()
        .filter_map(|node| {
            let node = node.as_object()?;
            Some(LevelNode {
                name: node.get("name")?.as_str()?.to_owned(),
                link_name: the_name_a_link_gives(node)?.to_owned(),
                node_type: node
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    let mut any_wired = false;
    for node in nodes.iter_mut() {
        if let Some(node) = node.as_object_mut() {
            any_wired |= the_references_of_a_node_wired(node, links, rows, &level_nodes);
        }
    }
    any_wired
}

/// Drop each reference `node` holds in an input `rows` name, in place; whether any was. A node
/// placed on its own carries no link, so each reference it held is dropped, but for one its
/// input still takes.
pub(super) fn the_references_of_a_node_dropped(
    node: &mut Map<String, Value>,
    rows: &[InputThatHeldANodeReference],
) -> bool {
    the_references_of_a_node_wired(node, &mut Vec::new(), rows, &[])
}

fn the_references_of_a_node_wired(
    node: &mut Map<String, Value>,
    links: &mut Vec<Value>,
    rows: &[InputThatHeldANodeReference],
    level_nodes: &[LevelNode],
) -> bool {
    let Some(node_type) = node.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return false;
    };
    let target = the_name_a_link_gives(node).map(str::to_owned);
    let Some(input_values) = node
        .get_mut(NODE_INPUT_VALUES_KEY)
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let mut any_wired = false;
    for row in rows.iter().filter(|row| row.node_type == node_type) {
        let Some(referenced) = input_values
            .get(row.input)
            .and_then(|held| held.get(NODE_REFERENCE_NODE_KEY))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let source = level_nodes
            .iter()
            .find(|level_node| level_node.name == referenced);
        let source_output = source.and_then(|source| the_output_giving_out(source, row));
        if source_output.is_none() && row.keeps_any_other_reference {
            continue;
        }
        input_values.remove(row.input);
        any_wired = true;
        if let (Some(target), Some(source), Some(source_output)) =
            (target.as_deref(), source, source_output)
        {
            links.push(a_link(
                &source.link_name,
                source_output,
                target,
                row.wired_input,
            ));
        }
    }
    drop_the_input_values_when_empty(node);
    any_wired
}

/// The output `source` gives itself out on to the input of `row`: its connection output, or
/// the `value` of a `GetVariable` handing one on to an input that takes only a wire.
fn the_output_giving_out(
    source: &LevelNode,
    row: &InputThatHeldANodeReference,
) -> Option<&'static str> {
    if source.node_type == GET_VARIABLE_NODE_TYPE && !row.keeps_any_other_reference {
        return Some(GET_VARIABLE_VALUE_OUTPUT);
    }
    NODE_TYPES_GIVING_THEMSELVES_OUT
        .iter()
        .find(|giving| giving.node_type == source.node_type)
        .map(|giving| giving.connection_output)
}

#[cfg(test)]
mod tests;
