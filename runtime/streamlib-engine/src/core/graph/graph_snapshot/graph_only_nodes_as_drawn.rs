// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a graph holds that runs nothing, as drawn, and the graph snapshot with those graph-only nodes resolved.

use std::collections::HashSet;

use crate::core::graph::connection_wire_lowering::ConnectionWiresAsDrawn;
use crate::core::graph::graph_fragment_flattening::GraphFragmentsAsDrawn;
use crate::core::graph::graph_interface_resolution::GraphInterfaceAsDrawn;
use crate::core::graph::graph_variable_resolution::{
    GraphVariablesAsDrawn, SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY, SetVariableAndTheValueItHolds,
    ValueASetVariableHolds,
};
use crate::core::graph::{cast_exposed_name_to_url_safe, node_names_listed_for_a_refusal};
use crate::core::json_schema::{GraphResponse, LabelledNodeGroup, LinkPortRefOutput};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::{Error, PortDirection, Result};

use super::{
    GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode, refuse_a_port_the_node_does_not_have,
};

/// What the graph holds that runs nothing, as drawn: fragments, variables, interface nodes and
/// the stream level's labelled groups and canvas notes, so `graph` renders them back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphOnlyNodesAsDrawn {
    pub fragments_as_drawn: GraphFragmentsAsDrawn,
    pub variables_as_drawn: GraphVariablesAsDrawn,
    pub connection_wires_as_drawn: ConnectionWiresAsDrawn,
    pub interface_as_drawn: GraphInterfaceAsDrawn,
    pub groups_as_drawn: Vec<LabelledNodeGroup>,
    pub studio_as_drawn: Option<serde_json::Value>,
    pub links_set_aside: Vec<LinkSetAside>,
}

/// A link of a graph document held as drawn and never wired, because the graph's validation
/// refuses it or the engine cannot wire it, so the stream's graph still reads and edits and the
/// stream plays without it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSetAside {
    /// The link as the document draws it.
    pub link: GraphSnapshotLink,
    /// Why it is set aside, in the runtime's words.
    pub why_it_is_set_aside: String,
}

impl LinkSetAside {
    /// `link`, set aside for `refusal`.
    pub fn because_of(link: GraphSnapshotLink, refusal: &Error) -> Self {
        Self {
            link,
            why_it_is_set_aside: refusal.to_string(),
        }
    }

    /// The link and why it is refused, as a reader is told it.
    pub fn described(&self) -> String {
        format!(
            "its link `{}/{}` -> `{}/{}` is refused: {}",
            self.link.source.node,
            self.link.source.port,
            self.link.target.node,
            self.link.target.port,
            self.why_it_is_set_aside
        )
    }
}

/// A change to a node that a drawn link naming it by name blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeChangeADrawnLinkBlocks {
    /// Taking the node out.
    Removal,
    /// Giving the node another name.
    Rename,
}

impl NodeChangeADrawnLinkBlocks {
    fn as_refused(self) -> &'static str {
        match self {
            Self::Removal => "removed",
            Self::Rename => "renamed",
        }
    }
}

