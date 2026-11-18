// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the engine renders for a reader: a stream's graph, built as the control contract's
//! `graph` answer, and the node catalog the REST description and the MCP node catalog serve.
//!
//! The catalog structs mirror the serialization output of the runtime types and are used for
//! generating JSON Schema files. They implement both `Serialize` and `JsonSchema` to ensure
//! schemas stay in sync with actual serialization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::context::StreamClockSpec;
use crate::core::descriptors::{
    DeclaredPinType, NodeInputMode, PinAnnotations, PortFamilyDeclaration, SettingReadTime,
};
use crate::core::graph::legacy_config_of_a_node::{
    ConfigAndPinValues, ConfigFieldDescribedAsASetting, LegacyConfigOfANodeType,
    config_of_a_node_as_drawn, config_of_a_node_holding_no_pin_value,
    input_values_of_a_config_and_pin_values, the_config_fields_a_node_of_the_type_can_expose,
    the_config_fields_described_as_settings,
};
use crate::core::graph::{GraphEdgeWithComponents, GraphNodeWithComponents};

pub use streamlib_node_schema::NodeClassImportPath;
pub use streamlib_runtime_client_contract::local_api_wire_contract::{
    ConfigNodeReferenceEdgeOnTheWire, FunctionNodeLanguageOnTheWire, GraphFragmentOnTheWire,
    GraphNodeKindOnTheWire, GraphNodeOnTheWire, GraphNodePortOnTheWire, GraphNodePortsOnTheWire,
    InputValueComesFromOnTheWire, InputValueReadOnTheWire, LabelledNodeGroup,
    MachineWideGraphOnTheWire, PortKindOnTheWire, StreamGraphOnTheWire,
};

use crate::core::compiler::compiler_ops::function_node_pool::LinkCarriedThroughTheFunctionNodePool;

// =============================================================================
// Graph Response (/api/graph and the `graph` operation)
// =============================================================================

/// One stream's graph as `graph` answers it: the control contract's, with each link and the
/// clock the engine renders, so it loads back as a
/// [`GraphSnapshot`](crate::core::graph::graph_snapshot::GraphSnapshot).
pub type GraphResponse = StreamGraphOnTheWire<LinkOutput, StreamClockSpec>;

/// Every loaded stream's graph under the machine's name, as `graph` answers it when no stream
/// is named.
pub type MachineWideGraphResponse = MachineWideGraphOnTheWire<LinkOutput, StreamClockSpec>;

/// A node of a stream's graph, as `graph` answers it.
pub type GraphNodeOutput = GraphNodeOnTheWire<LinkOutput>;

/// The nodes and links a fragment holds, as `graph` answers them.
pub type GraphFragmentOutput = GraphFragmentOnTheWire<LinkOutput>;

/// The values a node holds as `graph` renders them: `input_values`, as its graph document holds
/// them, and what they split into for a client that reads no `input_values`, the `config` with
/// its checksum and the `pins`.
pub(crate) struct InputValuesRendered {
    pub(crate) input_values: serde_json::Map<String, serde_json::Value>,
    pub(crate) config: serde_json::Value,
    pub(crate) config_checksum: u64,
    pub(crate) pins: serde_json::Map<String, serde_json::Value>,
}

impl InputValuesRendered {
    /// `input_values`, split as a node whose type `legacy_config` describes takes them.
    pub(crate) fn split_by(
        legacy_config: &LegacyConfigOfANodeType,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::split_as(input_values, legacy_config.split(input_values))
    }

    /// `input_values` of a node that has no input of its own to hold a value on, a variable or
    /// an interface node: every one is its config.
    pub(crate) fn of_a_node_holding_no_pin_value(
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::split_as(
            input_values,
            ConfigAndPinValues {
                config: config_of_a_node_holding_no_pin_value(input_values),
                pin_values: serde_json::Map::new(),
            },
        )
    }

    fn split_as(
        input_values: &serde_json::Map<String, serde_json::Value>,
        config_and_pin_values: ConfigAndPinValues,
    ) -> Self {
        let ConfigAndPinValues { config, pin_values } = config_and_pin_values;
        Self {
            input_values: input_values.clone(),
            config_checksum: crate::core::utils::compute_json_checksum(&config),
            config,
            pins: pin_values,
        }
    }
}

