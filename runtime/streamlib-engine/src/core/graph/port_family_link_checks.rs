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
        .and_then(|port| port.family.as_ref())
}

fn input_port_family<'node>(
    node: &'node ProcessorNode,
    port_name: &str,
) -> Option<&'node PortFamilyDeclaration> {
    node.ports
        .inputs
        .iter()
        .find(|port| port.name == port_name)
        .and_then(|port| port.family.as_ref())
}

/// Refuse a link whose two ports' families, names, codec or formats do not
/// match, naming both endpoints; a decorator port at either end takes any link.
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
    let refusal = refusal_of_a_link_between_port_families(
        PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: from.port_name(),
            declaration: output_port_family(from_node, from.port_name()),
        },
        from_node.config.as_ref(),
        PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: to.port_name(),
            declaration: input_port_family(to_node, to.port_name()),
        },
    );
    match refusal {
        Some(refusal) => Err(Error::InvalidLink(refusal)),
        None => Ok(()),
    }
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
        let refusal = refusal_of_a_link_between_port_families(
            PortFamilyLinkEndpoint {
                node_name: &from_node.display_name,
                port_name: link.source().port_name(),
                declaration: output_port_family(from_node, link.source().port_name()),
            },
            Some(config_to_apply),
            PortFamilyLinkEndpoint {
                node_name: &to_node.display_name,
                port_name: link.to_port().port_name(),
                declaration: input_port_family(to_node, link.to_port().port_name()),
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
