// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A graph's fragments: each node holding a `fragment` places a graph generator's stored output.
//!
//! A fragment runs nothing itself. The graph the runtime runs holds its nodes instead, each
//! named by its path of names from the stream (`look/soft/blur`), with each link into one of
//! the fragment's ports carried to the nodes its `GraphInput` feeds and each link out of one
//! carried from the port its `GraphOutput` reads. What was drawn is kept beside it, so `graph`
//! renders each fragment back as one container.

use std::collections::{HashMap, HashSet};

use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::graph_interface_resolution::{
    GraphInterfaceAsDrawn, GraphInterfacePort, GraphInterfacePortDirection,
    is_a_graph_interface_node_type,
};
use crate::core::graph_snapshot::{GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode};
use crate::core::json_schema::{
    GraphFragmentOutput, GraphResponse, LinkOutput, LinkPortRefOutput, NodeKindOutput,
    ProcessorNodeOutput, ProcessorNodePortsOutput,
};
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::utils::compute_json_checksum;
use crate::core::{Error, Result};

/// What separates a fragment's name from the name of a node it holds, in the name the node
/// runs under.
pub const GRAPH_FRAGMENT_NODE_PATH_SEPARATOR: char = '/';

/// One fragment as drawn, with what its interface declares, so `graph` renders it back.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphFragmentAsDrawn {
    /// The fragment node as the document holds it, its stored output included.
    pub node: GraphSnapshotNode,
    /// The name the fragment's nodes run under, each followed by its own: its path of names
    /// from the stream.
    pub path_of_names: String,
    /// Where the fragment sat among its level's nodes.
    pub position_among_its_levels_nodes: usize,
    /// The ports its `GraphInput` and `GraphOutput` nodes declare, each linked port named as
    /// the graph the runtime runs names it.
    pub interface_as_drawn: GraphInterfaceAsDrawn,
    /// Every fragment this one holds.
    pub fragments_it_holds: Vec<GraphFragmentAsDrawn>,
}

/// One link of the stream's own level with an end on a fragment, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTouchingAFragmentAsDrawn {
    pub link: GraphSnapshotLink,
    /// Where the link sat among the graph's links.
    pub position_among_the_graphs_links: usize,
}

/// What a graph's fragments flattened from, so `graph` renders each back as a container.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphFragmentsAsDrawn {
    /// Every fragment of the stream's own level, in the order the graph drew them.
    pub fragments: Vec<GraphFragmentAsDrawn>,
    pub links_touching_a_fragment: Vec<LinkTouchingAFragmentAsDrawn>,
}

/// A graph with its fragments flattened into it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithFragmentsFlattened {
    /// The graph holding each fragment's nodes in its place: no fragment.
    pub graph_the_runtime_runs: GraphSnapshot,
    pub fragments_as_drawn: GraphFragmentsAsDrawn,
}

impl GraphSnapshot {
    /// Flatten this graph's fragments into it, refusing by name a link to a port a fragment's
    /// interface does not declare.
    pub fn with_graph_fragments_flattened(&self) -> Result<GraphSnapshotWithFragmentsFlattened> {
        if self.nodes.iter().all(|node| node.fragment.is_none()) {
            return Ok(GraphSnapshotWithFragmentsFlattened {
                graph_the_runtime_runs: self.clone(),
                fragments_as_drawn: GraphFragmentsAsDrawn::default(),
            });
        }
        let level = flatten_one_level(None, &self.nodes, &self.links)?;
        Ok(GraphSnapshotWithFragmentsFlattened {
            graph_the_runtime_runs: GraphSnapshot {
                asset: self.asset,
                stream: self.stream.clone(),
                clock: self.clock,
                nodes: level.nodes,
                links: level.links,
            },
            fragments_as_drawn: GraphFragmentsAsDrawn {
                fragments: level.fragments,
                links_touching_a_fragment: level.links_touching_a_fragment,
            },
        })
    }
}

/// One level of a graph — the stream's, or a fragment's — with the fragments it holds
/// flattened into it.
struct OneLevelFlattened {
    /// The level's own nodes, then every node of each fragment it holds.
    nodes: Vec<GraphSnapshotNode>,
    /// The level's own links, each end on a fragment carried to the port it stands for, then
    /// every link of each fragment it holds.
    links: Vec<GraphSnapshotLink>,
    fragments: Vec<GraphFragmentAsDrawn>,
    links_touching_a_fragment: Vec<LinkTouchingAFragmentAsDrawn>,
}

