// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::project_stream_document::STREAM_DOCUMENT_DISPLAY_NAME_KEY;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::stream_graph_fragment_fold::{
    StreamGraphDocumentValueAtItsPosition, StreamGraphFragmentFold,
};
use crate::stream_graph_studio_contract::{
    StreamGraphDocumentNote, StreamGraphNodeStudioFields, StreamGraphNoteFields,
    a_documents_studio_holding, the_notes_of_a_documents_studio,
};

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

/// Whether `node_type` is `GraphInput` or `GraphOutput`.
pub fn is_a_graph_interface_node_type(node_type: &str) -> bool {
    matches!(node_type, GRAPH_INPUT_NODE_TYPE | GRAPH_OUTPUT_NODE_TYPE)
}

/// One node as a stream's graph document holds it: the spec keys a load reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphDocumentNode {
    /// The node's UUID, the runtime's from its placement on.
    pub id: Uuid,
    /// The node's name, a label, cast as the runtime casts it.
    pub name: String,
    /// The import path of the node's type.
    #[serde(rename = "type")]
    pub node_type: String,
    /// The node's config.
    #[serde(default = "a_config_naming_no_setting")]
    pub config: Value,
    /// The value set on each unlinked input, by input name.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub pins: Map<String, Value>,
    /// The config fields the node takes as inputs too, each an input of the field's name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_config_fields: Vec<String>,
    /// What the studio draws the node with, under the `studio` contract.
    #[serde(default, skip_serializing_if = "StreamGraphNodeStudioFields::is_empty")]
    pub studio: Box<StreamGraphNodeStudioFields>,
    /// The stored output of the graph generator `type` names, on a fragment alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<Box<Value>>,
}

impl StreamGraphDocumentNode {
    /// The import path of each graph generator a refresh of this fragment node runs: its own
    /// `type`, then the `type` of each fragment nested in it, at every level.
    pub fn graph_generators_a_refresh_runs(&self) -> Vec<String> {
        fn push_each_nested_generator(fragment: &Value, generators: &mut Vec<String>) {
            for nested in fragment["nodes"].as_array().into_iter().flatten() {
                if let (Some(nested_fragment), Some(generator)) =
                    (nested.get("fragment"), nested["type"].as_str())
                {
                    generators.push(generator.to_owned());
                    push_each_nested_generator(nested_fragment, generators);
                }
            }
        }
        let Some(fragment) = &self.fragment else {
            return Vec::new();
        };
        let mut generators = vec![self.node_type.clone()];
        push_each_nested_generator(fragment, &mut generators);
        generators
    }
}

fn a_config_naming_no_setting() -> Value {
    Value::Object(Map::new())
}

/// One end of a link, its node by UUID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StreamGraphDocumentLinkEnd {
    /// The node's UUID.
    pub node: Uuid,
    /// The port's name.
    pub port: String,
}

/// One link of a stream's graph, from an output port to an input port.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StreamGraphDocumentLink {
    /// The output port it carries from.
    pub source: StreamGraphDocumentLinkEnd,
    /// The input port it carries into.
    pub target: StreamGraphDocumentLinkEnd,
}

/// What a labelled group says of its nodes; it changes nothing that runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamGraphGroupFields {
    /// The group's title.
    pub title: String,
    /// What the group's nodes do, for the next person who reads the graph.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The colour the studio draws the group with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub colour: String,
}

/// One labelled group of a stream's graph document; a node belongs to it by naming its `id`
/// as the node's `studio.group`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamGraphDocumentGroup {
    /// The group's UUID, the runtime's from its placement on.
    pub id: Uuid,
    #[serde(flatten)]
    pub fields: StreamGraphGroupFields,
}

/// The `studio` key a node names its labelled group's UUID under.
pub(crate) const STUDIO_GROUP_KEY: &str = "group";

/// The graph document's key holding its labelled groups.
const GROUPS_KEY: &str = "groups";

/// The graph document's key holding its own `studio` object: the canvas items that are not
/// nodes.
const DOCUMENT_STUDIO_KEY: &str = "studio";

/// What a fragment node holds of its graph generator's run: the generator, the promoted
/// settings it ran with and the graph it built.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphFragmentPlacement {
    /// The graph generator's import path, the fragment node's `type`.
    #[serde(rename = "type")]
    pub generator: String,
    /// The promoted settings the generator ran with, the fragment node's `config`.
    pub config: Value,
    /// The stored output, its nodes each holding its UUID and its links naming them by UUID.
    pub fragment: Value,
}

/// One change to a stream's graph, recorded with everything its inverse needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum StreamGraphOperation {
    /// The node was placed, with the links it brings.
    AddNode {
        node: StreamGraphDocumentNode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<StreamGraphDocumentLink>,
    },
    /// The node was taken out, with every link it had.
    RemoveNode {
        node: StreamGraphDocumentNode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<StreamGraphDocumentLink>,
    },
    /// The link was made.
    Connect { link: StreamGraphDocumentLink },
    /// The link was taken out.
    Disconnect { link: StreamGraphDocumentLink },
    /// The node's config was replaced whole.
    SetNodeConfig { node: Uuid, from: Value, to: Value },
    /// The value on one of the node's inputs was set, `None` being no value.
    SetPinValue {
        node: Uuid,
        input: String,
        from: Option<Value>,
        to: Option<Value>,
    },
    /// The node's name changed.
    RenameNode {
        node: Uuid,
        from: String,
        to: String,
    },
    /// The node's studio fields were replaced whole.
    SetStudioFields {
        node: Uuid,
        from: Box<StreamGraphNodeStudioFields>,
        to: Box<StreamGraphNodeStudioFields>,
    },
    /// The fragment node's generator was run again, and its stored output replaced whole.
    RefreshFragment {
        node: Uuid,
        from: StreamGraphFragmentPlacement,
        to: StreamGraphFragmentPlacement,
    },
    /// The config fields the node takes as inputs were replaced whole.
    SetExposedConfigFields {
        node: Uuid,
        from: Vec<String>,
        to: Vec<String>,
    },
    /// The labelled group was made, each of `nodes` joining it.
    AddGroup {
        group: StreamGraphDocumentGroup,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        nodes: Vec<Uuid>,
    },
    /// The labelled group's title, description and colour were replaced whole.
    SetGroupFields {
        group: Uuid,
        from: StreamGraphGroupFields,
        to: StreamGraphGroupFields,
    },
    /// The labelled group was taken out, each of `nodes`, every node it held, left ungrouped.
    RemoveGroup {
        group: StreamGraphDocumentGroup,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        nodes: Vec<Uuid>,
    },
    /// The canvas note was placed.
    AddNote { note: StreamGraphDocumentNote },
    /// The canvas note's text, position and size were replaced whole.
    SetNoteFields {
        note: Uuid,
        from: StreamGraphNoteFields,
        to: StreamGraphNoteFields,
    },
    /// The canvas note was taken out.
    RemoveNote { note: StreamGraphDocumentNote },
    /// The fold's plain nodes and their links were replaced with its fragment node.
    FoldIntoFragment { fold: StreamGraphFragmentFold },
    /// The fold's fragment node was replaced with the plain nodes and links it stands for.
    UnfoldFragment { fold: StreamGraphFragmentFold },
    /// The links of this level onto the ports of each node placing the stream asset
    /// `placed_stream` followed an edit inside that stream: each in `port_links_taken_out` taken
    /// out and each in `port_links_put_back` put back, at its position.
    CarryTheLinksOntoThePortsOfAPlacedStream {
        placed_stream: Uuid,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        port_links_taken_out: Vec<StreamGraphDocumentValueAtItsPosition>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        port_links_put_back: Vec<StreamGraphDocumentValueAtItsPosition>,
    },
}

