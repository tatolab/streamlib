// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A selection of a stream's nodes folded into one authored fragment, and a fragment unfolded
//! back into the plain nodes it holds.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE,
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, is_a_graph_interface_node_type, node_name_of, node_uuid_of,
};

/// The `type` of a fragment node a person authored by collapsing a selection: it names no graph
/// generator, so no refresh touches it.
pub const AUTHORED_FRAGMENT_NODE_TYPE: &str = "tatolab.project:AuthoredFragment";

/// A node or a link as the stream's own level of a graph document holds it, at its position
/// among that level's nodes or links.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphDocumentValueAtItsPosition {
    /// Its index among the level's nodes, or among its links.
    pub position: usize,
    /// The node or link as the document holds it.
    pub value: Value,
}

/// One fragment node of the stream's own level and the plain nodes it stands for: folded, the
/// level holds `fragment` and `fragment_links`; unfolded, it holds `nodes` and `links`. Each at
/// the position it takes, so folding and unfolding give back the exact document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphFragmentFold {
    /// The fragment node, its `fragment` holding what it places.
    pub fragment: StreamGraphDocumentValueAtItsPosition,
    /// Each link of the level with an end on the fragment node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragment_links: Vec<StreamGraphDocumentValueAtItsPosition>,
    /// Each plain node the fragment stands for.
    pub nodes: Vec<StreamGraphDocumentValueAtItsPosition>,
    /// Each link of the level with an end on one of `nodes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<StreamGraphDocumentValueAtItsPosition>,
}

impl StreamGraphFragmentFold {
    /// The fragment node's UUID.
    pub fn fragment_node(&self) -> Option<Uuid> {
        self.fragment.value.as_object().and_then(node_uuid_of)
    }

    /// The fragment node's name.
    pub fn fragment_name(&self) -> &str {
        self.fragment
            .value
            .as_object()
            .and_then(node_name_of)
            .unwrap_or_default()
    }
}