fn flatten_one_level(
    path_of_names: Option<&str>,
    nodes: &[GraphSnapshotNode],
    links: &[GraphSnapshotLink],
) -> Result<OneLevelFlattened> {
    let name_on_this_level = |name: &str| match path_of_names {
        Some(path_of_names) => format!("{path_of_names}{GRAPH_FRAGMENT_NODE_PATH_SEPARATOR}{name}"),
        None => name.to_string(),
    };
    let mut node_name_by_cast: HashMap<String, &str> = HashMap::new();
    for node in nodes {
        let cast = cast_of(&node.name)?;
        if node_name_by_cast.insert(cast.clone(), &node.name).is_some() {
            return Err(Error::NodeNameTaken {
                name: name_on_this_level(&node.name),
                cast,
            });
        }
    }
    let names_of_the_levels_running_nodes_cast: HashSet<String> = nodes
        .iter()
        .filter(|node| node.fragment.is_none())
        .filter_map(|node| cast_of(&node.name).ok())
        .collect();

    let mut own_nodes = Vec::new();
    let mut nodes_of_the_fragments_held = Vec::new();
    let mut links_of_the_fragments_held = Vec::new();
    let mut fragments = Vec::new();
    let mut fragment_by_cast_name: HashMap<String, usize> = HashMap::new();
    for (node_position, node) in nodes.iter().enumerate() {
        let Some(fragment) = &node.fragment else {
            let mut config = node.config.clone();
            if path_of_names.is_some() {
                name_each_node_reference_on_this_level_by_its_path(
                    &mut config,
                    &names_of_the_levels_running_nodes_cast,
                    &name_on_this_level,
                );
            }
            own_nodes.push(GraphSnapshotNode {
                name: name_on_this_level(&node.name),
                config,
                ..node.clone()
            });
            continue;
        };
        let fragment_path_of_names = name_on_this_level(&node.name);
        let inner = flatten_one_level(
            Some(&fragment_path_of_names),
            &fragment.nodes,
            &fragment.links,
        )?;
        let interface = GraphSnapshot {
            asset: None,
            stream: None,
            clock: Default::default(),
            nodes: inner.nodes,
            links: inner.links,
        }
        .with_graph_interface_resolved()
        .map_err(|refusal| {
            Error::GraphError(format!(
                "fragment `{fragment_path_of_names}` does not flatten: {refusal}"
            ))
        })?;
        nodes_of_the_fragments_held.extend(interface.graph_the_runtime_runs.nodes);
        links_of_the_fragments_held.extend(interface.graph_the_runtime_runs.links);
        fragment_by_cast_name.insert(cast_of(&node.name)?, fragments.len());
        fragments.push(GraphFragmentAsDrawn {
            node: node.clone(),
            path_of_names: fragment_path_of_names,
            position_among_its_levels_nodes: node_position,
            interface_as_drawn: interface.interface_as_drawn,
            fragments_it_holds: inner.fragments,
        });
    }

    let mut own_links = Vec::new();
    let mut links_carried_through_a_fragment = Vec::new();
    let mut links_touching_a_fragment = Vec::new();
    for (link_position, link) in links.iter().enumerate() {
        let source_fragment = fragment_by_cast_name
            .get(&cast_of(&link.source.node)?)
            .map(|&index| &fragments[index]);
        let target_fragment = fragment_by_cast_name
            .get(&cast_of(&link.target.node)?)
            .map(|&index| &fragments[index]);
        if source_fragment.is_none() && target_fragment.is_none() {
            own_links.push(GraphSnapshotLink {
                source: end_on_this_level(&link.source, &name_on_this_level),
                target: end_on_this_level(&link.target, &name_on_this_level),
            });
            continue;
        }
        let source = match source_fragment {
            Some(fragment) => fragment
                .declared_port(&link.source.port, GraphInterfacePortDirection::Output)?
                .linked_ports
                .first()
                .cloned()
                .ok_or_else(|| {
                    Error::GraphError(format!(
                        "fragment `{}` declares the Output `{}` from no port",
                        fragment.path_of_names, link.source.port
                    ))
                })?,
            None => end_on_this_level(&link.source, &name_on_this_level),
        };
        let targets = match target_fragment {
            Some(fragment) => fragment
                .declared_port(&link.target.port, GraphInterfacePortDirection::Input)?
                .linked_ports
                .clone(),
            None => vec![end_on_this_level(&link.target, &name_on_this_level)],
        };
        links_carried_through_a_fragment.extend(targets.into_iter().map(|target| {
            GraphSnapshotLink {
                source: source.clone(),
                target,
            }
        }));
        links_touching_a_fragment.push(LinkTouchingAFragmentAsDrawn {
            link: link.clone(),
            position_among_the_graphs_links: link_position,
        });
    }

    own_nodes.extend(nodes_of_the_fragments_held);
    own_links.extend(links_carried_through_a_fragment);
    own_links.extend(links_of_the_fragments_held);
    Ok(OneLevelFlattened {
        nodes: own_nodes,
        links: own_links,
        fragments,
        links_touching_a_fragment,
    })
}

