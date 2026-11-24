// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Connection wires: a wire into an input of a `Connection` type brings the node it comes from,
//! and carries nothing while the stream runs. The graph the runtime runs holds no such wire: its
//! input holds `{"node": "<the node>"}`, a node reference, which every check of a reference reads
//! and the referring node is handed. `graph` renders the wire back as drawn.

use std::collections::HashMap;

use streamlib_node_schema::NODE_REFERENCE_NODE_KEY;
use streamlib_runtime_client_contract::local_api_wire_contract::GraphLinkStateOnTheWire;

use crate::core::descriptors::{
    NodeClassImportPath, NodeInputMode, PinLinkEnd, PinLinkInputEnd, PinLinkOutcome, ResolvedPin,
    pin_link_verdict,
};
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, GraphSnapshotLink, refuse_a_port_the_node_does_not_have,
    the_ports_of_a_graph_document_node,
};
use crate::core::graph::{GraphNodePorts, PortInfo, cast_exposed_name_to_url_safe};
use crate::core::json_schema::{GraphResponse, LinkOutput, LinkPortRefOutput};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::{Error, PortDirection, Result};

#[cfg(test)]
mod tests;

/// The connection wires a graph drew, each lowered to the node reference its input holds in the
/// graph the runtime runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConnectionWiresAsDrawn {
    /// Each wire, between the nodes the runtime runs, in the order the graph held it.
    pub wires: Vec<GraphSnapshotLink>,
}

/// `end` as a refusal names it, `node/port`.
fn the_end_named(end: &LinkPortRefOutput) -> String {
    format!("{}/{}", end.node, end.port)
}

impl GraphSnapshot {
    /// This graph with each link into a `Connection` input lowered to the node reference that
    /// input holds, beside the wires so lowered. A wire is refused, naming both ends, when its
    /// output is not a `Connection` of the input's connection type, in the verdict's words, when
    /// its output is no port of its node, and when a second wire ends at the same input.
    pub fn with_connection_wires_lowered(
        &self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<(GraphSnapshot, ConnectionWiresAsDrawn)> {
        let mut ports_of_each_node_by_cast: HashMap<String, (Vec<PortInfo>, Vec<PortInfo>)> =
            HashMap::new();
        let mut node_index_by_cast: HashMap<String, usize> = HashMap::new();
        for (node_index, node) in self.nodes.iter().enumerate() {
            let cast = cast_exposed_name_to_url_safe(&node.name)?.into_owned();
            if let Some(ports) = the_ports_of_a_graph_document_node(node_types, node) {
                ports_of_each_node_by_cast.insert(cast.clone(), ports);
            }
            node_index_by_cast.insert(cast, node_index);
        }

        let mut lowered = GraphSnapshot {
            asset: self.asset,
            display_name: self.display_name.clone(),
            stream: self.stream.clone(),
            clock: self.clock,
            nodes: self.nodes.clone(),
            links: Vec::with_capacity(self.links.len()),
            groups: self.groups.clone(),
            studio: self.studio.clone(),
        };
        let mut wires = ConnectionWiresAsDrawn::default();
        let mut wire_into_each_input: HashMap<(String, String), &GraphSnapshotLink> =
            HashMap::new();
        for link in &self.links {
            let target_cast = cast_exposed_name_to_url_safe(&link.target.node)?.into_owned();
            let input_cast = cast_exposed_name_to_url_safe(&link.target.port)?.into_owned();
            let Some((input, input_pin)) = ports_of_each_node_by_cast
                .get(&target_cast)
                .and_then(|(inputs, _)| {
                    GraphNodePorts::the_port_of_a_type_a_name_finds(inputs, &input_cast)
                })
                .filter(|input| input.takes_a_connection())
                .and_then(|input| input.pin.as_ref().map(|pin| (input, pin)))
            else {
                lowered.links.push(link.clone());
                continue;
            };
            if let Some(member) = link.source.member.as_ref().or(link.target.member.as_ref()) {
                return Err(Error::GraphError(format!(
                    "`{}` is wired into `{}`, a connection input, naming member `{member}`, and a \
                     connection is one node, with no members. To fix: wire the whole port",
                    the_end_named(&link.source),
                    the_end_named(&link.target)
                )));
            }
            let source_cast = cast_exposed_name_to_url_safe(&link.source.node)?;
            let output_cast = cast_exposed_name_to_url_safe(&link.source.port)?;
            let Some(output) = ports_of_each_node_by_cast
                .get(source_cast.as_ref())
                .and_then(|(_, outputs)| {
                    GraphNodePorts::the_port_of_a_type_a_name_finds(outputs, &output_cast)
                })
            else {
                return Err(self.the_refusal_of_a_wire_from_no_output(node_types, link));
            };
            let target_index = node_index_by_cast.get(&target_cast).copied();
            let Some(target_node_type) = target_index
                .and_then(|target_index| self.nodes.get(target_index))
                .map(|target| &target.node_type)
            else {
                lowered.links.push(link.clone());
                continue;
            };
            refuse_a_wire_whose_output_gives_out_another_connection(
                link,
                output,
                WireTarget {
                    node_type: target_node_type,
                    input_mode: input.input_mode,
                    pin: input_pin,
                },
            )?;
            if let Some(first) =
                wire_into_each_input.insert((target_cast.clone(), input_cast.clone()), link)
            {
                return Err(Error::GraphError(format!(
                    "input `{}` of `{}` is wired from both `{}` and `{}`, and a connection \
                     input takes the one node its wire brings. To fix: take one of the wires out",
                    link.target.port,
                    link.target.node,
                    the_end_named(&first.source),
                    the_end_named(&link.source)
                )));
            }
            if let Some(target) =
                target_index.and_then(|target_index| lowered.nodes.get_mut(target_index))
            {
                target.input_values.insert(
                    input_cast,
                    serde_json::json!({ NODE_REFERENCE_NODE_KEY: link.source.node }),
                );
            }
            wires.wires.push(link.clone());
        }
        Ok((lowered, wires))
    }

    /// Why `link`, a wire into a connection input, is refused when its output is no port of its
    /// node: as a link of a graph document naming a port its node does not have is refused.
    fn the_refusal_of_a_wire_from_no_output(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        link: &GraphSnapshotLink,
    ) -> Error {
        let no_output = || {
            Error::GraphError(format!(
                "`{}` is wired into `{}`, and node `{}` holds no output of that name",
                the_end_named(&link.source),
                the_end_named(&link.target),
                link.source.node
            ))
        };
        let source_cast = cast_exposed_name_to_url_safe(&link.source.node);
        let Some(source) = self.nodes.iter().find(|node| {
            source_cast.as_ref().is_ok_and(|cast| {
                cast_exposed_name_to_url_safe(&node.name).is_ok_and(|held| held == *cast)
            })
        }) else {
            return no_output();
        };
        match refuse_a_port_the_node_does_not_have(
            node_types,
            &link.source.node,
            source,
            &link.source.port,
            PortDirection::Output,
        ) {
            Err(refusal) => refusal,
            Ok(()) => no_output(),
        }
    }
}

/// The input a connection wire ends at: its node's type, how it takes a value, and its pin.
struct WireTarget<'input> {
    node_type: &'input NodeClassImportPath,
    input_mode: Option<NodeInputMode>,
    pin: &'input ResolvedPin,
}

