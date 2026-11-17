// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! JSON Schema output types for API documentation.
//!
//! These structs mirror the serialization output of the runtime types and are used
//! for generating JSON Schema files. They implement both `Serialize` and `JsonSchema`
//! to ensure schemas stay in sync with actual serialization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::context::StreamClockSpec;
use crate::core::graph::{GraphEdgeWithComponents, GraphNodeWithComponents};

pub use streamlib_node_schema::NodeClassImportPath;

use crate::core::compiler::compiler_ops::function_node_pool::LinkCarriedThroughTheFunctionNodePool;

// =============================================================================
// Graph Response Schema (/api/graph)
// =============================================================================

/// Response from the `/api/graph` endpoint: the graph's spec — `stream`, `clock`,
/// `nodes[].name` / `type` / `config` and `links[].source` / `target` — with
/// the live keys beside it, so it loads back as a
/// [`GraphSnapshot`](crate::core::graph::graph_snapshot::GraphSnapshot).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphResponse {
    /// The UUID of the stream asset whose graph document this graph was loaded from: a spec key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// The name the stream is shown under: a spec key, which its graph document writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The stream id this graph was loaded under, its stream's address. Absent until a load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// The clock the stream runs on. Absent when it runs on real time.
    #[serde(default, skip_serializing_if = "StreamClockSpec::is_real")]
    pub clock: StreamClockSpec,
    /// All nodes in the graph.
    pub nodes: Vec<GraphNodeOutput>,
    /// All links between nodes' ports.
    pub links: Vec<LinkOutput>,
    /// Every reference edge: a node's config key naming another node. A live
    /// key, read back from the configs; absent when the graph holds none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ConfigNodeReferenceEdgeOutput>,
    /// The name of the machine the stream runs on, the first part of
    /// `<machine>/<stream id>/<node>/<port>`.
    pub machine: String,
    /// Every labelled group of the stream's own level: a spec key, read by nothing that runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,
    /// What Tato Studio draws beside the nodes, its canvas notes, under the studio contract: a
    /// spec key, read by nothing that runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<serde_json::Value>,
    /// The stream's function node pool, rendered as a node is: a live key, absent when no
    /// island runs in it. It is no node of the graph, so it is never a link's end; each link
    /// it carries renders as the link it was given, naming the pool's ports under
    /// `carried_through_the_function_node_pool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_node_pool: Option<GraphNodeOutput>,
}

/// A labelled group of one level's nodes, which a node joins by naming its `id` as its
/// `studio.group`; it changes nothing that runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
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

/// Response from `/api/graph` and the `graph` tool when no stream is named:
/// every loaded stream's graph under the machine's name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MachineWideGraphResponse {
    /// The machine's name, the first part of
    /// `<machine>/<stream id>/<node>/<port>`.
    pub machine: String,
    /// Each loaded stream's graph, as a call naming that stream returns it.
    pub streams: Vec<GraphResponse>,
}

/// A node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphNodeOutput {
    /// The node's UUID: a spec key, kept across every run and restart of its stream.
    pub id: String,
    /// The import path of the class this node is — a plain string.
    #[serde(rename = "type")]
    pub node_type: NodeClassImportPath,
    /// How the node's code runs. Absent on a node whose type nothing
    /// registers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NodeKindOutput>,
    /// The language a function node is written in; absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<FunctionNodeLanguageOutput>,
    /// The node's name, unique in its graph and cast to lowercase URL-safe —
    /// what a link end, an exposure and a port address name it by.
    pub name: String,
    /// Node configuration as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    /// Input and output ports.
    pub ports: GraphNodePortsOutput,
    /// The values set on the node's unlinked inputs, by input name.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub pins: serde_json::Map<String, serde_json::Value>,
    /// The config fields the node takes as inputs too, each an input of the field's name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_config_fields: Vec<String>,
    /// What the studio draws the node with: canvas position, group, comment, colour.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub studio: serde_json::Map<String, serde_json::Value>,
    /// The inputs of a function node that no link feeds, that carry no pin value and that
    /// declare no default; the node holds without evaluating until each has a value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs_waiting_for_a_value: Vec<String>,
    pub components: serde_json::Map<String, serde_json::Value>,
    /// What a fragment holds: its graph generator's stored output, each node rendered as this
    /// node is. A spec key, present on a fragment alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(no_recursion)]
    pub fragment: Option<GraphFragmentOutput>,
}