impl GraphOnlyNodesAsDrawn {
    /// Refuse `change` to the node named `node_name` while it is part of a fragment, while a
    /// drawn link between it and a fragment, a variable or an interface node names it, which
    /// would leave that link dangling in the graph `graph` renders, and while a connection wire
    /// names it, whose input holds the node it brings by its name.
    pub fn refuse_a_node_change_a_drawn_link_names(
        &self,
        node_name: &str,
        change: NodeChangeADrawnLinkBlocks,
    ) -> Result<()> {
        let node_cast = cast_exposed_name_to_url_safe(node_name)?;
        if let Some(fragment) = self
            .fragments_as_drawn
            .fragment_holding_the_running_node(&node_cast)
        {
            return Err(Error::GraphError(format!(
                "node `{node_name}` cannot be {}: it is part of fragment `{fragment}`, which \
                 its graph generator built and which is read-only; re-run the generator to \
                 change it",
                change.as_refused()
            )));
        }
        let names_the_node = |end: &LinkPortRefOutput| {
            cast_exposed_name_to_url_safe(&end.node).is_ok_and(|end_cast| end_cast == node_cast)
        };
        let links_drawn_to_a_graph_only_node = self
            .variables_as_drawn
            .links_touching_a_variable_node
            .iter()
            .chain(
                self.interface_as_drawn
                    .links_touching_an_interface_node
                    .iter()
                    .map(|drawn| &drawn.link),
            )
            .chain(
                self.fragments_as_drawn
                    .links_touching_a_fragment
                    .iter()
                    .map(|drawn| &drawn.link),
            );
        if let Some(wire) = self
            .connection_wires_as_drawn
            .wires
            .iter()
            .find(|wire| names_the_node(&wire.source) || names_the_node(&wire.target))
        {
            return Err(Error::GraphError(format!(
                "node `{node_name}` cannot be {}: the graph wires `{}/{}` into the connection \
                 input `{}/{}`, which holds the node it brings by its name. To fix: take the \
                 wire out first, then wire it again",
                change.as_refused(),
                wire.source.node,
                wire.source.port,
                wire.target.node,
                wire.target.port
            )));
        }
        for link in links_drawn_to_a_graph_only_node {
            if names_the_node(&link.source) || names_the_node(&link.target) {
                return Err(Error::GraphError(format!(
                    "node `{node_name}` cannot be {}: the graph links `{}/{}` to `{}/{}`, \
                     an end of which is a fragment, a variable or an interface node, and that link would \
                     name a node the graph no longer holds",
                    change.as_refused(),
                    link.source.node,
                    link.source.port,
                    link.target.node,
                    link.target.port
                )));
            }
        }
        Ok(())
    }

    /// The cast name of every fragment, variable and interface node drawn that runs nothing.
    pub fn node_names_cast(&self) -> HashSet<String> {
        let fragment_names_cast = self.fragments_as_drawn.fragment_names_flattened_and_cast();
        self.variables_as_drawn
            .variable_nodes
            .iter()
            .chain(
                self.interface_as_drawn
                    .interface_nodes
                    .iter()
                    .filter(|interface_node| !interface_node.declared_port.runs_as_a_source_node)
                    .map(|interface_node| &interface_node.node),
            )
            .filter_map(|node| {
                cast_exposed_name_to_url_safe(&node.name)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            })
            .chain(fragment_names_cast)
            .collect()
    }

    /// Render the graph-only nodes into `graph_response`, a render of the graph
    /// the runtime runs, in the reverse of the order they were resolved: the
    /// interface, then the variables, then each fragment as a container.
    pub fn render_into(
        &self,
        graph_response: &mut GraphResponse,
        node_types: &NodeTypesOneStreamResolves,
    ) {
        self.interface_as_drawn.render_into(graph_response);
        self.connection_wires_as_drawn.render_into(graph_response);
        self.variables_as_drawn.render_into(graph_response);
        self.fragments_as_drawn
            .render_into(graph_response, node_types);
        graph_response.groups = self.groups_as_drawn.clone();
        graph_response.studio = self.studio_as_drawn.clone();
        let cast = |name: &str| {
            cast_exposed_name_to_url_safe(name)
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_else(|_| name.to_string())
        };
        for set_aside in &self.links_set_aside {
            graph_response
                .links
                .push(crate::core::json_schema::LinkOutput {
                    source: LinkPortRefOutput {
                        node: cast(&set_aside.link.source.node),
                        port: set_aside.link.source.port.clone(),
                        member: set_aside.link.source.member.clone(),
                    },
                    target: LinkPortRefOutput {
                        node: cast(&set_aside.link.target.node),
                        port: set_aside.link.target.port.clone(),
                        member: set_aside.link.target.member.clone(),
                    },
                    state: crate::core::json_schema::LinkStateOutput::Error,
                    error_reason: Some(set_aside.why_it_is_set_aside.clone()),
                    ..crate::core::json_schema::LinkOutput::default()
                });
        }
    }
}

/// A graph with its variables and interface nodes resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithGraphOnlyNodesResolved {
    /// The graph the runtime runs: no variable or interface node.
    pub graph_the_runtime_runs: GraphSnapshot,
    pub graph_only_nodes_as_drawn: GraphOnlyNodesAsDrawn,
    /// Every `SetVariable` and its value, by the setter's name.
    pub values_the_set_variables_hold: Vec<SetVariableAndTheValueItHolds>,
}

