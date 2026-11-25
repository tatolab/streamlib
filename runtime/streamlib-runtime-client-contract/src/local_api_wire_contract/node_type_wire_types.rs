// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the three node type reads answer: `list_pin_types`, the pin types and what converts
//! each; `list_node_types`, the node types a project offers by the source their code comes
//! from; and `describe_node_types`, the typed inputs and outputs of the node types named.
//!
//! A port of a node type not yet typed keeps its family and annotation as open JSON beside a
//! missing pin type, as the old listing states them: their types derive another JSON Schema
//! generator than the contract's.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamlib_node_schema::NodeValueLifetime;
use streamlib_node_schema::pin_type::{
    BuiltInPinTypeName, DeclaredPinType, NamedPinTypeImportPath, NodeInputMode, PinAnnotationName,
    PinAnnotations, PinFlow, PinType, SettingReadTime,
};

use super::{
    GraphGeneratorPromotedSettingReadFromSource, NodeModuleNotReadable,
    ControlOperationRefusalOnTheWire, ProjectFilePlaceOnTheWire,
};

/// `list_pin_types`' answer: every built-in pin type, and each named type the project's node
/// code brings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListPinTypesToolResult {
    /// Each built-in pin type, in the order the contract's `x-pin-types` lists them.
    pub pin_types: Vec<PinTypeListed>,
    /// Each named type a module of the project, an import or an installed distribution
    /// declares, by its import path, read from source; empty while the project's node code
    /// does not list, as `named_pin_types_not_listed_because` then says.
    #[serde(default)]
    pub named_pin_types: Vec<NamedPinTypeListed>,
    /// Why the project's node code did not list, so no named type of it is answered; absent
    /// when it listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_pin_types_not_listed_because: Option<String>,
}

/// One named type a module declares with `@named_pin_type`, which a pin typed with it carries
/// as the bytes its class packs and links only to a pin of the same name or an input of any
/// type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NamedPinTypeListed {
    /// Where the module declaring it comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceListedOrigin,
    /// The pin type a pin of it states: `{"type": "named", "named_pin_type": …}`.
    pub pin_type: PinType,
    /// What a value of it is, as its declaration describes it.
    pub documentation: String,
    /// The file declaring it, relative to its source's root.
    pub path: String,
    /// The line of its `class`.
    pub line: u32,
}

/// One named type as its `@named_pin_type` declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NamedPinTypeReadFromSource {
    /// Its name: the class's import path, `<module>:<qualname>`.
    pub named_pin_type: NamedPinTypeImportPath,
    /// What a value of it is; empty where its `description` is no string in source.
    pub description: String,
    /// The file declaring it, relative to its source's root.
    pub path: String,
    /// The line of its `class`.
    pub line: u32,
}

/// One built-in pin type: the contract's row for it, and what converts it on this runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PinTypeListed {
    /// Its name: the `type` tag of its pin type.
    pub name: BuiltInPinTypeName,
    /// The words a person reads for it.
    pub label: String,
    /// What it carries, in a sentence or two.
    pub documentation: String,
    /// The flows a pin of it may state, its default first.
    pub flows: Vec<PinFlow>,
    /// The annotations a pin of it may state.
    pub annotations: Vec<PinAnnotationName>,
    /// Each way a Python declaration writes it.
    pub python_forms: Vec<String>,
    /// How the TypeScript SDK writes it: its name as a string literal.
    pub typescript_form: String,
    /// How a node declares a pin of a type built from another, a List or a bundle; absent on
    /// every type its name alone declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how_to_declare: Option<String>,
    /// The JSON Schema of its stored value, as the contract's components state it; absent on a
    /// type that stores no value. A described input states its own, with what its declaration
    /// adds folded in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_json_schema: Option<Value>,
    /// Each conversion a node type of this runtime states from a pin of this type, which a link
    /// from it to the type converted to is made through.
    pub converts_to: Vec<PinTypeConversionListed>,
}

