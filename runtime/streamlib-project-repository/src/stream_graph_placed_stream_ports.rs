// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ports of a node placing a stream are its stream's `GraphInput` and `GraphOutput` nodes.
//! On disk, a link onto one names the interface node by its UUID, so a port renamed inside the
//! placed stream keeps every link onto it; read, the link names the port by the name the
//! interface node declares now.

use std::collections::HashMap;

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::stream_document_upgrade::NODE_INPUT_VALUES_KEY;
use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, node_name_of, node_uuid_of,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlacedStreamPortDirection {
    Input,
    Output,
}

impl PlacedStreamPortDirection {
    /// Each end of a link, by its key, with the direction of the port it meets on the node at
    /// that end.
    pub(crate) const OF_EACH_LINK_END: [(&'static str, Self); 2] =
        [("target", Self::Input), ("source", Self::Output)];

    fn interface_node_type(self) -> &'static str {
        match self {
            Self::Input => GRAPH_INPUT_NODE_TYPE,
            Self::Output => GRAPH_OUTPUT_NODE_TYPE,
        }
    }

    fn of_the_interface_node_type(node_type: &str) -> Option<Self> {
        [Self::Input, Self::Output]
            .into_iter()
            .find(|direction| direction.interface_node_type() == node_type)
    }
}

/// Each `GraphInput` and `GraphOutput` node of a placed stream, by UUID, with the port it
/// declares on the node placing the stream.
pub(crate) struct PortsAPlacedStreamDeclares {
    port_of_each_interface_node: Vec<(Uuid, PlacedStreamPortDirection, String)>,
}

impl PortsAPlacedStreamDeclares {
    /// The ports `nodes`, the nodes of a placed stream's own level, declare.
    pub(crate) fn of<'node>(nodes: impl IntoIterator<Item = &'node Map<String, Value>>) -> Self {
        let port_of_each_interface_node = nodes
            .into_iter()
            .filter_map(|node_object| {
                let direction = PlacedStreamPortDirection::of_the_interface_node_type(
                    node_object.get("type")?.as_str()?,
                )?;
                let port = node_object
                    .get(NODE_INPUT_VALUES_KEY)?
                    .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)?
                    .as_str()?;
                Some((node_uuid_of(node_object)?, direction, port.to_owned()))
            })
            .collect();
        Self {
            port_of_each_interface_node,
        }
    }

    /// These ports, followed by `others`: a name or an interface node both declare is found
    /// among these.
    pub(crate) fn followed_by(mut self, others: Self) -> Self {
        self.port_of_each_interface_node
            .extend(others.port_of_each_interface_node);
        self
    }

    pub(crate) fn declares(&self, direction: PlacedStreamPortDirection, port: &str) -> bool {
        self.interface_node_declaring(direction, port).is_some()
    }

    pub(crate) fn interface_node_declaring(
        &self,
        direction: PlacedStreamPortDirection,
        port: &str,
    ) -> Option<Uuid> {
        self.port_of_each_interface_node
            .iter()
            .find(|(_, declared_direction, declared_port)| {
                *declared_direction == direction && declared_port == port
            })
            .map(|(interface_node, ..)| *interface_node)
    }

    pub(crate) fn port_of(
        &self,
        direction: PlacedStreamPortDirection,
        interface_node: Uuid,
    ) -> Option<&str> {
        self.port_of_each_interface_node
            .iter()
            .find(|(declared_by, declared_direction, _)| {
                *declared_by == interface_node && *declared_direction == direction
            })
            .map(|(_, _, port)| port.as_str())
    }
}

/// The stream a node placing one holds, its input value `stream`: `{"asset", "from"?}`.
pub(crate) fn the_stream_a_placing_node_holds(node_object: &Map<String, Value>) -> Option<&Value> {
    (node_object.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE))
        .then(|| {
            node_object
                .get(NODE_INPUT_VALUES_KEY)?
                .get(STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY)
        })
        .flatten()
        .filter(|held_stream| !held_stream.is_null())
}

