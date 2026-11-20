// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The refusal of a link that carries Bundle frames into an input that states no pin type.

use super::super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::super::{Graph, GraphNode, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::{PinLinkEndNamed, PinLinkRefusal, PinType};
use crate::core::{Error, PortDirection, Result};

/// The type `carried`, what a port carries, may carry that travels as Bundle frames: a bundle or
/// a List holding a Texture, Audio or an encoded unit, being that type or one of its
/// alternatives; `None` when it carries none.
pub(super) fn the_bundle_frame_type_among(carried: &PinTypeInItsGraph) -> Option<&PinType> {
    match carried {
        PinTypeInItsGraph::Is(pin_type) => Some(pin_type).filter(|t| t.holds_a_heavy_member()),
        PinTypeInItsGraph::OneOf(pin_types) => pin_types
            .iter()
            .find(|pin_type| pin_type.holds_a_heavy_member()),
        PinTypeInItsGraph::Any | PinTypeInItsGraph::Untyped => None,
    }
}

/// The type output `port` of `node` may carry in `graph` that travels as Bundle frames: the type
/// it resolves to, or one of the alternatives it declares; `None` when it carries none.
pub(crate) fn the_bundle_frame_type_an_output_may_carry(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
) -> Option<PinType> {
    the_bundle_frame_type_among(&the_pin_type_of_a_port_in_its_graph(
        graph,
        node,
        port,
        PortDirection::Output,
    ))
    .cloned()
}

/// The refusal of a link from the whole of output `output_name` of `from_node`, carrying
/// `bundle_frame_type`, into input `input_name` of `to_node`, which states no pin type.
pub(super) fn the_refusal_of_bundle_frames_into_an_untyped_input(
    from_node: &GraphNode,
    output_name: &str,
    bundle_frame_type: PinType,
    to_node: &GraphNode,
    input_name: &str,
) -> PinLinkRefusal {
    PinLinkRefusal::BundleFrameIntoAnUntypedInput {
        output: PinLinkEndNamed {
            node_name: from_node.display_name.clone(),
            port_name: output_name.to_owned(),
            member_name: None,
            pin_type: bundle_frame_type,
        },
        input_node_name: to_node.display_name.clone(),
        input_name: input_name.to_owned(),
    }
}

/// Refuse a link from the whole of `from`, an output that carries, or may carry as one of its
/// alternatives, a bundle or a List holding a Texture, Audio or an encoded unit, to `to`, an
/// input that states no pin type: the output's messages travel as Bundle frames, which an input
/// judged by its family alone, such as a relay's send node, reads as they arrive and cannot take
/// apart. The refusal names each member that makes it one.
pub(super) fn refuse_bundle_frames_into_an_untyped_input(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    if from.member().is_some() {
        return Ok(());
    }
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.node_run_id()).first(),
        graph.traversal().v(to.node_run_id()).first(),
    ) else {
        return Ok(());
    };
    let (Some(output_port), Some(input_port)) = (
        from_node.output_port(from.port_name()),
        to_node.input_port(to.port_name()),
    ) else {
        return Ok(());
    };
    let Some(bundle_frame_type) =
        the_bundle_frame_type_an_output_may_carry(graph, from_node, output_port)
    else {
        return Ok(());
    };
    if the_pin_type_of_a_port_in_its_graph(graph, to_node, input_port, PortDirection::Input)
        != PinTypeInItsGraph::Untyped
    {
        return Ok(());
    }
    Err(Error::PinLinkRefused(Box::new(
        the_refusal_of_bundle_frames_into_an_untyped_input(
            from_node,
            &output_port.name,
            bundle_frame_type,
            to_node,
            &input_port.name,
        ),
    )))
}