fn end_on_this_level(
    end: &LinkPortRefOutput,
    name_on_this_level: &impl Fn(&str) -> String,
) -> LinkPortRefOutput {
    LinkPortRefOutput {
        node: name_on_this_level(&end.node),
        port: end.port.clone(),
    }
}

/// Rename, in `config`, each node reference — an object of `node` and at most `port` — that
/// names a running node of this level, to the name that node runs under.
fn name_each_node_reference_on_this_level_by_its_path(
    config: &mut serde_json::Value,
    names_of_the_levels_running_nodes_cast: &HashSet<String>,
    name_on_this_level: &impl Fn(&str) -> String,
) {
    match config {
        serde_json::Value::Object(object) => {
            let is_a_node_reference = object.keys().all(|key| key == "node" || key == "port");
            if is_a_node_reference
                && let Some(serde_json::Value::String(referenced_node)) = object.get_mut("node")
                && cast_of(referenced_node)
                    .is_ok_and(|cast| names_of_the_levels_running_nodes_cast.contains(&cast))
            {
                *referenced_node = name_on_this_level(referenced_node);
                return;
            }
            for value in object.values_mut() {
                name_each_node_reference_on_this_level_by_its_path(
                    value,
                    names_of_the_levels_running_nodes_cast,
                    name_on_this_level,
                );
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                name_each_node_reference_on_this_level_by_its_path(
                    item,
                    names_of_the_levels_running_nodes_cast,
                    name_on_this_level,
                );
            }
        }
        _ => {}
    }
}