/// Redraw `node` from `input_values`, as a graph document holds them, over the values its inputs
/// already hold: every other value is its config. Its pins stay as they are.
pub(crate) fn redraw_a_node_over_its_pins(
    node: &mut GraphNodeOutput,
    input_values: &serde_json::Map<String, serde_json::Value>,
) {
    let config = config_of_a_node_as_drawn(input_values, &node.pins);
    node.config_checksum = crate::core::utils::compute_json_checksum(&config);
    node.config = Some(config);
    node.input_values = input_values.clone();
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

/// `node_kind` as the two keys a rendering spells it in: the kind, and a
/// function node's language.
fn node_kind_rendered_as_kind_and_language(
    node_kind: crate::core::descriptors::NodeKind,
) -> (
    GraphNodeKindOnTheWire,
    Option<FunctionNodeLanguageOnTheWire>,
) {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    match node_kind {
        NodeKind::Shader => (GraphNodeKindOnTheWire::Shader, None),
        NodeKind::Process => (GraphNodeKindOnTheWire::Process, None),
        NodeKind::Function { language } => (
            GraphNodeKindOnTheWire::Function,
            Some(match language {
                FunctionNodeLanguage::Python => FunctionNodeLanguageOnTheWire::Python,
                FunctionNodeLanguage::Rust => FunctionNodeLanguageOnTheWire::Rust,
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
    /// How this node type's code runs: `shader`, `function`, `process` or `fragment`.
    #[schemars(with = "String")]
    #[schema(value_type = String)]
    pub kind: GraphNodeKindOnTheWire,
    /// The language a function node is written in, `python` or `rust`; absent on every other
    /// kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    #[schema(value_type = Option<String>)]
    pub language: Option<FunctionNodeLanguageOnTheWire>,
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
    /// The config type's JSON Schema (draft 2020-12) of a node type not yet
    /// typed; each field it lists is also under `inputs` as a setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// The number and Boolean config fields `set_exposed_config_fields` takes on
    /// a node of this type not yet typed; absent when it takes none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposable_config_fields: Vec<String>,
    /// Whether the node paces itself on real time, as a live device does: in a
    /// stepped stream it stands outside the step barrier.
    #[serde(default)]
    pub paced_by_real_time: bool,
    /// Every input: each input port, then each setting, an input that takes
    /// no wire, which `set_node_input_value` sets by its name.
    pub inputs: Vec<PortDescriptorOutput>,
    /// Output port descriptors.
    pub outputs: Vec<PortDescriptorOutput>,
    /// Code examples in different languages.
    pub examples: CodeExamplesOutput,
}

impl NodeDescriptorOutput {
    /// Each input a link may end at: every input but a setting.
    pub fn inputs_taking_a_wire(&self) -> impl Iterator<Item = &PortDescriptorOutput> {
        self.inputs.iter().filter(|input| input.takes_a_wire())
    }
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
    /// The delivery profile this input port reads by — `"newest"` or
    /// `"ordered"`: the one it declares, else its pin's default; `None` on an
    /// output port.
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
    /// The port family this port declares and its family's keys. Absent on a
    /// setting, which carries nothing on a wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<PortFamilyDeclaration>,
    /// The pin type this port declares, or how it is found, as `pin_type`, and
    /// its flow, as `flow`. Both are absent from the rendering on a port whose
    /// type the code does not state exactly, which links by its family alone,
    /// and on a setting whose schema states no type exactly.
    #[serde(flatten)]
    pub pin: Option<PortDeclaredPinOutput>,
    /// Whether the input takes a wire, a stored value, or both, tagged by
    /// `mode`: `{"mode": "setting_only", "read": "at_setup"}` on a setting.
    /// Absent on an input port, and on an output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    #[schema(value_type = Option<Object>)]
    pub input_mode: Option<NodeInputMode>,
    /// The value a setting reads while no value is set on its node, as its
    /// schema states it. Absent on a port, and on a setting that states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    /// What a setting's schema states of it besides its type, such as the
    /// project assets a String holding an asset's UUID is picked from. Absent
    /// on a port, and on a setting that states nothing.
    #[serde(default, skip_serializing_if = "PinAnnotations::is_empty")]
    #[schemars(with = "Option<serde_json::Value>")]
    #[schema(value_type = Option<Object>)]
    pub annotations: PinAnnotations,
}

impl PortDescriptorOutput {
    /// Whether a link may end at this input: every input but a setting.
    pub fn takes_a_wire(&self) -> bool {
        !matches!(self.input_mode, Some(NodeInputMode::SettingOnly { .. }))
    }

    /// The input a config field of a node type not yet typed is: one that takes
    /// no wire, read when the node is set up.
    fn of_a_config_field(setting: ConfigFieldDescribedAsASetting) -> Self {
        Self {
            name: setting.name,
            description: setting.description,
            required: false,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            values_checked: None,
            family: None,
            pin: setting.pin_type.map(|pin_type| PortDeclaredPinOutput {
                flow: pin_type.default_flow(),
                pin_type: DeclaredPinType::Is { pin_type },
            }),
            input_mode: Some(NodeInputMode::SettingOnly {
                read: SettingReadTime::AtSetup,
            }),
            default_value: setting.default_value,
            annotations: setting.annotations,
        }
    }
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
            ports: ports_rendered_over_their_settled_contracts(node),
            input_values: input_values_of_a_config_and_pin_values(node.config.as_ref(), &node.pins),
            pins: node.pins.clone(),
            exposed_config_fields: node.exposed_config_fields.clone(),
            studio: node.studio.clone(),
            inputs_waiting_for_a_value: Vec::new(),
            components: node.serialize_components(),
            fragment: None,
        }
    }
}

/// The ports of `node`, each output with the stream facts it carries now and each input with a
/// `match_device` audio window replaced by whatever its own device stream settled it to.
fn ports_rendered_over_their_settled_contracts(
    node: &crate::core::graph::GraphNode,
) -> GraphNodePortsOnTheWire {
    let settled = node
        .get::<crate::core::graph::DeviceMatchedAudioWindowContractsComponent>()
        .map(|component| &*component.0);
    GraphNodePortsOnTheWire {
        inputs: node
            .ports
            .inputs
            .iter()
            .map(|port| input_port_rendered_over_any_settled_contract(port, settled))
            .collect(),
        outputs: node
            .ports
            .outputs
            .iter()
            .map(|port| GraphNodePortOnTheWire {
                stream_facts: node
                    .stream_facts_of_output_ports()
                    .current_facts_of(&port.name),
                ..GraphNodePortOnTheWire::from(port)
            })
            .collect(),
    }
}

/// One input port, with a `match_device` declaration replaced by whatever its own device stream
/// settled it to.
fn input_port_rendered_over_any_settled_contract(
    port: &crate::core::graph::PortInfo,
    settled: Option<&crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort>,
) -> GraphNodePortOnTheWire {
    let mut rendered = GraphNodePortOnTheWire::from(port);
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

/// `Some(false)` for a Records port, whose values no type checks; `None`
/// otherwise.
fn values_checked_of(family: &PortFamilyDeclaration) -> Option<bool> {
    (family.family == crate::core::descriptors::PortFamily::Records).then_some(false)
}

impl From<&crate::core::graph::PortInfo> for GraphNodePortOnTheWire {
    fn from(port: &crate::core::graph::PortInfo) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKindOnTheWire::default(),
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
            exposable_config_fields: the_config_fields_a_node_of_the_type_can_expose(
                &desc.node_class_import_path,
                desc.config_schema.as_ref(),
            )
            .into_iter()
            .map(|(field, _)| field)
            .collect(),
            paced_by_real_time: desc.paced_by_real_time,
            inputs: desc
                .inputs
                .iter()
                .map(PortDescriptorOutput::from)
                .chain(
                    the_config_fields_described_as_settings(desc)
                        .into_iter()
                        .map(PortDescriptorOutput::of_a_config_field),
                )
                .collect(),
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
            family: Some(port.family.clone()),
            pin: port.declared_pin().map(|declared| PortDeclaredPinOutput {
                pin_type: declared.pin_type,
                flow: declared.flow,
            }),
            input_mode: None,
            default_value: None,
            annotations: PinAnnotations::default(),
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
