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

use crate::core::descriptors::{DeclaredPinType, NodeInputMode, PinAnnotations, PinLinkVerdict};
use crate::core::graph::{GraphEdgeWithComponents, GraphNodeWithComponents};
use crate::core::nodes::{ConfigFieldDescribedAsASetting, the_config_fields_described_as_settings};

use crate::core::compiler::compiler_ops::function_node_pool::LinkCarriedThroughTheFunctionNodePool;

mod inputs_and_settings_of_a_node;
pub(crate) use inputs_and_settings_of_a_node::{
    InputsAndSettingsOfANodeType, redraw_a_node_over_the_values_its_inputs_hold,
    the_values_held_on_no_input_of,
};
pub use streamlib_node_schema::NodeClassImportPath;
use streamlib_runtime_client_contract::local_api_wire_contract::DiagnosticKindOnTheWire;
pub use streamlib_runtime_client_contract::local_api_wire_contract::{
    ConfigNodeReferenceEdgeOnTheWire, FunctionNodeLanguageOnTheWire, GraphFragmentOnTheWire,
    GraphLinkComponentsOnTheWire, GraphLinkOnTheWire, GraphLinkPortRefOnTheWire,
    GraphLinkStateOnTheWire, GraphNodeComponentsOnTheWire, GraphNodeKindOnTheWire,
    GraphNodeOnTheWire, GraphNodePortOnTheWire, GraphNodePortsOnTheWire,
    InputValueComesFromOnTheWire, InputValueReadOnTheWire, LabelledNodeGroup,
    LinkCarriedThroughTheFunctionNodePoolOnTheWire, MachineWideGraphOnTheWire,
    PinGroupSlotOnTheWire, PinLinkMarksOnTheWire, PortKindOnTheWire, PortStreamFactsOnTheWire,
    StreamGraphOnTheWire,
};

// =============================================================================
// Graph Response (the `graph` operation)
// =============================================================================

/// One stream's graph as `graph` answers it: the control contract's, with each link and the
/// clock the engine renders, so it loads back as a
/// [`GraphSnapshot`](crate::core::graph::graph_snapshot::GraphSnapshot).
pub type GraphResponse = StreamGraphOnTheWire;

/// Every loaded stream's graph under the machine's name, as `graph` answers it when no stream
/// is named.
pub type MachineWideGraphResponse = MachineWideGraphOnTheWire;

/// A node of a stream's graph, as `graph` answers it.
pub type GraphNodeOutput = GraphNodeOnTheWire;

/// The nodes and links a fragment holds, as `graph` answers them.
pub type GraphFragmentOutput = GraphFragmentOnTheWire;

/// A link of a stream's graph, as `graph` answers it.
pub type LinkOutput = GraphLinkOnTheWire;

/// One end of a link, a port on a node the graph holds, named by its node's name. A link's ends
/// are both in one stream.
pub type LinkPortRefOutput = GraphLinkPortRefOnTheWire;

/// The state of a link, as `graph` answers it.
pub type LinkStateOutput = GraphLinkStateOnTheWire;

/// The end of a link at `port_name` of the node `node_run_id`, named by its node's name.
fn link_end_of_a_port(
    node_run_id: &crate::core::graph::NodeRunId,
    port_name: &str,
    node_names: &NodeNamesByNodeRunId,
) -> LinkPortRefOutput {
    LinkPortRefOutput {
        node: node_names.name_of(node_run_id),
        port: port_name.to_string(),
        member: None,
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

// =============================================================================
// Node Catalog Response Schema (the MCP node catalog)
// =============================================================================

/// One list of the MCP node catalog: its node types.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegistryResponse {
    /// Every node type this process can add, with its descriptor.
    pub nodes: Vec<NodeDescriptorOutput>,
}

/// The MCP node catalog: the native types every stream can add, then each loaded stream's
/// Python types under that stream.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MachineWideRegistryResponse {
    /// The node types compiled into this runtime, which any stream can add.
    pub nodes: Vec<NodeDescriptorOutput>,
    /// Each loaded stream's types described in its own interpreter.
    pub streams: Vec<NodeTypesDescribedInOneStreamOutput>,
}

/// The node types one loaded stream's own interpreter described.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NodeRuntimeOutput {
    #[default]
    Rust,
    Python,
}

