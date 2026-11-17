// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// The link a refused operation names, each end as the caller named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct RefusedGraphLinkOnTheWire {
    /// The output port the link carries from.
    pub source: GraphLinkEndOnTheWire,
    /// The input port the link carries into.
    pub target: GraphLinkEndOnTheWire,
}

/// One end of a link a graph tool names: a node by its UUID and one of its ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphLinkEndOnTheWire {
    /// The node's UUID.
    pub node: String,
    /// The port's name.
    pub port: String,
}

/// A node a graph transaction added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionAddedNode {
    /// The stream it was added to, by stream id.
    pub stream: String,
    /// The UUID the runtime made for it.
    pub node: String,
    /// The name it took.
    pub name: String,
    /// The `as` its `add_node` gave it, which later operations of the transaction named it by.
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A labelled group a graph transaction made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionAddedGroup {
    /// The stream it was made in, by stream id.
    pub stream: String,
    /// The UUID the runtime made for it.
    pub group: String,
    /// Its title.
    pub title: String,
}

/// A canvas note a graph transaction placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionAddedNote {
    /// The stream it was placed in, by stream id.
    pub stream: String,
    /// The UUID the runtime made for it.
    pub note: String,
}

/// A link a graph transaction made, with the state the stream's graph reads it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionConnectedLink {
    /// The stream it was made in, by stream id.
    pub stream: String,
    /// The output port it carries from.
    pub source: GraphLinkEndOnTheWire,
    /// The input port it carries into.
    pub target: GraphLinkEndOnTheWire,
    /// `pending`, `wired` or `error`; absent when the graph could not be read after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Why a link reading `error` will carry nothing, in the refusing end's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
}

/// An input a graph transaction set a value on while a link feeds it, so the link's messages
/// are what the node reads until the link is taken out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionInputOverriddenByALink {
    /// The stream the node is in, by stream id.
    pub stream: String,
    /// The node's UUID.
    pub node: String,
    /// The input set.
    pub input: String,
    /// The UUID of the node the link carries from.
    pub from_node: String,
    /// The output port the link carries from.
    pub from_output: String,
}

/// A node a graph transaction built again, so it reads a setting it takes only when it is set
/// up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionRebuiltNode {
    /// The stream the node is in, by stream id.
    pub stream: String,
    /// The node's UUID.
    pub node: String,
}

/// The result of a graph tool: the transaction it applied, committed or live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphTransactionToolResult {
    /// `commit`, written to the graph documents and recorded in history, or `live`, applied
    /// to the running streams alone.
    pub mode: String,
    /// The transaction's UUID; absent on a live one, which history never records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// The commit on the project history's `main` that records it; absent on a live one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Each operation as applied, in order, tagged by its `operation`, its `stream` by
    /// stream id.
    pub operations: Vec<serde_json::Value>,
    /// Each node it added.
    pub added_nodes: Vec<GraphTransactionAddedNode>,
    /// Each link it made.
    pub connected_links: Vec<GraphTransactionConnectedLink>,
    /// Each labelled group it made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_groups: Vec<GraphTransactionAddedGroup>,
    /// Each canvas note it placed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_notes: Vec<GraphTransactionAddedNote>,
    /// Each node it built again to take a setting read at setup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes_rebuilt: Vec<GraphTransactionRebuiltNode>,
    /// Each input it set a value on that a link feeds, whose messages the node reads instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overridden_by_links: Vec<GraphTransactionInputOverriddenByALink>,
}
