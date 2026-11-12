// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
    /// port the edit renames carried to the new name, and each onto a port it takes away taken
    /// out with it, so its inverse puts the link back.
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
        let ports_before = FragmentPortsDeclaredInside::of(&inside);
        let mut edited_inside = inside;
        edited_inside.apply(&edit_inside)?;
        let ports_after = FragmentPortsDeclaredInside::of(&edited_inside);
        let fragment_text = fragment.to_string();
        let fragment_name = self.node(*fragment)?.name;
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
                let on_the_fragment = end["node"]
                    .as_str()
                    .is_some_and(|named| named == fragment_text || named == fragment_name);
                let Some(port) = end["port"].as_str().filter(|_| on_the_fragment) else {
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
        Ok(StreamGraphOperation::EditInsideFragment {
            fragment: *fragment,
            edit: Box::new(edit_inside),
            port_links_taken_out,
            port_links_put_back,
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

#[derive(Clone, Copy, PartialEq)]
enum FragmentPortDirection {
    Input,
    Output,
}

/// Each `GraphInput` and `GraphOutput` node of a fragment's inside, by UUID, with the port it
/// declares on the fragment node.
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
