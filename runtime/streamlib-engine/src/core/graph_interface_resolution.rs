// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A graph's interface: each `GraphInput` and `GraphOutput` node declares one
//! port the graph offers, named and of one family.
//!
//! An interface node runs nothing. The graph the runtime runs holds neither
//! the node nor its links; what each declares is kept beside it, so `graph`
//! renders the interface back and Tato Studio reads a stream's Outputs.

use std::collections::HashMap;

use crate::core::descriptors::{
    PortFamily, PortFamilyDeclaration, PortFamilyLinkEndpoint,
    refusal_of_a_link_between_port_families,
};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::graph_snapshot::{
    GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode, refuse_a_port_the_node_does_not_have,
};
use crate::core::json_schema::{
    GraphResponse, LinkOutput, LinkPortRefOutput, PortInfoOutput, PortKindOutput,
    ProcessorNodeOutput, ProcessorNodePortsOutput,
};
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::utils::compute_json_checksum;
use crate::core::{Error, PortDirection, Result};

/// The import path a graph names `GraphInput` by, in the built-in node module.
pub const GRAPH_INPUT_NODE_TYPE: &str = "tatolab.stream:GraphInput";

/// The import path a graph names `GraphOutput` by, in the built-in node module.
pub const GRAPH_OUTPUT_NODE_TYPE: &str = "tatolab.stream:GraphOutput";

/// The config key an interface node names the port it declares under.
pub const GRAPH_INTERFACE_PORT_CONFIG_KEY: &str = "port";

/// The config key an interface node names its port's family under.
pub const GRAPH_INTERFACE_FAMILY_CONFIG_KEY: &str = "family";

/// The one port an interface node has: a `GraphOutput`'s input, a
/// `GraphInput`'s output.
pub const GRAPH_INTERFACE_VALUE_PORT_NAME: &str = "value";

/// The component key an interface node, and a drawn link with an end on one,
/// renders what it declares under.
pub const GRAPH_INTERFACE_COMPONENT_KEY: &str = "graph_interface";

/// Whether `node_type` is `GraphInput` or `GraphOutput`.
pub fn is_a_graph_interface_node_type(node_type: &str) -> bool {
    matches!(node_type, GRAPH_INPUT_NODE_TYPE | GRAPH_OUTPUT_NODE_TYPE)
}

/// Which way a declared port faces the graph that adds this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInterfacePortDirection {
    Input,
    Output,
}

/// One port a graph's interface declares.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphInterfacePort {
    /// The interface node's name, as the graph wrote it.
    pub interface_node: String,
    /// The port's name, as the interface node's config wrote it.
    pub port: String,
    pub family: PortFamily,
    pub direction: GraphInterfacePortDirection,
    /// A `GraphOutput`'s one source port; a `GraphInput`'s every reader.
    pub linked_ports: Vec<LinkPortRefOutput>,
}

/// One interface node as drawn, with the port it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphInterfaceNodeAsDrawn {
    pub node: GraphSnapshotNode,
    /// Where the node sat among the graph's nodes.
    pub position_among_the_graphs_nodes: usize,
    pub declared_port: GraphInterfacePort,
}

/// One link with an end on an interface node, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTouchingAnInterfaceNodeAsDrawn {
    pub link: GraphSnapshotLink,
    /// Where the link sat among the graph's links.
    pub position_among_the_graphs_links: usize,
}

/// What a graph's interface nodes resolved to, and what was drawn, so `graph`
/// renders them back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphInterfaceAsDrawn {
    /// Every interface node, in the order the graph drew them.
    pub interface_nodes: Vec<GraphInterfaceNodeAsDrawn>,
    pub links_touching_an_interface_node: Vec<LinkTouchingAnInterfaceNodeAsDrawn>,
}

/// A graph with its interface nodes resolved out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithInterfaceResolved {
    /// The graph the runtime runs, holding no interface node.
    pub graph_the_runtime_runs: GraphSnapshot,
    pub interface_as_drawn: GraphInterfaceAsDrawn,
}

