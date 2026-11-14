// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The checks the graph makes between the two ports of a link by their port
//! families: at connect, and again when a live config update changes what an
//! output resolves from its node's config.

use super::{
    Graph, GraphEdgeWithComponents, InputLinkPortRef, OutputLinkPortRef, PendingDeletionComponent,
    ProcessorNode, ProcessorUniqueId,
};
use crate::core::descriptors::{
    PortFamilyDeclaration, PortFamilyLinkEndpoint, refusal_of_a_link_between_port_families,
};
use std::collections::HashSet;

use crate::core::{Error, Result};

fn output_port_family<'node>(
    node: &'node ProcessorNode,
    port_name: &str,
) -> Option<&'node PortFamilyDeclaration> {
    node.ports
        .outputs
        .iter()
        .find(|port| port.name == port_name)
        .map(|port| &port.family)
}

fn input_port_family<'node>(
    node: &'node ProcessorNode,
    port_name: &str,
) -> Option<&'node PortFamilyDeclaration> {
    node.ports
        .inputs
        .iter()
        .find(|port| port.name == port_name)
        .map(|port| &port.family)
}

/// The outputs a link out of `port_name` of `node` carries on: the output
/// itself, or, for one whose family follows an input of its node, every
/// output linked into that input, followed upstream. Empty while no input it
/// follows has a link yet, or the outputs it follows only loop back on
/// themselves, so its family is not settled.
pub(crate) fn the_outputs_a_link_carries_on<'graph>(
    graph: &'graph Graph,
    node: &'graph ProcessorNode,
    port_name: &'graph str,
) -> Vec<(
    &'graph ProcessorNode,
    &'graph str,
    &'graph PortFamilyDeclaration,
)> {
    let mut carried_on = Vec::new();
    let mut outputs_to_follow = vec![(node, port_name)];
    let mut outputs_followed: HashSet<(&ProcessorUniqueId, &str)> = HashSet::new();
    while let Some((node, port_name)) = outputs_to_follow.pop() {
        if !outputs_followed.insert((&node.id, port_name)) {
            continue;
        }
        let Some(declaration) = output_port_family(node, port_name) else {
            continue;
        };
        let Some(followed_input_port) = declaration.family_follows_input_port.as_deref() else {
            carried_on.push((node, port_name, declaration));
            continue;
        };
        let links_into_the_followed_input = graph.traversal().e(()).iter().filter(|link| {
            link.to_port().processor_id() == &node.id
                && link.to_port().port_name() == followed_input_port
                && !link.has::<PendingDeletionComponent>()
        });
        for link_into_the_followed_input in links_into_the_followed_input {
            if let Some(upstream_node) = graph
                .traversal()
                .v(link_into_the_followed_input.source().processor_id())
                .first()
            {
                outputs_to_follow.push((
                    upstream_node,
                    link_into_the_followed_input.source().port_name(),
                ));
            }
        }
    }
    carried_on
}

/// The family a link out of output `port_name` of `node_id` carries: the
/// output's own, or, for one whose family follows an input, that of the first
/// output it follows upstream; `None` while that is not settled.
pub(crate) fn the_family_a_link_out_of_an_output_carries(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
    port_name: &str,
) -> Option<PortFamilyDeclaration> {
    let node = graph.traversal().v(node_id).first()?;
    the_outputs_a_link_carries_on(graph, node, port_name)
        .into_iter()
        .map(|(_, _, declaration)| declaration.clone())
        .next()
}

/// Each input what arrives at `input_port_name` of `node` is carried on to by
/// the outputs that follow it, downstream through every output that follows
/// an input in turn, with the following output it leaves through.
fn the_inputs_an_input_is_carried_on_to<'graph>(
    graph: &'graph Graph,
    node: &'graph ProcessorNode,
    input_port_name: &'graph str,
) -> Vec<(
    &'graph ProcessorNode,
    &'graph str,
    &'graph ProcessorNode,
    &'graph str,
)> {
    let mut inputs_carried_on_to = Vec::new();
    let mut inputs_to_follow = vec![(node, input_port_name)];
    let mut inputs_followed: HashSet<(&ProcessorUniqueId, &str)> = HashSet::new();
    while let Some((following_node, following_input_port)) = inputs_to_follow.pop() {
        if !inputs_followed.insert((&following_node.id, following_input_port)) {
            continue;
        }
        let outputs_following_the_input = following_node.ports.outputs.iter().filter(|output| {
            output.family.family_follows_input_port.as_deref() == Some(following_input_port)
        });
        for following_output in outputs_following_the_input {
            let links_out_of_the_following_output = graph
                .traversal()
                .v(&following_node.id)
                .out_e()
                .iter()
                .filter(|link| {
                    link.source().port_name() == following_output.name
                        && !link.has::<PendingDeletionComponent>()
                });
            for link_out in links_out_of_the_following_output {
                let Some(downstream_node) = graph
                    .traversal()
                    .v(link_out.to_port().processor_id())
                    .first()
                else {
                    continue;
                };
                let downstream_input_port = link_out.to_port().port_name();
                inputs_carried_on_to.push((
                    following_node,
                    following_output.name.as_str(),
                    downstream_node,
                    downstream_input_port,
                ));
                inputs_to_follow.push((downstream_node, downstream_input_port));
            }
        }
    }
    inputs_carried_on_to
}

