// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphOperation, node_name_of, node_uuid_of,
};
use crate::stream_graph_fragment_fold::StreamGraphDocumentValueAtItsPosition;

/// The level of a stream's graph a path of placed streams reaches: the graph document of the
/// stream the last placed stream on the path references, and how each of its nodes and groups
/// is drawn in the stream the path starts from, under the UUID each placement on the path gives
/// it.
#[derive(Clone, Debug)]
pub struct StreamGraphLevelOfAPlacedStream {
    /// The stream asset whose document the level is; the starting stream's for an empty path.
    pub stream: Uuid,
    /// That stream's graph document, each node and group under its own UUID.
    pub document: StreamGraphDocument,
    placed_stream_node_of_each_step: Vec<Uuid>,
}

impl StreamGraphLevelOfAPlacedStream {
    /// Whether the path reached no placed stream: the level is the starting stream's own.
    pub fn is_the_streams_own_level(&self) -> bool {
        self.placed_stream_node_of_each_step.is_empty()
    }

    /// The UUID the node or group `own` of the level's document is drawn under in the stream
    /// the path starts from.
    pub fn the_drawn_uuid_of(&self, own: Uuid) -> Uuid {
        self.placed_stream_node_of_each_step
            .iter()
            .rev()
            .fold(own, |placed, placing_node| {
                Uuid::new_v5(placing_node, placed.as_bytes())
            })
    }

    /// The level's document with each node and group under the UUID it is drawn under.
    pub fn the_document_as_drawn(&self) -> Result<StreamGraphDocument, String> {
        let drawn_uuid_of_each_own: HashMap<String, String> = self
            .every_node_and_group_uuid_of_the_document()
            .into_iter()
            .map(|own| (own.to_string(), self.the_drawn_uuid_of(own).to_string()))
            .collect();
        let mut drawn = self.document.to_value();
        each_node_and_group_reference_renamed(&mut drawn, &drawn_uuid_of_each_own);
        StreamGraphDocument::from_value(drawn)
    }

    /// `operation_as_drawn`, made on [`Self::the_document_as_drawn`], as the operation on the
    /// level's document: each node and group it names under its own UUID.
    pub fn the_operation_on_the_document(
        &self,
        operation_as_drawn: StreamGraphOperation,
    ) -> Result<StreamGraphOperation, String> {
        let own_uuid_of_each_drawn: HashMap<String, String> = self
            .every_node_and_group_uuid_of_the_document()
            .into_iter()
            .map(|own| (self.the_drawn_uuid_of(own).to_string(), own.to_string()))
            .collect();
        the_operation_with_each_node_and_group_renamed(operation_as_drawn, &own_uuid_of_each_drawn)
    }

    /// `operation`, made on the level's document, as the stream the path starts from draws it:
    /// each node and group of the document before it, or of `document_after_it`, under its drawn
    /// UUID.
    pub fn the_operation_as_drawn(
        &self,
        operation: &StreamGraphOperation,
        document_after_it: &StreamGraphDocument,
    ) -> Result<StreamGraphOperation, String> {
        let mut own_uuids = self.every_node_and_group_uuid_of_the_document();
        push_each_node_and_group_uuid_of_the_level(&document_after_it.to_value(), &mut own_uuids);
        let drawn_uuid_of_each_own: HashMap<String, String> = own_uuids
            .into_iter()
            .map(|own| (own.to_string(), self.the_drawn_uuid_of(own).to_string()))
            .collect();
        the_operation_with_each_node_and_group_renamed(operation.clone(), &drawn_uuid_of_each_own)
    }

    fn every_node_and_group_uuid_of_the_document(&self) -> Vec<Uuid> {
        let mut uuids = Vec::new();
        push_each_node_and_group_uuid_of_the_level(&self.document.to_value(), &mut uuids);
        uuids
    }
}

