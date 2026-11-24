// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The checks the graph makes between the two ports of a link by their port
//! families: at connect, and again when a live config update changes what an
//! output resolves from its node's config.

use super::pin_type_link_checks::ALinkConnected;
use super::pin_type_resolution::{PinTypeInItsGraph, the_pin_type_of_a_port_in_its_graph};
use super::{Graph, GraphNode, InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::core::descriptors::{
    NamedPinTypeImportPath, PinType, PortFamilyDeclaration, PortFamilyLinkEndpoint,
    PortFamilyLinkRefusal, refusal_of_a_link_between_port_families,
};
use std::collections::HashSet;

use crate::core::{Error, PortDirection, Result};

fn output_port_family<'node>(
    node: &'node GraphNode,
    port_name: &str,
) -> Option<&'node PortFamilyDeclaration> {
    node.ports
        .outputs
        .iter()
        .find(|port| port.name == port_name)
        .map(|port| &port.family)
}

fn input_port_family<'node>(
    node: &'node GraphNode,
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
    node: &'graph GraphNode,
    port_name: &'graph str,
) -> Vec<(
    &'graph GraphNode,
    &'graph str,
    &'graph PortFamilyDeclaration,
)> {
    let mut carried_on = Vec::new();
    let mut outputs_to_follow = vec![(node, port_name)];
    let mut outputs_followed: HashSet<(&NodeRunId, &str)> = HashSet::new();
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
            link.to_port().node_run_id() == &node.id
                && link.to_port().port_name() == followed_input_port
                && graph.stands(link)
        });
        for link_into_the_followed_input in links_into_the_followed_input {
            if let Some(upstream_node) = graph
                .traversal()
                .v(link_into_the_followed_input.source().node_run_id())
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
    node_id: &NodeRunId,
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
pub(crate) fn the_inputs_an_input_is_carried_on_to<'graph>(
    graph: &'graph Graph,
    node: &'graph GraphNode,
    input_port_name: &'graph str,
) -> Vec<(
    &'graph GraphNode,
    &'graph str,
    &'graph GraphNode,
    &'graph str,
)> {
    let mut inputs_carried_on_to = Vec::new();
    let mut inputs_to_follow = vec![(node, input_port_name)];
    let mut inputs_followed: HashSet<(&NodeRunId, &str)> = HashSet::new();
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
                    link.source().port_name() == following_output.name && graph.stands(link)
                });
            for link_out in links_out_of_the_following_output {
                let Some(downstream_node) = graph
                    .traversal()
                    .v(link_out.to_port().node_run_id())
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
    (from_node, from_port_name, output_declaration): (&GraphNode, &str, &PortFamilyDeclaration),
    to_node: &GraphNode,
    to_port_name: &str,
    input_declaration: &PortFamilyDeclaration,
) -> Option<PortFamilyLinkRefusal> {
    refusal_of_a_link_between_port_families(
        PortFamilyLinkEndpoint {
            node_name: &from_node.display_name,
            port_name: from_port_name,
            declaration: output_declaration,
        },
        Some(&from_node.the_values_its_port_facts_read()),
        PortFamilyLinkEndpoint {
            node_name: &to_node.display_name,
            port_name: to_port_name,
            declaration: input_declaration,
        },
    )
}

/// How the family checks take a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FamiliesTakeTheLink {
    /// Nothing in them refuses it.
    Wholly,
    /// A saved graph's link is kept though its output may write an Image format an input it
    /// reaches does not read, in these words.
    ButForImageFormatsNotRead(String),
}