/// One conversion a node type states: its one wired input's type into its output's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PinTypeConversionListed {
    /// The type it takes.
    pub from: PinType,
    /// The type it gives.
    pub to: PinType,
    /// The node type that converts, what `add_node` takes.
    pub node_type: String,
}

/// `list_node_types`' answer: what a project and each source it draws node code from offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListNodeTypesToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The project first, then each import, then each installed distribution, then the node
    /// types the runtime brings itself.
    pub sources: Vec<NodeTypeSourceListed>,
}

/// Where a node type's code comes from, under `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum NodeTypeSourceListedOrigin {
    /// The project's own node code, outside `imports/`.
    Project,
    /// An import's node code as its folder under `imports/` holds it now.
    Dependency {
        /// The imported project's UUID.
        project: String,
        /// Its folder under `imports/`.
        folder: String,
        /// The version its copy carried.
        version: String,
        /// The imported project's name as the project file in its folder holds it; `null` when
        /// that file does not read.
        project_name: Option<String>,
    },
    /// An installed distribution that declares `tatolab.nodes` entry points.
    InstalledDistribution {
        /// The distribution's name.
        distribution: String,
        /// The distribution's version.
        distribution_version: String,
    },
    /// The node types the runtime brings itself, which every stream can add.
    BuiltIn,
}

/// One source's node types, graph generators, and the modules of it that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeSourceListed {
    /// Where the code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceListedOrigin,
    /// Each node type it declares; `describe_node_types` answers their inputs and outputs.
    pub node_types: Vec<NodeTypeListed>,
    /// Each `@graph_generator` it declares, by import path.
    pub graph_generators: Vec<GraphGeneratorListed>,
    /// Each module that does not parse, with why and where; the rest of the source still lists.
    pub unreadable_modules: Vec<NodeModuleNotReadable>,
}

/// One node type a source offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeListed {
    /// The type `add_node` takes: its import path, `<module>:<qualname>`.
    pub node_type: String,
    /// The first line of its description; empty when it declares none.
    pub documentation: String,
    /// The group it is filed under, a path where the group is inside another, as
    /// `Effects/Stylize`: the project's own, else an import's, else the one its author declared;
    /// null when none files it, or where its declaration source does not show it.
    pub category: Option<String>,
    /// Who filed it under `category`; null when nothing files it.
    #[serde(default)]
    pub category_from: Option<NodeTypeCategoryFrom>,
    /// How its code runs; null for an entry point whose declaration source does not show it,
    /// and for a node type the graph resolves itself, which no node code runs.
    pub kind: Option<NodeTypeKind>,
    /// Where the runtime's description of it comes from.
    pub described_from: NodeTypeDescribedFrom,
}

/// Who filed a node type under the group it is listed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeTypeCategoryFrom {
    /// The project moved it there with `set_node_type_group`; clearing that puts it back where
    /// an import or its author files it.
    Project,
    /// A project the project imports moved it there, and the project has not.
    Import,
    /// Its author declared it, with `@node.meta(category=…)` or `#[meta(category = …)]`.
    Declaration,
}

/// `set_node_type_group`'s answer: the group the project now files the node type under, and the
/// transaction that filed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SetNodeTypeGroupToolResult {
    /// The project's UUID.
    pub project: String,
    /// The node type, as `list_node_types` names it.
    pub node_type: String,
    /// The group the project now files it under; null when the project files it nowhere of its
    /// own, so an import's group or its author's stands.
    pub group: Option<String>,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// How a node type's code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeTypeKind {
    /// A class in a process of its own.
    Process,
    /// A function evaluated where its reader reads.
    Function,
    /// A shader on the GPU.
    Shader,
}