impl GraphFragmentAsDrawn {
    /// The port of `direction` named `port` the fragment's interface declares, refused naming
    /// every port of that direction it does declare.
    fn declared_port(
        &self,
        port: &str,
        direction: GraphInterfacePortDirection,
    ) -> Result<&GraphInterfacePort> {
        let port_cast = cast_of(port)?;
        let declared_of_the_direction: Vec<&GraphInterfacePort> = self
            .interface_as_drawn
            .declared_ports()
            .filter(|declared| declared.direction == direction)
            .collect();
        if let Some(declared) = declared_of_the_direction
            .iter()
            .find(|declared| cast_of(&declared.port).is_ok_and(|cast| cast == port_cast))
        {
            return Ok(declared);
        }
        let (what_the_link_does, port_kind) = match direction {
            GraphInterfacePortDirection::Input => ("enters", "Input"),
            GraphInterfacePortDirection::Output => ("leaves", "Output"),
        };
        let declared: Vec<String> = declared_of_the_direction
            .iter()
            .map(|declared| format!("`{}`", declared.port))
            .collect();
        Err(Error::GraphError(format!(
            "a link {what_the_link_does} fragment `{}` at `{port}`, which its graph generator \
             does not declare as an {port_kind}; its {port_kind}s are: {}",
            self.path_of_names,
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            }
        )))
    }

    /// The name each running node of the fragment runs under, cast, at every level inside it.
    fn running_node_names_cast(&self, names: &mut HashSet<String>) {
        let Some(fragment) = &self.node.fragment else {
            return;
        };
        for node in &fragment.nodes {
            if node.fragment.is_some()
                || is_a_graph_interface_node_type(node.processor_type.as_str())
            {
                continue;
            }
            names.insert(cast_or_as_written(
                &self.name_of_a_node_it_holds(&node.name),
            ));
        }
        for held in &self.fragments_it_holds {
            held.running_node_names_cast(names);
        }
    }

    fn name_of_a_node_it_holds(&self, node_name: &str) -> String {
        format!(
            "{}{GRAPH_FRAGMENT_NODE_PATH_SEPARATOR}{node_name}",
            self.path_of_names
        )
    }

    /// The fragment as `graph` renders it: one container holding its nodes and links, each
    /// running node taken out of `running_nodes_by_cast_name` and each link between two of
    /// them rendered with the live keys `running_links` holds.
    fn rendered_as_a_container(
        &self,
        running_nodes_by_cast_name: &mut HashMap<String, ProcessorNodeOutput>,
        running_links: &[LinkOutput],
    ) -> ProcessorNodeOutput {
        let stored = self.node.fragment.clone().unwrap_or_default();
        let mut nodes = Vec::new();
        for node in &stored.nodes {
            let name_it_runs_under = self.name_of_a_node_it_holds(&node.name);
            let mut rendered = if node.fragment.is_some() {
                match self
                    .fragments_it_holds
                    .iter()
                    .find(|held| held.path_of_names == name_it_runs_under)
                {
                    Some(held) => {
                        held.rendered_as_a_container(running_nodes_by_cast_name, running_links)
                    }
                    None => rendered_as_stored(node),
                }
            } else if let Some(interface_node) = self
                .interface_as_drawn
                .interface_node_named(&name_it_runs_under)
            {
                interface_node.rendered_as_a_node()
            } else {
                let mut running = running_nodes_by_cast_name
                    .remove(&cast_or_as_written(&name_it_runs_under))
                    .unwrap_or_else(|| rendered_as_stored(node));
                running.config = Some(node.config.clone());
                running.config_checksum = compute_json_checksum(&node.config);
                running
            };
            rendered.name = cast_or_as_written(&node.name);
            nodes.push(rendered);
        }
        let links = stored
            .links
            .iter()
            .map(|link| {
                let source_runs_as =
                    cast_or_as_written(&self.name_of_a_node_it_holds(&link.source.node));
                let target_runs_as =
                    cast_or_as_written(&self.name_of_a_node_it_holds(&link.target.node));
                let mut rendered = running_links
                    .iter()
                    .find(|running| {
                        running.source.node == source_runs_as
                            && running.source.port == cast_or_as_written(&link.source.port)
                            && running.target.node == target_runs_as
                            && running.target.port == cast_or_as_written(&link.target.port)
                    })
                    .cloned()
                    .unwrap_or_else(|| rendered_as_drawn(link));
                rendered.source.node = cast_or_as_written(&link.source.node);
                rendered.target.node = cast_or_as_written(&link.target.node);
                rendered
            })
            .collect();

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for declared in self.interface_as_drawn.declared_ports() {
            let port = declared.rendered_as_a_port_named(&cast_or_as_written(&declared.port));
            match declared.direction {
                GraphInterfacePortDirection::Input => inputs.push(port),
                GraphInterfacePortDirection::Output => outputs.push(port),
            }
        }
        ProcessorNodeOutput {
            id: self
                .node
                .id
                .map(|node_uuid| node_uuid.to_string())
                .unwrap_or_default(),
            processor_type: self.node.processor_type.clone(),
            kind: Some(NodeKindOutput::Fragment),
            language: None,
            name: cast_or_as_written(&self.node.name),
            config: Some(self.node.config.clone()),
            config_checksum: compute_json_checksum(&self.node.config),
            pins: self.node.pins.clone(),
            exposed_config_fields: self.node.exposed_config_fields.clone(),
            studio: self.node.studio.clone(),
            inputs_waiting_for_a_value: Vec::new(),
            ports: ProcessorNodePortsOutput { inputs, outputs },
            components: serde_json::Map::new(),
            fragment: Some(GraphFragmentOutput { nodes, links }),
        }
    }

    fn validate(
        &self,
        graph_the_runtime_runs: &GraphSnapshot,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<()> {
        self.interface_as_drawn
            .validate(graph_the_runtime_runs, node_types)
            .map_err(|refusal| {
                Error::GraphError(format!("fragment `{}`: {refusal}", self.path_of_names))
            })?;
        for held in &self.fragments_it_holds {
            held.validate(graph_the_runtime_runs, node_types)?;
        }
        Ok(())
    }

    fn path_of_names_cast_of_it_and_each_it_holds(&self, names: &mut HashSet<String>) {
        names.insert(cast_or_as_written(&self.path_of_names));
        for held in &self.fragments_it_holds {
            held.path_of_names_cast_of_it_and_each_it_holds(names);
        }
    }
}