/// The nodes and links a fragment holds, each link end naming a node of the fragment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphFragmentOutput {
    /// Every node of the fragment, a fragment it holds rendered as a container in turn.
    #[schema(no_recursion)]
    pub nodes: Vec<GraphNodeOutput>,
    /// Every link between those nodes' ports.
    pub links: Vec<LinkOutput>,
    /// Every labelled group of the fragment's nodes, made by its graph generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,
}

/// Container for node input and output ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphNodePortsOutput {
    /// Input ports that receive data.
    pub inputs: Vec<PortInfoOutput>,
    /// Output ports that send data.
    pub outputs: Vec<PortInfoOutput>,
}

/// Metadata about a port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortInfoOutput {
    /// Port name (e.g., "video_in", "audio_out").
    pub name: String,
    /// Human-readable description declared alongside the port.
    #[serde(default)]
    pub description: String,
    /// Kind of port: data, event, or control.
    #[serde(default)]
    pub port_kind: PortKindOutput,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// The samples each link into this input port may hold beyond its mailbox,
    /// declared; absent from the rendering on a port that takes the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none — the contract is opt-in, and a
    /// port without one renders exactly what it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
    /// The stream facts this output port carries now, tagged by family. Absent
    /// from the rendering on a port that states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_facts: Option<crate::core::descriptors::PortStreamFacts>,
    /// `false` on a Records port: its values are not checked against a type.
    /// Absent on every other family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_checked: Option<bool>,
    /// The port family this port declares and its family's keys. Absent from
    /// the rendering on a graph variable's value port, which keeps the family
    /// of the setter's source port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<crate::core::descriptors::PortFamilyDeclaration>,
    /// The value this input reads while nothing is linked into it and no pin
    /// value is set on its node. Absent from the rendering on a port that
    /// declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_default: Option<serde_json::Value>,
    /// The pin type this port carries, resolved, as `pin_type`, and its flow,
    /// as `flow`. Both are absent from the rendering on a port whose type the
    /// code does not state exactly, which links by its family alone.
    #[serde(flatten)]
    pub pin: Option<crate::core::descriptors::ResolvedPin>,
    /// On an input of a node of a live graph, the value it reads while unlinked, as `value`,
    /// and where that value comes from, as `value_comes_from`. Both are absent from the
    /// rendering on an input that takes no stored value and on a catalog port.
    #[serde(flatten)]
    pub value_read: Option<crate::core::graph::input_values::InputValueRead>,
}

/// The kind of port - determines how data flows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortKindOutput {
    #[default]
    Data,
    Event,
    Control,
}

/// A link (connection) between two node ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkOutput {
    /// Unique identifier for this link.
    pub id: String,
    /// Source endpoint (output port).
    pub source: LinkPortRefOutput,
    /// Target endpoint (input port).
    pub target: LinkPortRefOutput,
    /// Ring buffer capacity for the channel.
    #[serde(default)]
    pub capacity: usize,
    /// Current state of the link.
    #[serde(default)]
    pub state: LinkStateOutput,
    /// Why the link is in the `error` state, in the words of the helper
    /// process that could not open its port, or of the graph's validation that
    /// refused it as its stream loaded.
    ///
    /// Absent in every other state, so a reader that finds it knows the link
    /// carries nothing and will not start to: disconnect it and wire again.
    /// `state` stays a plain string so a check against `"wired"` is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Runtime components (dynamic, varies based on link state).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// One end of a link: a port on a node this graph holds. A link's ends are
/// both in one stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkPortRefOutput {
    /// The node's name.
    pub node: String,
    /// The port's own name on that node.
    pub port: String,
}