impl FamiliesTakeTheLink {
    /// Take `refusal` of a link connected as `link`: a saved graph's link is kept when its Image
    /// formats alone refuse it, the first such refusal kept in its words as `in_words` puts
    /// them; any other refusal is the link's.
    fn take(
        &mut self,
        refusal: PortFamilyLinkRefusal,
        link: ALinkConnected,
        in_words: impl FnOnce(String) -> String,
    ) -> Result<()> {
        match (refusal, link) {
            (PortFamilyLinkRefusal::ImageFormatsNotRead(words), ALinkConnected::OfASavedGraph) => {
                if *self == Self::Wholly {
                    *self = Self::ButForImageFormatsNotRead(in_words(words));
                }
                Ok(())
            }
            (refusal, _) => Err(Error::InvalidLink(in_words(refusal.to_string()))),
        }
    }
}

/// Refuse a link connected as `link` whose two ports' families, names, codec
/// or formats do not match, naming both endpoints; a saved graph's link whose
/// Image formats alone do not match is taken, as the answer says.
///
/// An output whose family follows an input of its node is checked as each
/// output linked into that input, once one is; and a link into such an input
/// is checked, as that output, against every input the following output
/// already feeds.
pub(crate) fn refuse_a_link_between_ports_whose_families_do_not_match(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    link: ALinkConnected,
) -> Result<FamiliesTakeTheLink> {
    let mut taken = FamiliesTakeTheLink::Wholly;
    let (Some(from_node), Some(to_node)) = (
        graph.traversal().v(from.node_run_id()).first(),
        graph.traversal().v(to.node_run_id()).first(),
    ) else {
        return Ok(taken);
    };
    let Some(input_declaration) = input_port_family(to_node, to.port_name()) else {
        return Ok(taken);
    };
    if output_port_family(from_node, from.port_name()).is_none() {
        return Ok(taken);
    }
    refuse_a_named_type_at_one_end_of_a_link_untyped_at_the_other(
        graph,
        (from_node, from),
        (to_node, to),
    )?;
    let inputs_carried_on_to = the_inputs_an_input_is_carried_on_to(graph, to_node, to.port_name());
    for carried_on in the_outputs_a_link_carries_on(graph, from_node, from.port_name()) {
        if let Some(refusal) =
            refusal_of_a_link_from_an_output(carried_on, to_node, to.port_name(), input_declaration)
        {
            taken.take(refusal, link, |words| words)?;
        }
        refuse_what_a_link_carries_on_past_its_input(
            carried_on,
            &inputs_carried_on_to,
            from,
            to,
            link,
            &mut taken,
        )?;
    }
    Ok(taken)
}

/// Refuse a link from `from` to `to` when one end carries a named type and the other is
/// untyped, naming the type and both ends: a named type travels as the bytes its class packs,
/// which belong to no family, so only a typed pin reads them.
fn refuse_a_named_type_at_one_end_of_a_link_untyped_at_the_other(
    graph: &Graph,
    (from_node, from): (&GraphNode, &OutputLinkPortRef),
    (to_node, to): (&GraphNode, &InputLinkPortRef),
) -> Result<()> {
    let pin_type_of = |node: &GraphNode, port_name: &str, direction: PortDirection| {
        let port = match direction {
            PortDirection::Input => node.input_port(port_name),
            PortDirection::Output => node.output_port(port_name),
        }?;
        Some(the_pin_type_of_a_port_in_its_graph(
            graph, node, port, direction,
        ))
    };
    let (Some(output_pin_type), Some(input_pin_type)) = (
        pin_type_of(from_node, from.port_name(), PortDirection::Output),
        pin_type_of(to_node, to.port_name(), PortDirection::Input),
    ) else {
        return Ok(());
    };
    let output = format!("`{}.{}`", from_node.display_name, from.port_name());
    let input = format!("`{}.{}`", to_node.display_name, to.port_name());
    let refusal = match (&output_pin_type, &input_pin_type) {
        (_, PinTypeInItsGraph::Untyped) => the_named_type_among(&output_pin_type).map(|named| {
            let named = named.as_str();
            format!(
                "cannot link {output} to {input}: {output} carries the named type `{named}`, \
                 whose bytes only a pin typed with it reads, and {input} states no pin type. \
                 To fix: link it to an input typed `{named}`"
            )
        }),
        (PinTypeInItsGraph::Untyped, _) => the_named_type_among(&input_pin_type).map(|named| {
            let named = named.as_str();
            format!(
                "cannot link {output} to {input}: {input} reads the named type `{named}`, which \
                 only a pin typed with it writes, and {output} states no pin type. To fix: link \
                 it from an output typed `{named}`"
            )
        }),
        _ => None,
    };
    refusal.map_or(Ok(()), |words| Err(Error::InvalidLink(words)))
}