impl StreamGraphDocument {
    /// The level the path of placed stream nodes `level` reaches from this document, the graph
    /// document of the stream asset `stream`, each node on the path named by the UUID it is drawn
    /// under in this stream and `document_of_the_stream` reading the document of each stream
    /// the path places. Refused for a node that places a stream of an import, holds a graph
    /// generator's output, or is no subgraph.
    pub fn the_level_a_path_of_placed_streams_reaches(
        self,
        stream: Uuid,
        level: &[Uuid],
        document_of_the_stream: &mut dyn FnMut(Uuid) -> Result<StreamGraphDocument, String>,
    ) -> Result<StreamGraphLevelOfAPlacedStream, String> {
        let mut reached = StreamGraphLevelOfAPlacedStream {
            stream,
            document: self,
            placed_stream_node_of_each_step: Vec::new(),
        };
        for drawn in level {
            let node_object = reached
                .document
                .node_objects()
                .find(|node_object| {
                    node_uuid_of(node_object).map(|own| reached.the_drawn_uuid_of(own))
                        == Some(*drawn)
                })
                .ok_or_else(|| reached.document.why_no_node_has(*drawn))?;
            let own = node_uuid_of(node_object).ok_or("each node holds its UUID")?;
            let placed_stream = the_stream_a_node_of_the_level_places(node_object, *drawn)?;
            reached.document = document_of_the_stream(placed_stream)?;
            reached.stream = placed_stream;
            reached.placed_stream_node_of_each_step.push(own);
        }
        Ok(reached)
    }

    /// The UUID of each node of this level placing the stream asset `stream` of the project.
    pub fn each_node_placing_the_stream(&self, stream: Uuid) -> Vec<Uuid> {
        self.node_objects()
            .filter(|node_object| {
                node_object.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE)
                    && node_object
                        .get("config")
                        .and_then(|config| config.get(STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY))
                        .is_some_and(|held_stream| {
                            held_stream.get("from").is_none()
                                && held_stream.get("asset").and_then(Value::as_str)
                                    == Some(stream.to_string().as_str())
                        })
            })
            .filter_map(node_uuid_of)
            .collect()
    }

    /// The operation carrying each link of this level onto a port of a node placing the stream
    /// asset `placed_stream` across an edit that turned that stream's document from
    /// `placed_before` into `placed_after`: a link onto a port the edit renames carried to the
    /// new name, and one onto a port it takes away taken out, so the inverse puts it back.
    /// `None` when no link changes.
    pub fn the_links_carried_onto_the_ports_of_a_placed_stream(
        &self,
        placed_stream: Uuid,
        placed_before: &StreamGraphDocument,
        placed_after: &StreamGraphDocument,
    ) -> Result<Option<StreamGraphOperation>, String> {
        let ports_before = FragmentPortsDeclaredInside::of(placed_before);
        let ports_after = FragmentPortsDeclaredInside::of(placed_after);
        let mut uuid_and_name_of_each_placing_node = Vec::new();
        for placing_node in self.each_node_placing_the_stream(placed_stream) {
            uuid_and_name_of_each_placing_node
                .push((placing_node.to_string(), self.node(placing_node)?.name));
        }
        let mut port_links_taken_out = Vec::new();
        let mut port_links_put_back = Vec::new();
        let mut links_after_the_edit = 0;
        for (position, link) in self.link_values().iter().enumerate() {
            let mut link_after_the_edit = link.clone();
            let mut taken_away = false;
            for (end_key, direction) in [
                ("target", FragmentPortDirection::Input),
                ("source", FragmentPortDirection::Output),
            ] {
                let end = &link[end_key];
                let on_a_placing_node = end["node"].as_str().is_some_and(|named| {
                    uuid_and_name_of_each_placing_node
                        .iter()
                        .any(|(uuid, name)| named == uuid || named == name)
                });
                let Some(port) = end["port"].as_str().filter(|_| on_a_placing_node) else {
                    continue;
                };
                if ports_after.declares(direction, port) {
                    continue;
                }
                match ports_before
                    .interface_node_declaring(direction, port)
                    .and_then(|interface_node| ports_after.port_of(direction, interface_node))
                {
                    Some(renamed_port) => {
                        link_after_the_edit[end_key]["port"] =
                            Value::String(renamed_port.to_owned());
                    }
                    None => taken_away = true,
                }
            }
            if taken_away {
                port_links_taken_out.push(StreamGraphDocumentValueAtItsPosition {
                    position,
                    value: link.clone(),
                });
            } else {
                if link_after_the_edit != *link {
                    port_links_taken_out.push(StreamGraphDocumentValueAtItsPosition {
                        position,
                        value: link.clone(),
                    });
                    port_links_put_back.push(StreamGraphDocumentValueAtItsPosition {
                        position: links_after_the_edit,
                        value: link_after_the_edit,
                    });
                }
                links_after_the_edit += 1;
            }
        }
        if port_links_taken_out.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream {
                placed_stream,
                port_links_taken_out,
                port_links_put_back,
            },
        ))
    }

    pub(crate) fn carry_the_links_onto_the_ports_of_a_placed_stream(
        &mut self,
        placed_stream: Uuid,
        port_links_taken_out: &[StreamGraphDocumentValueAtItsPosition],
        port_links_put_back: &[StreamGraphDocumentValueAtItsPosition],
    ) -> Result<(), String> {
        let mut links = self.link_values().to_vec();
        for taken_out in port_links_taken_out.iter().rev() {
            let Some(held_at) = self.position_of_the_link_with_the_ends_of(&links, taken_out)
            else {
                return Err(format!(
                    "the graph holds no link {}, onto a port of the placed stream {placed_stream} \
                     an edit inside it takes away",
                    taken_out.value
                ));
            };
            links.remove(held_at);
        }
        for put_back in port_links_put_back {
            if put_back.position > links.len() {
                return Err(format!(
                    "the link {} onto a port of the placed stream {placed_stream} goes back at \
                     position {}, past the graph's {} links",
                    put_back.value,
                    put_back.position,
                    links.len()
                ));
            }
            links.insert(put_back.position, put_back.value.clone());
        }
        *self.link_values_mut() = links;
        Ok(())
    }

    /// Where `links` holds the link joining `taken_out`'s two ends, at its recorded position
    /// when it is still there. A graph rendered from the running stream carries each link's
    /// running `id`, which the link as recorded need not, so the link is matched by its ends.
    fn position_of_the_link_with_the_ends_of(
        &self,
        links: &[Value],
        taken_out: &StreamGraphDocumentValueAtItsPosition,
    ) -> Option<usize> {
        let ends_taken_out = self.link_read_from(&taken_out.value)?;
        let joins_the_ends_taken_out =
            |held: &Value| self.link_read_from(held).as_ref() == Some(&ends_taken_out);
        if links
            .get(taken_out.position)
            .is_some_and(joins_the_ends_taken_out)
        {
            return Some(taken_out.position);
        }
        links.iter().position(joins_the_ends_taken_out)
    }
}