impl GraphSnapshot {
    /// Resolve this graph's interface nodes out of it, refusing by name two
    /// nodes whose names cast alike, an interface node naming no port or no
    /// family, two declaring one port name in one direction, a `GraphOutput`
    /// linked from no port or from several, and a link into a `GraphInput` or
    /// out of a `GraphOutput`.
    pub fn with_graph_interface_resolved(&self) -> Result<GraphSnapshotWithInterfaceResolved> {
        if !self
            .nodes
            .iter()
            .any(|node| is_a_graph_interface_node_type(node.processor_type.as_str()))
        {
            return Ok(GraphSnapshotWithInterfaceResolved {
                graph_the_runtime_runs: self.clone(),
                interface_as_drawn: GraphInterfaceAsDrawn::default(),
            });
        }

        let mut direction_of_each_interface_node_by_cast = HashMap::new();
        let mut node_name_by_cast: HashMap<String, &str> = HashMap::new();
        for node in &self.nodes {
            let cast = cast_of(&node.name)?;
            if node_name_by_cast.insert(cast.clone(), &node.name).is_some() {
                return Err(Error::NodeNameTaken {
                    name: node.name.clone(),
                    cast,
                });
            }
            let direction = match node.processor_type.as_str() {
                GRAPH_INPUT_NODE_TYPE => GraphInterfacePortDirection::Input,
                GRAPH_OUTPUT_NODE_TYPE => GraphInterfacePortDirection::Output,
                _ => continue,
            };
            direction_of_each_interface_node_by_cast.insert(cast, direction);
        }

        let mut runtime_links = Vec::new();
        let mut links_touching_an_interface_node = Vec::new();
        let mut linked_ports_by_interface_node_cast: HashMap<String, Vec<LinkPortRefOutput>> =
            HashMap::new();
        for (link_position, link) in self.links.iter().enumerate() {
            let source_direction =
                direction_of_each_interface_node_by_cast.get(&cast_of(&link.source.node)?);
            let target_direction =
                direction_of_each_interface_node_by_cast.get(&cast_of(&link.target.node)?);
            if source_direction.is_none() && target_direction.is_none() {
                runtime_links.push(link.clone());
                continue;
            }
            if source_direction == Some(&GraphInterfacePortDirection::Output) {
                return Err(Error::GraphError(format!(
                    "a link leaves `GraphOutput` node `{}`, which has no output: a \
                     `GraphOutput` is where a port leaves the graph",
                    link.source.node
                )));
            }
            if target_direction == Some(&GraphInterfacePortDirection::Input) {
                return Err(Error::GraphError(format!(
                    "a link enters `GraphInput` node `{}`, which has no input: a \
                     `GraphInput` is where a port enters the graph",
                    link.target.node
                )));
            }
            if source_direction.is_some() && target_direction.is_some() {
                return Err(Error::GraphError(format!(
                    "`GraphInput` node `{}` is linked straight to `GraphOutput` node `{}`; \
                     a graph's interface declares ports of nodes that run",
                    link.source.node, link.target.node
                )));
            }
            let (interface_end, other_end) = if source_direction.is_some() {
                (&link.source, &link.target)
            } else {
                (&link.target, &link.source)
            };
            refuse_an_interface_port_other_than_value(interface_end)?;
            linked_ports_by_interface_node_cast
                .entry(cast_of(&interface_end.node)?)
                .or_default()
                .push(other_end.clone());
            links_touching_an_interface_node.push(LinkTouchingAnInterfaceNodeAsDrawn {
                link: link.clone(),
                position_among_the_graphs_links: link_position,
            });
        }

        let mut interface_nodes = Vec::new();
        let mut runtime_nodes = Vec::new();
        let mut interface_node_by_port_cast: HashMap<(String, bool), String> = HashMap::new();
        for (node_position, node) in self.nodes.iter().enumerate() {
            let node_cast = cast_of(&node.name)?;
            let Some(&direction) = direction_of_each_interface_node_by_cast.get(&node_cast) else {
                runtime_nodes.push(node.clone());
                continue;
            };
            let (port, family) = port_and_family_an_interface_node_declares(node)?;
            let port_cast = cast_of(&port)?;
            let is_an_output = direction == GraphInterfacePortDirection::Output;
            if let Some(first) = interface_node_by_port_cast
                .insert((port_cast.clone(), is_an_output), node.name.clone())
            {
                return Err(Error::GraphError(format!(
                    "`{first}` and `{}` both declare the graph's {} `{port_cast}`, and a graph \
                     declares each port once",
                    node.name,
                    if is_an_output { "Output" } else { "Input" }
                )));
            }
            let linked_ports = linked_ports_by_interface_node_cast
                .remove(&node_cast)
                .unwrap_or_default();
            if is_an_output && linked_ports.len() != 1 {
                return Err(Error::GraphError(format!(
                    "`GraphOutput` node `{}` declares the Output `{port}` from {} links; link \
                     exactly one port into its `{GRAPH_INTERFACE_VALUE_PORT_NAME}` input",
                    node.name,
                    linked_ports.len()
                )));
            }
            interface_nodes.push(GraphInterfaceNodeAsDrawn {
                node: node.clone(),
                position_among_the_graphs_nodes: node_position,
                declared_port: GraphInterfacePort {
                    interface_node: node.name.clone(),
                    port,
                    family,
                    direction,
                    linked_ports,
                },
            });
        }

        for exposed in &self.exposed {
            if direction_of_each_interface_node_by_cast.contains_key(&cast_of(&exposed.node)?) {
                return Err(Error::GraphError(format!(
                    "the graph exposes `{}/{}`, an interface node's port; an interface node \
                     declares a port and has none to expose",
                    exposed.node, exposed.port
                )));
            }
        }

        Ok(GraphSnapshotWithInterfaceResolved {
            graph_the_runtime_runs: GraphSnapshot {
                stream: self.stream.clone(),
                clock: self.clock,
                nodes: runtime_nodes,
                links: runtime_links,
                exposed: self.exposed.clone(),
            },
            interface_as_drawn: GraphInterfaceAsDrawn {
                interface_nodes,
                links_touching_an_interface_node,
            },
        })
    }
}