impl LinkPortRefOutput {
    fn of_a_port(
        node_run_id: &crate::core::graph::NodeRunId,
        port_name: &str,
        node_names: &NodeNamesByNodeRunId,
    ) -> Self {
        Self {
            node: node_names.name_of(node_run_id),
            port: port_name.to_string(),
        }
    }
}

/// One reference edge: the config key `config_key` of node `node` names node
/// `referenced_node`. It carries no data and is never a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ConfigNodeReferenceEdgeOutput {
    /// The name of the node whose config holds the reference.
    pub node: String,
    /// The config key the reference sits under.
    pub config_key: String,
    /// The name of the node the reference names.
    pub referenced_node: String,
}

/// Every node's name by its node id, for rendering the ends a graph
/// holds by id under the name a reader knows them by.
pub(crate) struct NodeNamesByNodeRunId(
    std::collections::HashMap<crate::core::graph::NodeRunId, String>,
);

impl NodeNamesByNodeRunId {
    /// Index every node `nodes` yields.
    pub(crate) fn of<'node>(
        nodes: impl Iterator<Item = &'node crate::core::graph::GraphNode>,
    ) -> Self {
        Self(
            nodes
                .map(|node| (node.id.clone(), node.display_name.clone()))
                .collect(),
        )
    }

    /// An index holding no node, so every end renders under its id.
    #[cfg(test)]
    pub(crate) fn holding_no_node() -> Self {
        Self(std::collections::HashMap::new())
    }

    fn name_of(&self, node_run_id: &crate::core::graph::NodeRunId) -> String {
        self.0
            .get(node_run_id)
            .cloned()
            .unwrap_or_else(|| node_run_id.to_string())
    }
}

/// State of a link in the graph.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkStateOutput {
    /// Link exists in graph but not yet wired.
    #[default]
    Pending,
    /// Link is actively wired with a ring buffer channel.
    Wired,
    /// Link is being disconnected.
    Disconnecting,
    /// Link was disconnected.
    Disconnected,
    /// Link is in error state.
    Error,
}

// =============================================================================
// Registry Response Schema (/api/registry)
// =============================================================================

/// Response from the `/api/registry` endpoint, and the MCP node catalog.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct RegistryResponse {
    /// Every node type this process can add, with its descriptor.
    pub nodes: Vec<NodeDescriptorOutput>,
    /// The five port families, each with its description.
    pub families: Vec<PortFamilyDescriptorOutput>,
}

/// One port family, described as a node type is.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PortFamilyDescriptorOutput {
    /// The family, as a graph, a config and the wire spell it.
    pub family: crate::core::descriptors::PortFamily,
    /// What a port of this family carries: CommonMark, as declared.
    pub description: String,
}

impl PortFamilyDescriptorOutput {
    /// Every port family, in declaration order.
    pub fn every_port_family() -> Vec<Self> {
        crate::core::descriptors::PortFamily::ALL
            .into_iter()
            .map(|family| Self {
                family,
                description: family.description().to_string(),
            })
            .collect()
    }
}

/// Response from `/api/registry` when no stream is named, and the MCP node
/// catalog: the native types every stream can add, then each loaded stream's
/// Python types under that stream.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MachineWideRegistryResponse {
    /// The node types compiled into this runtime, which any stream can add.
    pub nodes: Vec<NodeDescriptorOutput>,
    /// The five port families, each with its description.
    pub families: Vec<PortFamilyDescriptorOutput>,
    /// Each loaded stream's types described in its own interpreter.
    pub streams: Vec<NodeTypesDescribedInOneStreamOutput>,
}

/// The node types one loaded stream's own interpreter described.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeTypesDescribedInOneStreamOutput {
    /// The stream's id.
    pub stream: String,
    /// The types described in its interpreter that a graph or a live add
    /// named, which only this stream adds.
    pub nodes: Vec<NodeDescriptorOutput>,
    /// The types installed modules declare in the stream's environment, under
    /// its `tatolab.nodes` entry points, which only this stream adds.
    pub installed: Vec<NodeTypeAnInstalledModuleDeclaresOutput>,
}

