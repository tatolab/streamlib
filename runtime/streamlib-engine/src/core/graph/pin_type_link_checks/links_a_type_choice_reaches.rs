// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The links a node's type choices reach, judged again once one of its inputs changed: each link
//! at a port whose type an input of the node chooses, at an output taking the type of such an
//! input, or at an output a conversion gives the type of by the node's settings, and on
//! downstream past each output taking the type of an input such a link feeds. A link whose two
//! ends no longer connect is marked as differing, and carries nothing until they connect again;
//! the change of the input is never refused for it.

use std::collections::HashSet;

use super::super::bundle_member_link_ends::refuse_a_member_its_port_does_not_hold;
use super::super::pin_type_resolution::{
    PinTypeInItsGraph, the_conversion_whose_output_follows_a_setting,
};
use super::super::{
    Graph, GraphEdgeWithComponents, GraphNode, Link, LinkPinTypesDifferComponent, LinkUniqueId,
    NodeRunId, PendingDeletionComponent, PortInfo,
};
use super::wired_links_past_an_input::the_wired_links_out_of;
use super::{the_ends_a_link_is_judged_by, the_refusal_without_its_fix, the_typed_ends_of};
use crate::core::descriptors::{DeclaredPinType, PinConversion};

/// What judging again the links a node's type choices reach found: each link whose ends differ
/// now, whose channel is to be closed while it is open, and each that differed and connects
/// again, to be wired again.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LinksATypeChoiceJudgedAgain {
    pub(crate) differing: Vec<LinkUniqueId>,
    pub(crate) connecting_again: Vec<LinkUniqueId>,
}

impl Graph {
    /// Judge again each link the type choices of the node `node_run_id` reach by the types its
    /// two ends carry now: mark each whose ends no longer connect as differing, naming both pin
    /// types, and take the mark off each that connects again. Answers each marked and each
    /// unmarked; nothing for a node this graph does not hold, or one no input of which chooses
    /// a type.
    pub(crate) fn judge_again_each_link_the_type_choices_of_the_node_reach(
        &mut self,
        node_run_id: &NodeRunId,
    ) -> LinksATypeChoiceJudgedAgain {
        let judged: Vec<OneLinkJudgedAgain> = {
            let Some(node) = self.traversal().v(node_run_id).first() else {
                return LinksATypeChoiceJudgedAgain::default();
            };
            let conversions = self.node_types_this_stream_resolves().pin_conversions();
            the_links_the_type_choices_of_a_node_reach(self, node)
                .into_iter()
                .map(|link| OneLinkJudgedAgain {
                    link_id: link.id.clone(),
                    differing_now: how_the_ends_of_a_link_differ_now(self, link, &conversions),
                    differed_before: link.has::<LinkPinTypesDifferComponent>(),
                })
                .collect()
        };
        let mut judged_again = LinksATypeChoiceJudgedAgain::default();
        for one in judged {
            let Some(link) = self.traversal_mut().e(&one.link_id).first_mut() else {
                continue;
            };
            match one.differing_now {
                Some(differing) => {
                    link.insert_component_without_rendering_it(differing);
                    judged_again.differing.push(one.link_id);
                }
                None if one.differed_before => {
                    link.remove::<LinkPinTypesDifferComponent>();
                    judged_again.connecting_again.push(one.link_id);
                }
                None => {}
            }
        }
        judged_again
    }
}

/// One link judged again: how its ends differ now, `None` when they connect, and whether it
/// was marked as differing before.
struct OneLinkJudgedAgain {
    link_id: LinkUniqueId,
    differing_now: Option<LinkPinTypesDifferComponent>,
    differed_before: bool,
}

/// Whether `node` has a port whose type one of its inputs chooses, or an output taking the type
/// of such an input, or one a conversion gives the type of by its settings.
pub(crate) fn a_type_choice_of_the_node_types_a_port(graph: &Graph, node: &GraphNode) -> bool {
    let typed = the_ports_a_type_choice_of_the_node_types(graph, node);
    !(typed.inputs.is_empty() && typed.outputs.is_empty())
}

/// The ports of a node an input of it types.
struct PortsATypeChoiceTypes<'graph> {
    inputs: Vec<&'graph str>,
    outputs: Vec<&'graph PortInfo>,
}