fn the_stream_a_node_of_the_level_places(
    node_object: &Map<String, Value>,
    drawn: Uuid,
) -> Result<Uuid, String> {
    let name = node_name_of(node_object).unwrap_or_default();
    match node_object.get("type").and_then(Value::as_str) {
        Some(STREAM_FRAGMENT_NODE_TYPE) => {
            let held_stream = node_object
                .get("config")
                .and_then(|config| config.get(STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY));
            if let Some(from) = held_stream.and_then(|held| held.get("from")) {
                return Err(format!(
                    "node {drawn} `{name}` places a stream of the import {from}, which is edited \
                     by opening the import's folder as a project of its own"
                ));
            }
            held_stream
                .and_then(|held| held.get("asset"))
                .and_then(Value::as_str)
                .and_then(|asset| Uuid::parse_str(asset).ok())
                .ok_or_else(|| {
                    format!(
                        "node {drawn} `{name}` names no stream: its config's \
                         `{STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY}` holds no `asset` UUID"
                    )
                })
        }
        Some(generator) if node_object.contains_key("fragment") => Err(format!(
            "node {drawn} `{name}` holds what the graph generator `{generator}` builds, which a \
             refresh replaces; unlock it with unlock_subgraph to edit inside it"
        )),
        _ => Err(format!(
            "node {drawn} `{name}` is no subgraph, so it has no inside to edit"
        )),
    }
}

/// `operation` with each node and group it names under the UUID `renamed` gives it; a config
/// or pin value it sets is a value of its own and is left as it is.
fn the_operation_with_each_node_and_group_renamed(
    operation: StreamGraphOperation,
    renamed: &HashMap<String, String>,
) -> Result<StreamGraphOperation, String> {
    let renamed_uuid = |uuid: Uuid| -> Uuid {
        renamed
            .get(&uuid.to_string())
            .and_then(|new_text| Uuid::parse_str(new_text).ok())
            .unwrap_or(uuid)
    };
    match operation {
        StreamGraphOperation::SetNodeConfig { node, from, to } => {
            Ok(StreamGraphOperation::SetNodeConfig {
                node: renamed_uuid(node),
                from,
                to,
            })
        }
        StreamGraphOperation::SetPinValue {
            node,
            input,
            from,
            to,
        } => Ok(StreamGraphOperation::SetPinValue {
            node: renamed_uuid(node),
            input,
            from,
            to,
        }),
        operation => {
            let mut renamed_operation = serde_json::to_value(operation)
                .map_err(|not_written| format!("the operation does not write: {not_written}"))?;
            each_node_and_group_reference_renamed(&mut renamed_operation, renamed);
            serde_json::from_value(renamed_operation)
                .map_err(|not_read| format!("the operation does not read back: {not_read}"))
        }
    }
}