/// One node type an installed distribution declares in `tatolab.nodes`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeTypeAnInstalledModuleDeclaresOutput {
    /// The distribution declaring it, as `pip` names it.
    pub distribution: String,
    /// The distribution's installed version.
    pub distribution_version: String,
    /// The entry point's name in `tatolab.nodes`.
    pub entry_point: String,
    /// The node type, as the stream's interpreter described it.
    pub node: NodeDescriptorOutput,
}

/// How a node type's code runs: a shader on the GPU, a function evaluated
/// where its reader reads, or a class in a process of its own.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum NodeKindOutput {
    Shader,
    Function,
    Process,
    /// A graph generator's stored output, flattened into the nodes it holds when it runs.
    Fragment,
}

/// The language a function node is written in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FunctionNodeLanguageOutput {
    Python,
    Rust,
}

/// `node_kind` as the two keys a rendering spells it in: the kind, and a
/// function node's language.
fn node_kind_rendered_as_kind_and_language(
    node_kind: crate::core::descriptors::NodeKind,
) -> (NodeKindOutput, Option<FunctionNodeLanguageOutput>) {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    match node_kind {
        NodeKind::Shader => (NodeKindOutput::Shader, None),
        NodeKind::Process => (NodeKindOutput::Process, None),
        NodeKind::Function { language } => (
            NodeKindOutput::Function,
            Some(match language {
                FunctionNodeLanguage::Python => FunctionNodeLanguageOutput::Python,
                FunctionNodeLanguage::Rust => FunctionNodeLanguageOutput::Rust,
            }),
        ),
    }
}

/// Runtime environment for a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum NodeRuntimeOutput {
    #[default]
    Rust,
    Python,
}

/// Descriptor for a node type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct NodeDescriptorOutput {
    /// The import path of the class this node type is — the `type` a graph
    /// node carries and `add_node` takes.
    #[serde(rename = "type")]
    pub node_class_import_path: NodeClassImportPath,
    /// How this node type's code runs.
    pub kind: NodeKindOutput,
    /// The language a function node is written in; absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<FunctionNodeLanguageOutput>,
    /// Human-readable description.
    pub description: String,
    /// The palette category an editor files this node type under; absent on a
    /// type that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Repository URL.
    pub repository: String,
    /// Runtime environment.
    #[serde(default)]
    pub runtime: NodeRuntimeOutput,
    /// Entrypoint for non-Rust runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// The config type's JSON Schema, as JSON Schema draft 2020-12 — what an
    /// agent reads to learn which keys this node's config takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// Whether the node paces itself on real time, as a live device does: in a
    /// stepped stream it stands outside the step barrier.
    #[serde(default)]
    pub paced_by_real_time: bool,
    /// Input port descriptors.
    pub inputs: Vec<PortDescriptorOutput>,
    /// Output port descriptors.
    pub outputs: Vec<PortDescriptorOutput>,
    /// Code examples in different languages.
    pub examples: CodeExamplesOutput,
}

/// Descriptor for a node port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortDescriptorOutput {
    /// Port name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Whether the port is required.
    pub required: bool,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// The samples each link into this input port may hold beyond its mailbox,
    /// declared; absent from the rendering on a port that takes the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
    /// `false` on a Records port: its values are not checked against a type.
    /// Absent on every other family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_checked: Option<bool>,
    /// The port family this port declares and its family's keys.
    pub family: crate::core::descriptors::PortFamilyDeclaration,
    /// The pin type this port declares, or how it is found, as `pin_type`, and
    /// its flow, as `flow`. Both are absent from the rendering on a port whose
    /// type the code does not state exactly, which links by its family alone.
    #[serde(flatten)]
    pub pin: Option<PortDeclaredPinOutput>,
}

/// The pin type a catalog port declares, and its flow.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortDeclaredPinOutput {
    /// The type it carries, or how that type is found. Its schema here is open
    /// JSON: only the control contract's schemars renders pin types.
    #[schemars(with = "serde_json::Value")]
    #[schema(value_type = Object)]
    pub pin_type: crate::core::descriptors::DeclaredPinType,
    /// How its messages are read: `value`, `stream` or `trigger`.
    #[schemars(with = "String")]
    #[schema(value_type = String)]
    pub flow: crate::core::descriptors::PinFlow,
}