/// A string schema naming one of `names`.
fn one_of_the_names_schema(names: impl IntoIterator<Item = &'static str>) -> schemars::Schema {
    let names: Vec<serde_json::Value> = names.into_iter().map(serde_json::Value::from).collect();
    schemars::json_schema!({ "type": "string", "enum": names })
}

fn every_node_kind_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    one_of_the_names_schema(GraphNodeKindOnTheWire::EVERY_KIND.map(GraphNodeKindOnTheWire::name))
}

fn every_function_node_language_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    one_of_the_names_schema(
        FunctionNodeLanguageOnTheWire::EVERY_LANGUAGE.map(FunctionNodeLanguageOnTheWire::name),
    )
}

/// Descriptor for a node type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeDescriptorOutput {
    /// The import path of the class this node type is — the `type` a graph
    /// node carries and `add_node` takes.
    #[serde(rename = "type")]
    pub node_class_import_path: NodeClassImportPath,
    /// How this node type's code runs.
    #[schemars(schema_with = "every_node_kind_schema")]
    pub kind: GraphNodeKindOnTheWire,
    /// The language a function node is written in; absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "every_function_node_language_schema")]
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

/// Descriptor for a node port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    /// `false` on a port of no pin type: its values are not checked against a
    /// type. Absent on every other port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_checked: Option<bool>,
    /// The pin type this port declares, or how it is found, as `pin_type`, and
    /// its flow, as `flow`. Both are absent from the rendering on a port whose
    /// type the code does not state, which takes the type of its first link,
    /// and on a setting whose schema states no type exactly.
    #[serde(flatten)]
    pub pin: Option<PortDeclaredPinOutput>,
    /// Whether the input takes a wire, a stored value, or both, tagged by
    /// `mode`: `{"mode": "setting_only"}` on an input handed to its node, and
    /// the mode an input port declares. Absent on an input port that declares
    /// none, and on an output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<NodeInputMode>,
    /// The value a setting reads while no value is set on its node, as its
    /// schema or its declaration states it. Absent on an input that is no
    /// setting, and on a setting that states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    /// What it states besides its type: a port's range, step, unit, meaning and
    /// control, as its pin declares them, and a setting's, as its schema states
    /// them, such as the project assets a String holding an asset's UUID is
    /// picked from. Absent on one that states nothing.
    #[serde(default, skip_serializing_if = "PinAnnotations::is_empty")]
    pub annotations: PinAnnotations,
}

impl PortDescriptorOutput {
    /// The input a config field of a node type not yet typed is: a setting read
    /// when the node is set up, which takes no wire until its type is typed.
    fn of_a_config_field(setting: ConfigFieldDescribedAsASetting) -> Self {
        Self {
            name: setting.name,
            description: setting.description,
            required: false,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            values_checked: None,
            pin: setting.pin_type.map(|pin_type| PortDeclaredPinOutput {
                flow: pin_type.default_flow(),
                pin_type: DeclaredPinType::Is { pin_type },
            }),
            input_mode: Some(NodeInputMode::HandedToItsNode),
            default_value: setting.default_value,
            annotations: setting.annotations,
        }
    }
}

/// The pin type a catalog port declares, and its flow.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PortDeclaredPinOutput {
    /// The type it carries, or how that type is found.
    pub pin_type: crate::core::descriptors::DeclaredPinType,
    /// How its messages are read: `value`, `stream` or `trigger`.
    #[schemars(with = "String")]
    pub flow: crate::core::descriptors::PinFlow,
}

/// Code examples for a node in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
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
            ports: ports_rendered_over_their_settled_contracts(node),
            input_values: node.input_values.clone(),
            studio: node.studio.clone(),
            inputs_waiting_for_a_value: Vec::new(),
            components: the_components_of_a_node_with_its_health(node),
            fragment: None,
            live: None,
        }
    }
}

/// Every component of `node` `graph` renders, with the node's health and each diagnostic
/// behind it.
fn the_components_of_a_node_with_its_health(
    node: &crate::core::graph::GraphNode,
) -> GraphNodeComponentsOnTheWire {
    let mut components = node.render_components();
    if let Some((health, diagnostics)) = node.health_and_diagnostics() {
        components.health = Some(health);
        components.diagnostics = diagnostics;
    }
    components
}

