// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The answer `graph` gives: a stream's graph, its nodes, their ports and the value each input
//! reads, its links and what the runtime keeps on each. The engine builds these types and
//! nothing mirrors them.
//!
//! A port's family, audio window and stream facts are open JSON here: their types derive another
//! JSON Schema generator than the contract's.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use streamlib_node_schema::NodeClassImportPath;
use streamlib_node_schema::pin_type::ResolvedPin;

use super::{GraphLinkOnTheWire, GraphNodeComponentsOnTheWire, StreamClockSpec};

/// `graph`'s answer: the graph of the stream it names, or every loaded stream's graph under the
/// machine's name when it names none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum GraphToolResult {
    /// The graph of the stream `stream` named.
    OneStream(Box<StreamGraphOnTheWire>),
    /// Every loaded stream's graph, when no `stream` was named.
    EveryStream(MachineWideGraphOnTheWire),
}

/// Every loaded stream's graph under the machine's name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct MachineWideGraphOnTheWire {
    /// The machine's name, the first part of `<machine>/<stream id>/<node>/<port>`.
    pub machine: String,
    /// Each loaded stream's graph, as a call naming that stream answers it.
    pub streams: Vec<StreamGraphOnTheWire>,
}

/// One stream's graph: its spec keys, the stream document's own, with the live keys beside
/// them, so it loads back as a graph document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct StreamGraphOnTheWire {
    /// The UUID of the stream asset whose graph document this graph was loaded from: a spec key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// The name the stream is shown under: a spec key, which its graph document writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The stream id this graph was loaded under, its stream's address. Absent until a load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// The clock the stream runs on, tagged by its `mode`: a spec key. Absent when it runs on
    /// real time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<StreamClockSpec>,
    /// Every node of the stream's own level.
    pub nodes: Vec<GraphNodeOnTheWire>,
    /// Every link between those nodes' ports.
    pub links: Vec<GraphLinkOnTheWire>,
    /// Every reference edge: a node's config key naming another node. A live key, read back
    /// from the configs; absent when the graph holds none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ConfigNodeReferenceEdgeOnTheWire>,
    /// The name of the machine the stream runs on, the first part of
    /// `<machine>/<stream id>/<node>/<port>`.
    pub machine: String,
    /// Every labelled group of the stream's own level: a spec key, read by nothing that runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,
    /// What Tato Studio draws beside the nodes, its canvas notes, under the studio contract: a
    /// spec key, read by nothing that runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<Value>,
    /// The stream's function node pool, rendered as a node is: a live key, absent when no
    /// island runs in it. It is no node of the graph, so it is never a link's end; each link it
    /// carries renders as the link it was given, naming the pool's ports under
    /// `carried_through_the_function_node_pool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_node_pool: Option<GraphNodeOnTheWire>,
}

/// A labelled group of one level's nodes, which a node joins by naming its `id` as its
/// `studio.group`; it changes nothing that runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LabelledNodeGroup {
    /// The group's UUID, the runtime's from its placement on.
    pub id: String,
    /// The group's title.
    pub title: String,
    /// What the group's nodes do, for the next person who reads the graph.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The colour the studio draws the group with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub colour: String,
}

/// One reference edge: the config key `config_key` of node `node` names node
/// `referenced_node`. It carries no data and is never a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ConfigNodeReferenceEdgeOnTheWire {
    /// The name of the node whose config holds the reference.
    pub node: String,
    /// The config key the reference sits under.
    pub config_key: String,
    /// The name of the node the reference names.
    pub referenced_node: String,
}

/// A node of a stream's graph, a fragment holding the nodes and links of its graph generator's
/// stored output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphNodeOnTheWire {
    /// The node's UUID: a spec key, kept across every run and restart of its stream.
    pub id: String,
    /// The import path of the class this node is.
    #[serde(rename = "type")]
    pub node_type: NodeClassImportPath,
    /// How the node's code runs. Absent on a node whose type nothing registers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<GraphNodeKindOnTheWire>,
    /// The language a function node is written in; absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<FunctionNodeLanguageOnTheWire>,
    /// The node's name, unique in its graph and cast to lowercase URL-safe: what a link end, an
    /// exposure and a port address name it by.
    pub name: String,
    /// The input values a node type not yet typed takes whole as its config, the rest of
    /// `input_values` being its `pins`. Served for a client that reads no `input_values`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// A checksum of `config`, for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    /// Every input port and output port.
    pub ports: GraphNodePortsOnTheWire,
    /// The values the node's graph document holds for its inputs, by input name: a sparse map,
    /// so an input it leaves out reads its default. A value set live shows only as its input's
    /// `value`.
    #[serde(default)]
    pub input_values: Map<String, Value>,
    /// The input values that are no config field, by input name; empty when none is saved.
    /// Served for a client that reads no `input_values`.
    #[serde(default)]
    pub pins: Map<String, Value>,
    /// The config fields the node takes as inputs too, each an input of the field's name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_config_fields: Vec<String>,
    /// What the studio draws the node with: canvas position, group, comment, colour.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub studio: Map<String, Value>,
    /// The inputs of a function node that no link feeds, that carry no saved value and that
    /// declare no default; the node holds without evaluating until each has a value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs_waiting_for_a_value: Vec<String>,
    /// The node's live state and counters, by component.
    pub components: GraphNodeComponentsOnTheWire,
    /// What a fragment holds: its graph generator's stored output, each node rendered as this
    /// node is. A spec key, present on a fragment alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<GraphFragmentOnTheWire>,
}