/// Code examples for a node in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct CodeExamplesOutput {
    /// Rust code example.
    pub rust: String,
    /// Python code example.
    pub python: String,
    /// TypeScript code example.
    pub typescript: String,
}

// =============================================================================
// Conversion from Runtime Types
// =============================================================================

impl From<&crate::core::graph::GraphNode> for GraphNodeOutput {
    fn from(node: &crate::core::graph::GraphNode) -> Self {
        let (kind, language) = match node.node_kind() {
            Some(node_kind) => {
                let (kind, language) = node_kind_rendered_as_kind_and_language(node_kind);
                (Some(kind), language)
            }
            None => (None, None),
        };
        Self {
            id: node.node_uuid.to_string(),
            node_type: node.node_type.clone(),
            kind,
            language,
            name: node.display_name.clone(),
            config: node.config.clone(),
            config_checksum: node.config_checksum,
            pins: node.pins.clone(),
            exposed_config_fields: node.exposed_config_fields.clone(),
            studio: node.studio.clone(),
            inputs_waiting_for_a_value: Vec::new(),
            ports: GraphNodePortsOutput::rendered_for_a_node_over_its_settled_contracts(node),
            components: node.serialize_components(),
            fragment: None,
        }
    }
}

impl GraphNodePortsOutput {
    fn rendered_for_a_node_over_its_settled_contracts(
        node: &crate::core::graph::GraphNode,
    ) -> Self {
        let settled = node
            .get::<crate::core::graph::DeviceMatchedAudioWindowContractsComponent>()
            .map(|component| &*component.0);
        Self {
            inputs: node
                .ports
                .inputs
                .iter()
                .map(|port| PortInfoOutput::rendered_over_any_settled_contract(port, settled))
                .collect(),
            outputs: node
                .ports
                .outputs
                .iter()
                .map(|port| PortInfoOutput {
                    stream_facts: node
                        .stream_facts_of_output_ports()
                        .current_facts_of(&port.name),
                    ..PortInfoOutput::from(port)
                })
                .collect(),
        }
    }
}

impl PortInfoOutput {
    /// One input port, with a `match_device` declaration replaced by whatever
    /// its own device stream settled it to.
    fn rendered_over_any_settled_contract(
        port: &crate::core::graph::PortInfo,
        settled: Option<&crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort>,
    ) -> Self {
        let mut rendered = PortInfoOutput::from(port);
        if !matches!(
            rendered.audio_window,
            Some(crate::core::descriptors::AudioWindowContract::MatchDevice {})
        ) {
            return rendered;
        }
        if let Some(values) =
            settled.and_then(|contracts| contracts.settled_declaration_for_input_port(&port.name))
        {
            rendered.audio_window = Some(crate::core::descriptors::AudioWindowContract::Device(
                values,
            ));
        }
        rendered
    }
}

/// `Some(false)` for a Records port, whose values no type checks; `None`
/// otherwise.
fn values_checked_of(family: &crate::core::descriptors::PortFamilyDeclaration) -> Option<bool> {
    (family.family == crate::core::descriptors::PortFamily::Records).then_some(false)
}

impl From<&crate::core::graph::PortInfo> for PortInfoOutput {
    fn from(port: &crate::core::graph::PortInfo) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKindOutput::from(port.port_kind),
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            stream_facts: None,
            values_checked: values_checked_of(&port.family),
            family: Some(port.family.clone()),
            pin_default: port.pin_default.clone(),
            pin: port.pin.clone(),
            value_read: None,
        }
    }
}

impl From<crate::core::graph::PortKind> for PortKindOutput {
    fn from(kind: crate::core::graph::PortKind) -> Self {
        match kind {
            crate::core::graph::PortKind::Data => PortKindOutput::Data,
            crate::core::graph::PortKind::Event => PortKindOutput::Event,
            crate::core::graph::PortKind::Control => PortKindOutput::Control,
        }
    }
}