/// The ports of `node`, each output with the stream facts it carries now and each input with a
/// `match_device` audio window replaced by whatever its own device stream settled it to.
fn ports_rendered_over_their_settled_contracts(
    node: &crate::core::graph::GraphNode,
) -> GraphNodePortsOnTheWire {
    let settled = node
        .get::<crate::core::graph::DeviceMatchedAudioWindowContractsComponent>()
        .map(|component| &*component.0);
    let pin_group_slot = |port_name: &str| {
        node.the_pin_group_slot_of(port_name)
            .map(|(group, slot_port)| PinGroupSlotOnTheWire {
                group: group.name.clone(),
                slot: slot_port.slot,
                pin: slot_port.pin.to_owned(),
            })
    };
    GraphNodePortsOnTheWire {
        inputs: node
            .ports
            .inputs
            .iter()
            .map(|port| GraphNodePortOnTheWire {
                pin_group_slot: pin_group_slot(&port.name),
                ..input_port_rendered_over_any_settled_contract(node, port, settled)
            })
            .collect(),
        outputs: node
            .ports
            .outputs
            .iter()
            .map(|port| GraphNodePortOnTheWire {
                stream_facts: node
                    .stream_facts_of_output_ports()
                    .current_facts_of(&port.name)
                    .and_then(|facts| {
                        PortStreamFactsOnTheWire::of_the_facts_on_a_port_of(
                            facts,
                            port.pin.as_ref(),
                        )
                    }),
                pin_group_slot: pin_group_slot(&port.name),
                ..GraphNodePortOnTheWire::from(port)
            })
            .collect(),
    }
}

/// One input port of `node`, saying whether a link may end at it, with a `match_device`
/// declaration replaced by whatever its own device stream settled it to.
fn input_port_rendered_over_any_settled_contract(
    node: &crate::core::graph::GraphNode,
    port: &crate::core::graph::PortInfo,
    settled: Option<&crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort>,
) -> GraphNodePortOnTheWire {
    let mut rendered = GraphNodePortOnTheWire {
        takes_a_wire: Some(port.takes_a_wire_on_a_node_of(node.node_type())),
        ..GraphNodePortOnTheWire::from(port)
    };
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

/// `Some(false)` for a port of no pin type, whose values no type checks; `None`
/// otherwise.
fn values_checked_of(states_a_pin_type: bool) -> Option<bool> {
    (!states_a_pin_type).then_some(false)
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
            values_checked: values_checked_of(
                port.pin.is_some() || port.declared_pin_type.is_some(),
            ),
            pin_default: port.pin_default.clone(),
            pin: port.pin.clone(),
            annotations: port.annotations.0.clone(),
            takes_a_wire: None,
            value_read: None,
            pin_group_slot: None,
        }
    }
}

impl From<&crate::core::graph::GraphNodePorts> for GraphNodePortsOnTheWire {
    fn from(ports: &crate::core::graph::GraphNodePorts) -> Self {
        Self {
            inputs: ports
                .inputs
                .iter()
                .map(GraphNodePortOnTheWire::from)
                .collect(),
            outputs: ports
                .outputs
                .iter()
                .map(GraphNodePortOnTheWire::from)
                .collect(),
        }
    }
}

/// `link` as `graph` answers it, each end on this runtime named by its node's name, saying what
/// it converts and warns of as `verdict`, the judgement of its two typed ends, does.
pub(crate) fn rendered_link(
    link: &crate::core::graph::Link,
    node_names: &NodeNamesByNodeRunId,
    verdict: Option<&PinLinkVerdict>,
) -> LinkOutput {
    let rendered = RenderedLinkState::of(link);
    let mut components = link.render_components();
    // `LinkStateComponent` renders under `components.state` too, and for a link waiting on a
    // helper it holds the `Pending` the op stamped. Left alone it would contradict the
    // top-level state a reader was told to check — the same disagreement the top-level state
    // reads the component to avoid, in the other direction.
    let (state, error_reason) = rendered.into_state_and_its_reason();
    render_the_state_component_as(&mut components, state);
    let mut rendered = LinkOutput {
        id: link.id.to_string(),
        source: LinkPortRefOutput {
            member: link.source.member().map(str::to_owned),
            ..link_end_of_a_port(
                link.source.node_run_id(),
                link.source.port_name(),
                node_names,
            )
        },
        target: link_end_of_a_port(
            link.target.node_run_id(),
            link.target.port_name(),
            node_names,
        ),
        capacity: link.capacity.get(),
        state,
        error_reason,
        components,
        ..LinkOutput::default()
    };
    mark_what_a_link_converts_and_warns_of(&mut rendered, verdict);
    rendered
}