/// How a node's code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum GraphNodeKindOnTheWire {
    /// A shader on the GPU.
    Shader,
    /// A function evaluated where its reader reads.
    Function,
    /// A class in a process of its own.
    Process,
    /// A graph generator's stored output, flattened into the nodes it holds when it runs.
    Fragment,
}

impl GraphNodeKindOnTheWire {
    /// Every kind a node runs as.
    pub const EVERY_KIND: [Self; 4] = [Self::Shader, Self::Function, Self::Process, Self::Fragment];

    /// The name a reader reads this kind under.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Shader => "shader",
            Self::Function => "function",
            Self::Process => "process",
            Self::Fragment => "fragment",
        }
    }
}

/// The language a function node is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FunctionNodeLanguageOnTheWire {
    /// Python.
    Python,
    /// Rust.
    Rust,
}

impl FunctionNodeLanguageOnTheWire {
    /// Every language a function node is written in.
    pub const EVERY_LANGUAGE: [Self; 2] = [Self::Python, Self::Rust];

    /// The name a reader reads this language under.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }
}

/// The nodes and links a fragment holds, each link end naming a node of the fragment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphFragmentOnTheWire {
    /// Every node of the fragment, a fragment it holds rendered as a container in turn.
    pub nodes: Vec<GraphNodeOnTheWire>,
    /// Every link between those nodes' ports.
    pub links: Vec<GraphLinkOnTheWire>,
    /// Every labelled group of the fragment's nodes, made by its graph generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,
}

/// A node's input ports and output ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphNodePortsOnTheWire {
    /// The ports that receive data, each with the value it reads while unlinked.
    pub inputs: Vec<GraphNodePortOnTheWire>,
    /// The ports that send data.
    pub outputs: Vec<GraphNodePortOnTheWire>,
}

/// One port of a node of a stream's graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphNodePortOnTheWire {
    /// The port's name.
    pub name: String,
    /// What the port carries, as declared beside it.
    #[serde(default)]
    pub description: String,
    /// Always `data`, served until the contract's version changes; how a port's messages are
    /// read is its `flow`.
    #[serde(default)]
    pub port_kind: PortKindOnTheWire,
    /// The delivery profile an input port reads by, `newest` or `ordered`: the one it declares,
    /// else its pin's default; null on an output port.
    pub delivery_profile: Option<String>,
    /// The samples each link into this input port may hold beyond its mailbox, declared;
    /// absent on a port that takes the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// The window contract an audio input port declares, tagged by `resolved_from`; absent on
    /// a port that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<Value>"))]
    pub audio_window: Option<streamlib_node_schema::AudioWindowContract>,
    /// The stream facts this output port carries now, tagged by `family`; absent on a port that
    /// states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<Value>"))]
    pub stream_facts: Option<streamlib_node_schema::PortStreamFacts>,
    /// `false` on a Records port, whose values no type checks; absent on every other family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_checked: Option<bool>,
    /// The port family this port declares and its family's keys. Absent on a graph variable's
    /// value port, which keeps the family of the setter's source port. Served for a client that
    /// reads no `pin_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<Value>"))]
    pub family: Option<streamlib_node_schema::PortFamilyDeclaration>,
    /// The value this input reads while nothing is linked into it and no value is set on its
    /// node, null when it declares a null default; absent on a port that declares none.
    #[serde(
        default,
        deserialize_with = "a_value_that_may_be_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub pin_default: Option<Value>,
    /// The pin type the port carries, resolved, as `pin_type`, and its flow, as `flow`. Both
    /// are absent on a port whose type the code does not state exactly, which links by its
    /// family alone.
    #[serde(flatten)]
    pub pin: Option<ResolvedPin>,
    /// On an input, the value it reads while unlinked, as `value`, and where that value comes
    /// from, as `value_comes_from`. Both are absent on an input that takes no stored value and
    /// no link feeds, and on an output.
    #[serde(flatten, deserialize_with = "a_read_only_when_either_key_is_present")]
    pub value_read: Option<InputValueReadOnTheWire>,
}

/// The kind a port was once declared as; every port is `data`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum PortKindOnTheWire {
    /// Every port.
    #[default]
    Data,
    /// No port is declared so any more.
    Event,
    /// No port is declared so any more.
    Control,
}

/// The value one input reads while no link feeds it, and where it comes from: no `value` and
/// `value_comes_from` `wire` when a link feeds it, `value` null and no `value_comes_from` when it
/// reads nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InputValueReadOnTheWire {
    /// The value it reads, null when it reads nothing; absent when a link feeds it.
    #[serde(
        default,
        deserialize_with = "a_value_that_may_be_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<Value>,
    /// Where the value comes from; absent when it reads nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_comes_from: Option<InputValueComesFromOnTheWire>,
}

/// A `value` present on the wire, null among them: only an absent one reads as `None`.
pub(super) fn a_value_that_may_be_null<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// A port's `value` and `value_comes_from`, read only when either key is present: an output, or
/// an input that takes no stored value and no link feeds, reads none.
fn a_read_only_when_either_key_is_present<'de, D>(
    deserializer: D,
) -> Result<Option<InputValueReadOnTheWire>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let read = InputValueReadOnTheWire::deserialize(deserializer)?;
    Ok((read.value.is_some() || read.value_comes_from.is_some()).then_some(read))
}

/// Where the value an input reads comes from, as `graph` renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputValueComesFromOnTheWire {
    /// A value set live, which no document holds.
    Live,
    /// The value the stream's graph document holds.
    Saved,
    /// The default the input declares.
    Default,
    /// A link feeds the input; it holds no value.
    Wire,
}
