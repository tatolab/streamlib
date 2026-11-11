// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_OUTPUT_NODE_TYPE,
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphOperation, node_name_of,
    node_uuid_of,
};
use crate::stream_graph_fragment_fold::{
    AUTHORED_FRAGMENT_NODE_TYPE, StreamGraphDocumentValueAtItsPosition,
};

impl StreamGraphDocument {
    /// The level inside the authored fragment `level` names, a path of fragment nodes from this
    /// level inward, as a document of its own; this level itself for an empty path. A graph
    /// generator's output and a placed stream are refused, as is a node that is no fragment.
    pub fn the_level_inside(&self, level: &[Uuid]) -> Result<StreamGraphDocument, String> {
        let Some((fragment, inner_level)) = level.split_first() else {
            return Ok(self.clone());
        };
        let fragment_object = self.an_authored_fragment_of_this_level(*fragment)?;
        StreamGraphDocument::from_value(fragment_object["fragment"].clone())
            .map_err(|not_read| format!("node {fragment}'s fragment does not read: {not_read}"))?
            .the_level_inside(inner_level)
    }

    /// `edit`, an operation on the level inside the authored fragment `level` names, as the
    /// operation on this level that makes it: each link of an enclosing level onto a fragment
    /// port the edit takes away taken out with it, so its inverse puts the link back.
    pub fn the_edit_at_the_level(
        &self,
        level: &[Uuid],
        edit: StreamGraphOperation,
    ) -> Result<StreamGraphOperation, String> {
        let Some((fragment, inner_level)) = level.split_first() else {
            return Ok(edit);
        };
        let inside = self.the_level_inside(std::slice::from_ref(fragment))?;
        let edit_inside = inside.the_edit_at_the_level(inner_level, edit)?;
        let mut edited_inside = inside;
        edited_inside.apply(&edit_inside)?;
        let ports_left = FragmentPortsDeclaredInside::of(&edited_inside);
        let fragment_text = fragment.to_string();
        let fragment_name = self.node(*fragment)?.name;
        let port_links_taken_out = self
            .link_values()
            .iter()
            .enumerate()
            .filter(|(_, link)| {
                let names_a_port_taken_away = |end_key: &str, ports_left: &HashSet<String>| {
                    let end = &link[end_key];
                    end["node"]
                        .as_str()
                        .is_some_and(|named| named == fragment_text || named == fragment_name)
                        && end["port"]
                            .as_str()
                            .is_some_and(|port| !ports_left.contains(port))
                };
                names_a_port_taken_away("target", &ports_left.inputs)
                    || names_a_port_taken_away("source", &ports_left.outputs)
            })
            .map(|(position, link)| StreamGraphDocumentValueAtItsPosition {
                position,
                value: link.clone(),
            })
            .collect();
        Ok(StreamGraphOperation::EditInsideFragment {
            fragment: *fragment,
            edit: Box::new(edit_inside),
            port_links_taken_out,
            port_links_put_back: Vec::new(),
        })
    }

    pub(crate) fn apply_the_edit_inside_a_fragment(
        &mut self,
        fragment: Uuid,
        edit: &StreamGraphOperation,
        port_links_taken_out: &[StreamGraphDocumentValueAtItsPosition],
        port_links_put_back: &[StreamGraphDocumentValueAtItsPosition],
    ) -> Result<(), String> {
        let mut inside = self.the_level_inside(&[fragment])?;
        inside.apply(edit)?;
        let mut links = self.link_values().to_vec();
        for taken_out in port_links_taken_out.iter().rev() {
            if links.get(taken_out.position) != Some(&taken_out.value) {
                return Err(format!(
                    "the graph holds no link {} at position {}, onto a port of node {fragment} \
                     the edit inside it takes away",
                    taken_out.value, taken_out.position
                ));
            }
            links.remove(taken_out.position);
        }
        for put_back in port_links_put_back {
            if put_back.position > links.len() {
                return Err(format!(
                    "the link {} onto node {fragment}'s port goes back at position {}, past the \
                     graph's {} links",
                    put_back.value,
                    put_back.position,
                    links.len()
                ));
            }
            links.insert(put_back.position, put_back.value.clone());
        }
        *self.link_values_mut() = links;
        self.fragment_object_mut(fragment)?
            .insert("fragment".to_owned(), inside.to_value());
        Ok(())
    }

    fn an_authored_fragment_of_this_level(
        &self,
        fragment: Uuid,
    ) -> Result<&Map<String, Value>, String> {
        let fragment_object = self
            .node_objects()
            .find(|node_object| node_uuid_of(node_object) == Some(fragment))
            .ok_or_else(|| self.why_no_node_has(fragment))?;
        let name = node_name_of(fragment_object).unwrap_or_default();
        match fragment_object.get("type").and_then(Value::as_str) {
            Some(AUTHORED_FRAGMENT_NODE_TYPE) if fragment_object.contains_key("fragment") => {
                Ok(fragment_object)
            }
            Some(STREAM_FRAGMENT_NODE_TYPE) => Err(format!(
                "node {fragment} `{name}` places another stream, which is edited as its own \
                 stream, never from inside the node placing it"
            )),
            Some(generator) if fragment_object.contains_key("fragment") => Err(format!(
                "node {fragment} `{name}` holds what the graph generator `{generator}` builds, \
                 which a refresh replaces; unlock it with unlock_generator_fragment to edit \
                 inside it"
            )),
            _ => Err(format!(
                "node {fragment} `{name}` is no subgraph, so it has no inside to edit"
            )),
        }
    }

    fn fragment_object_mut(&mut self, fragment: Uuid) -> Result<&mut Map<String, Value>, String> {
        let why_none = self.why_no_node_has(fragment);
        self.node_values_mut()
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|node_object| node_uuid_of(node_object) == Some(fragment))
            .ok_or(why_none)
    }
}

/// The port names a fragment's `GraphInput` and `GraphOutput` nodes declare on the fragment node.
struct FragmentPortsDeclaredInside {
    inputs: HashSet<String>,
    outputs: HashSet<String>,
}

impl FragmentPortsDeclaredInside {
    fn of(inside: &StreamGraphDocument) -> Self {
        let mut declared = Self {
            inputs: HashSet::new(),
            outputs: HashSet::new(),
        };
        for node_object in inside.node_objects() {
            let Some(port) = node_object
                .get("config")
                .and_then(|config| config.get(GRAPH_INTERFACE_PORT_CONFIG_KEY))
                .and_then(Value::as_str)
            else {
                continue;
            };
            match node_object.get("type").and_then(Value::as_str) {
                Some(GRAPH_INPUT_NODE_TYPE) => {
                    declared.inputs.insert(port.to_owned());
                }
                Some(GRAPH_OUTPUT_NODE_TYPE) => {
                    declared.outputs.insert(port.to_owned());
                }
                _ => {}
            }
        }
        declared
    }
}
