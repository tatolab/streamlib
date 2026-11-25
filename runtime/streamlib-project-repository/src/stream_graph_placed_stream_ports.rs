// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ports of a node placing a stream are its stream's `GraphInput` and `GraphOutput` nodes.
//! On disk, a link onto one names the interface node by its UUID, so a port renamed inside the
//! placed stream keeps every link onto it; read, the link names the port by the name the
//! interface node declares now.

use std::convert::Infallible;

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

    fn interface_node_kind(self) -> &'static str {
        match self {
            Self::Input => "GraphInput",
            Self::Output => "GraphOutput",
        }
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
                let direction = match node_object.get("type").and_then(Value::as_str) {
                    Some(GRAPH_INPUT_NODE_TYPE) => PlacedStreamPortDirection::Input,
                    Some(GRAPH_OUTPUT_NODE_TYPE) => PlacedStreamPortDirection::Output,
                    _ => return None,
                };
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
            let Some((placing_node, ports)) = placing_nodes.iter().find(|(node_object, _)| {
                node_uuid_of(node_object).is_some_and(|own| own.to_string() == named)
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

/// Each end of `links` on a node placing a stream named by the UUID of the interface node
/// declaring its port, at the level holding `nodes` and at every level of a graph generator's
/// stored output it holds; a port no interface node declares is left as it is.
pub(crate) fn each_link_onto_a_placed_stream_named_by_its_interface_node(
    nodes: &mut [Value],
    links: &mut [Value],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
) {
    each_level_renamed(nodes, links, ports_of, &mut |ports, direction, port| {
        ports
            .interface_node_declaring(direction, port)
            .map(|interface_node| interface_node.to_string())
    });
}

/// Each end of `links` on a node placing a stream that names its port by the UUID of an
/// interface node named by the port that node declares, at the level holding `nodes` and at
/// every level of a graph generator's stored output it holds; an end naming an interface node
/// the stream does not hold is left as it is.
pub(crate) fn each_link_onto_a_placed_stream_named_by_its_port(
    nodes: &mut [Value],
    links: &mut [Value],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
) {
    each_level_renamed(nodes, links, ports_of, &mut |ports, direction, port| {
        Uuid::parse_str(port)
            .ok()
            .and_then(|interface_node| ports.port_of(direction, interface_node))
            .map(str::to_owned)
    });
}

fn each_level_renamed(
    nodes: &mut [Value],
    links: &mut [Value],
    ports_of: &mut impl FnMut(&Value) -> Option<PortsAPlacedStreamDeclares>,
    renamed_port: &mut impl FnMut(
        &PortsAPlacedStreamDeclares,
        PlacedStreamPortDirection,
        &str,
    ) -> Option<String>,
) {
    let Ok(()) = each_end_on_a_placing_node_renamed::<Infallible>(
        nodes,
        links,
        &mut |node_object| ports_of(the_stream_a_placing_node_holds(node_object)?),
        &mut |ports, _, direction, port| Ok(renamed_port(ports, direction, port)),
    );
    for node_object in nodes.iter_mut().filter_map(Value::as_object_mut) {
        if node_object.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE) {
            continue;
        }
        let Some(Value::Object(stored_output)) = node_object.get_mut("fragment") else {
            continue;
        };
        if !matches!(stored_output.get("links"), Some(Value::Array(_))) {
            continue;
        }
        let Some(Value::Array(mut stored_links)) = stored_output.remove("links") else {
            continue;
        };
        if let Some(Value::Array(stored_nodes)) = stored_output.get_mut("nodes") {
            each_level_renamed(stored_nodes, &mut stored_links, ports_of, renamed_port);
        }
        stored_output.insert("links".to_owned(), Value::Array(stored_links));
    }
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
        node_name_of(placing_node).unwrap_or_default(),
        direction.interface_node_kind(),
    )
}