/// Refuse `link`, a wire into `target`, unless `output` gives out a `Connection` of the input's
/// connection type, in the words of the link verdict.
fn refuse_a_wire_whose_output_gives_out_another_connection(
    link: &GraphSnapshotLink,
    output: &PortInfo,
    target: WireTarget<'_>,
) -> Result<()> {
    let input_type = &target.pin.pin_type;
    let Some(output_pin) = output.pin.as_ref() else {
        return Err(Error::GraphError(format!(
            "`{}` is wired into `{}`, a {input_type}, and `{}` declares no pin type, so it \
             gives out no node. To fix: wire an output of that connection type",
            the_end_named(&link.source),
            the_end_named(&link.target),
            link.source.port
        )));
    };
    let verdict = pin_link_verdict(
        PinLinkEnd {
            node_name: &link.source.node,
            port_name: &link.source.port,
            member_name: None,
            pin_type: &output_pin.pin_type,
            meaning: None,
            flow: Some(output_pin.flow),
        },
        PinLinkInputEnd {
            end: PinLinkEnd {
                node_name: &link.target.node,
                port_name: &link.target.port,
                member_name: None,
                pin_type: input_type,
                meaning: None,
                flow: Some(target.pin.flow),
            },
            node_type: target.node_type,
            input_mode: target.input_mode,
        },
        &[],
    );
    match verdict.outcome {
        PinLinkOutcome::Connects => Ok(()),
        PinLinkOutcome::Refused(refusal) => Err(Error::GraphError(refusal.to_string())),
        PinLinkOutcome::ConnectsWithFreeConversion(_)
        | PinLinkOutcome::ConnectsThroughConvertNode(_) => Err(Error::GraphError(format!(
            "`{}` gives out {}, and `{}` takes a {input_type} only",
            the_end_named(&link.source),
            output_pin.pin_type,
            the_end_named(&link.target)
        ))),
    }
}

impl ConnectionWiresAsDrawn {
    /// Render each wire into `graph_response`, a render of the graph the runtime runs, as a wired
    /// link with no id, since no running link stands for it, its input holding no value, as
    /// drawn; one with an end the render does not hold, or whose name does not cast, is left
    /// out.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        let rendered_node_by_cast: HashMap<String, usize> = graph_response
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(node_index, node)| {
                cast_exposed_name_to_url_safe(&node.name)
                    .ok()
                    .map(|cast| (cast.into_owned(), node_index))
            })
            .collect();
        for wire in &self.wires {
            let (Some(source_node), Some(target_node), Some(input)) = (
                cast_exposed_name_to_url_safe(&wire.source.node).ok(),
                cast_exposed_name_to_url_safe(&wire.target.node).ok(),
                cast_exposed_name_to_url_safe(&wire.target.port).ok(),
            ) else {
                continue;
            };
            let (Some(_), Some(&target_index)) = (
                rendered_node_by_cast.get(source_node.as_ref()),
                rendered_node_by_cast.get(target_node.as_ref()),
            ) else {
                continue;
            };
            let target = &mut graph_response.nodes[target_index];
            target.input_values.remove(input.as_ref());
            target.pins.remove(input.as_ref());
            graph_response.links.push(LinkOutput {
                source: LinkPortRefOutput {
                    node: source_node.into_owned(),
                    port: wire.source.port.clone(),
                    member: None,
                },
                target: LinkPortRefOutput {
                    node: target_node.into_owned(),
                    port: wire.target.port.clone(),
                    member: None,
                },
                state: GraphLinkStateOnTheWire::Wired,
                ..LinkOutput::default()
            });
        }
    }
}
