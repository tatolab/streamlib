// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A disconnect that leaves a slot of a pin group following its wires reached by no link, with a
//! later slot still linked, closes the gap in the same transaction: each later slot's links and
//! values move one slot down per gap, as disconnects, connects and value sets the transaction
//! records, so one undo puts every slot back where it was.

use streamlib::sdk::descriptors::{PinGroupDescriptor, PinGroupSlotCount, PinGroupSlotPort};

use super::*;

impl StreamGraphTransactionExecutor<'_> {
    /// Apply `request`, a disconnect, as [`Self::apply_one_request`] does, then close each gap
    /// it leaves in a pin group following its wires on either end's node, as the staged level
    /// then holds it; one move refused reverses those made live before it.
    pub(super) fn apply_a_disconnect_closing_the_gap_it_leaves(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        let StreamGraphAction::Disconnect {
            stream,
            level,
            source,
            target,
        } = &request
        else {
            return self
                .apply_one_request(request, stream_id, staged, InputValueCheck::MadeOnApply)
                .map(|applied| vec![applied]);
        };
        let (stream, level) = (*stream, level.clone());
        let ends = [source.node.clone(), target.node.clone()];
        let mut applied_by_the_disconnect = vec![self.apply_one_request(
            request,
            stream_id,
            staged,
            InputValueCheck::MadeOnApply,
        )?];
        let Some(stream_operations) = staged
            .streams
            .get(stream_id)
            .map(|edits| Arc::clone(&edits.stream_operations))
        else {
            return Ok(applied_by_the_disconnect);
        };
        let moves: Vec<StreamGraphAction> = {
            let Ok(drawn) = self
                .the_level_a_request_reaches(
                    stream_id,
                    &level,
                    &staged.streams,
                    &staged.staged_graph_of_each_stream_not_running,
                    staged.project_repository,
                )
                .and_then(|reached| reached.the_document_as_drawn().map_err(Into::into))
            else {
                return Ok(applied_by_the_disconnect);
            };
            let mut nodes: Vec<Uuid> = ends
                .iter()
                .filter_map(|end| {
                    staged
                        .node_by_label
                        .get(end)
                        .copied()
                        .or_else(|| Uuid::parse_str(end).ok())
                })
                .collect();
            nodes.dedup();
            nodes
                .into_iter()
                .filter_map(|node| drawn.node(node).ok())
                .flat_map(|node| {
                    let pin_groups =
                        stream_operations.the_pin_groups_a_node_type_declares(&node.node_type);
                    the_moves_closing_each_gap_of_the_node(&drawn, &node, &pin_groups)
                })
                .map(|move_one| move_one.into_an_action(stream, &level))
                .collect()
        };
        for move_one in moves {
            match self.apply_one_request(move_one, stream_id, staged, InputValueCheck::MadeOnApply)
            {
                Ok(applied) => applied_by_the_disconnect.push(applied),
                Err(refusal) => {
                    let applied_live = applied_by_the_disconnect
                        .into_iter()
                        .filter_map(|applied| Some((stream_id.to_owned(), applied.applied_live?)))
                        .collect();
                    self.reverse_what_was_applied_live(&staged.streams, applied_live);
                    return Err(refusal);
                }
            }
        }
        Ok(applied_by_the_disconnect)
    }
}

/// One step of moving a pin group's slot down.
#[derive(Debug, Clone, PartialEq)]
enum ASlotMoved {
    Disconnect(StreamGraphDocumentLink),
    Connect(StreamGraphDocumentLink),
    SetInputValue {
        node: Uuid,
        input: String,
        value: Option<Value>,
    },
}

impl ASlotMoved {
    /// This step as a transaction's action on the stream `stream` at `level`.
    fn into_an_action(self, stream: Uuid, level: &[Uuid]) -> StreamGraphAction {
        let end = |end: StreamGraphDocumentLinkEnd| StreamGraphActionLinkEnd {
            node: end.node.to_string(),
            port: end.port,
            member: end.member,
        };
        match self {
            Self::Disconnect(link) => StreamGraphAction::Disconnect {
                stream,
                level: level.to_vec(),
                source: end(link.source),
                target: end(link.target),
            },
            Self::Connect(link) => StreamGraphAction::Connect {
                stream,
                level: level.to_vec(),
                source: end(link.source),
                target: end(link.target),
            },
            Self::SetInputValue { node, input, value } => StreamGraphAction::SetNodeInputValue {
                stream,
                level: level.to_vec(),
                node: node.to_string(),
                input,
                member: None,
                value,
            },
        }
    }
}

/// The steps closing each gap of each pin group following its wires on `node`, as `level`
/// holds it: each slot a link reaches past a slot none reaches moves down to the first free
/// one, its links taken out and made again into the lower slot, and the values of its inputs
/// moved with it, each value the lower slot held taken off first.
fn the_moves_closing_each_gap_of_the_node(
    level: &StreamGraphDocument,
    node: &StreamGraphDocumentNode,
    pin_groups: &[PinGroupDescriptor],
) -> Vec<ASlotMoved> {
    let links = level.links_touching(node.id);
    let slot_of = |end: &StreamGraphDocumentLinkEnd, group: &PinGroupDescriptor| {
        (end.node == node.id && group.holds_the_slot_port(&end.port))
            .then(|| PinGroupSlotPort::named(&end.port).map(|port| port.slot))
            .flatten()
    };
    let mut moves = Vec::new();
    for group in pin_groups
        .iter()
        .filter(|group| group.slot_count() == PinGroupSlotCount::FollowsItsWires)
    {
        let mut slots_linked: Vec<usize> = links
            .iter()
            .flat_map(|link| [slot_of(&link.source, group), slot_of(&link.target, group)])
            .flatten()
            .collect();
        slots_linked.sort_unstable();
        slots_linked.dedup();
        for (to, from) in slots_linked.into_iter().enumerate() {
            if to == from {
                continue;
            }
            let moved_end = |end: &StreamGraphDocumentLinkEnd| match slot_of(end, group) {
                Some(slot) if slot == from => StreamGraphDocumentLinkEnd {
                    port: group.slot_port_name(
                        to,
                        PinGroupSlotPort::named(&end.port).map_or("", |port| port.pin),
                    ),
                    ..end.clone()
                },
                _ => end.clone(),
            };
            for link in links.iter().filter(|link| {
                slot_of(&link.source, group) == Some(from)
                    || slot_of(&link.target, group) == Some(from)
            }) {
                moves.push(ASlotMoved::Disconnect(link.clone()));
                moves.push(ASlotMoved::Connect(StreamGraphDocumentLink {
                    source: moved_end(&link.source),
                    target: moved_end(&link.target),
                }));
            }
            for input in &group.inputs {
                let (from_input, to_input) = (
                    group.slot_port_name(from, &input.name),
                    group.slot_port_name(to, &input.name),
                );
                let moved_value = node.input_values.get(&from_input).cloned();
                if node.input_values.contains_key(&to_input) || moved_value.is_some() {
                    moves.push(ASlotMoved::SetInputValue {
                        node: node.id,
                        input: to_input,
                        value: moved_value.clone(),
                    });
                }
                if moved_value.is_some() {
                    moves.push(ASlotMoved::SetInputValue {
                        node: node.id,
                        input: from_input,
                        value: None,
                    });
                }
            }
        }
    }
    moves
}

#[cfg(test)]
mod tests;
