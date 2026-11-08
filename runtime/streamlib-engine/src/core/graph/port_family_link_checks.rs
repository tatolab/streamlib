// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

/// The output a link out of `port_name` of `node` carries on: the output
/// itself, or, for one whose family follows an input of its node, the output
/// linked into that input, followed upstream. `None` while an input it follows
/// has no link yet, so its family is not settled.
fn the_output_a_link_carries_on<'graph>(
    graph: &'graph Graph,
    node: &'graph ProcessorNode,
    port_name: &'graph str,
) -> Option<(
    &'graph ProcessorNode,
    &'graph str,
    &'graph PortFamilyDeclaration,
)> {
    let mut carried_on = (node, port_name);
    for _ in 0..MOST_OUTPUTS_A_FAMILY_FOLLOWS_THROUGH {
        let (node, port_name) = carried_on;
        let declaration = output_port_family(node, port_name)?;
        let Some(followed_input_port) = declaration.family_follows_input_port.as_deref() else {
            return Some((node, port_name, declaration));
        };
        let link_into_the_followed_input = graph.traversal().e(()).iter().find(|link| {
            link.to_port().processor_id() == &node.id
                && link.to_port().port_name() == followed_input_port
                && !link.has::<PendingDeletionComponent>()
        })?;
        let upstream_node = graph
            .traversal()
            .v(link_into_the_followed_input.source().processor_id())
            .first()?;
        carried_on = (
            upstream_node,
            link_into_the_followed_input.source().port_name(),
        );
    }
    None
}

/// How many outputs whose family follows an input one link's check follows
/// upstream before it stops, which bounds a loop of them.
const MOST_OUTPUTS_A_FAMILY_FOLLOWS_THROUGH: usize = 64;

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
/// An output whose family follows an input of its node is checked as the
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
    let Some(carried_on) = the_output_a_link_carries_on(graph, from_node, from.port_name()) else {
        return Ok(());
    };
    if let Some(refusal) =
        refusal_of_a_link_from_an_output(carried_on, to_node, to.port_name(), input_declaration)
    {
        return Err(Error::InvalidLink(refusal));
    }
    let outputs_following_the_input = to_node.ports.outputs.iter().filter(|output| {
        output.family.family_follows_input_port.as_deref() == Some(to.port_name())
    });
    for following_output in outputs_following_the_input {
        let links_out_of_the_following_output = graph
            .traversal()
            .v(&to_node.id)
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
            let Some(downstream_input_declaration) =
                input_port_family(downstream_node, link_out.to_port().port_name())
            else {
                continue;
            };
            if let Some(refusal) = refusal_of_a_link_from_an_output(
                carried_on,
                downstream_node,
                link_out.to_port().port_name(),
                downstream_input_declaration,
            ) {
                return Err(Error::InvalidLink(format!(
                    "cannot link {from} to {to}: {}.{} carries on what {to} takes, and it \
                     already feeds {}.{}: {refusal}",
                    to_node.display_name,
                    following_output.name,
                    downstream_node.display_name,
                    link_out.to_port().port_name()
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
        let refusal = refusal_of_a_link_between_port_families(
            PortFamilyLinkEndpoint {
                node_name: &from_node.display_name,
                port_name: link.source().port_name(),
                declaration: output_declaration,
            },
            Some(config_to_apply),
            PortFamilyLinkEndpoint {
                node_name: &to_node.display_name,
                port_name: link.to_port().port_name(),
                declaration: input_declaration,
            },
        );
        if let Some(refusal) = refusal {
            return Err(Error::Config(format!(
                "config update for {} refused: under it the wired link {} → {} is refused: \
                 {refusal}",
                from_node.display_name,
                link.source(),
                link.to_port()
            )));
        }
    }
    Ok(())
}
