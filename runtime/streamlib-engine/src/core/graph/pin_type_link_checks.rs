// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The judgement of a link by its two ends' pin types, or by their families when one is untyped.

use super::port_family_link_checks::{
    FamiliesTakeTheLink, refuse_a_link_between_ports_whose_families_do_not_match,
    refuse_what_a_link_carries_on_past_its_input, the_inputs_an_input_is_carried_on_to,
    the_outputs_a_link_carries_on,
};
use super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{
    PinLinkEnd, PinLinkInputEnd, PinLinkRefusal, PinLinkVerdict, PinType, PortFamilyDeclaration,
    PortFamilyLinkEndpoint, pin_link_verdict, refusal_of_the_names_and_codec_a_link_carries,
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

/// Whose link a connect makes, which decides how leniently it is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ALinkConnected {
    /// A link made now: one whose two typed ends the verdict finds no rule connecting, or whose
    /// Image output may write a format an input it reaches does not read, is refused, and one
    /// an island cannot take is refused before it is made.
    MadeNow,
    /// A link a saved graph already holds, so the graph opens: one the verdict refuses by its
    /// pin types alone, or whose Image formats alone are refused, is kept when its ports'
    /// families, names and codec take it, each refusal logged as a warning, and one an island
    /// cannot take is left to the commit that builds the stream to refuse.
    OfASavedGraph,
}

/// The two ports of a link from `from` to `to` and their ends, when both are typed.
struct TypedLinkEnds<'graph> {
    from_node: &'graph GraphNode,
    output_port: &'graph PortInfo,
    output: PinLinkEnd<'graph>,
    to_node: &'graph GraphNode,
    input_port: &'graph PortInfo,
    input: PinLinkInputEnd<'graph>,
}

/// The typed ends of a link from `from` to `to` in `graph`; `None` when either end is no node
/// of it, or either port is untyped.
fn the_typed_ends_of<'graph>(
    graph: &'graph Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Option<TypedLinkEnds<'graph>> {
    let from_node = graph.traversal().v(from.node_run_id()).first()?;
    let to_node = graph.traversal().v(to.node_run_id()).first()?;
    let output_port = from_node.output_port(from.port_name())?;
    let input_port = to_node.input_port(to.port_name())?;
    Some(TypedLinkEnds {
        from_node,
        output_port,
        output: the_typed_link_end(from_node, output_port)?,
        to_node,
        input_port,
        input: PinLinkInputEnd {
            end: the_typed_link_end(to_node, input_port)?,
            input_mode: input_port.input_mode,
        },
    })
}

/// What a link is judged by at its two ends: each port's pin type, where it has one, and its
/// family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TheEndsALinkIsJudgedBy {
    output_pin_type: Option<PinType>,
    output_family: PortFamilyDeclaration,
    input_pin_type: Option<PinType>,
    input_family: PortFamilyDeclaration,
}

/// What a link from `from` to `to` is judged by at its ends; `None` when either end is no port
/// of `graph`.
pub(crate) fn the_ends_a_link_is_judged_by(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Option<TheEndsALinkIsJudgedBy> {
    let output_port = graph
        .traversal()
        .v(from.node_run_id())
        .first()?
        .output_port(from.port_name())?;
    let input_port = graph
        .traversal()
        .v(to.node_run_id())
        .first()?
        .input_port(to.port_name())?;
    Some(TheEndsALinkIsJudgedBy {
        output_pin_type: output_port.pin.as_ref().map(|pin| pin.pin_type.clone()),
        output_family: output_port.family.clone(),
        input_pin_type: input_port.pin.as_ref().map(|pin| pin.pin_type.clone()),
        input_family: input_port.family.clone(),
    })
}

/// Whether `connect` would refuse a link from `from` to `to` made now in `graph`.
pub(crate) fn a_link_made_now_would_be_refused(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> bool {
    the_link_its_pin_types_or_families_take(graph, from, to, ALinkConnected::MadeNow).is_err()
}

/// Refuse a link from `from` to `to` its verdict among the conversions `graph`'s stream
/// resolves, or its families, refuse; `a_link_connected` says whether one the verdict, or its
/// Image formats, refuse alone is kept. Answers the verdict of a link between two typed ends
/// that connects, and `None` for one its families judge or take.
pub(crate) fn refuse_a_link_its_pin_types_or_families_refuse(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<Option<PinLinkVerdict>> {
    let LinkTaken { verdict, families } =
        the_link_its_pin_types_or_families_take(graph, from, to, a_link_connected)?;
    if let FamiliesTakeTheLink::ButForImageFormatsNotRead(refusal) = families {
        tracing::warn!(
            from = %from,
            to = %to,
            "a saved link is kept, its ports' families taking it, though its output may write an \
             Image format an input it reaches does not read: {refusal}"
        );
    }
    Ok(verdict)
}

/// How a link neither its verdict nor its families refuse is taken: the verdict of its two typed
/// ends when it connects, and how its families take it.
struct LinkTaken {
    verdict: Option<PinLinkVerdict>,
    families: FamiliesTakeTheLink,
}

/// How a link from `from` to `to` that neither its verdict nor its families refuse is taken, as
/// [`refuse_a_link_its_pin_types_or_families_refuse`] judges it.
fn the_link_its_pin_types_or_families_take(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkTaken> {
    let Some(TypedLinkEnds {
        from_node,
        output_port,
        output,
        to_node,
        input_port,
        input,
    }) = the_typed_ends_of(graph, from, to)
    else {
        return refuse_a_link_between_ports_whose_families_do_not_match(
            graph,
            from,
            to,
            a_link_connected,
        )
        .map(|families| LinkTaken {
            verdict: None,
            families,
        });
    };
    let conversions = graph.node_types_this_stream_resolves().pin_conversions();
    let verdict = pin_link_verdict(output, input, &conversions);
    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
        if a_link_connected == ALinkConnected::OfASavedGraph
            && matches!(refusal, PinLinkRefusal::PinTypesDoNotConnect { .. })
            && let Ok(families) = refuse_a_link_between_ports_whose_families_do_not_match(
                graph,
                from,
                to,
                a_link_connected,
            )
        {
            tracing::warn!(
                from = %from,
                to = %to,
                "a saved link is kept, its ports' families taking it, though its pin types \
                 refuse it: {refusal}"
            );
            return Ok(LinkTaken {
                verdict: None,
                families,
            });
        }
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
    let mut families = FamiliesTakeTheLink::Wholly;
    let inputs_carried_on_to = the_inputs_an_input_is_carried_on_to(graph, to_node, to.port_name());
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        refuse_what_a_link_carries_on_past_its_input(
            carried_on,
            &inputs_carried_on_to,
            from,
            to,
            a_link_connected,
            &mut families,
        )?;
    }
    Ok(LinkTaken {
        verdict: Some(verdict),
        families,
    })
}