impl GraphFragmentsAsDrawn {
    /// The path of names of the fragment holding the running node whose cast name is
    /// `node_name_cast`, if a fragment holds it.
    pub fn fragment_holding_the_running_node(&self, node_name_cast: &str) -> Option<&str> {
        fn find_in<'drawn>(
            fragments: &'drawn [GraphFragmentAsDrawn],
            node_name_cast: &str,
        ) -> Option<&'drawn str> {
            for fragment in fragments {
                if let Some(holder) = find_in(&fragment.fragments_it_holds, node_name_cast) {
                    return Some(holder);
                }
                let mut names = HashSet::new();
                fragment.running_node_names_cast(&mut names);
                if names.contains(node_name_cast) {
                    return Some(&fragment.path_of_names);
                }
            }
            None
        }
        find_in(&self.fragments, node_name_cast)
    }

    /// The path of names of every fragment at every level, cast: names no node of the stream
    /// may take while the fragment stands.
    pub fn fragment_names_flattened_and_cast(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for fragment in &self.fragments {
            fragment.path_of_names_cast_of_it_and_each_it_holds(&mut names);
        }
        names
    }

    /// Refuse a port a fragment's interface declares, at any level, that the link rules
    /// refuse between it and the port of `graph_the_runtime_runs` linked to it.
    pub fn validate(
        &self,
        graph_the_runtime_runs: &GraphSnapshot,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<()> {
        for fragment in &self.fragments {
            fragment.validate(graph_the_runtime_runs, node_types)?;
        }
        Ok(())
    }

    /// Render each fragment into `graph_response`, a render of the graph the runtime runs: its
    /// nodes and every link with an end on one taken out, and the fragment put back where it
    /// was drawn as one container holding them, with each drawn link to it.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        if self.fragments.is_empty() {
            return;
        }
        let mut running_node_names_cast = HashSet::new();
        for fragment in &self.fragments {
            fragment.running_node_names_cast(&mut running_node_names_cast);
        }
        let mut running_nodes_by_cast_name = HashMap::new();
        graph_response.nodes.retain(|node| {
            if running_node_names_cast.contains(&node.name) {
                running_nodes_by_cast_name.insert(node.name.clone(), node.clone());
                return false;
            }
            true
        });
        let mut running_links = Vec::new();
        graph_response.links.retain(|link| {
            if running_node_names_cast.contains(&link.source.node)
                || running_node_names_cast.contains(&link.target.node)
            {
                running_links.push(link.clone());
                return false;
            }
            true
        });
        for fragment in &self.fragments {
            let container =
                fragment.rendered_as_a_container(&mut running_nodes_by_cast_name, &running_links);
            let position = fragment
                .position_among_its_levels_nodes
                .min(graph_response.nodes.len());
            graph_response.nodes.insert(position, container);
        }
        for drawn in &self.links_touching_a_fragment {
            let position = drawn
                .position_among_the_graphs_links
                .min(graph_response.links.len());
            graph_response
                .links
                .insert(position, rendered_as_drawn(&drawn.link));
        }
    }
}

/// A node no running node stands for, rendered with its spec keys alone.
fn rendered_as_stored(node: &GraphSnapshotNode) -> ProcessorNodeOutput {
    ProcessorNodeOutput {
        id: node
            .id
            .map(|node_uuid| node_uuid.to_string())
            .unwrap_or_default(),
        processor_type: node.processor_type.clone(),
        kind: None,
        language: None,
        name: cast_or_as_written(&node.name),
        config: Some(node.config.clone()),
        config_checksum: compute_json_checksum(&node.config),
        pins: node.pins.clone(),
        exposed_config_fields: node.exposed_config_fields.clone(),
        studio: node.studio.clone(),
        inputs_waiting_for_a_value: Vec::new(),
        ports: ProcessorNodePortsOutput {
            inputs: Vec::new(),
            outputs: Vec::new(),
        },
        components: serde_json::Map::new(),
        fragment: None,
    }
}