impl GraphSnapshotWithGraphOnlyNodesResolved {
    /// Check the graph as [`Self::validate`] does, setting aside each link whose addition it
    /// refuses, in the order the graph holds them, so every node and each other link is kept;
    /// refused as `validate` refuses it when the graph holding none of its links does not pass.
    pub fn validated_setting_aside_each_link_it_refuses(
        mut self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<Self> {
        let Err(refusal) = self.validate(node_types) else {
            return Ok(self);
        };
        let links = std::mem::take(&mut self.graph_the_runtime_runs.links);
        if self.validate(node_types).is_err() {
            return Err(refusal);
        }
        for link in links {
            self.graph_the_runtime_runs.links.push(link);
            let Err(link_refusal) = self.validate(node_types) else {
                continue;
            };
            if let Some(link) = self.graph_the_runtime_runs.links.pop() {
                self.graph_only_nodes_as_drawn
                    .links_set_aside
                    .push(LinkSetAside::because_of(link, &link_refusal));
            }
        }
        Ok(self)
    }

    /// Check the graph the runtime runs, that each `SetVariable` holds a node
    /// of that graph, or an output port one of its nodes has, whether or not
    /// a get reads it, and that each interface port is linked to ports of its
    /// family.
    pub fn validate(&self, node_types: &NodeTypesOneStreamResolves) -> Result<()> {
        let graph = &self.graph_the_runtime_runs;
        graph.validate_a_graph_holding_no_graph_only_node(node_types)?;
        self.graph_only_nodes_as_drawn
            .interface_as_drawn
            .validate(graph, node_types)?;
        self.graph_only_nodes_as_drawn
            .fragments_as_drawn
            .validate(graph, node_types)?;
        let node_named = |node_name: &str| -> Result<Option<&GraphSnapshotNode>> {
            let cast = cast_exposed_name_to_url_safe(node_name)?;
            Ok(graph.nodes.iter().find(|node| {
                cast_exposed_name_to_url_safe(&node.name).is_ok_and(|node_cast| node_cast == cast)
            }))
        };
        let the_names_the_graph_holds =
            || node_names_listed_for_a_refusal(graph.nodes.iter().map(|node| node.name.as_str()));
        for SetVariableAndTheValueItHolds {
            set_variable_node,
            value,
        } in &self.values_the_set_variables_hold
        {
            let held_node_name = match value {
                ValueASetVariableHolds::PortOutput(source) => &source.node,
                ValueASetVariableHolds::NodeReference(referenced_node) => referenced_node,
            };
            let Some(held_node) = node_named(held_node_name)? else {
                if let Some(interface_node) = self
                    .graph_only_nodes_as_drawn
                    .interface_as_drawn
                    .interface_node_named(held_node_name)
                {
                    if let ValueASetVariableHolds::PortOutput(source) = value
                        && let Some(refusal) = interface_node
                            .declared_port
                            .refusal_of_an_output_it_does_not_have(source)
                    {
                        return Err(Error::GraphError(format!(
                            "`SetVariable` node `{set_variable_node}` holds a port the graph \
                             does not hold: {refusal}"
                        )));
                    }
                    continue;
                }
                return Err(Error::GraphError(match value {
                    ValueASetVariableHolds::PortOutput(_) => format!(
                        "`SetVariable` node `{set_variable_node}` holds node \
                         `{held_node_name}`, which the graph does not hold. The graph holds: {}",
                        the_names_the_graph_holds()
                    ),
                    ValueASetVariableHolds::NodeReference(_) => format!(
                        "input `{SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY}` of `SetVariable` node \
                         `{set_variable_node}` names node `{held_node_name}`, which the graph \
                         does not hold. The graph holds: {}",
                        the_names_the_graph_holds()
                    ),
                }));
            };
            if let ValueASetVariableHolds::PortOutput(source) = value {
                refuse_a_port_the_node_does_not_have(
                    node_types,
                    &source.node,
                    held_node,
                    &source.port,
                    PortDirection::Output,
                )?;
            }
        }
        Ok(())
    }
}
