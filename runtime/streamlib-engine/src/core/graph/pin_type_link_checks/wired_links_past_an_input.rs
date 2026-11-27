// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The walk downstream of an input: each wired link an output following that input feeds, on
//! past each input such a link feeds, judged by what arrives at the input.

use std::collections::HashSet;

use super::super::bundle_member_link_ends::what_a_link_end_at_a_port_carries;
use super::super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::super::{Graph, GraphEdgeWithComponents, GraphNode, Link, PendingDeletionComponent};
use super::bundle_frames_into_an_untyped_input::{
    the_bundle_frame_type_among, the_refusal_of_bundle_frames_into_an_untyped_input,
};
use super::named_type_at_an_untyped_end::the_named_type_among;
use super::{TypedEnd, is_typed, verdict_between, what_the_input_end_of_a_link_carries};
use crate::core::PortDirection;
use crate::core::descriptors::PinConversion;

/// Why the first wired link an output following input `input_name` of `node` feeds, or an
/// output following an input such a link feeds, on downstream, is refused once `carried` is what
/// arrives at that input, among `conversions`; `None` when each connects under it.
pub(super) fn the_first_wired_link_past_an_input_refused(
    graph: &Graph,
    node: &GraphNode,
    input_name: &str,
    carried: &PinTypeInItsGraph,
    conversions: &[PinConversion],
) -> Option<String> {
    let mut inputs_passed = HashSet::from([(node.id.clone(), input_name.to_owned())]);
    let mut inputs_to_pass = vec![(node, input_name.to_owned(), carried.clone())];
    while let Some((node, input_name, carried)) = inputs_to_pass.pop() {
        for following_output in node.the_outputs_following_the_input(&input_name) {
            for link_out in the_wired_links_out_of(graph, node, &following_output.name) {
                let Some(downstream_node) = graph
                    .traversal()
                    .v(link_out.to_port().node_run_id())
                    .first()
                else {
                    continue;
                };
                let Some(downstream_input) =
                    downstream_node.input_port(link_out.to_port().port_name())
                else {
                    continue;
                };
                let (downstream_member, downstream_pin_type) = what_the_input_end_of_a_link_carries(
                    link_out.to_port(),
                    the_pin_type_of_a_port_in_its_graph(
                        graph,
                        downstream_node,
                        downstream_input,
                        PortDirection::Input,
                    ),
                );
                let (member, carried_on) = match what_a_link_end_at_a_port_carries(
                    link_out.source().member(),
                    carried.clone(),
                ) {
                    Ok(what_it_carries) => what_it_carries,
                    Err(member) => {
                        return Some(format!(
                            "output `{}` of `{}` takes the type linked into `{input_name}`, \
                                 and under it holds no member `{member}`, which the wired link \
                                 {} → {} starts at",
                            following_output.name,
                            node.display_name,
                            link_out.source(),
                            link_out.to_port(),
                        ));
                    }
                };
                if is_typed(&downstream_pin_type) {
                    let verdict = verdict_between(
                        TypedEnd {
                            node,
                            port: following_output,
                            member: member.as_ref(),
                            pin_type: &carried_on,
                        },
                        TypedEnd {
                            node: downstream_node,
                            port: downstream_input,
                            member: downstream_member.as_ref(),
                            pin_type: &downstream_pin_type,
                        },
                        conversions,
                    );
                    if let Some(refusal) = verdict.refusal_of_the_link_made_as_it_is() {
                        return Some(format!(
                            "output `{}` of `{}` takes the type linked into `{input_name}`, and \
                             under it the wired link {} → {} is refused: {refusal}",
                            following_output.name,
                            node.display_name,
                            link_out.source(),
                            link_out.to_port(),
                        ));
                    }
                } else if member.is_none()
                    && let Some(bundle_frame_type) = the_bundle_frame_type_among(&carried_on)
                {
                    let refusal = the_refusal_of_bundle_frames_into_an_untyped_input(
                        node,
                        &following_output.name,
                        bundle_frame_type.clone(),
                        downstream_node,
                        &downstream_input.name,
                    );
                    return Some(format!(
                        "output `{}` of `{}` takes the type linked into `{input_name}`, and \
                         under it the wired link {} → {} is refused: {refusal}",
                        following_output.name,
                        node.display_name,
                        link_out.source(),
                        link_out.to_port(),
                    ));
                } else if let Some(named) = the_named_type_among(&carried_on) {
                    return Some(format!(
                        "output `{}` of `{}` takes the type linked into `{input_name}`, so it \
                         would carry the named type `{}` into the wired link {} → {}, whose \
                         input states no pin type: only a pin typed with a named type reads its \
                         bytes. To fix: remove that link first, or link `{input_name}` from an \
                         output of another type",
                        following_output.name,
                        node.display_name,
                        named.as_str(),
                        link_out.source(),
                        link_out.to_port(),
                    ));
                }
                if inputs_passed.insert((downstream_node.id.clone(), downstream_input.name.clone()))
                {
                    inputs_to_pass.push((
                        downstream_node,
                        downstream_input.name.clone(),
                        carried_on,
                    ));
                }
            }
        }
    }
    None
}

/// Each link wired out of output `output_name` of `node` in `graph`, none being taken off.
pub(super) fn the_wired_links_out_of<'graph>(
    graph: &'graph Graph,
    node: &GraphNode,
    output_name: &'graph str,
) -> impl Iterator<Item = &'graph Link> + 'graph {
    graph
        .traversal()
        .v(&node.id)
        .out_e()
        .iter()
        .filter(move |link| {
            link.source().port_name() == output_name && !link.has::<PendingDeletionComponent>()
        })
}
