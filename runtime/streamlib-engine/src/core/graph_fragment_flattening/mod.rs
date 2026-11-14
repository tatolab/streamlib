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

use streamlib_processor_schema::{
    ConfigKeyHoldingANodeReference, NODE_REFERENCE_NODE_KEY, node_name_a_config_value_references,
};

use crate::core::graph::{
    cast_exposed_name_to_url_safe, config_keys_of_a_node_type_holding_a_node_reference,
};
use crate::core::graph_interface_resolution::{
    GraphInterfaceAsDrawn, GraphInterfaceLevel, GraphInterfacePort, GraphInterfacePortDirection,
    is_a_graph_interface_node_type, the_family_a_port_linked_to_an_interface_node_carries,
};
use crate::core::graph_snapshot::{GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode};
use crate::core::graph_variable_resolution::{
    SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY, SET_VARIABLE_NODE_TYPE,
};
use crate::core::json_schema::{
    GraphFragmentOutput, GraphResponse, LinkOutput, LinkPortRefOutput, LinkStateOutput,
    NodeKindOutput, ProcessorNodeOutput, ProcessorNodePortsOutput,
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
    /// Each of its own links with an end on a fragment it holds.
    pub links_touching_a_fragment_it_holds: Vec<LinkTouchingAFragmentAsDrawn>,
}