impl StreamGraphOperation {
    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self.clone() {
            Self::AddNode { node, links } => Self::RemoveNode { node, links },
            Self::RemoveNode { node, links } => Self::AddNode { node, links },
            Self::Connect { link } => Self::Disconnect { link },
            Self::Disconnect { link } => Self::Connect { link },
            Self::SetNodeConfig { node, from, to } => Self::SetNodeConfig {
                node,
                from: to,
                to: from,
            },
            Self::SetPinValue {
                node,
                input,
                from,
                to,
            } => Self::SetPinValue {
                node,
                input,
                from: to,
                to: from,
            },
            Self::RenameNode { node, from, to } => Self::RenameNode {
                node,
                from: to,
                to: from,
            },
            Self::SetStudioFields { node, from, to } => Self::SetStudioFields {
                node,
                from: to,
                to: from,
            },
            Self::RefreshFragment { node, from, to } => Self::RefreshFragment {
                node,
                from: to,
                to: from,
            },
            Self::SetExposedConfigFields { node, from, to } => Self::SetExposedConfigFields {
                node,
                from: to,
                to: from,
            },
            Self::AddGroup { group, nodes } => Self::RemoveGroup { group, nodes },
            Self::RemoveGroup { group, nodes } => Self::AddGroup { group, nodes },
            Self::SetGroupFields { group, from, to } => Self::SetGroupFields {
                group,
                from: to,
                to: from,
            },
            Self::AddNote { note } => Self::RemoveNote { note },
            Self::RemoveNote { note } => Self::AddNote { note },
            Self::SetNoteFields { note, from, to } => Self::SetNoteFields {
                note,
                from: to,
                to: from,
            },
            Self::FoldIntoFragment { fold } => Self::UnfoldFragment { fold },
            Self::UnfoldFragment { fold } => Self::FoldIntoFragment { fold },
            Self::CarryTheLinksOntoThePortsOfAPlacedStream {
                placed_stream,
                port_links_taken_out,
                port_links_put_back,
            } => Self::CarryTheLinksOntoThePortsOfAPlacedStream {
                placed_stream,
                port_links_taken_out: port_links_put_back,
                port_links_put_back: port_links_taken_out,
            },
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        let end = |end: &StreamGraphDocumentLinkEnd| format!("{}/{}", end.node, end.port);
        match self {
            Self::AddNode { node, .. } => format!("add_node {} {}", node.id, node.name),
            Self::RemoveNode { node, .. } => format!("remove_node {} {}", node.id, node.name),
            Self::Connect { link } => {
                format!("connect {} -> {}", end(&link.source), end(&link.target))
            }
            Self::Disconnect { link } => {
                format!("disconnect {} -> {}", end(&link.source), end(&link.target))
            }
            Self::SetNodeConfig { node, .. } => format!("set_node_config {node}"),
            Self::SetPinValue { node, input, .. } => format!("set_pin_value {node}/{input}"),
            Self::RenameNode { node, to, .. } => format!("rename_node {node} {to}"),
            Self::SetStudioFields { node, .. } => format!("set_studio_fields {node}"),
            Self::RefreshFragment { node, to, .. } => {
                format!("refresh_fragment {node} {}", to.generator)
            }
            Self::SetExposedConfigFields { node, .. } => {
                format!("set_exposed_config_fields {node}")
            }
            Self::AddGroup { group, .. } => {
                format!("add_group {} {}", group.id, group.fields.title)
            }
            Self::SetGroupFields { group, .. } => format!("set_group_fields {group}"),
            Self::RemoveGroup { group, .. } => {
                format!("remove_group {} {}", group.id, group.fields.title)
            }
            Self::AddNote { note } => format!("add_note {}", note.id),
            Self::SetNoteFields { note, .. } => format!("set_note_fields {note}"),
            Self::RemoveNote { note } => format!("remove_note {}", note.id),
            Self::FoldIntoFragment { fold } => format!(
                "fold_into_fragment {} {}",
                fold.fragment_node().unwrap_or_default(),
                fold.fragment_name()
            ),
            Self::UnfoldFragment { fold } => format!(
                "unfold_fragment {} {}",
                fold.fragment_node().unwrap_or_default(),
                fold.fragment_name()
            ),
            Self::CarryTheLinksOntoThePortsOfAPlacedStream { placed_stream, .. } => {
                format!("carry_the_links_onto_the_ports_of_the_placed_stream {placed_stream}")
            }
        }
    }

    /// Whether a running stream can take the operation only once the project's graph documents
    /// hold it: it places a stream, or follows an edit inside one, and a running stream places a
    /// stream as that stream's document is on disk.
    pub fn reaches_a_running_stream_once_the_documents_hold_it(&self) -> bool {
        match self {
            Self::FoldIntoFragment { fold } | Self::UnfoldFragment { fold } => {
                fold.fragment.value.get("type").and_then(Value::as_str)
                    == Some(STREAM_FRAGMENT_NODE_TYPE)
            }
            Self::CarryTheLinksOntoThePortsOfAPlacedStream { .. } => true,
            _ => false,
        }
    }

    /// The node the operation acts on; none for an operation on a labelled group.
    pub fn node(&self) -> Option<Uuid> {
        match self {
            Self::AddNode { node, .. } | Self::RemoveNode { node, .. } => Some(node.id),
            Self::Connect { link } | Self::Disconnect { link } => Some(link.target.node),
            Self::SetNodeConfig { node, .. }
            | Self::SetPinValue { node, .. }
            | Self::RenameNode { node, .. }
            | Self::SetStudioFields { node, .. }
            | Self::RefreshFragment { node, .. }
            | Self::SetExposedConfigFields { node, .. } => Some(*node),
            Self::FoldIntoFragment { fold } | Self::UnfoldFragment { fold } => fold.fragment_node(),
            Self::AddGroup { .. }
            | Self::CarryTheLinksOntoThePortsOfAPlacedStream { .. }
            | Self::SetGroupFields { .. }
            | Self::RemoveGroup { .. }
            | Self::AddNote { .. }
            | Self::SetNoteFields { .. }
            | Self::RemoveNote { .. } => None,
        }
    }

    /// Each config the operation leaves a node with, by that node's UUID: its own node's, and
    /// that of every node at every level of a fragment it places.
    pub fn configs_it_sets(&self) -> Vec<(Uuid, &Value)> {
        let mut configs = Vec::new();
        match self {
            Self::AddNode { node, .. } => {
                configs.push((node.id, &node.config));
                if let Some(fragment) = &node.fragment {
                    push_the_config_of_each_node_at_every_level(fragment, &mut configs);
                }
            }
            Self::SetNodeConfig { node, to, .. } => configs.push((*node, to)),
            Self::RefreshFragment { node, to, .. } => {
                configs.push((*node, &to.config));
                push_the_config_of_each_node_at_every_level(&to.fragment, &mut configs);
            }
            Self::FoldIntoFragment { fold } => {
                push_the_config_of_each_of_the_nodes_at_every_level(
                    std::slice::from_ref(&fold.fragment.value),
                    &mut configs,
                );
            }
            Self::UnfoldFragment { fold } => {
                for node in &fold.nodes {
                    push_the_config_of_each_of_the_nodes_at_every_level(
                        std::slice::from_ref(&node.value),
                        &mut configs,
                    );
                }
            }
            _ => {}
        }
        configs
    }
}

/// A stream's graph document, as the runtime reads and edits it: a JSON object whose keys it
/// keeps whether or not it reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamGraphDocument {
    node_values: Vec<Value>,
    link_values: Vec<Value>,
    every_other_key: Map<String, Value>,
}

impl StreamGraphDocument {
    /// The name its stream asset is shown under, `None` when it writes none.
    pub fn display_name(&self) -> Option<&str> {
        self.every_other_key
            .get(STREAM_DOCUMENT_DISPLAY_NAME_KEY)
            .and_then(Value::as_str)
    }

    /// Show its stream asset under `display_name`, or under its file name for `None`.
    pub fn set_display_name(&mut self, display_name: Option<&str>) {
        match display_name {
            Some(display_name) => {
                self.every_other_key.insert(
                    STREAM_DOCUMENT_DISPLAY_NAME_KEY.to_owned(),
                    Value::String(display_name.to_owned()),
                );
            }
            None => {
                self.every_other_key
                    .remove(STREAM_DOCUMENT_DISPLAY_NAME_KEY);
            }
        }
    }

    /// Read a graph document, or a `graph` render of one, from its parsed JSON.
    pub fn from_value(document: Value) -> Result<Self, String> {
        let Value::Object(mut every_other_key) = document else {
            return Err("a graph document is a JSON object".to_owned());
        };
        let mut array_under = |key: &str| match every_other_key.remove(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => Ok(items),
            Some(_) => Err(format!("a graph document's `{key}` is an array")),
        };
        let node_values = array_under("nodes")?;
        let link_values = array_under("links")?;
        refuse_a_node_of_a_tatolab_type_holding_an_inline_graph(&node_values)?;
        let mut document = Self {
            node_values,
            link_values,
            every_other_key,
        };
        document.drop_the_graph_each_stream_fragment_places();
        Ok(document)
    }