/// The first named type among what a port carries in its graph; `None` when it carries none.
pub(super) fn the_named_type_among(
    pin_type: &PinTypeInItsGraph,
) -> Option<&NamedPinTypeImportPath> {
    let pin_types = match pin_type {
        PinTypeInItsGraph::Is(pin_type) => std::slice::from_ref(pin_type),
        PinTypeInItsGraph::OneOf(pin_types) => pin_types.as_slice(),
        PinTypeInItsGraph::Any | PinTypeInItsGraph::Untyped => &[],
    };
    pin_types.iter().find_map(|pin_type| match pin_type {
        PinType::Named { named_pin_type } => Some(named_pin_type),
        _ => None,
    })
}

/// Each input what arrives at an input is carried on to, with the following output it leaves
/// through, as [`the_inputs_an_input_is_carried_on_to`] finds them.
pub(crate) type InputsCarriedOnTo<'graph> = [(
    &'graph GraphNode,
    &'graph str,
    &'graph GraphNode,
    &'graph str,
)];

/// Refuse a link from `from` into `to`, connected as `link`, when an output that follows `to`'s
/// input carries what `carried_on` sends on into one of `inputs_carried_on_to` whose family
/// refuses it; a saved graph's link whose Image formats alone are refused is noted in `taken`.
pub(crate) fn refuse_what_a_link_carries_on_past_its_input(
    carried_on: (&GraphNode, &str, &PortFamilyDeclaration),
    inputs_carried_on_to: &InputsCarriedOnTo<'_>,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    link: ALinkConnected,
    taken: &mut FamiliesTakeTheLink,
) -> Result<()> {
    for &(following_node, following_output_name, downstream_node, downstream_input_port) in
        inputs_carried_on_to
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
            taken.take(refusal, link, |words| {
                format!(
                    "cannot link {from} to {to}: {}.{} carries on what {to} takes, and it \
                     feeds {}.{}: {words}",
                    following_node.display_name,
                    following_output_name,
                    downstream_node.display_name,
                    downstream_input_port
                )
            })?;
        }
    }
    Ok(())
}

/// Refuse `input_values`, the values node `node_run_id` would hold after a set of a setting it
/// reads at setup, under which a link out of it, or what a passer carries on from one, would be
/// refused by its families, names or codec. Its Image formats are not judged again here: a wired
/// link has passed them or, a saved graph's, been kept despite them, and the one node whose
/// settings set them, a convert node whose output follows a setting, has its links judged under
/// the new values by
/// [`super::refuse_input_values_a_wired_link_out_of_a_convert_node_would_refuse`].
pub(crate) fn refuse_input_values_a_wired_link_out_of_the_node_would_refuse(
    graph: &Graph,
    node_run_id: &NodeRunId,
    input_values: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let input_values = &serde_json::Value::Object(input_values.clone());
    let Some(from_node) = graph.traversal().v(node_run_id).first() else {
        return Ok(());
    };
    let outbound_links = graph
        .traversal()
        .v(node_run_id)
        .out_e()
        .iter()
        .filter(|link| graph.stands(link));
    for link in outbound_links {
        let Some(to_node) = graph.traversal().v(link.to_port().node_run_id()).first() else {
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
                Some(input_values),
                PortFamilyLinkEndpoint {
                    node_name: &reached_node.display_name,
                    port_name: reached_input_port,
                    declaration: reached_input_declaration,
                },
            );
            if let Some(PortFamilyLinkRefusal::Other(refusal)) = refusal {
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
