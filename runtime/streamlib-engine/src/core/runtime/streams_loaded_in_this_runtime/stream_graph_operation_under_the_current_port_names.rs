// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph operation as a stream applies it: each input value and link end it names under the
//! current name of the port its node type declares, so an older edit, or a caller, naming a port
//! by a name it had reaches the port, and what is applied, answered and recorded holds the name
//! it has now.

use streamlib_project_repository::{
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphOperation,
};
use uuid::Uuid;

use crate::core::PortDirection;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::{GraphNodePorts, PortInfo};
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
                .map(|(inputs, outputs)| (node.id, GraphNodePorts { inputs, outputs })),
            _ => None,
        };
        self.compiler.scope_reading(|graph| {
            the_operation_under_the_current_port_names(graph_operation, |node| match &node_added {
                Some((added, ports)) if *added == node => Some(ports.clone()),
                _ => graph.node_by_uuid(node).map(|held| held.ports.clone()),
            })
        })
    }
}

/// `graph_operation` with each input value and link end it names under the current name of its
/// port, each node's ports as `ports_of` answers them by its UUID; a name on a node `ports_of`
/// does not answer, or that finds no port, is left as given.
pub(crate) fn the_operation_under_the_current_port_names(
    graph_operation: StreamGraphOperation,
    ports_of: impl Fn(Uuid) -> Option<GraphNodePorts>,
) -> StreamGraphOperation {
    let current_name = |node: Uuid, name: String, direction: PortDirection| -> String {
        let Some(ports) = ports_of(node) else {
            return name;
        };
        let ports = match direction {
            PortDirection::Input => &ports.inputs,
            PortDirection::Output => &ports.outputs,
        };
        PortInfo::the_one_a_name_finds(ports, &name).map_or(name, |port| port.name.clone())
    };
    let current_link = |link: StreamGraphDocumentLink| StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            port: current_name(link.source.node, link.source.port, PortDirection::Output),
            node: link.source.node,
        },
        target: StreamGraphDocumentLinkEnd {
            port: current_name(link.target.node, link.target.port, PortDirection::Input),
            node: link.target.node,
        },
    };
    match graph_operation {
        StreamGraphOperation::AddNode { mut node, links } => {
            if let Some(ports) = ports_of(node.id) {
                node.input_values = ports
                    .input_values_under_the_current_names(std::mem::take(&mut node.input_values));
            }
            StreamGraphOperation::AddNode {
                node,
                links: links.into_iter().map(current_link).collect(),
            }
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
            from,
            to,
        } => StreamGraphOperation::SetNodeInputValue {
            input: current_name(node, input, PortDirection::Input),
            node,
            from,
            to,
        },
        other => other,
    }
}