impl GraphInterfaceAsDrawn {
    /// Every port the interface declares, in the order its nodes were drawn.
    pub fn declared_ports(&self) -> impl Iterator<Item = &GraphInterfacePort> {
        self.interface_nodes
            .iter()
            .map(|interface_node| &interface_node.declared_port)
    }

    /// Whether `node_name` names one of the interface's nodes.
    pub fn holds_an_interface_node_named(&self, node_name: &str) -> bool {
        let Ok(cast) = cast_of(node_name) else {
            return false;
        };
        self.interface_nodes
            .iter()
            .any(|interface_node| cast_of(&interface_node.node.name).is_ok_and(|name| name == cast))
    }

    /// Refuse a port the interface declares whose linked port is not a port of
    /// the right direction on a node of `graph_the_runtime_runs`, or that the
    /// link rules refuse between it and a port of the declared family.
    pub fn validate(
        &self,
        graph_the_runtime_runs: &GraphSnapshot,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<()> {
        for declared in self.declared_ports() {
            for linked_port in &declared.linked_ports {
                let linked_cast = cast_of(&linked_port.node)?;
                let Some(linked_node) = graph_the_runtime_runs
                    .nodes
                    .iter()
                    .find(|node| cast_of(&node.name).is_ok_and(|cast| cast == linked_cast))
                else {
                    return Err(Error::GraphError(format!(
                        "interface node `{}` is linked to node `{}`, which the graph does not \
                         hold",
                        declared.interface_node, linked_port.node
                    )));
                };
                declared.refuse_a_linked_port_that_does_not_meet_it(
                    node_types,
                    linked_node,
                    linked_port,
                    &linked_node.config,
                )?;
            }
        }
        Ok(())
    }

    /// Refuse a config update for `node` under which a port the interface
    /// declares would no longer meet the port of `node` linked to it.
    pub fn refuse_a_config_update_that_breaks_a_declared_port(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        node: &GraphSnapshotNode,
        config_to_apply: &serde_json::Value,
    ) -> Result<()> {
        let node_cast = cast_of(&node.name)?;
        for declared in self.declared_ports() {
            for linked_port in &declared.linked_ports {
                if cast_of(&linked_port.node)? != node_cast {
                    continue;
                }
                declared
                    .refuse_a_linked_port_that_does_not_meet_it(
                        node_types,
                        node,
                        linked_port,
                        config_to_apply,
                    )
                    .map_err(|refusal| {
                        Error::Config(format!(
                            "config update for {} refused: {refusal}",
                            node.name
                        ))
                    })?;
            }
        }
        Ok(())
    }

    /// Render the interface into `graph_response`, a render of the graph the
    /// runtime runs: each interface node and each drawn link with an end on
    /// one back at its drawn position.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        for drawn in &self.links_touching_an_interface_node {
            let position = drawn
                .position_among_the_graphs_links
                .min(graph_response.links.len());
            let mut components = serde_json::Map::new();
            if let Some(declared) = self.port_a_link_meets(&drawn.link) {
                components.insert(
                    GRAPH_INTERFACE_COMPONENT_KEY.to_string(),
                    declared.as_component(),
                );
            }
            graph_response.links.insert(
                position,
                LinkOutput {
                    id: String::new(),
                    source: LinkPortRefOutput {
                        node: cast_or_as_written(&drawn.link.source.node),
                        port: drawn.link.source.port.clone(),
                    },
                    target: LinkPortRefOutput {
                        node: cast_or_as_written(&drawn.link.target.node),
                        port: drawn.link.target.port.clone(),
                    },
                    capacity: Default::default(),
                    state: Default::default(),
                    error_reason: None,
                    components,
                },
            );
        }
        for interface_node in &self.interface_nodes {
            let declared = &interface_node.declared_port;
            let value_port = PortInfoOutput {
                name: GRAPH_INTERFACE_VALUE_PORT_NAME.to_string(),
                description: String::new(),
                port_kind: PortKindOutput::default(),
                delivery_profile: None,
                borrow_quota: None,
                audio_window: None,
                catalog_entry: None,
                family: Some(PortFamilyDeclaration::of_family(declared.family)),
            };
            let (inputs, outputs) = match declared.direction {
                GraphInterfacePortDirection::Output => (vec![value_port], Vec::new()),
                GraphInterfacePortDirection::Input => (Vec::new(), vec![value_port]),
            };
            let mut components = serde_json::Map::new();
            components.insert(
                GRAPH_INTERFACE_COMPONENT_KEY.to_string(),
                declared.as_component(),
            );
            let position = interface_node
                .position_among_the_graphs_nodes
                .min(graph_response.nodes.len());
            graph_response.nodes.insert(
                position,
                ProcessorNodeOutput {
                    id: String::new(),
                    processor_type: interface_node.node.processor_type.clone(),
                    name: cast_or_as_written(&interface_node.node.name),
                    config: Some(interface_node.node.config.clone()),
                    config_checksum: compute_json_checksum(&interface_node.node.config),
                    ports: ProcessorNodePortsOutput { inputs, outputs },
                    components,
                },
            );
        }
    }

    fn port_a_link_meets(&self, link: &GraphSnapshotLink) -> Option<&GraphInterfacePort> {
        let source_cast = cast_or_as_written(&link.source.node);
        let target_cast = cast_or_as_written(&link.target.node);
        self.declared_ports().find(|declared| {
            let interface_cast = cast_or_as_written(&declared.interface_node);
            interface_cast == source_cast || interface_cast == target_cast
        })
    }
}

