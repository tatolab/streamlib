// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::project_stream_document::STREAM_DOCUMENT_DISPLAY_NAME_KEY;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::stream_document_upgrade::{
    NODE_INPUT_VALUES_KEY, fold_the_retired_fields_into, put_input_values,
    stream_document_in_the_current_shape, the_retired_field_holding_no_object,
    upgrade_a_node_in_place, upgrade_stream_document_in_place,
};
use crate::stream_graph_studio_contract::{
    StreamGraphDocumentNote, StreamGraphNodeStudioFields, a_documents_studio_holding,
    the_notes_of_a_documents_studio,
};

mod fragment_node_uuids_kept_by_name;
mod input_values_and_asset_references_at_every_level;
mod stream_graph_operation;

pub use input_values_and_asset_references_at_every_level::{
    asset_references_made_references_into_the_import, assets_a_nodes_input_values_reference,
};
use input_values_and_asset_references_at_every_level::{
    push_the_input_values_of_each_node_at_every_level,
    push_the_input_values_of_each_of_the_nodes_at_every_level,
};

pub use fragment_node_uuids_kept_by_name::fragment_with_each_node_uuid_kept_by_its_name;
pub use stream_graph_operation::StreamGraphOperation;

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

/// One node as a stream's graph document holds it: the spec keys a load reads. A node an older
/// runtime recorded, with `config` and `pins`, reads with them folded into `input_values`, and
/// one holding either as anything but an object is refused naming the node and the key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StreamGraphDocumentNodeAsRecorded")]
pub struct StreamGraphDocumentNode {
    /// The node's UUID, the runtime's from its placement on.
    pub id: Uuid,
    /// The node's name, a label, cast as the runtime casts it.
    pub name: String,
    /// The import path of the node's type.
    #[serde(rename = "type")]
    pub node_type: String,
    /// The value the node holds for each input, by input name; an input it names no value for
    /// reads its declared default.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub input_values: Map<String, Value>,
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

/// A node as any runtime recorded it, before its retired fields are folded into `input_values`.
#[derive(Deserialize)]
struct StreamGraphDocumentNodeAsRecorded {
    id: Uuid,
    name: String,
    #[serde(rename = "type")]
    node_type: String,
    #[serde(default)]
    input_values: Map<String, Value>,
    #[serde(default)]
    pins: Option<Value>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    exposed_config_fields: Vec<String>,
    #[serde(default)]
    studio: Box<StreamGraphNodeStudioFields>,
    #[serde(default)]
    fragment: Option<Box<Value>>,
}

impl TryFrom<StreamGraphDocumentNodeAsRecorded> for StreamGraphDocumentNode {
    type Error = String;

    fn try_from(recorded: StreamGraphDocumentNodeAsRecorded) -> Result<Self, String> {
        let StreamGraphDocumentNodeAsRecorded {
            id,
            name,
            node_type,
            mut input_values,
            pins,
            config,
            exposed_config_fields,
            studio,
            fragment,
        } = recorded;
        if let Some(key) = the_retired_field_holding_no_object(pins.as_ref(), config.as_ref()) {
            return Err(format!(
                "node {id} `{name}` holds its `{key}` as no object, so its values do not read"
            ));
        }
        fold_the_retired_fields_into(&mut input_values, pins, config);
        Ok(Self {
            id,
            name,
            node_type,
            input_values,
            exposed_config_fields,
            studio,
            fragment,
        })
    }
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
    /// The promoted settings the generator ran with, the fragment node's `input_values`; an
    /// older runtime recorded them as `config`.
    #[serde(alias = "config", default)]
    pub input_values: Map<String, Value>,
    /// The stored output, its nodes each holding its UUID and its links naming them by UUID.
    pub fragment: Value,
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
        let document = stream_document_in_the_current_shape(document);
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

    /// Each node's input values by its UUID, at every level of the fragments the document
    /// stores.
    pub fn input_values_of_every_node_at_every_level(&self) -> Vec<(Uuid, &Map<String, Value>)> {
        let mut input_values = Vec::new();
        push_the_input_values_of_each_of_the_nodes_at_every_level(
            &self.node_values,
            &mut input_values,
        );
        input_values
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
    /// names or already holds what it adds. A node it places in an older shape, such as a graph
    /// generator's stored output or a node an edit recorded before this runtime, takes the
    /// current one.
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
                let mut node_value = serde_json::to_value(node).map_err(|not_written| {
                    format!("node {} does not write: {not_written}", node.id)
                })?;
                upgrade_a_node_in_place(&mut node_value);
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
            StreamGraphOperation::SetNodeConfig { node, from, to } => self
                .change_the_input_values_of(*node, |input_values| {
                    for field in from.as_object().into_iter().flat_map(Map::keys) {
                        input_values.remove(field);
                    }
                    for (field, value) in to.as_object().into_iter().flatten() {
                        if !value.is_null() {
                            input_values.insert(field.clone(), value.clone());
                        }
                    }
                }),
            StreamGraphOperation::SetPinValue {
                node, input, to, ..
            }
            | StreamGraphOperation::SetNodeInputValue {
                node, input, to, ..
            } => self.change_the_input_values_of(*node, |input_values| match to {
                Some(value) => {
                    input_values.insert(input.clone(), value.clone());
                }
                None => {
                    input_values.remove(input);
                }
            }),
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
                put_input_values(node_object, to.input_values.clone());
                let mut fragment = to.fragment.clone();
                upgrade_stream_document_in_place(&mut fragment);
                node_object.insert("fragment".to_owned(), fragment);
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

    /// Change the input values the node whose UUID is `node` holds with `change`, its key left
    /// out when it holds none.
    fn change_the_input_values_of(
        &mut self,
        node: Uuid,
        change: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<(), String> {
        let node_object = self.node_object_mut(node)?;
        let mut input_values = match node_object.remove(NODE_INPUT_VALUES_KEY) {
            None => Map::new(),
            Some(Value::Object(input_values)) => input_values,
            Some(_) => {
                return Err(format!(
                    "node {node}'s `{NODE_INPUT_VALUES_KEY}` is not an object"
                ));
            }
        };
        change(&mut input_values);
        put_input_values(node_object, input_values);
        Ok(())
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

/// The `type` of a fragment node that references a stream, this project's or an import's, whose
/// graph each load reads from that stream's document.
pub const STREAM_FRAGMENT_NODE_TYPE: &str = "tatolab.project:StreamFragment";

/// The type of the built-in function node a loop of function nodes closes through: it hands
/// out its input as it was at the last evaluation.
pub const PREVIOUS_VALUE_NODE_TYPE: &str = "tatolab.stream:PreviousValue";

/// The input port of a `PreviousValue` node.
pub const PREVIOUS_VALUE_NODE_INPUT_PORT: &str = "value";

/// The output port of a `PreviousValue` node.
pub const PREVIOUS_VALUE_NODE_OUTPUT_PORT: &str = "result";

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

/// The input of a stream's fragment node naming the stream it holds.
pub const STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: &str = "stream";

#[cfg(test)]
mod tests;
