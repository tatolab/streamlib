// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port a node type renamed, met under a name it had: in a request, which is then made and
//! answered under the current name, and in a stream document a transaction edits, whose values
//! and links under a former name move to the current one in the same commit, so the next save
//! holds only current names.

use streamlib::sdk::error::PortDirection;
use streamlib::sdk::graph::GraphNodePorts;

use super::*;

/// `request` with each input and link end it names under the current name of its port, each
/// node, by UUID or by the `as` of an earlier add in `node_by_label`, as `staged_graph` holds it
/// and `stream_operations` resolves its type. A name that is current, or finds no port, is left
/// as the caller sent it, for the request's own refusal to name.
pub(super) fn the_request_under_the_current_port_names(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &HashMap<String, Uuid>,
    stream_operations: &dyn RuntimeOperations,
) -> StreamGraphAction {
    let current_name = |node: &str, name: String, direction: PortDirection| -> String {
        node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())
            .and_then(|node| staged_graph.node(node).ok())
            .and_then(|node| stream_operations.the_ports_a_node_type_declares(&node.node_type))
            .and_then(|ports| ports.current_name(direction, &name).map(str::to_string))
            .unwrap_or(name)
    };
    let current_end =
        |end: StreamGraphActionLinkEnd, direction: PortDirection| StreamGraphActionLinkEnd {
            port: current_name(&end.node, end.port, direction),
            node: end.node,
        };
    match request {
        StreamGraphAction::SetPinValue {
            stream,
            level,
            node,
            input,
            value,
        } => StreamGraphAction::SetPinValue {
            input: current_name(&node, input, PortDirection::Input),
            stream,
            level,
            node,
            value,
        },
        StreamGraphAction::SetNodeInputValue {
            stream,
            level,
            node,
            input,
            value,
        } => StreamGraphAction::SetNodeInputValue {
            input: current_name(&node, input, PortDirection::Input),
            stream,
            level,
            node,
            value,
        },
        StreamGraphAction::Connect {
            stream,
            level,
            source,
            target,
        } => StreamGraphAction::Connect {
            source: current_end(source, PortDirection::Output),
            target: current_end(target, PortDirection::Input),
            stream,
            level,
        },
        StreamGraphAction::Disconnect {
            stream,
            level,
            source,
            target,
        } => StreamGraphAction::Disconnect {
            source: current_end(source, PortDirection::Output),
            target: current_end(target, PortDirection::Input),
            stream,
            level,
        },
        other => other,
    }
}

/// The operations that move each value and link end `document` holds under a name a port had
/// to the port's current name, as `stream_operations` resolves each node's type.
///
/// An input holding values under several of its names reads the one the engine's one order
/// reads ([`GraphNodePorts::inputs_holding_a_value_under_a_name_they_had`]): each of those
/// values is taken off, in that order, and the current name set to the one read. A link under a
/// name a port had is taken out, with every other link that names the same ports, and the link
/// between the current names put back once.
///
/// A running stream holds each value and link once, under the current names, and an undo or a
/// redo applies these to it too, last first for an undo; in both orders the input ends reading
/// what it read before, and the ports end joined by one link, as a link the running stream
/// already holds is not connected again and a disconnect under a name a port had of a link it
/// does not hold leaves it as it is.
pub(super) fn the_operations_moving_each_former_port_name(
    document: &StreamGraphDocument,
    stream_operations: &dyn RuntimeOperations,
) -> Vec<StreamGraphOperation> {
    let nodes = document.nodes();
    let ports_of_each_node: HashMap<Uuid, GraphNodePorts> = nodes
        .iter()
        .filter_map(|node| {
            stream_operations
                .the_ports_a_node_type_declares(&node.node_type)
                .map(|ports| (node.id, ports))
        })
        .collect();
    let mut moves = Vec::new();
    for node in &nodes {
        let Some(ports) = ports_of_each_node.get(&node.id) else {
            continue;
        };
        for moved in ports.inputs_holding_a_value_under_a_name_they_had(|name| {
            node.input_values.contains_key(name)
        }) {
            for name in &moved.names_in_the_order_read {
                moves.push(StreamGraphOperation::SetNodeInputValue {
                    node: node.id,
                    input: name.clone(),
                    from: node.input_values.get(name).cloned(),
                    to: None,
                });
            }
            moves.push(StreamGraphOperation::SetNodeInputValue {
                node: node.id,
                input: moved.current_name,
                from: None,
                to: node
                    .input_values
                    .get(&moved.names_in_the_order_read[0])
                    .cloned(),
            });
        }
    }
    let current_end =
        |end: &StreamGraphDocumentLinkEnd, direction: PortDirection| StreamGraphDocumentLinkEnd {
            node: end.node,
            port: ports_of_each_node
                .get(&end.node)
                .and_then(|ports| ports.current_name(direction, &end.port))
                .unwrap_or(&end.port)
                .to_string(),
        };
    let links = document.links();
    let mut links_under_former_names_of_each_link: Vec<(
        StreamGraphDocumentLink,
        Vec<StreamGraphDocumentLink>,
    )> = Vec::new();
    for link in &links {
        let moved = StreamGraphDocumentLink {
            source: current_end(&link.source, PortDirection::Output),
            target: current_end(&link.target, PortDirection::Input),
        };
        if moved == *link {
            continue;
        }
        match links_under_former_names_of_each_link
            .iter_mut()
            .find(|(held, _)| *held == moved)
        {
            Some((_, under_former_names)) => under_former_names.push(link.clone()),
            None => links_under_former_names_of_each_link.push((moved, vec![link.clone()])),
        }
    }
    for (moved, under_former_names) in links_under_former_names_of_each_link {
        if links.contains(&moved) {
            moves.push(StreamGraphOperation::Disconnect {
                link: moved.clone(),
            });
        }
        for link in under_former_names {
            moves.push(StreamGraphOperation::Disconnect { link });
        }
        moves.push(StreamGraphOperation::Connect { link: moved });
    }
    moves
}