impl GraphInterfacePort {
    /// Refuse `linked_port` of `linked_node`, configured as `linked_node_config`,
    /// unless it is a port of the right direction the link rules let meet a
    /// port of this one's family: a `GraphOutput`'s source must write it, and
    /// each reader of a `GraphInput` must take it.
    fn refuse_a_linked_port_that_does_not_meet_it(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        linked_node: &GraphSnapshotNode,
        linked_port: &LinkPortRefOutput,
        linked_node_config: &serde_json::Value,
    ) -> Result<()> {
        let direction_of_the_linked_port = match self.direction {
            GraphInterfacePortDirection::Output => PortDirection::Output,
            GraphInterfacePortDirection::Input => PortDirection::Input,
        };
        refuse_a_port_the_node_does_not_have(
            node_types,
            &linked_port.node,
            linked_node,
            &linked_port.port,
            direction_of_the_linked_port,
        )?;
        let port_cast = cast_of(&linked_port.port)?;
        let Some((inputs, outputs)) = node_types.port_info(&linked_node.processor_type) else {
            return Ok(());
        };
        let linked_declaration = match self.direction {
            GraphInterfacePortDirection::Output => outputs,
            GraphInterfacePortDirection::Input => inputs,
        }
        .into_iter()
        .find(|port| port.name == port_cast)
        .and_then(|port| port.family);
        let declared_family = PortFamilyDeclaration::of_family(self.family);
        let interface_end = PortFamilyLinkEndpoint {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            declaration: Some(&declared_family),
        };
        let linked_end = PortFamilyLinkEndpoint {
            node_name: &linked_port.node,
            port_name: &linked_port.port,
            declaration: linked_declaration.as_ref(),
        };
        let refusal = match self.direction {
            GraphInterfacePortDirection::Output => refusal_of_a_link_between_port_families(
                linked_end,
                Some(linked_node_config),
                interface_end,
            ),
            GraphInterfacePortDirection::Input => {
                refusal_of_a_link_between_port_families(interface_end, None, linked_end)
            }
        };
        match refusal {
            Some(refusal) => Err(Error::GraphError(format!(
                "`{}` node `{}` declares `{}` as {}, and the link rules refuse it: {refusal}",
                match self.direction {
                    GraphInterfacePortDirection::Output => "GraphOutput",
                    GraphInterfacePortDirection::Input => "GraphInput",
                },
                self.interface_node,
                self.port,
                self.family
            ))),
            None => Ok(()),
        }
    }

