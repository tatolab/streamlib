// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link as `graph` answers it: its two ends, its live state, what it converts and warns of, and
//! what the runtime keeps on it.

use serde::{Deserialize, Serialize};

use super::PinLinkWarningOnTheWire;

/// One link of a stream's graph, from an output port to an input port.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphLinkOnTheWire {
    /// The link's id; empty on a link drawn with an end on a node that runs as no node of its
    /// own, a graph interface node or a fragment, which no running link stands for.
    pub id: String,
    /// The output port the link carries from.
    pub source: GraphLinkPortRefOnTheWire,
    /// The input port the link carries into.
    pub target: GraphLinkPortRefOnTheWire,
    /// The samples the link's channel holds.
    #[serde(default)]
    pub capacity: usize,
    /// The link's state now.
    #[serde(default)]
    pub state: GraphLinkStateOnTheWire,
    /// Why the link is in the `error` state, in the words of the helper process that could not
    /// open its port, or of the graph's validation that refused it as its stream loaded. Absent
    /// in every other state, so a reader that finds it knows the link carries nothing and will
    /// not start to: disconnect it and wire again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// `true` when the input reads what the output carries as its own type at no cost: Boolean,
    /// Integer and Float among themselves, a Vector into a Vector or a Color, one number into a
    /// Vector or a Color, or a Menu into a String. Absent on every other link.
    #[serde(default, skip_serializing_if = "is_false")]
    pub free_conversion: bool,
    /// What the link does that a person may not expect, as the judgement of its two ends' pin
    /// types says; none of these blocks it. Absent when it warns of nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PinLinkWarningOnTheWire>,
    /// What the runtime keeps on the link, by what it is.
    pub components: GraphLinkComponentsOnTheWire,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One end of a link of a stream's graph: a node by its name, and one of its ports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphLinkPortRefOnTheWire {
    /// The node's name.
    pub node: String,
    /// The port's own name on that node.
    pub port: String,
}

/// The state of a link of a stream's graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GraphLinkStateOnTheWire {
    /// In the graph, not wired yet.
    #[default]
    Pending,
    /// Wired, its channel carrying samples.
    Wired,
    /// Being disconnected.
    Disconnecting,
    /// Disconnected.
    Disconnected,
    /// Wiring failed; `error_reason` says why.
    Error,
}

/// What the runtime keeps on a link, each under its own key; a key is absent where the link
/// keeps nothing of that kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphLinkComponentsOnTheWire {
    /// The link's state as wiring stamped it, spelled with a capital: always the link's `state`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<GraphLinkStateComponentOnTheWire>,
    /// `true` on a link a disconnect is removing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_deletion: Option<bool>,
    /// On a link from a function node island into a reader that cannot evaluate it, the ports of
    /// the stream's function node pool it is carried through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_through_the_function_node_pool:
        Option<LinkCarriedThroughTheFunctionNodePoolOnTheWire>,
    /// On a link out of a function node island that its reader evaluates, each input port of
    /// that reader the link is read through, sorted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_through_the_reader_of_its_function_node_island: Option<Vec<String>>,
    /// On a link with an end on a graph interface node, the port of the stream that node
    /// declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_interface: Option<GraphInterfacePortOnTheWire>,
    /// On a link with an end on a graph variable's node, the variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_variable: Option<GraphVariableOnTheWire>,
}

/// A link's state as wiring stamped it on the link, spelled with a capital.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum GraphLinkStateComponentOnTheWire {
    /// In the graph, not wired yet.
    Pending,
    /// Wired, its channel carrying samples.
    Wired,
    /// Being disconnected.
    Disconnecting,
    /// Disconnected.
    Disconnected,
    /// Wiring failed.
    Error,
}

impl From<GraphLinkStateOnTheWire> for GraphLinkStateComponentOnTheWire {
    fn from(state: GraphLinkStateOnTheWire) -> Self {
        match state {
            GraphLinkStateOnTheWire::Pending => Self::Pending,
            GraphLinkStateOnTheWire::Wired => Self::Wired,
            GraphLinkStateOnTheWire::Disconnecting => Self::Disconnecting,
            GraphLinkStateOnTheWire::Disconnected => Self::Disconnected,
            GraphLinkStateOnTheWire::Error => Self::Error,
        }
    }
}

/// The two ports of the stream's function node pool a link is carried through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LinkCarriedThroughTheFunctionNodePoolOnTheWire {
    /// The pool's input port the island's output feeds.
    pub input_port: String,
    /// The pool's output port that feeds the reader.
    pub output_port: String,
}

/// The port of its stream a graph interface node declares, on the node and on each link with an
/// end on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphInterfacePortOnTheWire {
    /// Whether the stream takes the port in or gives it out.
    pub direction: GraphInterfacePortDirectionOnTheWire,
    /// The port's name on the stream.
    pub port: String,
    /// The port family the port carries; null while the runtime waits for a link to settle it.
    pub family: Option<String>,
    /// On a `GraphInput` node, the value its `value` input reads now: the one set live, else its
    /// saved one. Absent elsewhere, and where it reads none.
    #[serde(
        default,
        deserialize_with = "a_value_that_may_be_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_value: Option<serde_json::Value>,
}

/// A `current_value` present on the wire, null among them: only an absent one reads as `None`.
fn a_value_that_may_be_null<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// Whether a stream takes a port it declares in or gives it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GraphInterfacePortDirectionOnTheWire {
    /// The stream takes it in, through a `GraphInput` node.
    Input,
    /// The stream gives it out, through a `GraphOutput` node.
    Output,
}

/// The graph variable a variable's node, or a link with an end on one, stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphVariableOnTheWire {
    /// The variable's name; null on a node no setter or getter names one for.
    pub variable: Option<String>,
}