impl StreamGraphDocument {
    /// The fold of the nodes `nodes`, each of the stream's own level, into a new authored
    /// fragment node `fragment` named `fragment_name`: each link between two of them kept inside
    /// it, and each link that crossed the selection carried through a `GraphInput` or
    /// `GraphOutput` node inside it, of the family `family_of_an_output_port` reads at the link's
    /// source.
    pub fn the_fold_of_nodes_into_an_authored_fragment(
        &self,
        nodes: &[Uuid],
        fragment: Uuid,
        fragment_name: &str,
        family_of_an_output_port: &dyn Fn(&StreamGraphDocumentLinkEnd) -> Result<String, String>,
        new_node_uuid: &mut dyn FnMut() -> Uuid,
    ) -> Result<StreamGraphFragmentFold, String> {
        if nodes.is_empty() {
            return Err("a selection collapsed into a subgraph holds at least one node".to_owned());
        }
        let selected: HashSet<Uuid> = nodes.iter().copied().collect();
        if selected.len() != nodes.len() {
            return Err("a selection names each of its nodes once".to_owned());
        }
        let mut folded_nodes = Vec::new();
        let mut inner_names = HashSet::new();
        for node in nodes {
            let (position, value) = self
                .node_values_of_the_level()
                .iter()
                .enumerate()
                .find(|(_, value)| value.as_object().and_then(node_uuid_of) == Some(*node))
                .ok_or_else(|| self.why_no_node_has(*node))?;
            let node_object = value.as_object().ok_or("a node is a JSON object")?;
            let node_type = node_object.get("type").and_then(Value::as_str);
            if node_type.is_some_and(is_a_graph_interface_node_type) {
                return Err(format!(
                    "node {node} is one of the stream's own `GraphInput` and `GraphOutput` nodes, \
                     which stay on the stream's level as its interface"
                ));
            }
            inner_names.insert(node_name_of(node_object).unwrap_or_default().to_owned());
            folded_nodes.push(StreamGraphDocumentValueAtItsPosition {
                position,
                value: value.clone(),
            });
        }
        folded_nodes.sort_by_key(|node| node.position);
        if let Some(holder) = self.node_objects().find(|node_object| {
            node_name_of(node_object) == Some(fragment_name)
                && !node_uuid_of(node_object).is_some_and(|node| selected.contains(&node))
        }) {
            return Err(format!(
                "the subgraph cannot be named `{fragment_name}`: node {} has that name",
                node_uuid_of(holder).map_or_else(String::new, |uuid| uuid.to_string())
            ));
        }

        let mut folded_links = Vec::new();
        let mut inner_links = Vec::new();
        let mut links_into_the_selection: Vec<(
            StreamGraphDocumentLinkEnd,
            Vec<StreamGraphDocumentLinkEnd>,
        )> = Vec::new();
        let mut links_out_of_the_selection: Vec<(
            StreamGraphDocumentLinkEnd,
            Vec<StreamGraphDocumentLinkEnd>,
        )> = Vec::new();
        for (position, value) in self.link_values().iter().enumerate() {
            let Some(link) = self.link_read_from(value) else {
                continue;
            };
            let source_selected = selected.contains(&link.source.node);
            let target_selected = selected.contains(&link.target.node);
            if !source_selected && !target_selected {
                continue;
            }
            folded_links.push(StreamGraphDocumentValueAtItsPosition {
                position,
                value: value.clone(),
            });
            match (source_selected, target_selected) {
                (true, true) => inner_links.push(
                    serde_json::to_value(&link)
                        .map_err(|not_written| format!("a link does not write: {not_written}"))?,
                ),
                (false, true) => {
                    push_the_end_under(&mut links_into_the_selection, link.target, link.source)
                }
                _ => push_the_end_under(&mut links_out_of_the_selection, link.source, link.target),
            }
        }

        let mut interface_nodes = Vec::new();
        let mut fragment_links = Vec::new();
        let name_of_the_node = |node: Uuid| -> String {
            self.node_objects()
                .find(|node_object| node_uuid_of(node_object) == Some(node))
                .and_then(node_name_of)
                .unwrap_or_default()
                .to_owned()
        };
        for (target, sources) in links_into_the_selection {
            let port = a_name_not_yet_taken(
                &format!("{}-{}", name_of_the_node(target.node), target.port),
                &mut inner_names,
            );
            let graph_input = new_node_uuid();
            interface_nodes.push(an_interface_node(
                graph_input,
                &port,
                GRAPH_INPUT_NODE_TYPE,
                &family_of_an_output_port(&sources[0])?,
            ));
            inner_links.push(link_value(
                StreamGraphDocumentLinkEnd {
                    node: graph_input,
                    port: GRAPH_INTERFACE_VALUE_PORT_NAME.to_owned(),
                },
                target,
            )?);
            for source in sources {
                fragment_links.push(link_value(
                    source,
                    StreamGraphDocumentLinkEnd {
                        node: fragment,
                        port: port.clone(),
                    },
                )?);
            }
        }
        for (source, targets) in links_out_of_the_selection {
            let port = a_name_not_yet_taken(
                &format!("{}-{}", name_of_the_node(source.node), source.port),
                &mut inner_names,
            );
            let graph_output = new_node_uuid();
            interface_nodes.push(an_interface_node(
                graph_output,
                &port,
                GRAPH_OUTPUT_NODE_TYPE,
                &family_of_an_output_port(&source)?,
            ));
            inner_links.push(link_value(
                source,
                StreamGraphDocumentLinkEnd {
                    node: graph_output,
                    port: GRAPH_INTERFACE_VALUE_PORT_NAME.to_owned(),
                },
            )?);
            for target in targets {
                fragment_links.push(link_value(
                    StreamGraphDocumentLinkEnd {
                        node: fragment,
                        port: port.clone(),
                    },
                    target,
                )?);
            }
        }

        let mut fragment_nodes: Vec<Value> =
            folded_nodes.iter().map(|node| node.value.clone()).collect();
        fragment_nodes.extend(interface_nodes);
        let fragment_node = StreamGraphDocumentNode {
            id: fragment,
            name: fragment_name.to_owned(),
            node_type: AUTHORED_FRAGMENT_NODE_TYPE.to_owned(),
            config: Value::Object(Map::new()),
            pins: Map::new(),
            exposed_config_fields: Vec::new(),
            studio: Default::default(),
            fragment: Some(Box::new(
                json!({ "nodes": fragment_nodes, "links": inner_links }),
            )),
        };
        let first_link_position = folded_links
            .first()
            .map_or(self.link_values().len(), |link| link.position);
        Ok(StreamGraphFragmentFold {
            fragment: StreamGraphDocumentValueAtItsPosition {
                position: folded_nodes[0].position,
                value: serde_json::to_value(&fragment_node)
                    .map_err(|not_written| format!("the subgraph does not write: {not_written}"))?,
            },
            fragment_links: at_positions_from(first_link_position, fragment_links),
            nodes: folded_nodes,
            links: folded_links,
        })
    }

