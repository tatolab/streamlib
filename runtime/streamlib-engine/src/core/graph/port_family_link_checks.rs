// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The checks the graph makes between the two ports of a link by their port
//! families: at connect, and again when a live config update changes the names
//! an output resolves from its node's config.

use super::{
    Graph, GraphEdgeWithComponents, InputLinkPortRef, OutputLinkPortRef, PendingDeletionComponent,
    ProcessorNode, ProcessorUniqueId,
};
use crate::core::descriptors::{
    PortFamilyDeclaration, PortFamilyLinkEndpoint, output_port_names_resolved_against_config,
    refusal_of_a_link_between_port_families, refusal_of_required_names,
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

/// Refuse a config update for `processor_id` under which an output's names,
/// resolved from `config_to_apply`, drop a name a link wired out of it
/// requires, naming the link.
pub(crate) fn refuse_a_config_update_that_drops_a_name_a_wired_link_requires(
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
        if input_declaration.requires.is_empty() {
            continue;
        }
        let output = PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: link.source().port_name(),
            declaration: Some(output_declaration),
        };
        let input = PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: link.to_port().port_name(),
            declaration: Some(input_declaration),
        };
        let resolved_names = output_port_names_resolved_against_config(
            output_declaration.names.as_ref(),
            Some(config_to_apply),
        );
        let refusal = match resolved_names {
            Some(Ok(output_names)) => {
                refusal_of_required_names(output, &output_names, input, &input_declaration.requires)
            }
            Some(Err(unresolved)) => {
                Some(format!("{output}'s names cannot be resolved: {unresolved}"))
            }
            None => None,
        };
        if let Some(refusal) = refusal {
            return Err(Error::Config(format!(
                "config update for {} refused: the wired link {} → {} requires names this \
                 config drops: {refusal}",
                from_node.display_name,
                link.source(),
                link.to_port()
            )));
        }
    }
    Ok(())
}