/// A link no running link stands for, rendered with its ends alone.
fn rendered_as_drawn(link: &GraphSnapshotLink) -> LinkOutput {
    LinkOutput {
        id: String::new(),
        source: LinkPortRefOutput {
            node: cast_or_as_written(&link.source.node),
            port: link.source.port.clone(),
        },
        target: LinkPortRefOutput {
            node: cast_or_as_written(&link.target.node),
            port: link.target.port.clone(),
        },
        capacity: Default::default(),
        state: Default::default(),
        error_reason: None,
        components: serde_json::Map::new(),
    }
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
    use crate::core::graph_interface_resolution::{GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE};
    use crate::core::json_schema::LinkStateOutput;

    const LOOK_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a01";
    const SOFT_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a02";
    const BLUR_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a03";
    const TINT_UUID: &str = "6f0a1c52-3d0e-4c1b-9a43-6a0c1f8f0a04";

    fn blur_then_tint_fragment() -> serde_json::Value {
        serde_json::json!({
            "nodes": [
                {"name": "source", "type": GRAPH_INPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
                {"id": BLUR_UUID, "name": "blur", "type": "looks.nodes:Blur",
                 "config": {"radius": 8}},
                {"id": TINT_UUID, "name": "tint", "type": "looks.nodes:Tint",
                 "config": {"reads_radius_of": {"node": "blur"}}},
                {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE,
                 "config": {"port": "video", "family": "image"}},
            ],
            "links": [
                {"source": {"node": "source", "port": "value"},
                 "target": {"node": BLUR_UUID, "port": "video"}},
                {"source": {"node": "blur", "port": "video_out"},
                 "target": {"node": "tint", "port": "video"}},
                {"source": {"node": "tint", "port": "video_out"},
                 "target": {"node": "out", "port": "value"}},
            ],
        })
    }

    fn a_camera_through_look_holding_soft_into_a_display() -> serde_json::Value {
        serde_json::json!({
            "nodes": [
                {"name": "camera", "type": "tatolab.camera:CameraSource", "config": {}},
                {"id": LOOK_UUID, "name": "look", "type": "looks.generators:look",
                 "config": {}, "studio": {"x": 40},
                 "fragment": {
                     "nodes": [
                         {"name": "source", "type": GRAPH_INPUT_NODE_TYPE,
                          "config": {"port": "video", "family": "image"}},
                         {"id": SOFT_UUID, "name": "soft",
                          "type": "looks.generators:blur_then_tint",
                          "config": {"radius": 8},
                          "fragment": blur_then_tint_fragment()},
                         {"name": "out", "type": GRAPH_OUTPUT_NODE_TYPE,
                          "config": {"port": "video", "family": "image"}},
                     ],
                     "links": [
                         {"source": {"node": "source", "port": "value"},
                          "target": {"node": SOFT_UUID, "port": "video"}},
                         {"source": {"node": "soft", "port": "video"},
                          "target": {"node": "out", "port": "value"}},
                     ],
                 }},
                {"name": "display", "type": "tatolab.display:Display", "config": {}},
            ],
            "links": [
                {"source": {"node": "camera", "port": "video"},
                 "target": {"node": LOOK_UUID, "port": "video"}},
                {"source": {"node": "look", "port": "video"},
                 "target": {"node": "display", "port": "video"}},
            ],
        })
    }

    fn flattened(graph: serde_json::Value) -> Result<GraphSnapshotWithFragmentsFlattened> {
        GraphSnapshot::from_graph_document(graph)
            .and_then(|graph| graph.with_graph_fragments_flattened())
    }

    fn link_named(source: (&str, &str), target: (&str, &str)) -> GraphSnapshotLink {
        GraphSnapshotLink {
            source: LinkPortRefOutput {
                node: source.0.to_string(),
                port: source.1.to_string(),
            },
            target: LinkPortRefOutput {
                node: target.0.to_string(),
                port: target.1.to_string(),
            },
        }
    }

    #[test]
    fn a_nested_fragment_flattens_under_its_path_of_names_keeping_each_nodes_uuid() {
        let flattening =
            flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
        let graph = &flattening.graph_the_runtime_runs;
        let names: Vec<&str> = graph.nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(
            names,
            ["camera", "display", "look/soft/blur", "look/soft/tint"]
        );
        assert_eq!(
            graph.nodes[2].id.map(|id| id.to_string()).as_deref(),
            Some(BLUR_UUID)
        );
        assert_eq!(
            graph.nodes[3].id.map(|id| id.to_string()).as_deref(),
            Some(TINT_UUID)
        );
        assert_eq!(
            graph.links,
            [
                link_named(("camera", "video"), ("look/soft/blur", "video")),
                link_named(("look/soft/tint", "video_out"), ("display", "video")),
                link_named(("look/soft/blur", "video_out"), ("look/soft/tint", "video")),
            ]
        );
        assert_eq!(
            flattening
                .fragments_as_drawn
                .fragment_names_flattened_and_cast(),
            HashSet::from(["look".to_string(), "look-soft".to_string()])
        );
    }

    #[test]
    fn a_node_reference_inside_a_fragment_names_the_node_by_its_path() {
        let flattening =
            flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
        assert_eq!(
            flattening.graph_the_runtime_runs.nodes[3].config,
            serde_json::json!({"reads_radius_of": {"node": "look/soft/blur"}})
        );
    }

    #[test]
    fn a_link_to_a_port_the_fragment_does_not_declare_is_refused_naming_the_ports_it_does() {
        let mut graph = a_camera_through_look_holding_soft_into_a_display();
        graph["links"][0]["target"]["port"] = serde_json::json!("audio");
        let refusal = flattened(graph).expect_err("refused").to_string();
        assert!(
            refusal.contains(
                "a link enters fragment `look` at `audio`, which its graph generator does not \
                 declare as an Input; its Inputs are: `video`"
            ),
            "{refusal}"
        );
    }

    #[test]
    fn a_graph_holding_no_fragment_flattens_to_itself() {
        let graph = serde_json::json!({
            "nodes": [{"name": "camera", "type": "tatolab.camera:CameraSource"}],
        });
        let flattening = flattened(graph.clone()).expect("flattens");
        assert_eq!(
            flattening.graph_the_runtime_runs,
            GraphSnapshot::from_graph_document(graph).unwrap()
        );
        assert_eq!(
            flattening.fragments_as_drawn,
            GraphFragmentsAsDrawn::default()
        );
    }

    #[test]
    fn a_key_inside_a_fragment_the_runtime_does_not_read_is_refused_naming_its_path() {
        let mut graph = a_camera_through_look_holding_soft_into_a_display();
        graph["nodes"][1]["fragment"]["nodes"][1]["fragment"]["nodes"][1]["colour"] =
            serde_json::json!("red");
        let refusal = flattened(graph).expect_err("refused").to_string();
        assert!(
            refusal.contains("node `look/soft/blur` carries the key `colour`"),
            "{refusal}"
        );
    }

    #[test]
    fn a_fragment_written_as_its_graph_document_reads_back_unchanged() {
        let graph =
            GraphSnapshot::from_graph_document(a_camera_through_look_holding_soft_into_a_display())
                .unwrap()
                .with_a_uuid_minted_for_each_node_holding_none();
        let graph = GraphSnapshot {
            asset: Some(uuid::Uuid::new_v4()),
            ..graph
        };
        let document: serde_json::Value =
            serde_json::from_slice(&graph.graph_document_bytes().unwrap()).unwrap();
        assert_eq!(
            document["nodes"][1]["fragment"]["nodes"][1]["fragment"]["links"][1]["source"]["node"],
            serde_json::json!(BLUR_UUID)
        );
        assert_eq!(GraphSnapshot::from_graph_document(document).unwrap(), graph);
    }

    fn rendered_as_running(node: &GraphSnapshotNode) -> ProcessorNodeOutput {
        ProcessorNodeOutput {
            components: serde_json::Map::from_iter([(
                "running".to_string(),
                serde_json::json!(true),
            )]),
            ..rendered_as_stored(node)
        }
    }

    fn a_render_of(graph: &GraphSnapshot) -> GraphResponse {
        GraphResponse {
            asset: None,
            stream: None,
            clock: Default::default(),
            nodes: graph.nodes.iter().map(rendered_as_running).collect(),
            links: graph
                .links
                .iter()
                .map(|link| LinkOutput {
                    id: format!("{}->{}", link.source.node, link.target.node),
                    state: LinkStateOutput::Wired,
                    ..rendered_as_drawn(link)
                })
                .collect(),
            references: Vec::new(),
            machine: "rig".to_string(),
        }
    }

    #[test]
    fn graph_renders_each_fragment_as_a_container_inside_its_container_that_reads_back() {
        let flattening =
            flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
        let mut render = a_render_of(&flattening.graph_the_runtime_runs);
        flattening.fragments_as_drawn.render_into(&mut render);

        let names: Vec<&str> = render.nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, ["camera", "look", "display"]);
        let look = &render.nodes[1];
        assert_eq!(look.kind, Some(NodeKindOutput::Fragment));
        assert_eq!(look.id, LOOK_UUID);
        assert_eq!(look.ports.inputs[0].name, "video");
        assert_eq!(look.ports.outputs[0].name, "video");
        let look_holds = look.fragment.as_ref().expect("look holds its fragment");
        let names: Vec<&str> = look_holds
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, ["source", "soft", "out"]);
        let soft = &look_holds.nodes[1];
        assert_eq!(soft.kind, Some(NodeKindOutput::Fragment));
        let soft_holds = soft.fragment.as_ref().expect("soft holds its fragment");
        let names: Vec<&str> = soft_holds
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, ["source", "blur", "tint", "out"]);
        assert_eq!(
            soft_holds.nodes[1].components["running"],
            serde_json::json!(true)
        );
        let blur_into_tint = &soft_holds.links[1];
        assert_eq!(
            (
                blur_into_tint.source.node.as_str(),
                blur_into_tint.target.node.as_str()
            ),
            ("blur", "tint")
        );
        assert_eq!(blur_into_tint.state, LinkStateOutput::Wired);
        assert_eq!(
            soft_holds.nodes[2].config,
            Some(serde_json::json!({"reads_radius_of": {"node": "blur"}}))
        );
        let outer_links: Vec<(&str, &str)> = render
            .links
            .iter()
            .map(|link| (link.source.node.as_str(), link.target.node.as_str()))
            .collect();
        assert_eq!(outer_links, [("camera", "look"), ("look", "display")]);

        let read_back = GraphSnapshot::from_graph_document(serde_json::to_value(&render).unwrap())
            .expect("the render loads");
        let written =
            GraphSnapshot::from_graph_document(a_camera_through_look_holding_soft_into_a_display())
                .unwrap();
        assert_eq!(
            read_back
                .with_graph_fragments_flattened()
                .unwrap()
                .graph_the_runtime_runs,
            written
                .with_graph_fragments_flattened()
                .unwrap()
                .graph_the_runtime_runs
        );
    }

    #[test]
    fn two_fragments_of_one_name_on_one_level_are_refused_naming_it() {
        let mut graph = a_camera_through_look_holding_soft_into_a_display();
        let second_look = serde_json::json!({
            "name": "Look", "type": "looks.generators:look",
            "fragment": {"nodes": [], "links": []},
        });
        graph["nodes"].as_array_mut().unwrap().push(second_look);
        let refusal = flattened(graph).expect_err("refused");
        assert!(
            matches!(&refusal, Error::NodeNameTaken { cast, .. } if cast == "look"),
            "{refusal}"
        );
    }

    #[test]
    fn a_node_inside_a_fragment_is_read_only_to_a_live_removal_or_rename() {
        let flattening =
            flattened(a_camera_through_look_holding_soft_into_a_display()).expect("flattens");
        let drawn = crate::core::graph_snapshot::GraphOnlyNodesAsDrawn {
            fragments_as_drawn: flattening.fragments_as_drawn,
            ..Default::default()
        };
        let refusal = drawn
            .refuse_a_node_change_a_drawn_link_names(
                "look/soft/blur",
                crate::core::graph_snapshot::NodeChangeADrawnLinkBlocks::Removal,
            )
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains("it is part of fragment `look/soft`, which its graph generator built"),
            "{refusal}"
        );
    }
}
