// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port a node type renamed, met under a name it had: in a request, which is then made and
//! answered under the current name, and in a stream document a transaction edits, whose values
//! and links under a former name move to the current one in the same commit, so the next save
//! holds only current names.

use streamlib::sdk::error::PortDirection;

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
            .and_then(|node| {
                stream_operations.the_current_name_a_former_port_name_finds(
                    &node.node_type,
                    direction,
                    &name,
                )
            })
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
/// to the port's current name, as `stream_operations` resolves each node's type: a value under a
/// former name is taken off, with any under the current name, and the current name set to the
/// value the input reads, the one held under the current name when there is one; a link is
/// taken out and put back between the ports' current names, unless one already is.
///
/// A running stream holds each of these values and links once, under the current name, and an
/// undo or a redo applies them to it too, last first for an undo; each value is taken off before
/// it is set in both orders, so the input ends reading what it read before.
pub(super) fn the_operations_moving_each_former_port_name(
    document: &StreamGraphDocument,
    stream_operations: &dyn RuntimeOperations,
) -> Vec<StreamGraphOperation> {
    let nodes = document.nodes();
    let current_name = |node: Uuid, name: &str, direction: PortDirection| {
        nodes.iter().find(|held| held.id == node).and_then(|held| {
            stream_operations.the_current_name_a_former_port_name_finds(
                &held.node_type,
                direction,
                name,
            )
        })
    };
    let mut moves = Vec::new();
    for node in &nodes {
        for (name, value) in &node.input_values {
            let Some(current) = current_name(node.id, name, PortDirection::Input) else {
                continue;
            };
            let held_under_the_current_name = node.input_values.get(&current);
            if let Some(held) = held_under_the_current_name {
                moves.push(StreamGraphOperation::SetNodeInputValue {
                    node: node.id,
                    input: current.clone(),
                    from: Some(held.clone()),
                    to: None,
                });
            }
            moves.push(StreamGraphOperation::SetNodeInputValue {
                node: node.id,
                input: name.clone(),
                from: Some(value.clone()),
                to: None,
            });
            moves.push(StreamGraphOperation::SetNodeInputValue {
                node: node.id,
                input: current,
                from: None,
                to: Some(held_under_the_current_name.unwrap_or(value).clone()),
            });
        }
    }
    let links = document.links();
    let mut links_put_back: Vec<StreamGraphDocumentLink> = Vec::new();
    for link in &links {
        let source = current_name(link.source.node, &link.source.port, PortDirection::Output);
        let target = current_name(link.target.node, &link.target.port, PortDirection::Input);
        if source.is_none() && target.is_none() {
            continue;
        }
        let moved = StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: link.source.node,
                port: source.unwrap_or_else(|| link.source.port.clone()),
            },
            target: StreamGraphDocumentLinkEnd {
                node: link.target.node,
                port: target.unwrap_or_else(|| link.target.port.clone()),
            },
        };
        moves.push(StreamGraphOperation::Disconnect { link: link.clone() });
        if !links.contains(&moved) && !links_put_back.contains(&moved) {
            links_put_back.push(moved.clone());
            moves.push(StreamGraphOperation::Connect { link: moved });
        }
    }
    moves
}