    fn as_component(&self) -> serde_json::Value {
        serde_json::json!({
            "direction": match self.direction {
                GraphInterfacePortDirection::Input => "input",
                GraphInterfacePortDirection::Output => "output",
            },
            GRAPH_INTERFACE_PORT_CONFIG_KEY: self.port,
            GRAPH_INTERFACE_FAMILY_CONFIG_KEY: self.family.wire_name(),
        })
    }
}

fn port_and_family_an_interface_node_declares(
    node: &GraphSnapshotNode,
) -> Result<(String, PortFamily)> {
    let kind = if node.processor_type.as_str() == GRAPH_OUTPUT_NODE_TYPE {
        "GraphOutput"
    } else {
        "GraphInput"
    };
    if let Some(unread_key) = node.config.as_object().and_then(|config| {
        config.keys().find(|key| {
            ![
                GRAPH_INTERFACE_PORT_CONFIG_KEY,
                GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(Error::GraphError(format!(
            "`{kind}` node `{}` carries the config key `{unread_key}`, which it does not take. \
             It takes: `{GRAPH_INTERFACE_PORT_CONFIG_KEY}`, `{GRAPH_INTERFACE_FAMILY_CONFIG_KEY}`",
            node.name
        )));
    }
    let port = node
        .config
        .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)
        .and_then(serde_json::Value::as_str)
        .filter(|port| !port.is_empty())
        .ok_or_else(|| {
            Error::GraphError(format!(
                "`{kind}` node `{}` names no port: its config takes \
                 `{GRAPH_INTERFACE_PORT_CONFIG_KEY}`, a non-empty string",
                node.name
            ))
        })?;
    let family_wire_name = node
        .config
        .get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY)
        .and_then(serde_json::Value::as_str);
    let family = PortFamily::ALL
        .into_iter()
        .find(|family| Some(family.wire_name()) == family_wire_name)
        .ok_or_else(|| {
            Error::GraphError(format!(
                "`{kind}` node `{}` declares its port of family {}, and its config's \
                 `{GRAPH_INTERFACE_FAMILY_CONFIG_KEY}` is one of {}",
                node.name,
                family_wire_name.map_or_else(|| "nothing".to_string(), |name| format!("{name:?}")),
                PortFamily::ALL.map(|family| family.wire_name()).join(", ")
            ))
        })?;
    Ok((port.to_string(), family))
}

fn refuse_an_interface_port_other_than_value(end: &LinkPortRefOutput) -> Result<()> {
    if cast_exposed_name_to_url_safe(&end.port)
        .is_ok_and(|port_cast| port_cast == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "a link meets interface node `{}` at the port `{}`; an interface node's one port is \
         `{GRAPH_INTERFACE_VALUE_PORT_NAME}`",
        end.node, end.port
    )))
}

fn cast_of(name: &str) -> Result<String> {
    Ok(cast_exposed_name_to_url_safe(name)?.into_owned())
}

