// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The refusal of a link that carries a named type to or from an end its graph does not type.

use super::super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef};
use crate::core::descriptors::{NamedPinTypeImportPath, PinType};
use crate::core::{Error, PortDirection, Result};

/// Refuse a link from `from` to `to` when one end carries a named type and the other
/// carries no type yet, naming the type and both ends: a named type travels as the bytes its
/// class packs, which only a pin typed with it reads.
pub(super) fn refuse_a_named_type_at_one_end_of_a_link_untyped_at_the_other(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.node_run_id()).first(),
        graph.traversal().v(to.node_run_id()).first(),
    ) else {
        return Ok(());
    };
    let pin_type_of = |node: &GraphNode, port_name: &str, direction: PortDirection| {
        let port = match direction {
            PortDirection::Input => node.input_port(port_name),
            PortDirection::Output => node.output_port(port_name),
        }?;
        Some(the_pin_type_of_a_port_in_its_graph(
            graph, node, port, direction,
        ))
    };
    let (Some(output_pin_type), Some(input_pin_type)) = (
        pin_type_of(from_node, from.port_name(), PortDirection::Output),
        pin_type_of(to_node, to.port_name(), PortDirection::Input),
    ) else {
        return Ok(());
    };
    let output = format!("`{}.{}`", from_node.display_name, from.port_name());
    let input = format!("`{}.{}`", to_node.display_name, to.port_name());
    let refusal = match (&output_pin_type, &input_pin_type) {
        (_, PinTypeInItsGraph::Untyped) => the_named_type_among(&output_pin_type).map(|named| {
            let named = named.as_str();
            format!(
                "cannot link {output} to {input}: {output} carries the named type `{named}`, \
                 whose bytes only a pin typed with it reads, and {input} states no pin type. \
                 To fix: link it to an input typed `{named}`"
            )
        }),
        (PinTypeInItsGraph::Untyped, _) => the_named_type_among(&input_pin_type).map(|named| {
            let named = named.as_str();
            format!(
                "cannot link {output} to {input}: {input} reads the named type `{named}`, which \
                 only a pin typed with it writes, and {output} states no pin type. To fix: link \
                 it from an output typed `{named}`"
            )
        }),
        _ => None,
    };
    refusal.map_or(Ok(()), |words| Err(Error::InvalidLink(words)))
}

/// The first named type among what a port carries in its graph; `None` when it carries none.
pub(crate) fn the_named_type_among(
    pin_type: &PinTypeInItsGraph,
) -> Option<&NamedPinTypeImportPath> {
    pin_type
        .the_types_it_may_carry()
        .iter()
        .find_map(|pin_type| match pin_type {
            PinType::Named { named_pin_type } => Some(named_pin_type),
            _ => None,
        })
}