    /// Drop the `fragment` of each stream fragment node: a stream fragment is a reference, its
    /// graph read from the stream it names at each load and never stored.
    fn drop_the_graph_each_stream_fragment_places(&mut self) {
        for node_object in self.node_values.iter_mut().filter_map(Value::as_object_mut) {
            if node_object.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE) {
                node_object.remove("fragment");
            }
        }
    }

    /// Read a graph document from its bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        Self::from_value(
            serde_json::from_slice(bytes)
                .map_err(|not_parsed| format!("it does not parse: {not_parsed}"))?,
        )
    }

    /// The document as one JSON object.
    pub fn to_value(&self) -> Value {
        let mut document = self.every_other_key.clone();
        document.insert("nodes".to_owned(), Value::Array(self.node_values.clone()));
        document.insert("links".to_owned(), Value::Array(self.link_values.clone()));
        Value::Object(document)
    }

    /// The document's bytes in canonical form.
    pub fn canonical_bytes(&self) -> serde_json::Result<Vec<u8>> {
        canonical_project_json_bytes(&self.to_value())
    }

    /// The node whose UUID is `node`, as the document holds it.
    pub fn node(&self, node: Uuid) -> Result<StreamGraphDocumentNode, String> {
        let node_object = self
            .node_objects()
            .find(|node_object| node_uuid_of(node_object) == Some(node))
            .ok_or_else(|| self.why_no_node_has(node))?;
        serde_json::from_value(Value::Object(node_object.clone()))
            .map_err(|not_a_node| format!("node {node} does not read: {not_a_node}"))
    }

    /// The top-level node named `name`, as the document holds it, if one is.
    pub fn node_named(&self, name: &str) -> Option<StreamGraphDocumentNode> {
        self.node_objects()
            .find(|node_object| node_name_of(node_object) == Some(name))
            .and_then(|node_object| serde_json::from_value(Value::Object(node_object.clone())).ok())
    }

    /// Every labelled group of the stream's own level, as the document holds it.
    pub fn groups(&self) -> Result<Vec<StreamGraphDocumentGroup>, String> {
        match self.every_other_key.get(GROUPS_KEY) {
            None => Ok(Vec::new()),
            Some(groups) => serde_json::from_value(groups.clone())
                .map_err(|not_read| format!("the graph's `groups` do not read: {not_read}")),
        }
    }

    /// The labelled group whose UUID is `group`, as the document holds it.
    pub fn group(&self, group: Uuid) -> Result<StreamGraphDocumentGroup, String> {
        let groups = self.groups()?;
        groups
            .iter()
            .find(|held| held.id == group)
            .cloned()
            .ok_or_else(|| {
                let held: Vec<String> = groups
                    .iter()
                    .map(|held| format!("{} `{}`", held.id, held.fields.title))
                    .collect();
                if held.is_empty() {
                    format!("the graph holds no group {group}; it holds no group at all")
                } else {
                    format!(
                        "the graph holds no group {group}. It holds: {}",
                        held.join(", ")
                    )
                }
            })
    }

    /// The UUID of every top-level node whose `studio.group` names the group `group`.
    pub fn nodes_of_the_group(&self, group: Uuid) -> Vec<Uuid> {
        self.node_objects()
            .filter(|node_object| studio_group_of(node_object) == Some(group))
            .filter_map(node_uuid_of)
            .collect()
    }

    /// Every canvas note of the graph, as its `studio.notes` holds them.
    pub fn notes(&self) -> Result<Vec<StreamGraphDocumentNote>, String> {
        the_notes_of_a_documents_studio(self.every_other_key.get(DOCUMENT_STUDIO_KEY))
    }

    /// The canvas note whose UUID is `note`.
    pub fn note(&self, note: Uuid) -> Result<StreamGraphDocumentNote, String> {
        let notes = self.notes()?;
        notes
            .iter()
            .find(|held| held.id == note)
            .cloned()
            .ok_or_else(|| {
                let held: Vec<String> = notes.iter().map(|held| held.id.to_string()).collect();
                if held.is_empty() {
                    format!("the graph holds no note {note}; it holds no note at all")
                } else {
                    format!(
                        "the graph holds no note {note}. It holds: {}",
                        held.join(", ")
                    )
                }
            })
    }

    fn set_the_notes(&mut self, notes: &[StreamGraphDocumentNote]) -> Result<(), String> {
        match a_documents_studio_holding(notes)? {
            Some(studio) => {
                self.every_other_key
                    .insert(DOCUMENT_STUDIO_KEY.to_owned(), studio);
            }
            None => {
                self.every_other_key.remove(DOCUMENT_STUDIO_KEY);
            }
        }
        Ok(())
    }

    fn set_the_groups(&mut self, groups: &[StreamGraphDocumentGroup]) -> Result<(), String> {
        if groups.is_empty() {
            self.every_other_key.remove(GROUPS_KEY);
        } else {
            self.every_other_key.insert(
                GROUPS_KEY.to_owned(),
                serde_json::to_value(groups)
                    .map_err(|not_written| format!("the groups do not write: {not_written}"))?,
            );
        }
        Ok(())
    }

    fn set_the_studio_group_of(&mut self, node: Uuid, group: Option<Uuid>) -> Result<(), String> {
        let node_object = self.node_object_mut(node)?;
        let studio = node_object
            .entry("studio")
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(studio) = studio else {
            return Err(format!("node {node}'s `studio` is not an object"));
        };
        match group {
            Some(group) => {
                studio.insert(
                    STUDIO_GROUP_KEY.to_owned(),
                    Value::String(group.to_string()),
                );
            }
            None => {
                studio.remove(STUDIO_GROUP_KEY);
            }
        }
        if studio.is_empty() {
            node_object.remove("studio");
        }
        Ok(())
    }

    /// Every top-level node, as the document holds it.
    pub fn nodes(&self) -> Vec<StreamGraphDocumentNode> {
        self.node_objects()
            .filter_map(|node_object| {
                serde_json::from_value(Value::Object(node_object.clone())).ok()
            })
            .collect()
    }

    /// Each node's config by its UUID, at every level of the fragments the document stores.
    pub fn configs_of_every_node_at_every_level(&self) -> Vec<(Uuid, &Value)> {
        let mut configs = Vec::new();
        push_the_config_of_each_of_the_nodes_at_every_level(&self.node_values, &mut configs);
        configs
    }

    /// The name of each top-level node holding no UUID, with the node as the document holds it.
    pub fn nodes_holding_no_uuid(&self) -> Vec<(String, Value)> {
        self.node_objects()
            .filter(|node_object| node_uuid_of(node_object).is_none())
            .map(|node_object| {
                (
                    node_name_of(node_object).unwrap_or_default().to_owned(),
                    Value::Object(node_object.clone()),
                )
            })
            .collect()
    }

    /// Every top-level fragment node, as the document holds it.
    pub fn fragment_nodes(&self) -> Vec<StreamGraphDocumentNode> {
        self.node_objects()
            .filter(|node_object| node_object.contains_key("fragment"))
            .filter_map(|node_object| {
                serde_json::from_value(Value::Object(node_object.clone())).ok()
            })
            .collect()
    }

    /// Every link with an end on the node whose UUID is `node`.
    pub fn links_touching(&self, node: Uuid) -> Vec<StreamGraphDocumentLink> {
        self.links()
            .into_iter()
            .filter(|link| link.source.node == node || link.target.node == node)
            .collect()
    }

    /// Every link whose ends both name a node the document holds, each end by UUID.
    pub fn links(&self) -> Vec<StreamGraphDocumentLink> {
        self.link_values()
            .iter()
            .filter_map(|link| self.link_read_from(link))
            .collect()
    }

    /// Apply `operation`, refused in its own words when the document does not hold what it
    /// names or already holds what it adds.
    pub fn apply(&mut self, operation: &StreamGraphOperation) -> Result<(), String> {
        self.apply_the_operation(operation)?;
        self.drop_the_graph_each_stream_fragment_places();
        Ok(())
    }

    fn apply_the_operation(&mut self, operation: &StreamGraphOperation) -> Result<(), String> {
        match operation {
            StreamGraphOperation::AddNode { node, links } => {
                if self.node(node.id).is_ok() {
                    return Err(format!(
                        "the graph already holds node {}; a node's UUID is never reused",
                        node.id
                    ));
                }
                if let Some(holder) = self
                    .node_objects()
                    .find(|node_object| node_name_of(node_object) == Some(node.name.as_str()))
                {
                    return Err(format!(
                        "node {} cannot be named `{}`: node {} has that name",
                        node.id,
                        node.name,
                        node_uuid_of(holder).map_or_else(String::new, |uuid| uuid.to_string())
                    ));
                }
                let node_value = serde_json::to_value(node).map_err(|not_written| {
                    format!("node {} does not write: {not_written}", node.id)
                })?;
                self.node_values_mut().push(node_value);
                for link in links {
                    self.connect(link)?;
                }
                Ok(())
            }
            StreamGraphOperation::RemoveNode { node, .. } => {
                self.node(node.id)?;
                let node_text = node.id.to_string();
                let name = node.name.clone();
                self.link_values_mut().retain(|link| {
                    ![link.get("source"), link.get("target")]
                        .into_iter()
                        .flatten()
                        .any(|end| link_end_names_the_node(end, &node_text, &name))
                });
                self.node_values_mut().retain(|node_value| {
                    node_value.as_object().and_then(node_uuid_of) != Some(node.id)
                });
                Ok(())
            }
            StreamGraphOperation::Connect { link } => self.connect(link),
            StreamGraphOperation::FoldIntoFragment { fold } => self
                .swap_values_on_the_streams_level(
                    &fold.nodes,
                    &fold.links,
                    std::slice::from_ref(&fold.fragment),
                    &fold.fragment_links,
                ),
            StreamGraphOperation::UnfoldFragment { fold } => self.swap_values_on_the_streams_level(
                std::slice::from_ref(&fold.fragment),
                &fold.fragment_links,
                &fold.nodes,
                &fold.links,
            ),
            StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream {
                placed_stream,
                port_links_taken_out,
                port_links_put_back,
            } => self.carry_the_links_onto_the_ports_of_a_placed_stream(
                *placed_stream,
                port_links_taken_out,
                port_links_put_back,
            ),
            StreamGraphOperation::Disconnect { link } => {
                let index = self
                    .link_values()
                    .iter()
                    .position(|held| self.link_read_from(held).as_ref() == Some(link))
                    .ok_or_else(|| {
                        format!(
                            "the graph holds no link {}/{} -> {}/{}",
                            link.source.node, link.source.port, link.target.node, link.target.port
                        )
                    })?;
                self.link_values_mut().remove(index);
                Ok(())
            }
            StreamGraphOperation::SetNodeConfig { node, to, .. } => {
                self.node_object_mut(*node)?
                    .insert("config".to_owned(), to.clone());
                Ok(())
            }
            StreamGraphOperation::SetPinValue {
                node, input, to, ..
            } => {
                let node_object = self.node_object_mut(*node)?;
                let pins = node_object
                    .entry("pins")
                    .or_insert_with(|| Value::Object(Map::new()));
                let Value::Object(pins) = pins else {
                    return Err(format!("node {node}'s `pins` is not an object"));
                };
                match to {
                    Some(value) => {
                        pins.insert(input.clone(), value.clone());
                    }
                    None => {
                        pins.remove(input);
                    }
                }
                if pins.is_empty() {
                    node_object.remove("pins");
                }
                Ok(())
            }
            StreamGraphOperation::RenameNode { node, to, from } => {
                if let Some(holder) = self.node_objects().find(|node_object| {
                    node_name_of(node_object) == Some(to.as_str())
                        && node_uuid_of(node_object) != Some(*node)
                }) {
                    return Err(format!(
                        "node {node} cannot be named `{to}`: node {} has that name",
                        node_uuid_of(holder).map_or_else(String::new, |uuid| uuid.to_string())
                    ));
                }
                self.node_object_mut(*node)?
                    .insert("name".to_owned(), Value::String(to.clone()));
                let node_text = node.to_string();
                for link in self.link_values_mut() {
                    for end_key in ["source", "target"] {
                        if let Some(Value::Object(end)) = link.get_mut(end_key)
                            && end.get("node").and_then(Value::as_str) == Some(from.as_str())
                        {
                            end.insert("node".to_owned(), Value::String(node_text.clone()));
                        }
                    }
                }
                Ok(())
            }
            StreamGraphOperation::SetStudioFields { node, to, .. } => {
                let node_object = self.node_object_mut(*node)?;
                if to.is_empty() {
                    node_object.remove("studio");
                } else {
                    node_object.insert("studio".to_owned(), Value::Object(to.to_map()));
                }
                Ok(())
            }
            StreamGraphOperation::RefreshFragment { node, to, .. } => {
                let node_object = self.node_object_mut(*node)?;
                if !node_object.contains_key("fragment") {
                    return Err(format!(
                        "node {node} `{}` is no fragment, so no graph generator's run refreshes \
                         it",
                        node_name_of(node_object).unwrap_or_default()
                    ));
                }
                node_object.insert("type".to_owned(), Value::String(to.generator.clone()));
                node_object.insert("config".to_owned(), to.config.clone());
                node_object.insert("fragment".to_owned(), to.fragment.clone());
                Ok(())
            }
            StreamGraphOperation::SetExposedConfigFields { node, to, .. } => {
                let node_object = self.node_object_mut(*node)?;
                if to.is_empty() {
                    node_object.remove("exposed_config_fields");
                } else {
                    node_object.insert(
                        "exposed_config_fields".to_owned(),
                        Value::Array(to.iter().cloned().map(Value::String).collect()),
                    );
                }
                Ok(())
            }
            StreamGraphOperation::AddGroup { group, nodes } => {
                let mut groups = self.groups()?;
                if groups.iter().any(|held| held.id == group.id) {
                    return Err(format!(
                        "the graph already holds group {}; a group's UUID is never reused",
                        group.id
                    ));
                }
                for node in nodes {
                    let node_object = self
                        .node_objects()
                        .find(|node_object| node_uuid_of(node_object) == Some(*node))
                        .ok_or_else(|| self.why_no_node_has(*node))?;
                    if let Some(holder) = studio_group_of(node_object)
                        .and_then(|held| groups.iter().find(|group| group.id == held))
                    {
                        return Err(format!(
                            "node {node} `{}` already belongs to group {} `{}`; take it out of                              that group with set_studio_fields first",
                            node_name_of(node_object).unwrap_or_default(),
                            holder.id,
                            holder.fields.title
                        ));
                    }
                }
                groups.push(group.clone());
                self.set_the_groups(&groups)?;
                for node in nodes {
                    self.set_the_studio_group_of(*node, Some(group.id))?;
                }
                Ok(())
            }
            StreamGraphOperation::SetGroupFields { group, to, .. } => {
                let mut groups = self.groups()?;
                self.group(*group)?;
                for held in &mut groups {
                    if held.id == *group {
                        held.fields = to.clone();
                    }
                }
                self.set_the_groups(&groups)
            }
            StreamGraphOperation::RemoveGroup { group, .. } => {
                let mut groups = self.groups()?;
                self.group(group.id)?;
                groups.retain(|held| held.id != group.id);
                self.set_the_groups(&groups)?;
                for node in self.nodes_of_the_group(group.id) {
                    self.set_the_studio_group_of(node, None)?;
                }
                Ok(())
            }
            StreamGraphOperation::AddNote { note } => {
                let mut notes = self.notes()?;
                if notes.iter().any(|held| held.id == note.id) {
                    return Err(format!(
                        "the graph already holds note {}; a note's UUID is never reused",
                        note.id
                    ));
                }
                notes.push(note.clone());
                self.set_the_notes(&notes)
            }
            StreamGraphOperation::SetNoteFields { note, to, .. } => {
                self.note(*note)?;
                let mut notes = self.notes()?;
                for held in &mut notes {
                    if held.id == *note {
                        held.fields = to.clone();
                    }
                }
                self.set_the_notes(&notes)
            }
            StreamGraphOperation::RemoveNote { note } => {
                self.note(note.id)?;
                let mut notes = self.notes()?;
                notes.retain(|held| held.id != note.id);
                self.set_the_notes(&notes)
            }
        }
    }

    fn connect(&mut self, link: &StreamGraphDocumentLink) -> Result<(), String> {
        for end in [&link.source, &link.target] {
            self.node(end.node)?;
        }
        if self.links().contains(link) {
            return Err(format!(
                "the graph already links {}/{} -> {}/{}",
                link.source.node, link.source.port, link.target.node, link.target.port
            ));
        }
        let link_value = serde_json::to_value(link)
            .map_err(|not_written| format!("the link does not write: {not_written}"))?;
        self.link_values_mut().push(link_value);
        Ok(())
    }

    pub(crate) fn why_no_node_has(&self, node: Uuid) -> String {
        let held: Vec<String> = self
            .node_objects()
            .map(|node_object| {
                format!(
                    "{} `{}`",
                    node_uuid_of(node_object)
                        .map_or_else(|| "no UUID".to_owned(), |uuid| uuid.to_string()),
                    node_name_of(node_object).unwrap_or_default()
                )
            })
            .collect();
        if held.is_empty() {
            format!("the graph holds no node {node}; it holds no node at all")
        } else {
            format!(
                "the graph holds no node {node}. It holds: {}",
                held.join(", ")
            )
        }
    }

    pub(crate) fn node_objects(&self) -> impl Iterator<Item = &Map<String, Value>> {
        self.node_values.iter().filter_map(Value::as_object)
    }

    fn node_object_mut(&mut self, node: Uuid) -> Result<&mut Map<String, Value>, String> {
        let why_none = self.why_no_node_has(node);
        self.node_values_mut()
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|node_object| node_uuid_of(node_object) == Some(node))
            .ok_or(why_none)
    }

    pub(crate) fn node_values_mut(&mut self) -> &mut Vec<Value> {
        &mut self.node_values
    }

    pub(crate) fn node_values_of_the_level(&self) -> &[Value] {
        &self.node_values
    }

    pub(crate) fn link_values(&self) -> &[Value] {
        &self.link_values
    }

    pub(crate) fn link_values_mut(&mut self) -> &mut Vec<Value> {
        &mut self.link_values
    }

    /// `link` with each end's node by UUID, an end naming its node by name resolved through
    /// the nodes the document holds.
    pub(crate) fn link_read_from(&self, link: &Value) -> Option<StreamGraphDocumentLink> {
        let end = |end_key: &str| -> Option<StreamGraphDocumentLinkEnd> {
            let end = link.get(end_key)?;
            let named = end.get("node")?.as_str()?;
            let node = Uuid::parse_str(named).ok().or_else(|| {
                self.node_objects()
                    .find(|node_object| node_name_of(node_object) == Some(named))
                    .and_then(node_uuid_of)
            })?;
            Some(StreamGraphDocumentLinkEnd {
                node,
                port: end.get("port")?.as_str()?.to_owned(),
            })
        };
        Some(StreamGraphDocumentLink {
            source: end("source")?,
            target: end("target")?,
        })
    }
}