fn cast_or_as_written(name: &str) -> String {
    cast_exposed_name_to_url_safe(name)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithInterfaceResolved> {
        GraphSnapshot::from_graph_document(graph)
            .and_then(|graph| graph.with_graph_interface_resolved())
    }

    fn refusal_of(graph: serde_json::Value) -> String {
        resolved(graph)
            .expect_err("the graph is refused")
            .to_string()
    }

    fn a_camera_offered_as_an_output_and_a_display_fed_by_an_input() -> serde_json::Value {
        serde_json::json!({
            "nodes": [
                {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
                {"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
                {"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE,
                 "config": {"port": "overlay", "family": "image"}},
                {"name": "display", "type": "tatolab.display:Display", "config": {}},
            ],
            "links": [
                {"source": {"node": "camera", "port": "video"},
                 "target": {"node": "video", "port": "value"}},
                {"source": {"node": "overlay", "port": "value"},
                 "target": {"node": "display", "port": "video"}},
            ],
        })
    }

    #[test]
    fn interface_nodes_leave_the_graph_the_runtime_runs_and_their_ports_are_kept_beside_it() {
        let resolution = resolved(a_camera_offered_as_an_output_and_a_display_fed_by_an_input())
            .expect("the interface resolves");
        let names: Vec<&str> = resolution
            .graph_the_runtime_runs
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, ["camera", "display"]);
        assert!(resolution.graph_the_runtime_runs.links.is_empty());
        let ports: Vec<&GraphInterfacePort> =
            resolution.interface_as_drawn.declared_ports().collect();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].direction, GraphInterfacePortDirection::Output);
        assert_eq!(ports[0].port, "video");
        assert_eq!(ports[0].family, PortFamily::Image);
        assert_eq!(ports[0].linked_ports[0].node, "camera");
        assert_eq!(ports[1].direction, GraphInterfacePortDirection::Input);
        assert_eq!(ports[1].linked_ports[0].node, "display");
    }

    #[test]
    fn a_graph_with_no_interface_node_resolves_to_itself() {
        let graph = serde_json::json!({
            "nodes": [{"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}}],
            "links": [],
        });
        let resolution = resolved(graph.clone()).expect("resolves");
        assert_eq!(
            resolution.graph_the_runtime_runs,
            GraphSnapshot::from_graph_document(graph).expect("parses")
        );
    }

    #[test]
    fn two_outputs_declaring_one_port_are_refused_naming_both() {
        let refusal = refusal_of(serde_json::json!({
            "nodes": [
                {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
                {"name": "first", "type": GRAPH_OUTPUT_NODE_TYPE,
                 "config": {"port": "Video", "family": "image"}},
                {"name": "second", "type": GRAPH_OUTPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
            ],
            "links": [
                {"source": {"node": "camera", "port": "video"},
                 "target": {"node": "first", "port": "value"}},
                {"source": {"node": "camera", "port": "video"},
                 "target": {"node": "second", "port": "value"}},
            ],
        }));
        assert!(
            refusal.contains("`first`")
                && refusal.contains("`second`")
                && refusal.contains("`video`"),
            "got {refusal}"
        );
    }

    #[test]
    fn an_input_and_an_output_may_share_a_port_name() {
        let resolution = resolved(serde_json::json!({
            "nodes": [
                {"name": "effect", "type": "my_app:Effect", "config": {}},
                {"name": "video-in", "type": GRAPH_INPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
                {"name": "video-out", "type": GRAPH_OUTPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
            ],
            "links": [
                {"source": {"node": "video-in", "port": "value"},
                 "target": {"node": "effect", "port": "video_in"}},
                {"source": {"node": "effect", "port": "video_out"},
                 "target": {"node": "video-out", "port": "value"}},
            ],
        }))
        .expect("an Input and an Output of one name resolve");
        assert_eq!(resolution.interface_as_drawn.declared_ports().count(), 2);
    }

    #[test]
    fn an_output_linked_from_no_port_is_refused() {
        let unlinked = refusal_of(serde_json::json!({
            "nodes": [{"name": "video", "type": GRAPH_OUTPUT_NODE_TYPE,
                       "config": {"port": "video", "family": "image"}}],
            "links": [],
        }));
        assert!(unlinked.contains("from 0 links"), "got {unlinked}");
    }

    #[test]
    fn a_link_into_an_input_or_out_of_an_output_is_refused_naming_the_node() {
        let mut into_an_input = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
        into_an_input["links"]
            .as_array_mut()
            .expect("links")
            .push(serde_json::json!({
                "source": {"node": "camera", "port": "video"},
                "target": {"node": "overlay", "port": "value"},
            }));
        let refusal = refusal_of(into_an_input);
        assert!(
            refusal.contains("enters `GraphInput` node `overlay`"),
            "got {refusal}"
        );

        let mut out_of_an_output = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
        out_of_an_output["links"]
            .as_array_mut()
            .expect("links")
            .push(serde_json::json!({
                "source": {"node": "video", "port": "value"},
                "target": {"node": "display", "port": "video"},
            }));
        let refusal = refusal_of(out_of_an_output);
        assert!(
            refusal.contains("leaves `GraphOutput` node `video`"),
            "got {refusal}"
        );
    }

    #[test]
    fn an_interface_node_naming_no_port_an_unknown_family_or_another_key_is_refused() {
        let declaring = |config: serde_json::Value| {
            refusal_of(serde_json::json!({
                "nodes": [{"name": "overlay", "type": GRAPH_INPUT_NODE_TYPE, "config": config}],
                "links": [],
            }))
        };
        assert!(declaring(serde_json::json!({"family": "image"})).contains("names no port"));
        let unknown_family = declaring(serde_json::json!({"port": "overlay", "family": "video"}));
        assert!(
            unknown_family.contains("\"video\"") && unknown_family.contains("records"),
            "got {unknown_family}"
        );
        let another_key =
            declaring(serde_json::json!({"port": "overlay", "family": "image", "name": "overlay"}));
        assert!(another_key.contains("`name`"), "got {another_key}");
    }

    #[test]
    fn a_link_at_another_port_of_an_interface_node_is_refused() {
        let mut graph = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
        graph["links"][0]["target"]["port"] = serde_json::json!("video");
        let refusal = refusal_of(graph);
        assert!(refusal.contains("one port is `value`"), "got {refusal}");
    }

    #[test]
    fn the_render_puts_each_interface_node_and_link_back_where_it_was_drawn() {
        let resolution = resolved(a_camera_offered_as_an_output_and_a_display_fed_by_an_input())
            .expect("resolves");
        let a_rendered_node = |name: &str| ProcessorNodeOutput {
            id: format!("{name}-id"),
            processor_type: crate::core::descriptors::ProcessorClassImportPath::new("my_app:Node")
                .expect("import path"),
            name: name.to_string(),
            config: Some(serde_json::json!({})),
            config_checksum: 0,
            ports: ProcessorNodePortsOutput {
                inputs: Vec::new(),
                outputs: Vec::new(),
            },
            components: serde_json::Map::new(),
        };
        let mut graph_response: GraphResponse = serde_json::from_value(serde_json::json!({
            "machine": "rig", "nodes": [], "links": [],
        }))
        .expect("an empty render parses");
        graph_response.nodes = vec![a_rendered_node("camera"), a_rendered_node("display")];
        resolution
            .interface_as_drawn
            .render_into(&mut graph_response);
        let names: Vec<&str> = graph_response
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, ["camera", "video", "overlay", "display"]);
        assert_eq!(
            graph_response.nodes[1].components[GRAPH_INTERFACE_COMPONENT_KEY],
            serde_json::json!({"direction": "output", "port": "video", "family": "image"})
        );
        assert_eq!(graph_response.links.len(), 2);
        assert_eq!(graph_response.links[0].target.node, "video");
        assert_eq!(graph_response.links[1].source.node, "overlay");
    }

    #[test]
    fn a_node_whose_name_casts_like_an_interface_nodes_is_refused_as_a_name_taken() {
        let mut graph = a_camera_offered_as_an_output_and_a_display_fed_by_an_input();
        graph["nodes"][0]["name"] = serde_json::json!("Video");
        graph["links"][0]["source"]["node"] = serde_json::json!("Video");
        match resolved(graph) {
            Err(Error::NodeNameTaken { cast, .. }) => assert_eq!(cast, "video"),
            other => panic!("expected NodeNameTaken, got {other:?}"),
        }
    }
}
