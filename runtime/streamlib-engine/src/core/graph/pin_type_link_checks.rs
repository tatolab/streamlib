// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The judgement of a link by the pin types of its two ends: the verdict, with the conversions
//! the registered node types state, when both ends are typed; the family check otherwise.

use super::port_family_link_checks::{
    refuse_a_link_between_ports_whose_families_do_not_match,
    refuse_what_a_link_carries_on_past_its_input, the_outputs_a_link_carries_on,
};
use super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{PinLinkEnd, PinLinkVerdict, pin_link_verdict};
use crate::core::nodes::NODE_REGISTRY;
use crate::core::{Error, Result};

/// The end of a link at `port` of `node`, when the port is typed.
fn the_typed_link_end<'graph>(
    node: &'graph GraphNode,
    port: &'graph PortInfo,
) -> Option<PinLinkEnd<'graph>> {
    let pin = port.pin.as_ref()?;
    Some(PinLinkEnd {
        node_name: &node.display_name,
        port_name: &port.name,
        member_name: None,
        pin_type: &pin.pin_type,
        meaning: port.meaning.as_deref(),
        input_mode: port.input_mode,
    })
}

/// The verdict on a link from `from` to `to`, with the conversions every registered node type
/// states; `None` unless the graph holds both ends and both are typed.
pub(crate) fn the_pin_link_verdict_of_a_link(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Option<PinLinkVerdict> {
    let from_node = graph.traversal().v(from.node_run_id()).first()?;
    let to_node = graph.traversal().v(to.node_run_id()).first()?;
    let output = the_typed_link_end(from_node, from_node.output_port(from.port_name())?)?;
    let input = the_typed_link_end(to_node, to_node.input_port(to.port_name())?)?;
    Some(pin_link_verdict(
        output,
        input,
        &NODE_REGISTRY.pin_conversions(),
    ))
}

/// Refuse a link from `from` to `to` that does not connect as it is. A link between two typed
/// ends is refused in its verdict's words unless it connects or converts for free; a link
/// needing a convert node is refused too, naming the conversions to place. A link with an end
/// that is not typed is judged by the two ports' families.
///
/// Either way, what an output that follows `to`'s input carries on is checked against each
/// input it already feeds.
pub(crate) fn refuse_a_link_its_pin_types_or_families_refuse(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    let Some(verdict) = the_pin_link_verdict_of_a_link(graph, from, to) else {
        return refuse_a_link_between_ports_whose_families_do_not_match(graph, from, to);
    };
    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
        return Err(Error::PinLinkRefused(Box::new(refusal.clone())));
    }
    let Some(from_node) = graph.traversal().v(from.node_run_id()).first() else {
        return Ok(());
    };
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        refuse_what_a_link_carries_on_past_its_input(graph, carried_on, from, to)?;
    }
    Ok(())
}