pub(crate) fn node_uuid_of(node_object: &Map<String, Value>) -> Option<Uuid> {
    node_object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn studio_group_of(node_object: &Map<String, Value>) -> Option<Uuid> {
    node_object
        .get("studio")?
        .get(STUDIO_GROUP_KEY)?
        .as_str()
        .and_then(|group| Uuid::parse_str(group).ok())
}

pub(crate) fn node_name_of(node_object: &Map<String, Value>) -> Option<&str> {
    node_object.get("name").and_then(Value::as_str)
}

fn link_end_names_the_node(end: &Value, node_text: &str, node_name: &str) -> bool {
    end.get("node")
        .and_then(Value::as_str)
        .is_some_and(|named| named == node_text || named == node_name)
}

/// `fresh_fragment`, a graph generator's output naming its nodes and link ends by name, with
/// each node given a UUID and each link end naming its node by it: the UUID the node of the
/// same name holds in `previous_fragment`, at the same level of nested fragments, and a new
/// one from `new_node_uuid` for a node no earlier run built.
pub fn fragment_with_each_node_uuid_kept_by_its_name(
    fresh_fragment: &Value,
    previous_fragment: Option<&Value>,
    new_node_uuid: &mut dyn FnMut() -> Uuid,
) -> Result<Value, String> {
    let Value::Object(fresh_level) = fresh_fragment else {
        return Err("a fragment is a JSON object of `nodes` and `links`".to_owned());
    };
    let previous_node_of_the_name = |name: &str| -> Option<&Map<String, Value>> {
        previous_fragment?
            .get("nodes")?
            .as_array()?
            .iter()
            .filter_map(Value::as_object)
            .find(|previous_node| node_name_of(previous_node) == Some(name))
    };
    let mut kept_level = fresh_level.clone();
    let mut uuid_of_the_name: Map<String, Value> = Map::new();
    let mut nodes = Vec::new();
    for fresh_node in fresh_level
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Value::Object(fresh_node) = fresh_node else {
            return Err("a fragment's node is a JSON object".to_owned());
        };
        let name = node_name_of(fresh_node)
            .ok_or("a fragment's node holds its `name`")?
            .to_owned();
        let previous_node = previous_node_of_the_name(&name);
        let node_uuid = previous_node
            .and_then(node_uuid_of)
            .unwrap_or_else(&mut *new_node_uuid);
        let mut kept_node = fresh_node.clone();
        kept_node.insert("id".to_owned(), Value::String(node_uuid.to_string()));
        if let Some(inner_fragment) = fresh_node.get("fragment") {
            kept_node.insert(
                "fragment".to_owned(),
                fragment_with_each_node_uuid_kept_by_its_name(
                    inner_fragment,
                    previous_node.and_then(|previous_node| previous_node.get("fragment")),
                    new_node_uuid,
                )?,
            );
        }
        uuid_of_the_name.insert(name, Value::String(node_uuid.to_string()));
        nodes.push(Value::Object(kept_node));
    }
    let mut links = Vec::new();
    for fresh_link in fresh_level
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut kept_link = fresh_link.clone();
        for end_key in ["source", "target"] {
            if let Some(Value::Object(end)) = kept_link.get_mut(end_key)
                && let Some(node_uuid) = end
                    .get("node")
                    .and_then(Value::as_str)
                    .and_then(|named| uuid_of_the_name.get(named))
                    .cloned()
            {
                end.insert("node".to_owned(), node_uuid);
            }
        }
        links.push(kept_link);
    }
    if let Some(fresh_groups) = fresh_level.get(GROUPS_KEY) {
        let groups = groups_with_each_uuid_kept_by_its_title(
            fresh_groups,
            previous_fragment.and_then(|previous| previous.get(GROUPS_KEY)),
            &uuid_of_the_name,
            &mut nodes,
            new_node_uuid,
        )?;
        kept_level.insert(GROUPS_KEY.to_owned(), Value::Array(groups));
    }
    kept_level.insert("nodes".to_owned(), Value::Array(nodes));
    kept_level.insert("links".to_owned(), Value::Array(links));
    Ok(Value::Object(kept_level))
}

