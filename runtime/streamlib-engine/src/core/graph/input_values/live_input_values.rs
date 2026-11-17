// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values set live on a stream's inputs, held apart from its graph so a live set changes no
//! node, link or port, and the value each input reads while no link feeds it: its live value,
//! else its saved one, else its declared default.

use std::collections::{BTreeMap, HashMap};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::core::descriptors::NodeKind;
use crate::core::graph::{Graph, GraphNode, PortInfo};
use crate::core::pubsub::RuntimeEvent;

/// The values set live on one stream's inputs, by node UUID and then input. A set takes no graph
/// write lock, so a running stream's live values change while its graph is read.
#[derive(Debug, Default)]
pub struct LiveInputValuesOfAStream {
    values: RwLock<HashMap<Uuid, BTreeMap<String, Value>>>,
}

impl LiveInputValuesOfAStream {
    /// Set `value` live on input `input` of `node`, `None` taking the live value off, answering
    /// the live value it held before.
    pub fn set(&self, node: Uuid, input: &str, value: Option<Value>) -> Option<Value> {
        let mut values = self.values.write();
        match value {
            Some(value) => values
                .entry(node)
                .or_default()
                .insert(input.to_owned(), value),
            None => {
                let values_of_the_node = values.get_mut(&node)?;
                let before = values_of_the_node.remove(input);
                if values_of_the_node.is_empty() {
                    values.remove(&node);
                }
                before
            }
        }
    }

    /// The value set live on input `input` of `node`; `None` when none is.
    pub fn value_of(&self, node: Uuid, input: &str) -> Option<Value> {
        self.values
            .read()
            .get(&node)
            .and_then(|values_of_the_node| values_of_the_node.get(input))
            .cloned()
    }

    /// Each value set live on an input of `node`, by input.
    pub fn values_of_the_node(&self, node: Uuid) -> Map<String, Value> {
        self.values
            .read()
            .get(&node)
            .into_iter()
            .flatten()
            .map(|(input, value)| (input.clone(), value.clone()))
            .collect()
    }

    /// Forget every value set live on an input of `node`, as the node leaves the graph.
    pub fn forget_the_node(&self, node: Uuid) {
        self.values.write().remove(&node);
    }
}

/// Where the value an input reads comes from, as `graph` renders it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InputValueComesFrom {
    /// A value set live, which no document holds.
    Live,
    /// The value the stream's graph document holds.
    Saved,
    /// The default the input's port declares.
    Default,
    /// A link feeds the input, so it reads what arrives and holds no value.
    Wire,
}

/// The value one input reads while no link feeds it and where it comes from: `value` absent and
/// `value_comes_from` `wire` when a link feeds it, `value` null and no `value_comes_from` when it
/// reads nothing.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema,
)]
pub struct InputValueRead {
    /// The value it reads; absent when a link feeds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub value: Option<Value>,
    /// Where the value comes from; absent when it reads nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_comes_from: Option<InputValueComesFrom>,
}

/// Whether input `port` of a node running as `node_kind` takes a stored value: what its pin type
/// holds and, on a node that is no function or shader node, only a Channels input; on a port
/// that names no pin type, any input of a function node.
pub(crate) fn the_input_port_takes_a_value(node_kind: Option<NodeKind>, port: &PortInfo) -> bool {
    let read_whole_by_its_node = matches!(
        node_kind,
        Some(NodeKind::Function { .. } | NodeKind::Shader)
    );
    match &port.pin {
        Some(pin) if !pin.pin_type.holds_a_stored_value() => false,
        Some(_) if read_whole_by_its_node => true,
        None if matches!(node_kind, Some(NodeKind::Function { .. })) => true,
        _ => port.family.family == crate::core::descriptors::PortFamily::Channels,
    }
}

