// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The refusal of a link that carries Bundle frames into an input that states no pin type.

use super::super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::super::{Graph, InputLinkPortRef, OutputLinkPortRef};
use crate::core::descriptors::{PinLinkEndNamed, PinLinkRefusal};
use crate::core::{Error, PortDirection, Result};

/// Refuse a link from the whole of `from`, an output carrying a bundle or a List that holds a
/// Texture, Audio or an encoded unit, to `to`, an input that states no pin type: the output's
/// messages travel as Bundle frames, which an input judged by its family alone, such as a
/// relay's send node, reads as they arrive and cannot take apart. The refusal names each
/// member that makes it one.
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
    let PinTypeInItsGraph::Is(output_pin_type) =
        the_pin_type_of_a_port_in_its_graph(graph, from_node, output_port, PortDirection::Output)
    else {
        return Ok(());
    };
    if !output_pin_type.holds_a_heavy_member()
        || the_pin_type_of_a_port_in_its_graph(graph, to_node, input_port, PortDirection::Input)
            != PinTypeInItsGraph::Untyped
    {
        return Ok(());
    }
    Err(Error::PinLinkRefused(Box::new(
        PinLinkRefusal::BundleFrameIntoAnUntypedInput {
            output: PinLinkEndNamed {
                node_name: from_node.display_name.clone(),
                port_name: output_port.name.clone(),
                member_name: None,
                pin_type: output_pin_type,
            },
            input_node_name: to_node.display_name.clone(),
            input_name: input_port.name.clone(),
        },
    )))
}