/// Mark `rendered` with what `verdict`, the judgement of its two typed ends, says it does: a
/// free conversion, and the warnings it gave. A link no verdict judged, one whose end is untyped,
/// is marked with neither.
pub(crate) fn mark_what_a_link_converts_and_warns_of(
    rendered: &mut LinkOutput,
    verdict: Option<&PinLinkVerdict>,
) {
    rendered.marks = PinLinkMarksOnTheWire::of(verdict);
}

/// `carried`, a link the graph carries through the function node pool as two links, as it was
/// given: from the island to the reader, naming the pool's ports it runs through, in the error
/// either of its links is in, else in the state of the link out of the pool, saying what it
/// converts and warns of as `verdict` does.
pub(crate) fn rendered_link_carried_through_the_function_node_pool(
    carried: &LinkCarriedThroughTheFunctionNodePool<'_>,
    node_names: &NodeNamesByNodeRunId,
    verdict: Option<&PinLinkVerdict>,
) -> LinkOutput {
    let into_the_pool = rendered_link(carried.into_the_function_node_pool, node_names, None);
    let out_of_the_pool = rendered_link(carried.out_of_the_function_node_pool, node_names, None);
    let (state, error_reason) = if into_the_pool.state == LinkStateOutput::Error {
        (into_the_pool.state, into_the_pool.error_reason)
    } else {
        (out_of_the_pool.state, out_of_the_pool.error_reason)
    };
    let mut components = out_of_the_pool.components;
    components.carried_through_the_function_node_pool =
        Some(LinkCarriedThroughTheFunctionNodePoolOnTheWire {
            input_port: into_the_pool.target.port,
            output_port: out_of_the_pool.source.port,
        });
    render_the_state_component_as(&mut components, state);
    let mut rendered = LinkOutput {
        id: carried.carried_link_id.to_string(),
        source: into_the_pool.source,
        target: out_of_the_pool.target,
        capacity: out_of_the_pool.capacity,
        state,
        error_reason,
        components,
        ..LinkOutput::default()
    };
    mark_what_a_link_converts_and_warns_of(&mut rendered, verdict);
    rendered
}

/// Set a link's `components.state`, which `LinkStateComponent` renders, to the state the link
/// reports, so the two never disagree.
fn render_the_state_component_as(
    components: &mut GraphLinkComponentsOnTheWire,
    state: LinkStateOutput,
) {
    if let Some(rendered_state) = &mut components.state {
        *rendered_state = state.into();
    }
}

/// Why `link` is not wired, as it reports its state `error`: its two ends no longer carry types
/// that connect, or the end that refused it refused; `None` for a link in any other state.
pub(crate) fn why_the_link_is_not_wired(
    link: &crate::core::graph::Link,
) -> Option<WhyALinkIsNotWired> {
    match RenderedLinkState::of(link) {
        RenderedLinkState::ErrorBecause(why) => Some(why),
        RenderedLinkState::WithoutAReason(LinkStateOutput::Error) => {
            Some(WhyALinkIsNotWired::ItsWiringFailed)
        }
        RenderedLinkState::WithoutAReason(_) => None,
    }
}

/// Why a link is not wired, each failure its own kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WhyALinkIsNotWired {
    /// Its two ends no longer carry types that connect, since an input choosing the type of one
    /// of them changed; it carries nothing until they connect again or it is removed.
    ItsPinTypesDiffer(crate::core::graph::LinkPinTypesDifferComponent),
    /// An end refused to open its side of it, in that end's own words; it is wired again by
    /// itself.
    AnEndRefused(String),
    /// Its wiring failed and gave no reason, so nothing wires it again by itself.
    ItsWiringFailed,
}