/// The value input `port` of `node` in `graph` reads, `is_linked` when a link feeds it: wire;
/// else its live value, its saved one, which is its config field's for a config field the node
/// exposes as an input, or its declared default; else nothing. `None` for an input that takes
/// no stored value and no link feeds.
pub(crate) fn the_value_an_input_reads(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    is_linked: bool,
) -> Option<InputValueRead> {
    if is_linked {
        return Some(InputValueRead {
            value: None,
            value_comes_from: Some(InputValueComesFrom::Wire),
        });
    }
    let exposed_config_field = node.is_an_exposed_config_field_input(&port.name);
    if !exposed_config_field && !the_input_port_takes_a_value(node.node_kind(), port) {
        return None;
    }
    let saved = if exposed_config_field {
        node.config
            .as_ref()
            .and_then(|config| config.get(&port.name))
            .filter(|value| !value.is_null())
            .cloned()
    } else {
        node.pins.get(&port.name).cloned()
    };
    let read = graph
        .live_input_values()
        .value_of(node.node_uuid, &port.name)
        .map(|value| (value, InputValueComesFrom::Live))
        .or_else(|| saved.map(|value| (value, InputValueComesFrom::Saved)))
        .or_else(|| {
            port.pin_default
                .clone()
                .map(|value| (value, InputValueComesFrom::Default))
        });
    Some(match read {
        Some((value, value_comes_from)) => InputValueRead {
            value: Some(value),
            value_comes_from: Some(value_comes_from),
        },
        None => InputValueRead {
            value: Some(Value::Null),
            value_comes_from: None,
        },
    })
}

/// The `InputValueDidChange` announcing what input `input` of `node` in `graph` reads now; `None`
/// when the node has no such input.
pub(crate) fn the_input_value_change(
    graph: &Graph,
    node: &GraphNode,
    input: &str,
) -> Option<RuntimeEvent> {
    let port = node.input_port(input)?;
    Some(RuntimeEvent::InputValueDidChange {
        node: node.node_uuid,
        input: input.to_owned(),
        value_read: the_value_an_input_reads(
            graph,
            node,
            port,
            graph.link_into(&node.id, input).is_some(),
        ),
    })
}

/// The pin values `node` of `graph` reads, each value set live on one of its inputs over the
/// saved one.
pub(crate) fn the_pin_values_with_the_live_ones_over_them(
    graph: &Graph,
    node: &GraphNode,
) -> Map<String, Value> {
    let mut pin_values = node.pins.clone();
    pin_values.extend(graph.live_input_values().values_of_the_node(node.node_uuid));
    pin_values
}

/// One value a live set makes: the node, by UUID, its input, and the value, `None` taking the
/// input's live value off.
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueToSetLive {
    /// The node, by UUID.
    pub node: Uuid,
    /// The input, by name.
    pub input: String,
    /// The value, `None` taking the live value off.
    pub value: Option<Value>,
}

/// What a live set of values made: each set apart from the graph, or none, because one is held
/// on the graph and the set takes the path a graph edit takes.
#[derive(Debug, Clone, PartialEq)]
pub enum InputValuesSetLive {
    /// Each value set live apart from the graph, in the order asked; no node, link or port
    /// changed.
    ApartFromTheGraph(Vec<InputValueSetLive>),
    /// One value is held on the graph: a setting, a config field exposed as an input, an input
    /// its node does not have, or a node the graph does not hold. None was set.
    OnTheGraph,
}

/// Why a live set of values was refused, every value left as it was.
#[derive(Debug)]
pub enum LiveInputValueSetRefusal {
    /// The input of the value at `set_index` does not take it.
    TheInputDoesNotTakeIt {
        /// The index of the refused value among those asked.
        set_index: usize,
        /// Why its input does not take it.
        refusal: super::InputValueRefusal,
    },
    /// A helper or island reading one of the values was not handed it.
    NotHandedOver(crate::core::Error),
}

/// One value set live apart from the graph: the input, the live value it held before, the
/// saved value under it, and the output of the link that feeds the input, by node UUID and port,
/// when one does.
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueSetLive {
    /// The input, by name.
    pub input: String,
    /// The value set live on it before the set; `None` when none was.
    pub live_value_before: Option<Value>,
    /// The saved value under the live one; `None` when the graph document holds none.
    pub saved_value: Option<Value>,
    /// The output a link feeds the input from, as `(node UUID, port)`, which overrides the value.
    pub linked_from: Option<(Uuid, String)>,
}
