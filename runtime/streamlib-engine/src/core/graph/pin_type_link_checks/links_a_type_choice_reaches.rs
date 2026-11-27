// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The links a node's type choices reach, judged again once one of its inputs changed: each link
//! at a port whose type an input of the node chooses, at an output taking the type of such an
//! input, or at an output a conversion gives the type of by the node's settings, and on
//! downstream past each output taking the type of an input such a link feeds, and each link
//! already marked, which an edit upstream may have made connect. A link whose two ends no
//! longer connect is marked as differing, and carries nothing until they connect again; the
//! change of the input is never refused for it.

use std::collections::HashSet;

use super::super::bundle_member_link_ends::refuse_a_member_its_port_does_not_hold;
use super::super::pin_type_resolution::{
    PinTypeInItsGraph, the_conversion_whose_output_follows_a_setting,
    the_input_an_output_takes_the_type_of,
};
use super::super::{
    Graph, GraphEdgeWithComponents, GraphNode, InputLinkPortRef, Link, LinkPinTypesDifferComponent,
    LinkState, LinkStateComponent, LinkUniqueId, NodeRunId, OutputLinkPortRef,
    PendingDeletionComponent, PortInfo,
};
use super::wired_links_past_an_input::the_links_drawn_out_of;
use super::{the_ends_a_link_is_judged_by, the_refusal_without_its_fix, the_typed_ends_of};
use crate::core::descriptors::{DeclaredPinType, PinConversion};

/// What judging links again found, changing nothing yet: each link whose ends differ now, with
/// how, and each marked as differing that connects again.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LinksATypeChoiceJudgedAgain {
    pub(crate) differing: Vec<(LinkUniqueId, LinkPinTypesDifferComponent)>,
    pub(crate) connecting_again: Vec<LinkUniqueId>,
}

impl Graph {
    /// Judge again by the types their two ends carry now each link the type choices of each of
    /// `nodes` reach, and each link marked as differing, which an edit upstream of it may have
    /// made connect: each whose ends no longer connect, naming both pin types, and each marked
    /// that connects again. Changes nothing; [`Self::mark_each_link_judged_again`] marks them.
    pub(crate) fn judge_again_each_link_the_type_choices_of_the_nodes_reach(
        &self,
        nodes: &[NodeRunId],
    ) -> LinksATypeChoiceJudgedAgain {
        let conversions = self.node_types_this_stream_resolves().pin_conversions();
        let mut links: Vec<&Link> = Vec::new();
        let mut links_judged: HashSet<&LinkUniqueId> = HashSet::new();
        let marked = self.traversal().e(()).iter().filter(|link| {
            link.has::<LinkPinTypesDifferComponent>() && !link.has::<PendingDeletionComponent>()
        });
        let reached = nodes
            .iter()
            .filter_map(|node_run_id| self.traversal().v(node_run_id).first())
            .flat_map(|node| the_links_the_type_choices_of_a_node_reach(self, node));
        for link in marked.chain(reached) {
            if links_judged.insert(&link.id) {
                links.push(link);
            }
        }
        let mut judged_again = LinksATypeChoiceJudgedAgain::default();
        for link in links {
            match how_the_ends_of_a_link_differ_now(
                self,
                link.source(),
                link.to_port(),
                &conversions,
            ) {
                Some(differing) => judged_again.differing.push((link.id.clone(), differing)),
                None if link.has::<LinkPinTypesDifferComponent>() => {
                    judged_again.connecting_again.push(link.id.clone());
                }
                None => {}
            }
        }
        judged_again
    }

    /// Mark each link `judged_again` finds differing, and take the mark off each it finds
    /// connecting again, holding that one pending, to be wired.
    pub(crate) fn mark_each_link_judged_again(
        &mut self,
        judged_again: &LinksATypeChoiceJudgedAgain,
    ) {
        for (link_id, differing) in &judged_again.differing {
            if let Some(link) = self.traversal_mut().e(link_id).first_mut() {
                link.insert_component_without_rendering_it(differing.clone());
            }
        }
        for link_id in &judged_again.connecting_again {
            if let Some(link) = self.traversal_mut().e(link_id).first_mut() {
                link.remove::<LinkPinTypesDifferComponent>();
                link.insert(LinkStateComponent(LinkState::Pending));
            }
        }
    }
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
        for link_out in the_links_drawn_out_of(graph, node, output_name) {
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

/// Whether a link from `from` to `to` of a saved graph, refused as it is, is one whose ends
/// differ since a type choice reaching it changed: kept, so the compile holds it unwired as it
/// holds a link the change left differing, rather than refusing the graph it was saved in.
pub(crate) fn a_saved_link_differs_by_a_type_choice(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> bool {
    if the_nodes_whose_type_choices_reach_the_link(graph, from, to).is_empty() {
        return false;
    }
    let conversions = graph.node_types_this_stream_resolves().pin_conversions();
    how_the_ends_of_a_link_differ_now(graph, from, to, &conversions).is_some()
}

/// Each node whose type choices reach a link from `from` to `to`: the node of `to` when one of
/// its type choices types that input, and the node of `from` when one types that output, or,
/// for an output taking the type of an input, each node whose choice types an output linked
/// into that input, followed upstream.
pub(crate) fn the_nodes_whose_type_choices_reach_the_link(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Vec<NodeRunId> {
    let mut choosing: Vec<NodeRunId> = graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .filter(|to_node| {
            the_ports_a_type_choice_of_the_node_types(graph, to_node)
                .inputs
                .contains(&to.port_name())
        })
        .map(|to_node| to_node.id.clone())
        .into_iter()
        .collect();
    let mut outputs_to_follow = vec![(from.node_run_id().clone(), from.port_name().to_owned())];
    let mut outputs_followed: HashSet<(NodeRunId, String)> = HashSet::new();
    while let Some((node_run_id, output_name)) = outputs_to_follow.pop() {
        if !outputs_followed.insert((node_run_id.clone(), output_name.clone())) {
            continue;
        }
        let Some(node) = graph.traversal().v(&node_run_id).first() else {
            continue;
        };
        let typed = the_ports_a_type_choice_of_the_node_types(graph, node);
        if typed
            .outputs
            .iter()
            .any(|output| output.name == output_name)
        {
            if !choosing.contains(&node.id) {
                choosing.push(node.id.clone());
            }
            continue;
        }
        let Some(followed_input) = node
            .output_port(&output_name)
            .and_then(the_input_an_output_takes_the_type_of)
        else {
            continue;
        };
        for link_in in graph.traversal().v(&node.id).in_e().iter().filter(|link| {
            link.to_port().port_name() == followed_input && !link.has::<PendingDeletionComponent>()
        }) {
            outputs_to_follow.push((
                link_in.source().node_run_id().clone(),
                link_in.source().port_name().to_owned(),
            ));
        }
    }
    choosing
}

/// How the two ends of a link from `from` to `to` differ in `graph` now, naming the type each
/// carries; `None` when they connect under `conversions`, or when an end is not typed yet, which
/// is judged once it is.
fn how_the_ends_of_a_link_differ_now(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    conversions: &[PinConversion],
) -> Option<LinkPinTypesDifferComponent> {
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