/// Each node and group UUID `value` names, at any depth, replaced by its value in `renamed`: a
/// node's or group's `id`, a link end's or an operation's `node`, a `group`, and each entry of a
/// `nodes` list. A node's `config`, `pins` and `studio.metadata` are values of its own and are left
/// as they are.
fn each_node_and_group_reference_renamed(value: &mut Value, renamed: &HashMap<String, String>) {
    let rename = |text: &mut String| {
        if let Some(new_text) = renamed.get(text.as_str()) {
            *text = new_text.clone();
        }
    };
    match value {
        Value::Object(object) => {
            for (key, entry) in object.iter_mut() {
                match (key.as_str(), entry) {
                    ("config" | "pins" | "metadata", _) => {}
                    ("id" | "node" | "group", Value::String(text)) => rename(text),
                    ("nodes", Value::Array(entries)) => {
                        for entry in entries {
                            match entry {
                                Value::String(text) => rename(text),
                                entry => each_node_and_group_reference_renamed(entry, renamed),
                            }
                        }
                    }
                    (_, entry) => each_node_and_group_reference_renamed(entry, renamed),
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                each_node_and_group_reference_renamed(entry, renamed);
            }
        }
        _ => {}
    }
}

/// The UUID of each node and group of `level`, at every level of a fragment it stores.
fn push_each_node_and_group_uuid_of_the_level(level: &Value, uuids: &mut Vec<Uuid>) {
    for node in level["nodes"].as_array().into_iter().flatten() {
        if let Some(own) = node.as_object().and_then(node_uuid_of) {
            uuids.push(own);
        }
        if let Some(fragment) = node.get("fragment") {
            push_each_node_and_group_uuid_of_the_level(fragment, uuids);
        }
    }
    for group in level["groups"].as_array().into_iter().flatten() {
        if let Some(own) = group.as_object().and_then(node_uuid_of) {
            uuids.push(own);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FragmentPortDirection {
    Input,
    Output,
}

/// Each `GraphInput` and `GraphOutput` node of a placed stream's document, by UUID, with the
/// port it declares on the node placing it.
struct FragmentPortsDeclaredInside {
    port_of_each_interface_node: Vec<(Uuid, FragmentPortDirection, String)>,
}

impl FragmentPortsDeclaredInside {
    fn of(inside: &StreamGraphDocument) -> Self {
        let port_of_each_interface_node = inside
            .node_objects()
            .filter_map(|node_object| {
                let direction = match node_object.get("type").and_then(Value::as_str) {
                    Some(GRAPH_INPUT_NODE_TYPE) => FragmentPortDirection::Input,
                    Some(GRAPH_OUTPUT_NODE_TYPE) => FragmentPortDirection::Output,
                    _ => return None,
                };
                let port = node_object
                    .get("config")?
                    .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)?
                    .as_str()?;
                Some((node_uuid_of(node_object)?, direction, port.to_owned()))
            })
            .collect();
        Self {
            port_of_each_interface_node,
        }
    }

    fn declares(&self, direction: FragmentPortDirection, port: &str) -> bool {
        self.interface_node_declaring(direction, port).is_some()
    }

    fn interface_node_declaring(
        &self,
        direction: FragmentPortDirection,
        port: &str,
    ) -> Option<Uuid> {
        self.port_of_each_interface_node
            .iter()
            .find(|(_, declared_direction, declared_port)| {
                *declared_direction == direction && declared_port == port
            })
            .map(|(interface_node, ..)| *interface_node)
    }

    fn port_of(&self, direction: FragmentPortDirection, interface_node: Uuid) -> Option<&str> {
        self.port_of_each_interface_node
            .iter()
            .find(|(declared_by, declared_direction, _)| {
                *declared_by == interface_node && *declared_direction == direction
            })
            .map(|(_, _, port)| port.as_str())
    }
}