    /// The fold the fragment node `fragment` of the stream's own level unfolds: its nodes as
    /// plain nodes of the level, its `GraphInput` and `GraphOutput` nodes gone, and each link
    /// into or out of one of its ports carried straight to the node its interface fed or read.
    pub fn the_fold_a_fragment_unfolds(
        &self,
        fragment: Uuid,
    ) -> Result<StreamGraphFragmentFold, String> {
        let (fragment_position, fragment_value) = self
            .node_values_of_the_level()
            .iter()
            .enumerate()
            .find(|(_, value)| value.as_object().and_then(node_uuid_of) == Some(fragment))
            .ok_or_else(|| self.why_no_node_has(fragment))?;
        let fragment_object = fragment_value
            .as_object()
            .ok_or("a node is a JSON object")?;
        if fragment_object.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE) {
            return Err(format!(
                "node {fragment} places another stream, read from that stream's document at \
                 each load, and is not unlocked"
            ));
        }
        let stored = fragment_object
            .get("fragment")
            .ok_or_else(|| format!("node {fragment} is not a fragment"))?;
        let inner_node_values: Vec<&Value> =
            stored["nodes"].as_array().into_iter().flatten().collect();
        let inner_uuid_of_the_name = |named: &str| -> Option<Uuid> {
            Uuid::parse_str(named).ok().or_else(|| {
                inner_node_values
                    .iter()
                    .filter_map(|value| value.as_object())
                    .find(|node_object| node_name_of(node_object) == Some(named))
                    .and_then(node_uuid_of)
            })
        };
        let inner_end = |end: &Value| -> Option<StreamGraphDocumentLinkEnd> {
            Some(StreamGraphDocumentLinkEnd {
                node: inner_uuid_of_the_name(end.get("node")?.as_str()?)?,
                port: end.get("port")?.as_str()?.to_owned(),
            })
        };

        let mut interface_port_of_the_node = std::collections::HashMap::new();
        let mut unfolded_values = Vec::new();
        for inner in &inner_node_values {
            let inner_object = inner
                .as_object()
                .ok_or("a fragment's node is a JSON object")?;
            let inner_uuid =
                node_uuid_of(inner_object).ok_or("each node of a fragment holds its UUID")?;
            let inner_type = inner_object.get("type").and_then(Value::as_str);
            if inner_type.is_some_and(is_a_graph_interface_node_type) {
                let port = inner_object
                    .get("config")
                    .and_then(|config| config.get(GRAPH_INTERFACE_PORT_CONFIG_KEY))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                interface_port_of_the_node.insert(inner_uuid, port);
                continue;
            }
            let inner_name = node_name_of(inner_object).unwrap_or_default();
            if let Some(holder) = self.node_objects().find(|node_object| {
                node_name_of(node_object) == Some(inner_name)
                    && node_uuid_of(node_object) != Some(fragment)
            }) {
                return Err(format!(
                    "node {inner_uuid} `{inner_name}` of the fragment cannot join the stream's \
                     level: node {} has that name; rename one first",
                    node_uuid_of(holder).map_or_else(String::new, |uuid| uuid.to_string())
                ));
            }
            unfolded_values.push((*inner).clone());
        }

