// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::stream_graph_fragment_fold::{
    StreamGraphDocumentValueAtItsPosition, StreamGraphFragmentFold,
};
use crate::stream_graph_studio_contract::{
    StreamGraphDocumentNote, StreamGraphNodeStudioFields, StreamGraphNoteFields,
};

use super::{
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocumentGroup, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphDocumentNode, StreamGraphFragmentPlacement,
    StreamGraphGroupFields, push_the_config_of_each_node_at_every_level,
    push_the_config_of_each_of_the_nodes_at_every_level,
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