/// Where the runtime's description of a node type comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeTypeDescribedFrom {
    /// The runtime brings it, and states its ports itself.
    BuiltIn,
    /// Its declaration, read from source without running it; a port's pin type is not read
    /// there, so its ports state none.
    Source,
    /// A loaded stream of the project described it in its own interpreter, every port typed
    /// as the code states it.
    Interpreter,
}

/// One `@graph_generator` as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphGeneratorListed {
    /// The import path `import_graph_generator` takes: `<module>:<function>`.
    pub generator: String,
    /// Its docstring.
    pub description: String,
    /// Each parameter after the builder: a setting the fragment promotes.
    pub promoted_settings: Vec<GraphGeneratorPromotedSettingReadFromSource>,
    /// The file declaring it, relative to its source's root.
    pub path: String,
    /// The line of its `def`.
    pub line: u32,
}

/// `describe_node_types`' answer: each node type named, in the order named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DescribeNodeTypesToolResult {
    /// Each node type named, described, or with why it could not be.
    pub node_types: Vec<NodeTypeDescribedOrWhyNot>,
}

/// One node type named: its description, or why the runtime has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum NodeTypeDescribedOrWhyNot {
    /// Its inputs and outputs.
    Described(Box<NodeTypeDescription>),
    /// Why it could not be described.
    NotDescribed(Box<NodeTypeNotDescribed>),
}

/// One node type's inputs and outputs, as the best description the runtime has states them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeDescription {
    /// The type `add_node` takes.
    pub node_type: String,
    /// Where its code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceListedOrigin,
    /// Where this description comes from.
    pub described_from: NodeTypeDescribedFrom,
    /// What it does, as it describes itself.
    pub documentation: String,
    /// The group it is filed under: the project's own, else an import's, else the one its author
    /// declared; null when none files it.
    pub category: Option<String>,
    /// Who filed it under `category`; null when nothing files it.
    #[serde(default)]
    pub category_from: Option<NodeTypeCategoryFrom>,
    /// How its code runs; null where its declaration source does not show it, and for a node
    /// type the graph resolves itself, which no node code runs.
    pub kind: Option<NodeTypeKind>,
    /// Where it is declared, its path relative to its source's root; absent on a built-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<ProjectFilePlaceOnTheWire>,
    /// Every input: each port that takes a wire, then each setting, which takes a value only.
    pub inputs: Vec<NodeTypeInputDescribed>,
    /// Every output.
    pub outputs: Vec<NodeTypePortDescribed>,
    /// The settings of a node type not yet typed that `set_exposed_config_fields` still takes;
    /// absent when it takes none. Served until the contract's version changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "json-schema", schemars(extend("x-removed-in" = 4)))]
    pub exposable_config_fields: Vec<String>,
    /// Each pin group: a slot of inputs and outputs that repeats on each node, each slot's ports
    /// named `<group>.<index>.<pin>` among the node's ports in `graph`; absent when it declares
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin_groups: Vec<NodeTypePinGroupDescribed>,
}

/// A pin group of a node type: a slot of inputs and outputs that repeats on each node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypePinGroupDescribed {
    /// The group's name, the first part of each of its slots' port names.
    pub name: String,
    /// What the group is for.
    pub documentation: String,
    /// How many slots a node holds.
    pub slots: NodeTypePinGroupSlots,
    /// The inputs each slot holds, each named as its pin; a slot's port is named
    /// `<group>.<index>.<pin>`.
    pub inputs: Vec<NodeTypeInputDescribed>,
    /// The outputs each slot holds, each named as its pin.
    pub outputs: Vec<NodeTypePortDescribed>,
}

/// How many slots a node holds in a pin group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeTypePinGroupSlots {
    /// One empty slot after the last a link reaches: wiring the empty slot adds the next.
    FollowTheirWires,
    /// As many as the node's input named after the group says, a setting read at setup.
    SetByTheInputOfTheGroupsName,
}