        let mut kept_inner_links = Vec::new();
        let mut readers_of_each_input_port: Vec<(String, StreamGraphDocumentLinkEnd)> = Vec::new();
        let mut source_of_each_output_port: Vec<(String, StreamGraphDocumentLinkEnd)> = Vec::new();
        for inner_link in stored["links"].as_array().into_iter().flatten() {
            let (Some(source), Some(target)) = (
                inner_link.get("source").and_then(inner_end),
                inner_link.get("target").and_then(inner_end),
            ) else {
                continue;
            };
            match (
                interface_port_of_the_node.get(&source.node),
                interface_port_of_the_node.get(&target.node),
            ) {
                (None, None) => kept_inner_links.push(inner_link.clone()),
                (Some(port), None) => readers_of_each_input_port.push((port.clone(), target)),
                (None, Some(port)) => source_of_each_output_port.push((port.clone(), source)),
                (Some(_), Some(_)) => {}
            }
        }

        let mut fragment_links = Vec::new();
        let mut carried_links: Vec<StreamGraphDocumentLink> = Vec::new();
        for (position, value) in self.link_values().iter().enumerate() {
            let Some(link) = self.link_read_from(value) else {
                continue;
            };
            if link.target.node == fragment {
                carried_links.extend(
                    readers_of_each_input_port
                        .iter()
                        .filter(|(port, _)| *port == link.target.port)
                        .map(|(_, reader)| StreamGraphDocumentLink {
                            source: link.source.clone(),
                            target: reader.clone(),
                        }),
                );
            } else if link.source.node == fragment {
                carried_links.extend(
                    source_of_each_output_port
                        .iter()
                        .filter(|(port, _)| *port == link.source.port)
                        .map(|(_, source)| StreamGraphDocumentLink {
                            source: source.clone(),
                            target: link.target.clone(),
                        }),
                );
            } else {
                continue;
            }
            fragment_links.push(StreamGraphDocumentValueAtItsPosition {
                position,
                value: value.clone(),
            });
        }
        let mut unfolded_links = kept_inner_links;
        for carried in carried_links {
            let carried = serde_json::to_value(&carried)
                .map_err(|not_written| format!("a link does not write: {not_written}"))?;
            if !unfolded_links.contains(&carried) {
                unfolded_links.push(carried);
            }
        }
        let first_link_position = fragment_links.first().map_or(
            self.link_values().len(),
            |link: &StreamGraphDocumentValueAtItsPosition| link.position,
        );
        Ok(StreamGraphFragmentFold {
            fragment: StreamGraphDocumentValueAtItsPosition {
                position: fragment_position,
                value: fragment_value.clone(),
            },
            fragment_links,
            nodes: at_positions_from(fragment_position, unfolded_values),
            links: at_positions_from(first_link_position, unfolded_links),
        })
    }

    /// Take `nodes_out` and `links_out` off the stream's level and put `nodes_in` and
    /// `links_in` at their positions, refused when the level does not hold what is taken out,
    /// a link not taken out still meets a node that is, or a node put in takes a UUID or a name
    /// the level holds.
    pub(crate) fn swap_values_on_the_streams_level(
        &mut self,
        nodes_out: &[StreamGraphDocumentValueAtItsPosition],
        links_out: &[StreamGraphDocumentValueAtItsPosition],
        nodes_in: &[StreamGraphDocumentValueAtItsPosition],
        links_in: &[StreamGraphDocumentValueAtItsPosition],
    ) -> Result<(), String> {
        let uuids_out: Vec<Uuid> = nodes_out
            .iter()
            .map(|node| {
                node.value
                    .as_object()
                    .and_then(node_uuid_of)
                    .ok_or_else(|| "each node taken out holds its UUID".to_owned())
            })
            .collect::<Result<_, _>>()?;
        for node in &uuids_out {
            self.node(*node)?;
        }
        let links_read_out: Vec<StreamGraphDocumentLink> = links_out
            .iter()
            .map(|link| {
                self.link_read_from(&link.value)
                    .ok_or_else(|| "a link taken out names nodes the graph holds".to_owned())
            })
            .collect::<Result<_, _>>()?;
        let held_links = self.links();
        for link in &links_read_out {
            if !held_links.contains(link) {
                return Err(format!(
                    "the graph no longer links {}/{} -> {}/{}",
                    link.source.node, link.source.port, link.target.node, link.target.port
                ));
            }
        }
        if let Some(meeting) = held_links.iter().find(|link| {
            !links_read_out.contains(link)
                && (uuids_out.contains(&link.source.node) || uuids_out.contains(&link.target.node))
        }) {
            return Err(format!(
                "the graph links {}/{} -> {}/{} since, so its nodes are not where they were",
                meeting.source.node, meeting.source.port, meeting.target.node, meeting.target.port
            ));
        }
        let kept_link_values: Vec<Value> = self
            .link_values()
            .iter()
            .filter(|value| {
                self.link_read_from(value)
                    .is_none_or(|link| !links_read_out.contains(&link))
            })
            .cloned()
            .collect();
        *self.link_values_mut() = kept_link_values;
        self.node_values_mut().retain(|value| {
            !value
                .as_object()
                .and_then(node_uuid_of)
                .is_some_and(|node| uuids_out.contains(&node))
        });
        for node in nodes_in {
            let node_object = node
                .value
                .as_object()
                .ok_or("a node put in is a JSON object")?;
            let node_uuid = node_uuid_of(node_object).ok_or("each node put in holds its UUID")?;
            if self.node(node_uuid).is_ok() {
                return Err(format!(
                    "the graph already holds node {node_uuid}; a node's UUID is never reused"
                ));
            }
            let name = node_name_of(node_object).unwrap_or_default();
            if let Some(holder) = self
                .node_objects()
                .find(|held| node_name_of(held) == Some(name))
            {
                return Err(format!(
                    "node {node_uuid} cannot be named `{name}`: node {} has that name",
                    node_uuid_of(holder).map_or_else(String::new, |uuid| uuid.to_string())
                ));
            }
            let position = node.position.min(self.node_values_of_the_level().len());
            self.node_values_mut().insert(position, node.value.clone());
        }
        for link in links_in {
            let position = link.position.min(self.link_values().len());
            self.link_values_mut().insert(position, link.value.clone());
        }
        Ok(())
    }
}

