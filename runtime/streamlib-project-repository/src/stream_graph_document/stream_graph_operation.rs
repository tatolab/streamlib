// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::stream_document_upgrade::{upgrade_a_node_in_place, upgrade_stream_document_in_place};
use crate::stream_graph_fragment_fold::{
    StreamGraphDocumentValueAtItsPosition, StreamGraphFragmentFold,
};
use crate::stream_graph_studio_contract::{
    StreamGraphDocumentNote, StreamGraphNodeStudioFields, StreamGraphNoteFields,
};

#[cfg(feature = "history")]
use super::StreamGraphDocumentLinkEnd;
use super::{
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocumentGroup, StreamGraphDocumentLink,
    StreamGraphDocumentNode, StreamGraphFragmentPlacement, StreamGraphGroupFields,
    input_values_after_the_set, push_the_input_values_of_each_node_at_every_level,
    push_the_input_values_of_each_of_the_nodes_at_every_level, the_input_value_after_the_set,
    the_input_value_held_on,
};

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
    /// The node's config was replaced whole: each input value `from` names taken out, then each
    /// `to` names set.
    SetNodeConfig { node: Uuid, from: Value, to: Value },
    /// The value of one of the node's inputs was set, `None` being no value; an older runtime
    /// recorded it so.
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
    /// The value the node's input holds while no link feeds it was set, `None` being no value,
    /// so the input reads its declared default; with `member`, the value of that member of a
    /// bundle input alone, every other member left as it was.
    SetNodeInputValue {
        node: Uuid,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<String>,
        from: Option<Value>,
        to: Option<Value>,
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
            Self::SetNodeInputValue {
                node,
                input,
                member,
                from,
                to,
            } => Self::SetNodeInputValue {
                node,
                input,
                member,
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
            Self::SetNodeInputValue {
                node,
                input,
                member: Some(member),
                ..
            } => format!("set_node_input_value {node}/{input}.{member}"),
            Self::SetNodeInputValue { node, input, .. } => {
                format!("set_node_input_value {node}/{input}")
            }
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
            | Self::SetNodeInputValue { node, .. }
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

    /// The `set_node_config` that sets `to` on `node`, which takes `config_it_takes` whole:
    /// `from` is that config with the value `node` held of each other name `to` sets, such as
    /// one of its inputs, so the inverse puts back every value the edit overwrites.
    pub fn set_node_config_of(
        node: &StreamGraphDocumentNode,
        config_it_takes: Value,
        to: Map<String, Value>,
    ) -> Self {
        let mut from = config_it_takes;
        if let Value::Object(from_object) = &mut from {
            for (name, held) in to
                .keys()
                .filter_map(|name| Some((name, node.input_values.get(name)?)))
            {
                from_object
                    .entry(name.clone())
                    .or_insert_with(|| held.clone());
            }
        }
        Self::SetNodeConfig {
            node: node.id,
            from,
            to: Value::Object(to),
        }
    }

    /// The set of `node`'s input `input`, or of its member `member` when one is named, to `to`,
    /// `None` or `null` taking the value off, with the value it held as `from`. A member set on an
    /// input holding a value that is no object of members replaces that whole value, so it is
    /// the set of the whole input to the object it starts, and its undo puts the value back.
    pub fn set_node_input_value_of(
        node: &StreamGraphDocumentNode,
        input: String,
        member: Option<String>,
        to: Option<Value>,
    ) -> Self {
        let to = to.filter(|value| !value.is_null());
        if member.is_some()
            && let Some(held) = node.input_values.get(&input)
            && !held.is_object()
        {
            return Self::SetNodeInputValue {
                node: node.id,
                from: Some(held.clone()),
                to: the_input_value_after_the_set(
                    Some(held.clone()),
                    member.as_deref(),
                    to.as_ref(),
                ),
                input,
                member: None,
            };
        }
        Self::SetNodeInputValue {
            node: node.id,
            from: the_input_value_held_on(&node.input_values, &input, member.as_deref()).cloned(),
            input,
            member,
            to,
        }
    }

    /// The input value set a `set_pin_value` of `to` on `node`'s input `input` makes, `None`
    /// clearing it. A clear of a config field the node exposes as an input is refused: the node
    /// holds one value of the field, so the clear would take its config value out while the
    /// running node keeps it.
    pub fn set_pin_value_of(
        node: &StreamGraphDocumentNode,
        input: String,
        to: Option<Value>,
    ) -> Result<Self, String> {
        let to = to.filter(|value| !value.is_null());
        if to.is_none() && node.exposed_config_fields.contains(&input) {
            return Err(format!(
                "node {} `{}` takes `{input}` as an input of its config field of that name, so \
                 the value it holds is its config's: set it with set_node_config",
                node.id, node.name
            ));
        }
        Ok(Self::set_node_input_value_of(node, input, None, to))
    }

    /// Each node's input values the operation leaves it with, by that node's UUID: its own
    /// node's, and those of every node at every level of a fragment it places, read in the
    /// current shape as [`StreamGraphDocument::apply`](super::StreamGraphDocument::apply) places
    /// them. A config set whole is its own: the fields it sets are the input values it leaves;
    /// a value set on one input is that input's value.
    pub fn input_values_it_sets(&self) -> Vec<(Uuid, Map<String, Value>)> {
        let mut input_values = Vec::new();
        let mut fragments_it_places = Vec::new();
        let mut nodes_it_places = Vec::new();
        match self {
            Self::AddNode { node, .. } => {
                input_values.push((node.id, node.input_values.clone()));
                fragments_it_places.extend(node.fragment.as_deref().cloned());
            }
            Self::SetNodeConfig {
                node,
                to: Value::Object(to),
                ..
            } => input_values.push((*node, to.clone())),
            Self::SetPinValue {
                node,
                input,
                to: Some(value),
                ..
            } => input_values.push((*node, Map::from_iter([(input.clone(), value.clone())]))),
            Self::SetNodeInputValue {
                node,
                input,
                member,
                to: Some(value),
                ..
            } => input_values.push((
                *node,
                input_values_after_the_set(&Map::new(), input, member.as_deref(), Some(value)),
            )),
            Self::RefreshFragment { node, to, .. } => {
                input_values.push((*node, to.input_values.clone()));
                fragments_it_places.push(to.fragment.clone());
            }
            Self::FoldIntoFragment { fold } => nodes_it_places.push(fold.fragment.value.clone()),
            Self::UnfoldFragment { fold } => {
                nodes_it_places.extend(fold.nodes.iter().map(|node| node.value.clone()));
            }
            _ => {}
        }
        for fragment in &mut fragments_it_places {
            upgrade_stream_document_in_place(fragment);
        }
        for node in &mut nodes_it_places {
            upgrade_a_node_in_place(node);
        }
        let mut placed = Vec::new();
        for fragment in &fragments_it_places {
            push_the_input_values_of_each_node_at_every_level(fragment, &mut placed);
        }
        push_the_input_values_of_each_of_the_nodes_at_every_level(&nodes_it_places, &mut placed);
        input_values.extend(
            placed
                .into_iter()
                .map(|(node, placed_input_values)| (node, placed_input_values.clone())),
        );
        input_values
    }
}