/// `fresh_groups`, the groups a graph generator made of one level's nodes, each naming its
/// nodes by name, as the document holds them: each given the UUID the group of the same title
/// holds in `previous_groups`, or a new one from `new_group_uuid`, and each node it names given
/// that UUID as its `studio.group` in `nodes`.
fn groups_with_each_uuid_kept_by_its_title(
    fresh_groups: &Value,
    previous_groups: Option<&Value>,
    uuid_of_the_name: &Map<String, Value>,
    nodes: &mut [Value],
    new_group_uuid: &mut dyn FnMut() -> Uuid,
) -> Result<Vec<Value>, String> {
    let previous_groups: Vec<StreamGraphDocumentGroup> = previous_groups
        .and_then(|previous| serde_json::from_value(previous.clone()).ok())
        .unwrap_or_default();
    let mut groups = Vec::new();
    for fresh_group in fresh_groups
        .as_array()
        .ok_or("a fragment's `groups` is an array")?
    {
        let fields: StreamGraphGroupFields = serde_json::from_value(Value::Object(
            fresh_group
                .as_object()
                .ok_or("a fragment's group is a JSON object")?
                .iter()
                .filter(|(key, _)| key.as_str() != "nodes")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ))
        .map_err(|not_read| format!("a fragment's group does not read: {not_read}"))?;
        let group_uuid = previous_groups
            .iter()
            .find(|previous| previous.fields.title == fields.title)
            .map_or_else(&mut *new_group_uuid, |previous| previous.id);
        for named in fresh_group["nodes"].as_array().into_iter().flatten() {
            let node_name = named.as_str().unwrap_or_default();
            let node_uuid = uuid_of_the_name.get(node_name).ok_or_else(|| {
                format!(
                    "group `{}` names node `{node_name}`, which the fragment does not hold",
                    fields.title
                )
            })?;
            let node_object = nodes
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .find(|node_object| node_object.get("id") == Some(node_uuid))
                .ok_or_else(|| format!("the fragment holds no node {node_uuid}"))?;
            let studio = node_object
                .entry("studio")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(studio) = studio {
                studio.insert(
                    STUDIO_GROUP_KEY.to_owned(),
                    Value::String(group_uuid.to_string()),
                );
            }
        }
        groups.push(
            serde_json::to_value(StreamGraphDocumentGroup {
                id: group_uuid,
                fields,
            })
            .map_err(|not_written| format!("a group does not write: {not_written}"))?,
        );
    }
    Ok(groups)
}