impl LinkOutput {
    /// Render `link` with each end on this runtime named by its node's name.
    pub(crate) fn of_a_link(
        link: &crate::core::graph::Link,
        node_names: &NodeNamesByNodeRunId,
    ) -> Self {
        let rendered = RenderedLinkState::of(link);
        let mut components = link.serialize_components();
        // `LinkStateComponent` renders under `components.state` too, and for a
        // link waiting on a helper it holds the `Pending` the op stamped. Left
        // alone it would contradict the top-level state a reader was told to
        // check — the same disagreement the top-level state reads the component
        // to avoid, in the other direction.
        let (state, error_reason) = rendered.into_state_and_its_reason();
        render_the_state_component_as(&mut components, state);
        Self {
            id: link.id.to_string(),
            source: LinkPortRefOutput::of_a_port(
                link.source.node_run_id(),
                link.source.port_name(),
                node_names,
            ),
            target: LinkPortRefOutput::of_a_port(
                link.target.node_run_id(),
                link.target.port_name(),
                node_names,
            ),
            capacity: link.capacity.get(),
            state,
            error_reason,
            components,
        }
    }

    /// Render `carried`, a link the graph carries through the function node pool as two links,
    /// as it was given: from the island to the reader, naming the pool's ports it runs through,
    /// in the error either of its links is in, else in the state of the link out of the pool.
    pub(crate) fn of_a_link_carried_through_the_function_node_pool(
        carried: &LinkCarriedThroughTheFunctionNodePool<'_>,
        node_names: &NodeNamesByNodeRunId,
    ) -> Self {
        let into_the_pool = Self::of_a_link(carried.into_the_function_node_pool, node_names);
        let out_of_the_pool = Self::of_a_link(carried.out_of_the_function_node_pool, node_names);
        let (state, error_reason) = if into_the_pool.state == LinkStateOutput::Error {
            (into_the_pool.state, into_the_pool.error_reason)
        } else {
            (out_of_the_pool.state, out_of_the_pool.error_reason)
        };
        let mut components = out_of_the_pool.components;
        components.insert(
            "carried_through_the_function_node_pool".to_string(),
            serde_json::json!({
                "input_port": into_the_pool.target.port,
                "output_port": out_of_the_pool.source.port,
            }),
        );
        render_the_state_component_as(&mut components, state);
        Self {
            id: carried.carried_link_id.to_string(),
            source: into_the_pool.source,
            target: out_of_the_pool.target,
            capacity: out_of_the_pool.capacity,
            state,
            error_reason,
            components,
        }
    }
}

/// Set a link's `components.state`, which `LinkStateComponent` renders, to the state the link
/// reports, so the two never disagree.
fn render_the_state_component_as(
    components: &mut serde_json::Map<String, serde_json::Value>,
    state: LinkStateOutput,
) {
    if let Some(rendered_state) = components.get_mut("state") {
        *rendered_state = serde_json::json!(format!("{state:?}"));
    }
}

/// What a link reports as its state, and why where that is `error`.
enum RenderedLinkState {
    WithoutAReason(LinkStateOutput),
    ErrorBecause(String),
}

impl RenderedLinkState {
    /// Read one link's state off whichever cell holds the live answer.
    ///
    /// Wiring records its outcome on the component; the field is the state the
    /// link was created in. A link handed to an out-of-process end sits at
    /// `Pending` there until that end answers — the answer lands on a cell its
    /// bridge's reader thread fills, which holds no graph lock — so a link still
    /// carrying those cells reads its state off them instead. Once the
    /// disconnect path has moved the component past `Pending`, that
    /// stamp is the answer: a link on its way out is not `wired` because a
    /// helper once said so.
    fn of(link: &crate::core::graph::Link) -> Self {
        let stamped = link
            .get::<crate::core::graph::LinkStateComponent>()
            .map(|state| state.0)
            .unwrap_or(link.state);
        if stamped != crate::core::graph::LinkState::Pending {
            return Self::WithoutAReason(LinkStateOutput::from(stamped));
        }
        let Some(replies) = link.get::<crate::core::graph::OutOfProcessLinkWireRepliesComponent>()
        else {
            return Self::WithoutAReason(LinkStateOutput::from(stamped));
        };
        match replies.what_its_out_of_process_ends_have_answered() {
            crate::core::graph::OutOfProcessLinkWireProgress::AnEndHasNotAnsweredYet => {
                Self::WithoutAReason(LinkStateOutput::Pending)
            }
            crate::core::graph::OutOfProcessLinkWireProgress::EveryEndOpenedItsPort => {
                Self::WithoutAReason(LinkStateOutput::Wired)
            }
            crate::core::graph::OutOfProcessLinkWireProgress::AnEndRefused { reason } => {
                Self::ErrorBecause(reason)
            }
        }
    }