/// Each end of `links`, links of the level holding `nodes`, on a node placing a stream, its
/// port given the name `renamed_port` gives it from the ports `ports_of` reads that node's
/// stream to declare; an end it gives none, or on a node whose stream does not read, is left as
/// it is.
pub(crate) fn each_end_on_a_placing_node_renamed<Refusal>(
    nodes: &[Value],
    links: &mut [Value],
    ports_of: &mut impl FnMut(&Map<String, Value>) -> Option<PortsAPlacedStreamDeclares>,
    renamed_port: &mut impl FnMut(
        &PortsAPlacedStreamDeclares,
        &Map<String, Value>,
        PlacedStreamPortDirection,
        &str,
    ) -> Result<Option<String>, Refusal>,
) -> Result<(), Refusal> {
    let mut placing_nodes = Vec::new();
    for node_object in nodes.iter().filter_map(Value::as_object) {
        if the_stream_a_placing_node_holds(node_object).is_none() {
            continue;
        }
        if let Some(ports) = ports_of(node_object) {
            placing_nodes.push((node_object, ports));
        }
    }
    if placing_nodes.is_empty() {
        return Ok(());
    }
    for link in links.iter_mut() {
        for (end_key, direction) in PlacedStreamPortDirection::OF_EACH_LINK_END {
            let Some(end) = link.get_mut(end_key).and_then(Value::as_object_mut) else {
                continue;
            };
            let Some(named) = end.get("node").and_then(Value::as_str) else {
                continue;
            };
            let named_uuid = Uuid::parse_str(named).ok();
            let Some((placing_node, ports)) = placing_nodes.iter().find(|(node_object, _)| {
                named_uuid.is_some_and(|named| node_uuid_of(node_object) == Some(named))
                    || node_name_of(node_object) == Some(named)
            }) else {
                continue;
            };
            let Some(port) = end.get("port").and_then(Value::as_str) else {
                continue;
            };
            if let Some(renamed) = renamed_port(ports, placing_node, direction, port)? {
                end.insert("port".to_owned(), Value::String(renamed));
            }
        }
    }
    Ok(())
}

/// Each link end, at any depth of `value`, on a node placing a stream, its port named by the
/// UUID of the interface node declaring it in the ports `ports_of` reads the node's stream to
/// declare; a port no interface node declares is left as it is. The placing nodes are those
/// `value` holds and `placing_nodes_beside`, each by its UUID with the stream it holds.
pub(crate) fn each_link_onto_a_placed_stream_named_by_its_interface_node(
    value: &mut Value,
    placing_nodes_beside: &[(Uuid, Value)],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
) {
    each_link_end_on_a_placing_node_renamed(
        value,
        placing_nodes_beside,
        ports_of,
        &|ports, direction, port| {
            ports
                .interface_node_declaring(direction, port)
                .map(|interface_node| interface_node.to_string())
        },
    );
}

/// Each link end, at any depth of `value`, on a node placing a stream that names its port by
/// the UUID of an interface node, named by the port that node declares in the ports `ports_of`
/// reads the node's stream to declare; an end naming an interface node the stream does not hold
/// is left as it is. The placing nodes are those `value` holds and `placing_nodes_beside`.
pub(crate) fn each_link_onto_a_placed_stream_named_by_its_port(
    value: &mut Value,
    placing_nodes_beside: &[(Uuid, Value)],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
) {
    each_link_end_on_a_placing_node_renamed(
        value,
        placing_nodes_beside,
        ports_of,
        &|ports, direction, port| {
            Uuid::parse_str(port)
                .ok()
                .and_then(|interface_node| ports.port_of(direction, interface_node))
                .map(str::to_owned)
        },
    );
}

/// Each node placing a stream that `value` holds at any depth, by its UUID, with the stream it
/// holds.
pub(crate) fn each_placing_node_in(value: &Value, placing_nodes: &mut Vec<(Uuid, Value)>) {
    match value {
        Value::Object(object) => {
            if let (Some(held_stream), Some(own)) = (
                the_stream_a_placing_node_holds(object),
                node_uuid_of(object),
            ) {
                placing_nodes.push((own, held_stream.clone()));
            }
            for entry in object.values() {
                each_placing_node_in(entry, placing_nodes);
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                each_placing_node_in(entry, placing_nodes);
            }
        }
        _ => {}
    }
}

