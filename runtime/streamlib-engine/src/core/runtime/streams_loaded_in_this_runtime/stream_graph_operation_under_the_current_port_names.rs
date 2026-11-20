// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph operation as a stream applies it: each input value and link end it names under the
//! current name of the port its node type declares, so an older edit, or a caller, naming a port
//! by a name it had reaches the port, and what is applied, answered and recorded holds the name
//! it has now.

use streamlib_project_repository::{
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphOperation,
};
use uuid::Uuid;

use crate::core::PortDirection;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::GraphNodePorts;
use crate::core::runtime::LoadedStreamInThisRuntime;

impl LoadedStreamInThisRuntime {
    /// `graph_operation` with each input value and link end it names under the current name of
    /// its port, as this stream's graph holds each node and resolves the type of a node it adds.
    pub(super) fn the_operation_under_the_current_port_names(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> StreamGraphOperation {
        let node_added = match &graph_operation {
            StreamGraphOperation::AddNode { node, .. } => NodeClassImportPath::new(&node.node_type)
                .ok()
                .and_then(|node_type| self.node_types_this_stream_resolves().port_info(&node_type))
                .map(|ports| (node.id, ports)),
            _ => None,
        };
        self.compiler.scope_reading(|graph| {
            the_operation_under_the_current_port_names(graph_operation, |node| match &node_added {
                Some((added, ports)) if *added == node => Some(ports.clone()),
                _ => graph.node_by_uuid(node).map(|held| held.ports.clone()),
            })
        })
    }

    /// The node an add places, and the links it brings, each under the current port names, its
    /// own type read as this stream resolves it now: once an add has described a type the
    /// stream did not resolve before, its values and links are renamed by what it declares.
    pub(super) fn the_added_node_under_the_current_port_names(
        &self,
        node: StreamGraphDocumentNode,
        links: Vec<StreamGraphDocumentLink>,
    ) -> (StreamGraphDocumentNode, Vec<StreamGraphDocumentLink>) {
        let added = node.id;
        let ports_of_the_added = NodeClassImportPath::new(&node.node_type)
            .ok()
            .and_then(|node_type| self.node_types_this_stream_resolves().port_info(&node_type));
        self.compiler.scope_reading(|graph| {
            the_added_node_under_the_current_port_names(node, links, |node| {
                if node == added {
                    ports_of_the_added.clone()
                } else {
                    graph.node_by_uuid(node).map(|held| held.ports.clone())
                }
            })
        })
    }
}

/// `node` and the `links` it brings with each input value and link end under the current name
/// of its port, each node's ports as `ports_of` answers them by its UUID.
fn the_added_node_under_the_current_port_names(
    mut node: StreamGraphDocumentNode,
    links: Vec<StreamGraphDocumentLink>,
    ports_of: impl Fn(Uuid) -> Option<GraphNodePorts>,
) -> (StreamGraphDocumentNode, Vec<StreamGraphDocumentLink>) {
    if let Some(ports) = ports_of(node.id) {
        node.input_values =
            ports.input_values_under_the_current_names(std::mem::take(&mut node.input_values));
    }
    let links = links
        .into_iter()
        .map(|link| the_link_under_the_current_port_names(link, &ports_of))
        .collect();
    (node, links)
}

/// `link` with each end under the current name of its port, each node's ports as `ports_of`
/// answers them by its UUID.
fn the_link_under_the_current_port_names(
    link: StreamGraphDocumentLink,
    ports_of: &impl Fn(Uuid) -> Option<GraphNodePorts>,
) -> StreamGraphDocumentLink {
    let current_name =
        |end: StreamGraphDocumentLinkEnd, direction: PortDirection| StreamGraphDocumentLinkEnd {
            port: the_current_name(ports_of, end.node, end.port, direction),
            node: end.node,
            member: end.member,
        };
    StreamGraphDocumentLink {
        source: current_name(link.source, PortDirection::Output),
        target: current_name(link.target, PortDirection::Input),
    }
}

/// The current name of the `direction` port of node `node` that `name` finds, each node's ports
/// as `ports_of` answers them; `name` as given when nothing answers or no port is found.
fn the_current_name(
    ports_of: &impl Fn(Uuid) -> Option<GraphNodePorts>,
    node: Uuid,
    name: String,
    direction: PortDirection,
) -> String {
    ports_of(node)
        .and_then(|ports| ports.current_name(direction, &name).map(str::to_string))
        .unwrap_or(name)
}

/// `graph_operation` with each input value and link end it names under the current name of its
/// port, each node's ports as `ports_of` answers them by its UUID; a name on a node `ports_of`
/// does not answer, or that finds no port, is left as given.
pub(crate) fn the_operation_under_the_current_port_names(
    graph_operation: StreamGraphOperation,
    ports_of: impl Fn(Uuid) -> Option<GraphNodePorts>,
) -> StreamGraphOperation {
    let current_name = |node: Uuid, name: String, direction: PortDirection| {
        the_current_name(&ports_of, node, name, direction)
    };
    let current_link = |link| the_link_under_the_current_port_names(link, &ports_of);
    match graph_operation {
        StreamGraphOperation::AddNode { node, links } => {
            let (node, links) = the_added_node_under_the_current_port_names(node, links, &ports_of);
            StreamGraphOperation::AddNode { node, links }
        }
        StreamGraphOperation::Connect { link } => StreamGraphOperation::Connect {
            link: current_link(link),
        },
        StreamGraphOperation::Disconnect { link } => StreamGraphOperation::Disconnect {
            link: current_link(link),
        },
        StreamGraphOperation::SetPinValue {
            node,
            input,
            from,
            to,
        } => StreamGraphOperation::SetPinValue {
            input: current_name(node, input, PortDirection::Input),
            node,
            from,
            to,
        },
        StreamGraphOperation::SetNodeInputValue {
            node,
            input,
            member,
            from,
            to,
        } => StreamGraphOperation::SetNodeInputValue {
            input: current_name(node, input, PortDirection::Input),
            node,
            member,
            from,
            to,
        },
        other => other,
    }
}