/// The inputs of `node` whose type one of its inputs chooses, and the outputs whose type one of
/// its inputs chooses, that take the type of such an input, or that a conversion gives the type
/// of by its settings.
fn the_ports_a_type_choice_of_the_node_types<'graph>(
    graph: &Graph,
    node: &'graph GraphNode,
) -> PortsATypeChoiceTypes<'graph> {
    let declared_as = |port: &'graph PortInfo| {
        port.declared_pin_type
            .as_ref()
            .map(|declared| &declared.pin_type)
    };
    let inputs: Vec<&str> = node
        .ports
        .inputs
        .iter()
        .filter(|port| matches!(declared_as(port), Some(DeclaredPinType::SetByInput { .. })))
        .map(|port| port.name.as_str())
        .collect();
    let outputs = node
        .ports
        .outputs
        .iter()
        .filter(|port| match declared_as(port) {
            Some(DeclaredPinType::SetByInput { .. }) => true,
            Some(DeclaredPinType::SameAsInput { input_port_name }) => {
                inputs.contains(&input_port_name.as_str())
            }
            _ => the_conversion_whose_output_follows_a_setting(graph, node, port).is_some(),
        })
        .collect();
    PortsATypeChoiceTypes { inputs, outputs }
}

/// Each link, none being taken off, at a port of `node` one of its type choices types, and on
/// downstream past each output taking the type of an input such a link feeds, each once.
fn the_links_the_type_choices_of_a_node_reach<'graph>(
    graph: &'graph Graph,
    node: &'graph GraphNode,
) -> Vec<&'graph Link> {
    let typed = the_ports_a_type_choice_of_the_node_types(graph, node);
    let mut reached: Vec<&Link> = graph
        .traversal()
        .v(&node.id)
        .in_e()
        .iter()
        .filter(|link| {
            !link.has::<PendingDeletionComponent>()
                && typed.inputs.contains(&link.to_port().port_name())
        })
        .collect();
    let mut links_reached: HashSet<&LinkUniqueId> = reached.iter().map(|link| &link.id).collect();
    let mut outputs_to_follow: Vec<(&GraphNode, &str)> = typed
        .outputs
        .iter()
        .map(|output| (node, output.name.as_str()))
        .collect();
    let mut outputs_followed: HashSet<(&NodeRunId, &str)> = HashSet::new();
    while let Some((node, output_name)) = outputs_to_follow.pop() {
        if !outputs_followed.insert((&node.id, output_name)) {
            continue;
        }
        for link_out in the_wired_links_out_of(graph, node, output_name) {
            if links_reached.insert(&link_out.id) {
                reached.push(link_out);
            }
            let Some(downstream_node) = graph
                .traversal()
                .v(link_out.to_port().node_run_id())
                .first()
            else {
                continue;
            };
            for following_output in
                downstream_node.the_outputs_following_the_input(link_out.to_port().port_name())
            {
                outputs_to_follow.push((downstream_node, following_output.name.as_str()));
            }
        }
    }
    reached
}

/// How the two ends of `link` differ in `graph` now, naming the type each carries; `None` when
/// they connect under `conversions`, or when an end is not typed yet, which is judged once it is.
fn how_the_ends_of_a_link_differ_now(
    graph: &Graph,
    link: &Link,
    conversions: &[PinConversion],
) -> Option<LinkPinTypesDifferComponent> {
    let (from, to) = (link.source(), link.to_port());
    let why = match refuse_a_member_its_port_does_not_hold(graph, from, to) {
        Err(refusal) => refusal.to_string(),
        Ok(()) => the_typed_ends_of(graph, from, to)?
            .verdict(conversions)
            .refusal_of_the_link_made_as_it_is()?
            .to_string(),
    };
    let ends = the_ends_a_link_is_judged_by(graph, from, to)?;
    Some(LinkPinTypesDifferComponent {
        output_pin_type: in_words(&ends.output_pin_type),
        input_pin_type: in_words(&ends.input_pin_type),
        why: the_refusal_without_its_fix(&why).to_owned(),
    })
}

/// What a port carries, in words.
fn in_words(pin_type: &PinTypeInItsGraph) -> String {
    match pin_type {
        PinTypeInItsGraph::Is(pin_type) => pin_type.to_string(),
        PinTypeInItsGraph::OneOf(pin_types) => format!(
            "one of {}",
            pin_types
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PinTypeInItsGraph::Any => "any type".to_owned(),
        PinTypeInItsGraph::Untyped => "no type yet".to_owned(),
    }
}