/// The `type` of a fragment node that references a stream, this project's or an import's, whose
/// graph each load reads from that stream's document.
pub const STREAM_FRAGMENT_NODE_TYPE: &str = "tatolab.project:StreamFragment";

/// The namespace of the node types the runtime itself defines, none of which is a graph
/// generator, so none holds an inline graph of its own.
const NODE_TYPE_NAMESPACE_THE_RUNTIME_DEFINES: &str = "tatolab.";

/// Refused for a node at any level of `node_values` holding an inline `fragment` under a node
/// type the runtime defines: only a graph generator's frozen output is held inline, and a
/// subgraph is a stream of the project a stream fragment places.
fn refuse_a_node_of_a_tatolab_type_holding_an_inline_graph(
    node_values: &[Value],
) -> Result<(), String> {
    for node_object in node_values.iter().filter_map(Value::as_object) {
        let Some(fragment) = node_object.get("fragment") else {
            continue;
        };
        let node_type = node_object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if node_type.starts_with(NODE_TYPE_NAMESPACE_THE_RUNTIME_DEFINES)
            && node_type != STREAM_FRAGMENT_NODE_TYPE
        {
            return Err(format!(
                "node {} `{}` of type `{node_type}` holds an inline subgraph, which only a graph \
                 generator's output holds; a subgraph is a stream of the project placed with a \
                 `{STREAM_FRAGMENT_NODE_TYPE}` node",
                node_object
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                node_name_of(node_object).unwrap_or_default()
            ));
        }
        if let Some(inner_nodes) = fragment.get("nodes").and_then(Value::as_array) {
            refuse_a_node_of_a_tatolab_type_holding_an_inline_graph(inner_nodes)?;
        }
    }
    Ok(())
}

/// The config key of a stream's fragment node naming the stream it holds.
pub const STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: &str = "stream";

/// Make each reference in `value` to an asset of the project it was written in,
/// `{"asset": "<uuid>"}`, a reference into that project as an import, adding
/// `"from": "<project>"`; a reference already naming a project is left as it is.
pub fn asset_references_made_references_into_the_import(value: &mut Value, project: Uuid) {
    match value {
        Value::Object(object) => {
            let names_an_asset = object
                .get("asset")
                .and_then(Value::as_str)
                .is_some_and(|asset| Uuid::parse_str(asset).is_ok());
            if names_an_asset {
                object
                    .entry("from")
                    .or_insert_with(|| Value::String(project.to_string()));
            }
            for entry in object.values_mut() {
                asset_references_made_references_into_the_import(entry, project);
            }
        }
        Value::Array(items) => {
            for item in items {
                asset_references_made_references_into_the_import(item, project);
            }
        }
        _ => {}
    }
}

fn push_the_config_of_each_node_at_every_level<'fragment>(
    fragment: &'fragment Value,
    configs: &mut Vec<(Uuid, &'fragment Value)>,
) {
    if let Some(nodes) = fragment.get("nodes").and_then(Value::as_array) {
        push_the_config_of_each_of_the_nodes_at_every_level(nodes, configs);
    }
}

fn push_the_config_of_each_of_the_nodes_at_every_level<'fragment>(
    nodes: &'fragment [Value],
    configs: &mut Vec<(Uuid, &'fragment Value)>,
) {
    for node in nodes.iter().filter_map(Value::as_object) {
        if let (Some(node_uuid), Some(config)) = (node_uuid_of(node), node.get("config")) {
            configs.push((node_uuid, config));
        }
        if let Some(inner_fragment) = node.get("fragment") {
            push_the_config_of_each_node_at_every_level(inner_fragment, configs);
        }
    }
}

/// Every asset of this project a node's config references, `{"asset": "<uuid>"}`, by UUID;
/// a reference naming another project with `from` is that project's to check.
pub fn assets_a_node_config_references(config: &Value) -> Vec<Uuid> {
    let mut referenced = Vec::new();
    collect_asset_references(config, &mut referenced);
    referenced
}

