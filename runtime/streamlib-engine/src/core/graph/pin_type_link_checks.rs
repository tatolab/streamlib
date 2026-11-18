// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The judgement of a link by its two ends' pin types, or by their families when one is untyped.

use super::port_family_link_checks::{
    refuse_a_link_between_ports_whose_families_do_not_match,
    refuse_what_a_link_carries_on_past_its_input, the_inputs_an_input_is_carried_on_to,
    the_outputs_a_link_carries_on,
};
use super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{
    PinConversion, PinLinkEnd, PinLinkInputEnd, PinLinkVerdict, PortFamilyLinkEndpoint,
    pin_link_verdict, refusal_of_the_names_and_codec_a_link_carries,
};
use crate::core::{Error, Result};

/// The end of a link at `port` of `node`, when typed and neither config nor an input sets it.
fn the_typed_link_end<'graph>(
    node: &'graph GraphNode,
    port: &'graph PortInfo,
) -> Option<PinLinkEnd<'graph>> {
    let pin = port.pin.as_ref()?;
    let family = &port.family;
    if family.family_from_config_key.is_some()
        || family.codec_from_config_key.is_some()
        || family.family_follows_input_port.is_some()
    {
        return None;
    }
    Some(PinLinkEnd {
        node_name: &node.display_name,
        port_name: &port.name,
        member_name: None,
        pin_type: &pin.pin_type,
        meaning: port.meaning.as_deref(),
    })
}

/// Refuse a link from `from` to `to` its verdict among `conversions`, or its families, refuse.
/// Answers the verdict of a link between two typed ends that connects as it is, and `None` for
/// one its families judge.
pub(crate) fn refuse_a_link_its_pin_types_or_families_refuse(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    conversions: &[PinConversion],
) -> Result<Option<PinLinkVerdict>> {
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.node_run_id()).first(),
        graph.traversal().v(to.node_run_id()).first(),
    ) else {
        return Ok(None);
    };
    let typed_ends = from_node
        .output_port(from.port_name())
        .zip(to_node.input_port(to.port_name()))
        .and_then(|(output_port, input_port)| {
            Some((
                output_port,
                the_typed_link_end(from_node, output_port)?,
                input_port,
                the_typed_link_end(to_node, input_port)?,
            ))
        });
    let Some((output_port, output, input_port, input)) = typed_ends else {
        return refuse_a_link_between_ports_whose_families_do_not_match(graph, from, to)
            .map(|()| None);
    };
    let input = PinLinkInputEnd {
        end: input,
        input_mode: input_port.input_mode,
    };
    let verdict = pin_link_verdict(output, input, conversions);
    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
        return Err(Error::PinLinkRefused(Box::new(refusal.clone())));
    }
    if let Some(refusal) = refusal_of_the_names_and_codec_a_link_carries(
        PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: &output_port.name,
            declaration: &output_port.family,
        },
        output_port.family.family,
        from_node.config.as_ref(),
        PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: &input_port.name,
            declaration: &input_port.family,
        },
    ) {
        return Err(Error::InvalidLink(refusal));
    }
    let inputs_carried_on_to = the_inputs_an_input_is_carried_on_to(graph, to_node, to.port_name());
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        refuse_what_a_link_carries_on_past_its_input(carried_on, &inputs_carried_on_to, from, to)?;
    }
    Ok(Some(verdict))
}