/// The refusal of a link from `output` to the input `to_port_name` of
/// `to_node`, by their families, or `None` when they match.
fn refusal_of_a_link_from_an_output(
    (from_node, from_port_name, output_declaration): (&ProcessorNode, &str, &PortFamilyDeclaration),
    to_node: &ProcessorNode,
    to_port_name: &str,
    input_declaration: &PortFamilyDeclaration,
) -> Option<String> {
    refusal_of_a_link_between_port_families(
        PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: from_port_name,
            declaration: output_declaration,
        },
        from_node.config.as_ref(),
        PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: to_port_name,
            declaration: input_declaration,
        },
    )
}

/// Refuse a link whose two ports' families, names, codec or formats do not
/// match, naming both endpoints.
///
/// An output whose family follows an input of its node is checked as each
/// output linked into that input, once one is; and a link into such an input
/// is checked, as that output, against every input the following output
/// already feeds.
pub(crate) fn refuse_a_link_between_ports_whose_families_do_not_match(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.processor_id()).first(),
        graph.traversal().v(to.processor_id()).first(),
    ) else {
        return Ok(());
    };
    let Some(input_declaration) = input_port_family(to_node, to.port_name()) else {
        return Ok(());
    };
    if output_port_family(from_node, from.port_name()).is_none() {
        return Ok(());
    }
    let inputs_carried_on_to = the_inputs_an_input_is_carried_on_to(graph, to_node, to.port_name());
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        if let Some(refusal) =
            refusal_of_a_link_from_an_output(carried_on, to_node, to.port_name(), input_declaration)
        {
            return Err(Error::InvalidLink(refusal));
        }
        for (following_node, following_output_name, downstream_node, downstream_input_port) in
            &inputs_carried_on_to
        {
            let Some(downstream_input_declaration) =
                input_port_family(downstream_node, downstream_input_port)
            else {
                continue;
            };
            if let Some(refusal) = refusal_of_a_link_from_an_output(
                carried_on,
                downstream_node,
                downstream_input_port,
                downstream_input_declaration,
            ) {
                return Err(Error::InvalidLink(format!(
                    "cannot link {from} to {to}: {}.{} carries on what {to} takes, and it \
                     feeds {}.{}: {refusal}",
                    following_node.display_name,
                    following_output_name,
                    downstream_node.display_name,
                    downstream_input_port
                )));
            }
        }
    }
    Ok(())
}

/// Refuse a config update for `processor_id` under which a link wired out of
/// it would be refused — an output whose names, family or codec resolve from
/// `config_to_apply` no longer meeting the input it feeds — naming the link.
pub(crate) fn refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
    graph: &Graph,
    processor_id: &ProcessorUniqueId,
    config_to_apply: &serde_json::Value,
) -> Result<()> {
    let Some(from_node) = graph.traversal().v(processor_id).first() else {
        return Ok(());
    };
    let outbound_links = graph
        .traversal()
        .v(processor_id)
        .out_e()
        .iter()
        .filter(|link| !link.has::<PendingDeletionComponent>());
    for link in outbound_links {
        let Some(to_node) = graph.traversal().v(link.to_port().processor_id()).first() else {
            continue;
        };
        let (Some(output_declaration), Some(input_declaration)) = (
            output_port_family(from_node, link.source().port_name()),
            input_port_family(to_node, link.to_port().port_name()),
        ) else {
            continue;
        };
        if output_declaration.family_follows_input_port.is_some() {
            continue;
        }
        let inputs_the_link_reaches =
            std::iter::once((to_node, link.to_port().port_name(), input_declaration)).chain(
                the_inputs_an_input_is_carried_on_to(graph, to_node, link.to_port().port_name())
                    .into_iter()
                    .filter_map(|(_, _, downstream_node, downstream_input_port)| {
                        input_port_family(downstream_node, downstream_input_port).map(
                            |downstream_input_declaration| {
                                (
                                    downstream_node,
                                    downstream_input_port,
                                    downstream_input_declaration,
                                )
                            },
                        )
                    }),
            );
        for (reached_node, reached_input_port, reached_input_declaration) in inputs_the_link_reaches
        {
            let refusal = refusal_of_a_link_between_port_families(
                PortFamilyLinkEndpoint {
                    node_name: &from_node.display_name,
                    port_name: link.source().port_name(),
                    declaration: output_declaration,
                },
                Some(config_to_apply),
                PortFamilyLinkEndpoint {
                    node_name: &reached_node.display_name,
                    port_name: reached_input_port,
                    declaration: reached_input_declaration,
                },
            );
            if let Some(refusal) = refusal {
                return Err(Error::Config(format!(
                    "config update for {} refused: under it the wired link {} → {}, which \
                     reaches {}.{reached_input_port}, is refused: {refusal}",
                    from_node.display_name,
                    link.source(),
                    link.to_port(),
                    reached_node.display_name,
                )));
            }
        }
    }
    Ok(())
}