fn collect_asset_references(value: &Value, referenced: &mut Vec<Uuid>) {
    match value {
        Value::Object(object) => {
            if !object.contains_key("from")
                && let Some(asset) = object
                    .get("asset")
                    .and_then(Value::as_str)
                    .and_then(|asset| Uuid::parse_str(asset).ok())
            {
                referenced.push(asset);
            }
            for entry in object.values() {
                collect_asset_references(entry, referenced);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_asset_references(item, referenced);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_asset_reference_nested_in_another_is_made_an_import_reference_too() {
        let project = Uuid::new_v4();
        let outer = Uuid::new_v4().to_string();
        let inner = Uuid::new_v4().to_string();
        let mut config = serde_json::json!({ "asset": outer, "lut": { "asset": inner } });

        asset_references_made_references_into_the_import(&mut config, project);

        assert_eq!(
            config,
            serde_json::json!({
                "asset": outer, "from": project.to_string(),
                "lut": { "asset": inner, "from": project.to_string() },
            })
        );
    }

    use serde_json::json;

    use super::*;

    fn a_node(name: &str) -> StreamGraphDocumentNode {
        StreamGraphDocumentNode {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            node_type: "nodes.effect:Effect".to_owned(),
            config: json!({}),
            pins: Map::new(),
            exposed_config_fields: Vec::new(),
            studio: Default::default(),
            fragment: None,
        }
    }

    fn a_link(
        source: &StreamGraphDocumentNode,
        target: &StreamGraphDocumentNode,
    ) -> StreamGraphDocumentLink {
        StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: source.id,
                port: "video".to_owned(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: target.id,
                port: "video_in".to_owned(),
            },
        }
    }

    fn an_empty_document() -> StreamGraphDocument {
        StreamGraphDocument::from_value(json!({ "asset": Uuid::new_v4(), "nodes": [] })).unwrap()
    }

    #[test]
    fn every_operation_and_its_inverse_leave_the_document_as_it_was() {
        let mut document = an_empty_document();
        let source = a_node("source");
        let sink = a_node("sink");
        for node in [&source, &sink] {
            document
                .apply(&StreamGraphOperation::AddNode {
                    node: node.clone(),
                    links: Vec::new(),
                })
                .unwrap();
        }
        let link = a_link(&source, &sink);
        let operations = [
            StreamGraphOperation::Connect { link: link.clone() },
            StreamGraphOperation::SetNodeConfig {
                node: sink.id,
                from: json!({}),
                to: json!({"gain": 2}),
            },
            StreamGraphOperation::SetPinValue {
                node: sink.id,
                input: "threshold".to_owned(),
                from: None,
                to: Some(json!(0.5)),
            },
            StreamGraphOperation::RenameNode {
                node: sink.id,
                from: "sink".to_owned(),
                to: "meter".to_owned(),
            },
            StreamGraphOperation::SetStudioFields {
                node: sink.id,
                from: Default::default(),
                to: Box::new(
                    StreamGraphNodeStudioFields::try_from(
                        json!({"position": [10, 20]}).as_object().unwrap().clone(),
                    )
                    .unwrap(),
                ),
            },
            StreamGraphOperation::SetExposedConfigFields {
                node: sink.id,
                from: Vec::new(),
                to: vec!["gain".to_owned()],
            },
        ];
        for operation in &operations {
            let before = document.clone();
            document.apply(operation).unwrap();
            assert_ne!(document, before, "{operation:?} changed nothing");
            let after = document.clone();
            document.apply(&operation.inverse()).unwrap();
            assert_eq!(
                document.canonical_bytes().unwrap(),
                before.canonical_bytes().unwrap(),
                "{operation:?}"
            );
            document = after;
        }
        let removal = StreamGraphOperation::RemoveNode {
            node: document.node(sink.id).unwrap(),
            links: document.links_touching(sink.id),
        };
        let before = document.clone();
        document.apply(&removal).unwrap();
        assert!(document.links().is_empty());
        document.apply(&removal.inverse()).unwrap();
        assert_eq!(
            document.node(sink.id).unwrap(),
            before.node(sink.id).unwrap()
        );
        assert_eq!(document.links(), before.links());
    }

    fn a_look_fragment_run(blur_radius: u32, with_tint: bool) -> Value {
        let mut nodes = vec![
            json!({"name": "graphinput", "type": "tatolab.stream:GraphInput",
                   "config": {"port": "video", "family": "packets"}}),
            json!({"name": "soft", "type": "generators.soft:soft", "config": {},
            "fragment": {
                "nodes": [{"name": "blur", "type": "nodes.blur:Blur",
                           "config": {"radius": blur_radius}}],
                "links": [],
            }}),
        ];
        if with_tint {
            nodes.push(json!({"name": "tint", "type": "nodes.tint:Tint", "config": {}}));
        }
        json!({
            "nodes": nodes,
            "links": [
                {"source": {"node": "graphinput", "port": "value"},
                 "target": {"node": "soft", "port": "video"}},
            ],
        })
    }

    fn uuid_of_the_node_at(fragment: &Value, path: &[&str]) -> Uuid {
        let (name, inner_path) = path.split_first().unwrap();
        let node = fragment["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["name"] == *name)
            .unwrap();
        if inner_path.is_empty() {
            Uuid::parse_str(node["id"].as_str().unwrap()).unwrap()
        } else {
            uuid_of_the_node_at(&node["fragment"], inner_path)
        }
    }

    #[test]
    fn a_rerun_keeps_the_uuid_of_each_node_whose_name_is_unchanged_at_every_level() {
        let first_run = fragment_with_each_node_uuid_kept_by_its_name(
            &a_look_fragment_run(4, false),
            None,
            &mut Uuid::new_v4,
        )
        .unwrap();
        let second_run = fragment_with_each_node_uuid_kept_by_its_name(
            &a_look_fragment_run(8, true),
            Some(&first_run),
            &mut Uuid::new_v4,
        )
        .unwrap();

        for path in [&["graphinput"][..], &["soft"], &["soft", "blur"]] {
            assert_eq!(
                uuid_of_the_node_at(&second_run, path),
                uuid_of_the_node_at(&first_run, path),
                "{path:?}"
            );
        }
        assert_eq!(
            second_run["nodes"][1]["fragment"]["nodes"][0]["config"]["radius"],
            8
        );
        let tint = uuid_of_the_node_at(&second_run, &["tint"]);
        assert!(
            ![&["graphinput"][..], &["soft"], &["soft", "blur"]]
                .iter()
                .any(|path| uuid_of_the_node_at(&first_run, path) == tint)
        );
        assert_eq!(
            second_run["links"][0]["source"]["node"],
            json!(uuid_of_the_node_at(&first_run, &["graphinput"]).to_string())
        );
        assert_eq!(
            second_run["links"][0]["target"]["node"],
            json!(uuid_of_the_node_at(&first_run, &["soft"]).to_string())
        );
    }

    #[test]
    fn a_fragment_refreshed_and_refreshed_back_is_the_document_it_was() {
        let mut document = an_empty_document();
        let first_run = fragment_with_each_node_uuid_kept_by_its_name(
            &a_look_fragment_run(4, false),
            None,
            &mut Uuid::new_v4,
        )
        .unwrap();
        let mut look = a_node("look");
        look.node_type = "generators.look:look".to_owned();
        look.fragment = Some(Box::new(first_run.clone()));
        document
            .apply(&StreamGraphOperation::AddNode {
                node: look.clone(),
                links: Vec::new(),
            })
            .unwrap();
        let refresh = StreamGraphOperation::RefreshFragment {
            node: look.id,
            from: StreamGraphFragmentPlacement {
                generator: look.node_type.clone(),
                config: json!({}),
                fragment: first_run.clone(),
            },
            to: StreamGraphFragmentPlacement {
                generator: look.node_type.clone(),
                config: json!({"radius": 8}),
                fragment: fragment_with_each_node_uuid_kept_by_its_name(
                    &a_look_fragment_run(8, true),
                    Some(&first_run),
                    &mut Uuid::new_v4,
                )
                .unwrap(),
            },
        };
        let before = document.canonical_bytes().unwrap();

        document.apply(&refresh).unwrap();
        assert_eq!(document.node(look.id).unwrap().config, json!({"radius": 8}));
        document.apply(&refresh.inverse()).unwrap();

        assert_eq!(document.canonical_bytes().unwrap(), before);
    }

    #[test]
    fn a_refresh_of_a_node_holding_no_fragment_is_refused_naming_it() {
        let mut document = an_empty_document();
        let blur = a_node("blur");
        document
            .apply(&StreamGraphOperation::AddNode {
                node: blur.clone(),
                links: Vec::new(),
            })
            .unwrap();
        let placement = StreamGraphFragmentPlacement {
            generator: "generators.look:look".to_owned(),
            config: json!({}),
            fragment: json!({"nodes": [], "links": []}),
        };

        let refusal = document
            .apply(&StreamGraphOperation::RefreshFragment {
                node: blur.id,
                from: placement.clone(),
                to: placement,
            })
            .unwrap_err();

        assert_eq!(
            refusal,
            format!(
                "node {} `blur` is no fragment, so no graph generator's run refreshes it",
                blur.id
            )
        );
    }

    #[test]
    fn an_operation_naming_what_the_document_does_not_hold_is_refused_naming_it() {
        let mut document = an_empty_document();
        let held = a_node("held");
        document
            .apply(&StreamGraphOperation::AddNode {
                node: held.clone(),
                links: Vec::new(),
            })
            .unwrap();
        let absent = Uuid::new_v4();
        let refusal = document
            .apply(&StreamGraphOperation::RenameNode {
                node: absent,
                from: "a".to_owned(),
                to: "b".to_owned(),
            })
            .unwrap_err();
        assert!(refusal.contains(&absent.to_string()), "{refusal}");
        assert!(refusal.contains(&held.id.to_string()), "{refusal}");
        let refusal = document
            .apply(&StreamGraphOperation::AddNode {
                node: held.clone(),
                links: Vec::new(),
            })
            .unwrap_err();
        assert!(refusal.contains("never reused"), "{refusal}");
        let mut same_name = a_node("held");
        same_name.id = Uuid::new_v4();
        let refusal = document
            .apply(&StreamGraphOperation::AddNode {
                node: same_name,
                links: Vec::new(),
            })
            .unwrap_err();
        assert!(refusal.contains("has that name"), "{refusal}");
    }

    #[test]
    fn a_link_end_naming_its_node_by_name_reads_as_its_uuid() {
        let source = a_node("source");
        let sink = a_node("sink");
        let document = StreamGraphDocument::from_value(json!({
            "nodes": [source, sink],
            "links": [{
                "source": {"node": "source", "port": "video"},
                "target": {"node": sink.id, "port": "video_in"},
            }],
        }))
        .unwrap();
        assert_eq!(document.links(), [a_link(&source, &sink)]);
    }

    #[test]
    fn a_config_references_its_projects_assets_and_leaves_another_projects_to_it() {
        let image = Uuid::new_v4();
        let lut = Uuid::new_v4();
        let config = json!({
            "image": {"asset": image},
            "grading": [{"lut": {"asset": lut}}],
            "font": {"asset": Uuid::new_v4(), "from": Uuid::new_v4()},
            "label": "asset",
        });
        assert_eq!(assets_a_node_config_references(&config), [image, lut]);
    }

    #[test]
    fn an_operation_records_with_its_operation_tag() {
        let node = a_node("source");
        let recorded = serde_json::to_value(StreamGraphOperation::SetNodeConfig {
            node: node.id,
            from: json!({}),
            to: json!({"gain": 1}),
        })
        .unwrap();
        assert_eq!(recorded["operation"], "set_node_config");
        assert_eq!(
            serde_json::from_value::<StreamGraphOperation>(recorded)
                .unwrap()
                .node(),
            Some(node.id)
        );
    }

    #[test]
    fn a_fragment_refresh_runs_its_own_generator_and_each_nested_ones() {
        let document = StreamGraphDocument::from_value(json!({
            "nodes": [
                { "id": Uuid::new_v4(), "name": "camera", "type": "nodes.camera:Camera" },
                {
                    "id": Uuid::new_v4(), "name": "look", "type": "generators.looks:look",
                    "fragment": { "nodes": [
                        { "id": Uuid::new_v4(), "name": "blur", "type": "nodes.blur:Blur" },
                        {
                            "id": Uuid::new_v4(), "name": "soft", "type": "generators.soft:soft",
                            "fragment": { "nodes": [], "links": [] },
                        },
                    ], "links": [] },
                },
            ],
            "links": [],
        }))
        .unwrap();

        let fragment_nodes = document.fragment_nodes();

        assert_eq!(fragment_nodes.len(), 1);
        assert_eq!(
            fragment_nodes[0].graph_generators_a_refresh_runs(),
            ["generators.looks:look", "generators.soft:soft"]
        );
    }

    fn a_group_titled(title: &str) -> StreamGraphDocumentGroup {
        StreamGraphDocumentGroup {
            id: Uuid::new_v4(),
            fields: StreamGraphGroupFields {
                title: title.to_owned(),
                description: "what these nodes do".to_owned(),
                colour: "#3b82f6".to_owned(),
            },
        }
    }

    fn a_document_holding(nodes: &[&StreamGraphDocumentNode]) -> StreamGraphDocument {
        let mut document = an_empty_document();
        for node in nodes {
            document
                .apply(&StreamGraphOperation::AddNode {
                    node: (*node).clone(),
                    links: Vec::new(),
                })
                .unwrap();
        }
        document
    }

    #[test]
    fn a_group_operation_and_its_inverse_leave_the_document_as_it_was() {
        let source = a_node("source");
        let sink = a_node("sink");
        let mut document = a_document_holding(&[&source, &sink]);
        document
            .apply(&StreamGraphOperation::SetStudioFields {
                node: sink.id,
                from: Default::default(),
                to: Box::new(
                    StreamGraphNodeStudioFields::try_from(
                        json!({"position": [10, 20]}).as_object().unwrap().clone(),
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        let sources = a_group_titled("sources");
        let operations = [
            StreamGraphOperation::AddGroup {
                group: sources.clone(),
                nodes: vec![source.id, sink.id],
            },
            StreamGraphOperation::SetGroupFields {
                group: sources.id,
                from: sources.fields.clone(),
                to: StreamGraphGroupFields {
                    title: "every source".to_owned(),
                    description: String::new(),
                    colour: "#22c55e".to_owned(),
                },
            },
        ];
        for operation in &operations {
            let before = document.clone();
            document.apply(operation).unwrap();
            assert_ne!(document, before, "{operation:?} changed nothing");
            let after = document.clone();
            document.apply(&operation.inverse()).unwrap();
            assert_eq!(
                document.canonical_bytes().unwrap(),
                before.canonical_bytes().unwrap(),
                "{operation:?}"
            );
            document = after;
        }

        let removal = StreamGraphOperation::RemoveGroup {
            group: document.group(sources.id).unwrap(),
            nodes: document.nodes_of_the_group(sources.id),
        };
        let before = document.clone();
        document.apply(&removal).unwrap();
        assert!(document.groups().unwrap().is_empty());
        assert!(document.nodes_of_the_group(sources.id).is_empty());
        assert_eq!(
            *document.node(sink.id).unwrap().studio,
            StreamGraphNodeStudioFields::try_from(
                json!({"position": [10, 20]}).as_object().unwrap().clone()
            )
            .unwrap()
        );
        assert!(document.node(source.id).unwrap().studio.is_empty());
        document.apply(&removal.inverse()).unwrap();
        assert_eq!(
            document.canonical_bytes().unwrap(),
            before.canonical_bytes().unwrap()
        );
        assert_eq!(
            document.nodes_of_the_group(sources.id),
            vec![source.id, sink.id]
        );
    }

    #[test]
    fn a_node_of_one_group_is_refused_by_another_naming_the_group_it_belongs_to() {
        let source = a_node("source");
        let mut document = a_document_holding(&[&source]);
        let sources = a_group_titled("sources");
        document
            .apply(&StreamGraphOperation::AddGroup {
                group: sources.clone(),
                nodes: vec![source.id],
            })
            .unwrap();
        let before = document.clone();

        let refusal = document
            .apply(&StreamGraphOperation::AddGroup {
                group: a_group_titled("inputs"),
                nodes: vec![source.id],
            })
            .unwrap_err();

        assert!(refusal.contains(&sources.id.to_string()), "{refusal}");
        assert!(refusal.contains("`sources`"), "{refusal}");
        assert_eq!(document, before);
    }

    #[test]
    fn a_studio_group_naming_no_group_reads_as_no_member_of_any() {
        let document = StreamGraphDocument::from_value(json!({
            "nodes": [{ "id": Uuid::new_v4(), "name": "camera", "type": "nodes.camera:Camera",
                        "studio": { "group": "sources" } }],
        }))
        .unwrap();
        let group = a_group_titled("sources");

        assert!(document.groups().unwrap().is_empty());
        assert!(document.nodes_of_the_group(group.id).is_empty());
        assert!(
            document
                .group(group.id)
                .unwrap_err()
                .contains("no group at all")
        );
    }

    #[test]
    fn a_generators_group_takes_a_uuid_kept_by_its_title_and_marks_each_node_it_names() {
        let a_run_grouping = |title: &str| {
            json!({
                "nodes": [
                    {"name": "blur", "type": "nodes.blur:Blur", "config": {}},
                    {"name": "tint", "type": "nodes.tint:Tint", "config": {}},
                ],
                "links": [],
                "groups": [{"title": title, "colour": "#f97316", "nodes": ["blur", "tint"]}],
            })
        };
        let first_run = fragment_with_each_node_uuid_kept_by_its_name(
            &a_run_grouping("soften"),
            None,
            &mut Uuid::new_v4,
        )
        .unwrap();
        let group_uuid = first_run["groups"][0]["id"].clone();

        assert_eq!(
            first_run["groups"],
            json!([{"id": group_uuid, "title": "soften", "colour": "#f97316"}])
        );
        for node in first_run["nodes"].as_array().unwrap() {
            assert_eq!(node["studio"]["group"], group_uuid);
        }
        let rerun = fragment_with_each_node_uuid_kept_by_its_name(
            &a_run_grouping("soften"),
            Some(&first_run),
            &mut Uuid::new_v4,
        )
        .unwrap();
        assert_eq!(rerun, first_run);
        let retitled = fragment_with_each_node_uuid_kept_by_its_name(
            &a_run_grouping("blur then tint"),
            Some(&first_run),
            &mut Uuid::new_v4,
        )
        .unwrap();
        assert_ne!(retitled["groups"][0]["id"], group_uuid);
    }

    #[test]
    fn a_generators_group_naming_a_node_the_fragment_does_not_hold_is_refused_naming_both() {
        let refusal = fragment_with_each_node_uuid_kept_by_its_name(
            &json!({
                "nodes": [{"name": "blur", "type": "nodes.blur:Blur"}],
                "groups": [{"title": "soften", "nodes": ["glow"]}],
            }),
            None,
            &mut Uuid::new_v4,
        )
        .unwrap_err();

        assert!(refusal.contains("`soften`"), "{refusal}");
        assert!(refusal.contains("`glow`"), "{refusal}");
    }
}
