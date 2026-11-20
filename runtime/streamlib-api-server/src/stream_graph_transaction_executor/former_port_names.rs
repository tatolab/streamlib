// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port a node type renamed, met under a name it had: in a request, which is then made and
//! answered under the current name, and in a stream document a transaction edits, whose values
//! and links under a former name move to the current one in the same commit, so the next save
//! holds only current names.

use streamlib::sdk::error::PortDirection;
use streamlib::sdk::graph::GraphNodePorts;

use super::*;

/// The current name of the port `name` names in `direction` on a node of `node_type`, as
/// `stream_operations` resolves the type: the port's own name, also when `name` is one it had;
/// `name` as it is when it is current, finds no port, or no type is known.
pub(crate) fn the_current_name_of_a_port(
    name: String,
    direction: PortDirection,
    node_type: Option<String>,
    stream_operations: &dyn RuntimeOperations,
) -> String {
    node_type
        .and_then(|node_type| stream_operations.the_ports_a_node_type_declares(&node_type))
        .and_then(|ports| ports.current_name(direction, &name).map(str::to_string))
        .unwrap_or(name)
}

/// The current name of the port `name` names in `direction` on the node `node` names, by UUID
/// or by the `as` of an earlier add in `node_by_label`, as `staged_graph` holds it and
/// `stream_operations` resolves its type.
fn the_current_name_of_a_port_of_the_node(
    node: &str,
    name: String,
    direction: PortDirection,
    staged_graph: &StreamGraphDocument,
    node_by_label: &HashMap<String, Uuid>,
    stream_operations: &dyn RuntimeOperations,
) -> String {
    the_current_name_of_a_port(
        name,
        direction,
        node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())
            .and_then(|node| staged_graph.node(node).ok())
            .map(|node| node.node_type),
        stream_operations,
    )
}

/// The ends `source` and `target` of a link, each under the current name of its port, as
/// [`the_request_under_the_current_port_names`] names them.
pub(super) fn the_link_ends_under_their_current_port_names(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    staged_graph: &StreamGraphDocument,
    node_by_label: &HashMap<String, Uuid>,
    stream_operations: &dyn RuntimeOperations,
) -> (StreamGraphActionLinkEnd, StreamGraphActionLinkEnd) {
    let current_end =
        |end: StreamGraphActionLinkEnd, direction: PortDirection| StreamGraphActionLinkEnd {
            port: the_current_name_of_a_port_of_the_node(
                &end.node,
                end.port,
                direction,
                staged_graph,
                node_by_label,
                stream_operations,
            ),
            node: end.node,
            member: end.member,
        };
    (
        current_end(source, PortDirection::Output),
        current_end(target, PortDirection::Input),
    )
}

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
        the_current_name_of_a_port_of_the_node(
            node,
            name,
            direction,
            staged_graph,
            node_by_label,
            stream_operations,
        )
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
            member,
            value,
        } => StreamGraphAction::SetNodeInputValue {
            input: current_name(&node, input, PortDirection::Input),
            stream,
            level,
            node,
            member,
            value,
        },
        StreamGraphAction::Connect {
            stream,
            level,
            source,
            target,
        } => {
            let (source, target) = the_link_ends_under_their_current_port_names(
                source,
                target,
                staged_graph,
                node_by_label,
                stream_operations,
            );
            StreamGraphAction::Connect {
                source,
                target,
                stream,
                level,
            }
        }
        StreamGraphAction::Disconnect {
            stream,
            level,
            source,
            target,
        } => {
            let (source, target) = the_link_ends_under_their_current_port_names(
                source,
                target,
                staged_graph,
                node_by_label,
                stream_operations,
            );
            StreamGraphAction::Disconnect {
                source,
                target,
                stream,
                level,
            }
        }
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
fn the_operations_moving_each_former_port_name(
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
            for name in moved.names_held() {
                moves.push(StreamGraphOperation::SetNodeInputValue {
                    node: node.id,
                    input: name.clone(),
                    member: None,
                    from: node.input_values.get(name).cloned(),
                    to: None,
                });
            }
            moves.push(StreamGraphOperation::SetNodeInputValue {
                node: node.id,
                input: moved.current_name,
                member: None,
                from: None,
                to: node.input_values.get(&moved.name_read).cloned(),
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
            member: end.member.clone(),
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

/// Move each value and link end `document`, the graph document of the stream `stream_asset`,
/// holds under a name a port had to the port's current name, as `stream_operations` resolves
/// each node's type, recording each move in `moved`; answers whether anything moved.
pub(super) fn move_each_former_port_name(
    stream_asset: Uuid,
    document: &mut StreamGraphDocument,
    stream_operations: &dyn RuntimeOperations,
    moved: &mut Vec<ProjectOperation>,
) -> Result<bool, GraphTransactionRefusal> {
    let moves = the_operations_moving_each_former_port_name(document, stream_operations);
    let anything_moved = !moves.is_empty();
    for graph_operation in moves {
        document
            .apply(&graph_operation)
            .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
        moved.push(ProjectOperation::StreamGraph(
            StreamGraphOperationOnAStream {
                stream: stream_asset,
                graph_operation,
            },
        ));
    }
    Ok(anything_moved)
}

/// The graph document of each stream of the project that a stream of `streams` places, at any
/// depth, and that `streams` does not hold itself, with each former port name moved, by stream
/// asset: those a request can reach at a placed stream's level. Each is resolved by the loaded
/// stream that places it, the one whose node types its nodes run as, and only a document
/// something moved in is staged; each move is recorded in `moved`.
pub(super) fn the_placed_streams_with_each_former_port_name_moved(
    streams: &BTreeMap<String, StreamATransactionEdits>,
    project_repository: &ProjectRepository,
    moved: &mut Vec<ProjectOperation>,
) -> Result<BTreeMap<Uuid, StreamGraphDocument>, GraphTransactionRefusal> {
    let held_by_the_transaction: BTreeSet<Uuid> = streams
        .values()
        .map(|held| held.stream_location.stream_asset)
        .collect();
    let mut reached: BTreeSet<Uuid> = held_by_the_transaction.clone();
    let mut staged = BTreeMap::new();
    for held in streams.values() {
        let mut placed_to_visit: Vec<Uuid> =
            stream_assets_of_the_project_a_graph_document_places(&held.staged_graph.to_value())
                .into_iter()
                .collect();
        while let Some(placed_stream) = placed_to_visit.pop() {
            if !reached.insert(placed_stream) {
                continue;
            }
            let mut placed_graph = project_repository.stream_graph_document(placed_stream)?;
            placed_to_visit.extend(stream_assets_of_the_project_a_graph_document_places(
                &placed_graph.to_value(),
            ));
            if move_each_former_port_name(
                placed_stream,
                &mut placed_graph,
                held.stream_operations.as_ref(),
                moved,
            )? {
                staged.insert(placed_stream, placed_graph);
            }
        }
    }
    Ok(staged)
}