fn each_link_end_on_a_placing_node_renamed(
    value: &mut Value,
    placing_nodes_beside: &[(Uuid, Value)],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
    renamed_port: &dyn Fn(
        &PortsAPlacedStreamDeclares,
        PlacedStreamPortDirection,
        &str,
    ) -> Option<String>,
) {
    let mut placing_nodes = placing_nodes_beside.to_vec();
    each_placing_node_in(value, &mut placing_nodes);
    if placing_nodes.is_empty() {
        return;
    }
    let mut ports_of_each_held_stream: Vec<(Value, Option<PortsAPlacedStreamDeclares>)> =
        Vec::new();
    let mut ports_of_each_placing_node = HashMap::new();
    for (placing_node, held_stream) in placing_nodes {
        let read_at = match ports_of_each_held_stream
            .iter()
            .position(|(read, _)| *read == held_stream)
        {
            Some(read_at) => read_at,
            None => {
                let ports = ports_of(&held_stream);
                ports_of_each_held_stream.push((held_stream, ports));
                ports_of_each_held_stream.len() - 1
            }
        };
        ports_of_each_placing_node.insert(placing_node, read_at);
    }
    each_link_end_renamed(value, &mut |placing_node, direction, port| {
        let read_at = ports_of_each_placing_node.get(&placing_node)?;
        renamed_port(
            ports_of_each_held_stream[*read_at].1.as_ref()?,
            direction,
            port,
        )
    });
}

/// Each link, at any depth of `value`, an object holding a `source` and a `target` that each
/// name a node, with each end's port given the name `renamed_port` gives it.
fn each_link_end_renamed(
    value: &mut Value,
    renamed_port: &mut impl FnMut(Uuid, PlacedStreamPortDirection, &str) -> Option<String>,
) {
    match value {
        Value::Object(object) => {
            let is_a_link =
                PlacedStreamPortDirection::OF_EACH_LINK_END
                    .iter()
                    .all(|(end_key, _)| {
                        object
                            .get(*end_key)
                            .is_some_and(|end| end.get("node").is_some())
                    });
            if is_a_link {
                for (end_key, direction) in PlacedStreamPortDirection::OF_EACH_LINK_END {
                    let Some(end) = object.get_mut(end_key).and_then(Value::as_object_mut) else {
                        continue;
                    };
                    let (Some(node), Some(port)) = (
                        end.get("node")
                            .and_then(Value::as_str)
                            .and_then(|node| Uuid::parse_str(node).ok()),
                        end.get("port").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    if let Some(renamed) = renamed_port(node, direction, port) {
                        end.insert("port".to_owned(), Value::String(renamed));
                    }
                }
            }
            for entry in object.values_mut() {
                each_link_end_renamed(entry, renamed_port);
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                each_link_end_renamed(entry, renamed_port);
            }
        }
        _ => {}
    }
}

/// What `with_them` makes of the nodes and links of `level`, a graph document or a fragment;
/// `None` when the level holds no list of links.
pub(crate) fn with_the_nodes_and_links_of_the_level<Made>(
    level: &mut Map<String, Value>,
    with_them: impl FnOnce(&mut [Value], &mut [Value]) -> Made,
) -> Option<Made> {
    let mut links = std::mem::take(level.get_mut("links")?.as_array_mut()?);
    let mut no_nodes = Vec::new();
    let nodes = match level.get_mut("nodes") {
        Some(Value::Array(nodes)) => nodes,
        _ => &mut no_nodes,
    };
    let made = with_them(nodes, &mut links);
    if let Some(Value::Array(held)) = level.get_mut("links") {
        *held = links;
    }
    Some(made)
}

/// The refusal of a link end on the node `placing_node` naming, by its UUID, the interface node
/// `interface_node` its stream does not hold.
pub(crate) fn the_refusal_of_an_interface_node_the_placed_stream_does_not_hold(
    placing_node: &Map<String, Value>,
    direction: PlacedStreamPortDirection,
    interface_node: Uuid,
) -> String {
    format!(
        "a link onto node `{}` names its port by the `{}` node {interface_node}, which the \
         stream it places does not hold",
        node_name_of(placing_node)
            .map(str::to_owned)
            .or_else(|| node_uuid_of(placing_node).map(|own| own.to_string()))
            .unwrap_or_default(),
        direction
            .interface_node_type()
            .rsplit(':')
            .next()
            .unwrap_or_default(),
    )
}