fn push_the_end_under(
    ends_by_end: &mut Vec<(StreamGraphDocumentLinkEnd, Vec<StreamGraphDocumentLinkEnd>)>,
    end: StreamGraphDocumentLinkEnd,
    other_end: StreamGraphDocumentLinkEnd,
) {
    match ends_by_end.iter_mut().find(|(held, _)| *held == end) {
        Some((_, other_ends)) => other_ends.push(other_end),
        None => ends_by_end.push((end, vec![other_end])),
    }
}

fn an_interface_node(node: Uuid, port: &str, node_type: &str, family: &str) -> Value {
    json!({
        "id": node,
        "name": port,
        "type": node_type,
        "config": {
            GRAPH_INTERFACE_PORT_CONFIG_KEY: port,
            GRAPH_INTERFACE_FAMILY_CONFIG_KEY: family,
        },
    })
}

fn link_value(
    source: StreamGraphDocumentLinkEnd,
    target: StreamGraphDocumentLinkEnd,
) -> Result<Value, String> {
    serde_json::to_value(StreamGraphDocumentLink { source, target })
        .map_err(|not_written| format!("a link does not write: {not_written}"))
}

fn at_positions_from(
    first_position: usize,
    values: Vec<Value>,
) -> Vec<StreamGraphDocumentValueAtItsPosition> {
    values
        .into_iter()
        .enumerate()
        .map(|(offset, value)| StreamGraphDocumentValueAtItsPosition {
            position: first_position + offset,
            value,
        })
        .collect()
}

/// `wanted` made lowercase URL-safe, with the next free `-2`, `-3` … when `taken` holds it,
/// and added to `taken`.
fn a_name_not_yet_taken(wanted: &str, taken: &mut HashSet<String>) -> String {
    let mut url_safe = String::new();
    for character in wanted.chars() {
        if character.is_ascii_alphanumeric() {
            url_safe.push(character.to_ascii_lowercase());
        } else if !url_safe.ends_with('-') {
            url_safe.push('-');
        }
    }
    let url_safe = url_safe.trim_matches('-').to_owned();
    let mut name = url_safe.clone();
    let mut suffix = 2;
    while taken.contains(&name) {
        name = format!("{url_safe}-{suffix}");
        suffix += 1;
    }
    taken.insert(name.clone());
    name
}
