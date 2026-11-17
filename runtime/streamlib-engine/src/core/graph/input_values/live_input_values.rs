// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values set live on a stream's inputs, held apart from its graph so a live set changes no
//! node, link or port, and the value each input reads while no link feeds it: its live value,
//! else its saved one, else its declared default.

use std::collections::BTreeMap;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::core::descriptors::NodeKind;
use crate::core::graph::{Graph, GraphNode, PortInfo};

/// One input a value is set live on: its node's UUID, its name, and the member of a bundle input
/// it sets, `None` for the whole input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveInputValueKey {
    /// The node, by UUID.
    pub node: Uuid,
    /// The input, by name.
    pub input: String,
    /// The member of a bundle input, `None` for the whole input.
    pub member: Option<String>,
}

/// The values set live on one stream's inputs, keyed by node UUID, input and member. A set takes
/// no graph lock, so a running stream's live values change while its graph is read.
#[derive(Debug, Default)]
pub struct LiveInputValuesOfAStream {
    values: RwLock<BTreeMap<LiveInputValueKey, Value>>,
}

impl LiveInputValuesOfAStream {
    /// Set `value` live on input `input` of `node`, `None` taking the live value off, answering
    /// the live value it held before.
    pub fn set(&self, node: Uuid, input: &str, value: Option<Value>) -> Option<Value> {
        let key = LiveInputValueKey {
            node,
            input: input.to_owned(),
            member: None,
        };
        let mut values = self.values.write();
        match value {
            Some(value) => values.insert(key, value),
            None => values.remove(&key),
        }
    }

    /// The value set live on input `input` of `node`; `None` when none is.
    pub fn value_of(&self, node: Uuid, input: &str) -> Option<Value> {
        self.values
            .read()
            .get(&LiveInputValueKey {
                node,
                input: input.to_owned(),
                member: None,
            })
            .cloned()
    }

    /// Each value set live on a whole input of `node`, by input.
    pub fn values_of_the_node(&self, node: Uuid) -> Map<String, Value> {
        self.values
            .read()
            .iter()
            .filter(|(key, _)| key.node == node && key.member.is_none())
            .map(|(key, value)| (key.input.clone(), value.clone()))
            .collect()
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

/// Whether a link of `graph` feeds input `input` of `node`.
pub(crate) fn a_link_feeds_the_input(graph: &Graph, node: &GraphNode, input: &str) -> bool {
    graph
        .traversal()
        .e(())
        .iter()
        .any(|link| link.to_port().node_run_id() == &node.id && link.to_port().port_name() == input)
}

/// The pin values `node` of `graph` holds, each value set live on one of its inputs over the
/// saved one.
pub(crate) fn the_pin_values_with_the_live_ones_over_them(
    graph: &Graph,
    node: &GraphNode,
) -> Map<String, Value> {
    let mut pin_values = node.pins.clone();
    pin_values.extend(graph.live_input_values().values_of_the_node(node.node_uuid));
    pin_values
}

/// How a value set live on one input of a running node is held: apart from the graph, for an
/// input the node reads each run, or on the graph, for a setting, a config field exposed as an
/// input, or a node the running graph does not hold, which takes the path a graph edit takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveInputValueHolding {
    /// Held in the stream's live values; no node, link or port changes.
    ApartFromTheGraph,
    /// Made on the graph as an edit of it.
    OnTheGraph,
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