    /// The state, then its `error_reason`.
    fn into_state_and_its_reason(self) -> (LinkStateOutput, Option<String>) {
        match self {
            Self::WithoutAReason(state) => (state, None),
            Self::ErrorBecause(reason) => (LinkStateOutput::Error, Some(reason)),
        }
    }
}

impl From<crate::core::graph::LinkState> for LinkStateOutput {
    fn from(state: crate::core::graph::LinkState) -> Self {
        match state {
            crate::core::graph::LinkState::Pending => LinkStateOutput::Pending,
            crate::core::graph::LinkState::Wired => LinkStateOutput::Wired,
            crate::core::graph::LinkState::Disconnecting => LinkStateOutput::Disconnecting,
            crate::core::graph::LinkState::Disconnected => LinkStateOutput::Disconnected,
            crate::core::graph::LinkState::Error => LinkStateOutput::Error,
        }
    }
}

impl From<&crate::core::NodeDescriptor> for NodeDescriptorOutput {
    fn from(desc: &crate::core::NodeDescriptor) -> Self {
        let (kind, language) = node_kind_rendered_as_kind_and_language(desc.node_kind);
        Self {
            node_class_import_path: desc.node_class_import_path.clone(),
            kind,
            language,
            description: desc.description.clone(),
            category: desc.category.clone(),
            repository: desc.repository.clone(),
            runtime: NodeRuntimeOutput::from(&desc.runtime),
            entrypoint: desc.entrypoint.clone(),
            config_schema: desc.config_schema.clone(),
            paced_by_real_time: desc.paced_by_real_time,
            inputs: desc.inputs.iter().map(PortDescriptorOutput::from).collect(),
            outputs: desc
                .outputs
                .iter()
                .map(PortDescriptorOutput::from)
                .collect(),
            examples: CodeExamplesOutput::from(&desc.examples),
        }
    }
}

impl From<&crate::core::NodeRuntime> for NodeRuntimeOutput {
    fn from(runtime: &crate::core::NodeRuntime) -> Self {
        match runtime {
            crate::core::NodeRuntime::Rust => NodeRuntimeOutput::Rust,
            crate::core::NodeRuntime::Python => NodeRuntimeOutput::Python,
        }
    }
}

impl From<&crate::core::PortDescriptor> for PortDescriptorOutput {
    fn from(port: &crate::core::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            required: port.required,
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            values_checked: values_checked_of(&port.family),
            family: port.family.clone(),
            pin: port.declared_pin().map(|declared| PortDeclaredPinOutput {
                pin_type: declared.pin_type,
                flow: declared.flow,
            }),
        }
    }
}

impl From<&crate::core::CodeExamples> for CodeExamplesOutput {
    fn from(examples: &crate::core::CodeExamples) -> Self {
        Self {
            rust: examples.rust.clone(),
            python: examples.python.clone(),
            typescript: examples.typescript.clone(),
        }
    }
}

#[cfg(test)]
mod link_rendering_tests;

#[cfg(test)]
mod port_rendering_tests;

#[cfg(test)]
mod pin_type_rendering_tests;

#[cfg(test)]
mod graph_response_top_level_key_rendering_tests;

#[cfg(test)]
mod config_schema_rendering_tests;