/// One link of a level with an end on a fragment the level holds, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTouchingAFragmentAsDrawn {
    pub link: GraphSnapshotLink,
    /// Where the link sat among its level's links.
    pub position_among_the_graphs_links: usize,
    /// The links it was carried to through the fragment's ports, each end named as the graph
    /// the runtime runs names it.
    pub links_it_is_carried_to: Vec<GraphSnapshotLink>,
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
    /// interface does not declare. An inner node's config is renamed only at the keys its type's
    /// config schema, as `node_types` resolves it, marks as node references.
    pub fn with_graph_fragments_flattened(
        &self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<GraphSnapshotWithFragmentsFlattened> {
        if self.nodes.iter().all(|node| node.fragment.is_none()) {
            return Ok(GraphSnapshotWithFragmentsFlattened {
                graph_the_runtime_runs: self.clone(),
                fragments_as_drawn: GraphFragmentsAsDrawn::default(),
            });
        }
        let level = flatten_one_level(None, &self.nodes, &self.links, node_types)?;
        Ok(GraphSnapshotWithFragmentsFlattened {
            graph_the_runtime_runs: GraphSnapshot {
                asset: self.asset,
                display_name: self.display_name.clone(),
                stream: self.stream.clone(),
                clock: self.clock,
                nodes: level.nodes,
                links: level.links,
                groups: self.groups.clone(),
                studio: self.studio.clone(),
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
    node_types: &NodeTypesOneStreamResolves,
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
                name_each_node_reference_its_schema_marks_on_this_level_by_its_path(
                    &mut config,
                    &config_keys_holding_a_node_reference_of(node, node_types),
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
            node_types,
        )?;
        let fragment_level = GraphSnapshot {
            asset: None,
            display_name: None,
            stream: None,
            clock: Default::default(),
            nodes: inner.nodes,
            links: inner.links,
            groups: Vec::new(),
            studio: None,
        };
        let interface = fragment_level
            .with_graph_interface_resolved(
                GraphInterfaceLevel::AFragment,
                &|linked_port, direction| {
                    the_family_a_port_linked_to_an_interface_node_carries(
                        node_types,
                        &fragment_level,
                        linked_port,
                        direction,
                    )
                },
            )
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
            links_touching_a_fragment_it_holds: inner.links_touching_a_fragment,
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
        let links_it_is_carried_to: Vec<GraphSnapshotLink> = targets
            .into_iter()
            .map(|target| GraphSnapshotLink {
                source: source.clone(),
                target,
            })
            .collect();
        links_carried_through_a_fragment.extend(links_it_is_carried_to.iter().cloned());
        links_touching_a_fragment.push(LinkTouchingAFragmentAsDrawn {
            link: link.clone(),
            position_among_the_graphs_links: link_position,
            links_it_is_carried_to,
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

/// The config keys of `node` holding a node reference: those its type's schema marks, and a
/// `SetVariable`'s `node_reference`, which the graph resolves before any schema is read.
fn config_keys_holding_a_node_reference_of(
    node: &GraphSnapshotNode,
    node_types: &NodeTypesOneStreamResolves,
) -> Vec<String> {
    if node.processor_type.as_str() == SET_VARIABLE_NODE_TYPE {
        return vec![SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY.to_string()];
    }
    config_keys_of_a_node_type_holding_a_node_reference(node_types, &node.processor_type)
        .into_iter()
        .map(|ConfigKeyHoldingANodeReference { config_key, .. }| config_key)
        .collect()
}

/// Rename, in `config`, the node reference under each of `keys_holding_a_node_reference` that
/// names a running node of this level, to the name that node runs under.
fn name_each_node_reference_its_schema_marks_on_this_level_by_its_path(
    config: &mut serde_json::Value,
    keys_holding_a_node_reference: &[String],
    names_of_the_levels_running_nodes_cast: &HashSet<String>,
    name_on_this_level: &impl Fn(&str) -> String,
) {
    let Some(config_object) = config.as_object_mut() else {
        return;
    };
    for config_key in keys_holding_a_node_reference {
        let Some(config_value) = config_object.get_mut(config_key) else {
            continue;
        };
        let Some(referenced_node) = node_name_a_config_value_references(config_value) else {
            continue;
        };
        if cast_of(referenced_node)
            .is_ok_and(|cast| names_of_the_levels_running_nodes_cast.contains(&cast))
        {
            *config_value = serde_json::json!({
                NODE_REFERENCE_NODE_KEY: name_on_this_level(referenced_node)
            });
        }
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
        let mut running_node_names_cast_inside = HashSet::new();
        self.running_node_names_cast(&mut running_node_names_cast_inside);
        let links = stored
            .links
            .iter()
            .enumerate()
            .map(|(link_position, link)| {
                let links_it_runs_as = match self
                    .links_touching_a_fragment_it_holds
                    .iter()
                    .find(|drawn| drawn.position_among_the_graphs_links == link_position)
                {
                    Some(drawn) => drawn.links_it_is_carried_to.clone(),
                    None => vec![GraphSnapshotLink {
                        source: LinkPortRefOutput {
                            node: self.name_of_a_node_it_holds(&link.source.node),
                            port: link.source.port.clone(),
                        },
                        target: LinkPortRefOutput {
                            node: self.name_of_a_node_it_holds(&link.target.node),
                            port: link.target.port.clone(),
                        },
                    }],
                };
                let running_links_it_stands_for: Vec<&LinkOutput> = links_it_runs_as
                    .iter()
                    .flat_map(|link_it_runs_as| {
                        self.running_links_a_link_inside_it_stands_for(
                            link_it_runs_as,
                            &running_node_names_cast_inside,
                            running_links,
                        )
                    })
                    .collect();
                let mut rendered =
                    rendered_with_the_live_keys_of(link, &running_links_it_stands_for);
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
            fragment: Some(GraphFragmentOutput {
                nodes,
                links,
                groups: stored.groups.clone(),
            }),
        }
    }

    /// The running links `link_it_runs_as`, a link inside the fragment named as the graph the
    /// runtime runs names its ends, stands for: from one of its `GraphInput` nodes, the link
    /// into the input it feeds; into one of its `GraphOutput` nodes, each link from the port it
    /// reads to a node outside the fragment; between two running nodes, that link.
    fn running_links_a_link_inside_it_stands_for<'running>(
        &self,
        link_it_runs_as: &GraphSnapshotLink,
        running_node_names_cast_inside: &HashSet<String>,
        running_links: &'running [LinkOutput],
    ) -> Vec<&'running LinkOutput> {
        let is_an_interface_node_of_it = |end: &LinkPortRefOutput, direction| {
            self.interface_as_drawn
                .interface_node_named(&end.node)
                .is_some_and(|interface_node| interface_node.declared_port.direction == direction)
        };
        let from_a_graph_input =
            is_an_interface_node_of_it(&link_it_runs_as.source, GraphInterfacePortDirection::Input);
        let into_a_graph_output = is_an_interface_node_of_it(
            &link_it_runs_as.target,
            GraphInterfacePortDirection::Output,
        );
        running_links
            .iter()
            .filter(|running| {
                let source_matches = || ends_match(&running.source, &link_it_runs_as.source);
                let target_matches = || ends_match(&running.target, &link_it_runs_as.target);
                if from_a_graph_input {
                    target_matches()
                } else if into_a_graph_output {
                    source_matches()
                        && !running_node_names_cast_inside.contains(&running.target.node)
                } else {
                    source_matches() && target_matches()
                }
            })
            .collect()
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
            let running_links_it_stands_for: Vec<&LinkOutput> = drawn
                .links_it_is_carried_to
                .iter()
                .flat_map(|carried| {
                    running_links.iter().filter(move |running| {
                        ends_match(&running.source, &carried.source)
                            && ends_match(&running.target, &carried.target)
                    })
                })
                .collect();
            graph_response.links.insert(
                position,
                rendered_with_the_live_keys_of(&drawn.link, &running_links_it_stands_for),
            );
        }
    }
}

/// Whether `running`, an end of a running link as `graph` renders it, is `drawn` cast.
fn ends_match(running: &LinkPortRefOutput, drawn: &LinkPortRefOutput) -> bool {
    running.node == cast_or_as_written(&drawn.node)
        && running.port == cast_or_as_written(&drawn.port)
}

/// `link` rendered with the live keys of the running links it stands for: the first of them
/// not wired, else the first; with its ends alone when it stands for none.
fn rendered_with_the_live_keys_of(
    link: &GraphSnapshotLink,
    running_links_it_stands_for: &[&LinkOutput],
) -> LinkOutput {
    let drawn = rendered_as_drawn(link);
    let Some(running) = running_links_it_stands_for
        .iter()
        .find(|running| running.state != LinkStateOutput::Wired)
        .or_else(|| running_links_it_stands_for.first())
    else {
        return drawn;
    };
    LinkOutput {
        source: drawn.source,
        target: drawn.target,
        ..(*running).clone()
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
mod tests;