/// A node type named that the runtime could not describe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeNotDescribed {
    /// The type named.
    pub node_type: String,
    /// Where its code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceListedOrigin,
    /// Why it could not be described, with the call that checks its code when the runtime
    /// serves one.
    pub not_described_because: ControlOperationRefusalOnTheWire,
}

/// One port of a node type: its name, what it carries and how it is typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypePortDescribed {
    /// The port's name, what a link end and `set_node_input_value` name it by.
    pub name: String,
    /// What it carries, as declared beside it.
    pub documentation: String,
    /// Its pin type and flow; absent on a port whose type the code does not state exactly.
    #[serde(flatten)]
    pub pin: Option<NodeTypePortPin>,
    /// What it states besides its type, none of which decides a link.
    #[serde(default, skip_serializing_if = "PinAnnotations::is_empty")]
    pub annotations: PinAnnotations,
    /// The port family a port of a node type not yet typed declares, with its family's keys.
    /// Served until the contract's version changes, for a client that reads no `pin_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "json-schema", schemars(extend("x-removed-in" = 4)))]
    pub family: Option<Value>,
    /// A function or shader port's annotation as its source writes it, where only source
    /// describes its node type. Served until the contract's version changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "json-schema", schemars(extend("x-removed-in" = 4)))]
    pub annotation: Option<String>,
}

/// A port's pin type, or how it is found, and its flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypePortPin {
    /// The type it carries, or how that type is found.
    pub pin_type: DeclaredPinType,
    /// How its messages are read.
    pub flow: PinFlow,
}

/// One input of a node type: a port, with what a value set on it must be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeInputDescribed {
    /// Its name, documentation, pin type and annotations.
    #[serde(flatten)]
    pub port: NodeTypePortDescribed,
    /// Whether it takes a wire, a stored value or both; absent where only source describes its
    /// node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<NodeTypeInputMode>,
    /// Whether a link may end at it: `false` on a setting read each run, a setting of a type no
    /// message carries, every setting of a `GraphInput`, `GraphOutput`, `SetVariable` or
    /// `GetVariable`, and each legacy field of a node type not yet typed, which is no port.
    #[serde(default = "an_input_takes_a_wire_unless_it_says_not")]
    pub takes_a_wire: bool,
    /// When the node reads a setting: once at setup, a change rebuilding the node, or each run.
    /// Absent on an input that takes a wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<SettingReadTime>,
    /// The value it reads while nothing feeds it and no value is set, null for an input that
    /// reads nothing by default; absent when it declares none.
    #[serde(
        default,
        deserialize_with = "a_present_value_even_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_value: Option<Value>,
    /// Whether a value set live on it outlives Stop. Absent where only source describes its
    /// node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<NodeValueLifetime>,
    /// The JSON Schema a value set on it must satisfy, its range folded in; absent on an input
    /// that holds no stored value and where its type is found with the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_json_schema: Option<Value>,
}

/// An input that does not say whether it takes a wire is a port that takes one.
fn an_input_takes_a_wire_unless_it_says_not() -> bool {
    true
}

/// Whether an input takes a wire, a stored value, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeTypeInputMode {
    /// A link or a stored value, read each run.
    WireOrValue,
    /// A link only, read each run.
    WireOnly,
    /// A setting: a stored value, read when its `read` says; one read at setup also takes a
    /// link from an output that flows as a value, each change of either rebuilding its node.
    SettingOnly,
}

impl NodeTypeInputMode {
    /// The mode `mode` names, and when a setting of it is read.
    pub fn of(mode: NodeInputMode) -> (Self, Option<SettingReadTime>) {
        match mode {
            NodeInputMode::WireOrValue => (Self::WireOrValue, None),
            NodeInputMode::WireOnly => (Self::WireOnly, None),
            NodeInputMode::SettingOnly { read } => (Self::SettingOnly, Some(read)),
        }
    }
}

/// A key that is present deserializes to `Some`, `null` included, so a default of no value is
/// kept.
fn a_present_value_even_null<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}
