// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;

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
    /// What the studio draws the node with.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub studio: Map<String, Value>,
    /// The stored output of the graph generator `type` names, on a fragment alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<Box<Value>>,
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
        from: Map<String, Value>,
        to: Map<String, Value>,
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
        }
    }

    /// The node the operation acts on.
    pub fn node(&self) -> Uuid {
        match self {
            Self::AddNode { node, .. } | Self::RemoveNode { node, .. } => node.id,
            Self::Connect { link } | Self::Disconnect { link } => link.target.node,
            Self::SetNodeConfig { node, .. }
            | Self::SetPinValue { node, .. }
            | Self::RenameNode { node, .. }
            | Self::SetStudioFields { node, .. }
            | Self::RefreshFragment { node, .. }
            | Self::SetExposedConfigFields { node, .. } => *node,
        }
    }

    /// The config the operation leaves its node with, when it sets one.
    pub fn config_it_sets(&self) -> Option<&Value> {
        match self {
            Self::AddNode { node, .. } => Some(&node.config),
            Self::SetNodeConfig { to, .. } => Some(to),
            Self::RefreshFragment { to, .. } => Some(&to.config),
            _ => None,
        }
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
        Ok(Self {
            node_values,
            link_values,
            every_other_key,
        })
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
                    node_object.insert("studio".to_owned(), Value::Object(to.clone()));
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

    fn why_no_node_has(&self, node: Uuid) -> String {
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

    fn node_objects(&self) -> impl Iterator<Item = &Map<String, Value>> {
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

    fn node_values_mut(&mut self) -> &mut Vec<Value> {
        &mut self.node_values
    }

    fn link_values(&self) -> &[Value] {
        &self.link_values
    }

    fn link_values_mut(&mut self) -> &mut Vec<Value> {
        &mut self.link_values
    }

    /// `link` with each end's node by UUID, an end naming its node by name resolved through
    /// the nodes the document holds.
    fn link_read_from(&self, link: &Value) -> Option<StreamGraphDocumentLink> {
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

fn node_uuid_of(node_object: &Map<String, Value>) -> Option<Uuid> {
    node_object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn node_name_of(node_object: &Map<String, Value>) -> Option<&str> {
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
    kept_level.insert("nodes".to_owned(), Value::Array(nodes));
    kept_level.insert("links".to_owned(), Value::Array(links));
    Ok(Value::Object(kept_level))
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
            studio: Map::new(),
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
                from: Map::new(),
                to: json!({"position": [10, 20]}).as_object().unwrap().clone(),
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
            node.id
        );
    }
}