impl WhyALinkIsNotWired {
    /// The kind of problem it is.
    pub(crate) fn kind(&self) -> DiagnosticKindOnTheWire {
        match self {
            Self::ItsPinTypesDiffer(_) => DiagnosticKindOnTheWire::LinkPinTypesDiffer,
            Self::AnEndRefused(_) | Self::ItsWiringFailed => DiagnosticKindOnTheWire::LinkNotWired,
        }
    }

    /// What fixes it, in words.
    pub(crate) fn fix(&self) -> &'static str {
        match self {
            Self::ItsPinTypesDiffer(_) => {
                "change the input that chose the type back, or remove the link"
            }
            Self::AnEndRefused(_) => {
                "nothing while the end that refused it runs, as it is wired again by itself; \
                 reconnect it to try again now, or remove it"
            }
            Self::ItsWiringFailed => "reconnect it, or remove it",
        }
    }
}

impl std::fmt::Display for WhyALinkIsNotWired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ItsPinTypesDiffer(differing) => write!(
                formatter,
                "its output carries {} and its input reads {}, which do not connect: {}",
                differing.output_pin_type, differing.input_pin_type, differing.why
            ),
            Self::AnEndRefused(reason) => formatter.write_str(reason),
            Self::ItsWiringFailed => formatter.write_str("its wiring failed, and gave no reason"),
        }
    }
}

/// What a link reports as its state, and why where that is `error`.
enum RenderedLinkState {
    WithoutAReason(LinkStateOutput),
    ErrorBecause(WhyALinkIsNotWired),
}

impl RenderedLinkState {
    /// Read one link's state off whichever cell holds the live answer.
    ///
    /// A link whose ends no longer carry types that connect is in error for that, whatever its
    /// wiring last recorded. Wiring records its outcome on the component; the field is the state
    /// the link was created in. A link handed to an out-of-process end sits at `Pending` there
    /// until that end answers — the answer lands on a cell its bridge's reader thread fills,
    /// which holds no graph lock — so a link still carrying those cells reads its state off them
    /// instead. Once the disconnect path has moved the component past `Pending`, that stamp is
    /// the answer: a link on its way out is not `wired` because a helper once said so.
    fn of(link: &crate::core::graph::Link) -> Self {
        if let Some(differing) = link.get::<crate::core::graph::LinkPinTypesDifferComponent>() {
            return Self::ErrorBecause(WhyALinkIsNotWired::ItsPinTypesDiffer(differing.clone()));
        }
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
                Self::ErrorBecause(WhyALinkIsNotWired::AnEndRefused(reason))
            }
        }
    }

    /// The state, then its `error_reason`.
    fn into_state_and_its_reason(self) -> (LinkStateOutput, Option<String>) {
        match self {
            Self::WithoutAReason(state) => (state, None),
            Self::ErrorBecause(why) => (LinkStateOutput::Error, Some(why.to_string())),
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
        let declared_pin = port.declared_pin();
        let annotations = declared_pin
            .as_ref()
            .map(|declared| declared.annotations.clone())
            .unwrap_or_default();
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            required: port.required,
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            values_checked: values_checked_of(declared_pin.is_some()),
            pin: declared_pin.map(|declared| PortDeclaredPinOutput {
                pin_type: declared.pin_type,
                flow: declared.flow,
            }),
            input_mode: port.input_mode,
            default_value: port
                .input_mode
                .filter(|input_mode| input_mode.is_handed_to_its_node())
                .and(port.pin_default.clone()),
            annotations,
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

/// The schema the control contract file states under `name`: how the contract types an answer
/// the engine builds, such as `StreamGraphOnTheWire` for `graph`.
#[cfg(test)]
pub(crate) fn the_control_contract_schema_named(name: &str) -> serde_json::Value {
    let contract: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../streamlib-control-contract/contract.json"
    )))
    .expect("the control contract file is JSON");
    contract["components"]["schemas"]
        .get(name)
        .unwrap_or_else(|| panic!("the control contract states `{name}`"))
        .clone()
}

#[cfg(test)]
mod link_rendering_tests;

#[cfg(test)]
mod node_health_rendering_tests;

#[cfg(test)]
mod port_rendering_tests;

#[cfg(test)]
mod pin_type_rendering_tests;

#[cfg(test)]
mod graph_response_top_level_key_rendering_tests;

#[cfg(test)]
mod config_schema_rendering_tests;
