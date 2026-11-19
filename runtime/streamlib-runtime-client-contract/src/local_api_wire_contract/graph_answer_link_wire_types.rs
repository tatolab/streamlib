// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link as `graph` answers it: its two ends, its live state, what it converts and warns of, and
//! what the runtime keeps on it.

use serde::{Deserialize, Serialize};

use super::PinLinkMarksOnTheWire;

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
    /// What the link converts and warns of, as the judgement of its two typed ends says.
    #[serde(flatten)]
    pub marks: PinLinkMarksOnTheWire,
    /// What the runtime keeps on the link, by what it is.
    pub components: GraphLinkComponentsOnTheWire,
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

/// A link's state as wiring stamped it on the link, spelled with a capital: the key a reader
/// written before `state` was typed reads. Always the link's `state`, the one set of states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLinkStateComponentOnTheWire(pub GraphLinkStateOnTheWire);

impl From<GraphLinkStateOnTheWire> for GraphLinkStateComponentOnTheWire {
    fn from(state: GraphLinkStateOnTheWire) -> Self {
        Self(state)
    }
}

/// `spelled` with its first letter a capital; each link state is one word.
fn with_a_capital(spelled: &str) -> String {
    let mut letters = spelled.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

impl Serialize for GraphLinkStateComponentOnTheWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let spelled = serde_json::to_value(self.0).map_err(serde::ser::Error::custom)?;
        let spelled = spelled.as_str().unwrap_or_default();
        serializer.serialize_str(&with_a_capital(spelled))
    }
}

impl<'de> Deserialize<'de> for GraphLinkStateComponentOnTheWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spelled = String::deserialize(deserializer)?;
        serde_json::from_value(serde_json::Value::String(spelled.to_lowercase()))
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for GraphLinkStateComponentOnTheWire {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("GraphLinkStateComponentOnTheWire")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = GraphLinkStateOnTheWire::json_schema(generator);
        if let Some(states) = schema
            .get_mut("oneOf")
            .and_then(serde_json::Value::as_array_mut)
        {
            for state in states {
                if let Some(spelled) = state.get("const").and_then(serde_json::Value::as_str) {
                    state["const"] = serde_json::Value::String(with_a_capital(spelled));
                }
            }
        }
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "A link's state as wiring stamped it on the link, spelled with a capital: always \
                 the link's `state`."
                    .to_owned(),
            ),
        );
        schema
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
        deserialize_with = "super::graph_answer_wire_types::a_value_that_may_be_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_value: Option<serde_json::Value>,
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
