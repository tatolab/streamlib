// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticOnTheWire, DiagnosticPlaceOnTheWire, NodeHealthOnTheWire,
};

use super::super::components::{
    ComponentMap, ComponentRenderer, StateComponent, default_component_renderers,
    default_components,
};
use super::super::{GraphNodeWithComponents, GraphWeight, InputLinkPortRef, OutputLinkPortRef};
use super::{GraphNodePorts, NodeRunId, PortInfo};
use std::sync::Arc;

use crate::core::descriptors::{NodeClassImportPath, NodeKind};
use crate::core::output_port_stream_facts::{
    StreamFactsOfOutputPorts, UpstreamStreamFactsByInboundLink,
};

#[derive(Serialize, Deserialize)]
pub struct GraphNode {
    /// This node's id for one run of its stream, which names its channels.
    pub id: NodeRunId,
    /// The node's UUID: its spec key in the graph document, kept across every run.
    #[serde(default = "uuid::Uuid::new_v4")]
    pub node_uuid: uuid::Uuid,
    /// Every value the graph document sets on the node, by input name: its settings and the
    /// values on its unlinked inputs alike.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub input_values: serde_json::Map<String, serde_json::Value>,
    /// What the studio draws the node with.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub studio: serde_json::Map<String, serde_json::Value>,
    /// The import path of the class this node is an instance of.
    #[serde(rename = "type")]
    pub node_type: NodeClassImportPath,
    pub display_name: String,
    pub ports: GraphNodePorts,
    /// How the node's type runs, read off its registered descriptor; `None`
    /// on a node whose type nothing registers.
    #[serde(skip)]
    node_kind: Option<NodeKind>,
    /// Runtime components (not serialized by derive, but rendered by render_components).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
    /// The renderer of each inserted component type.
    #[serde(skip, default = "default_component_renderers")]
    component_renderers:
        Vec<ComponentRenderer<crate::core::json_schema::GraphNodeComponentsOnTheWire>>,
    /// The entries this node's output ports carry now, shared with whatever
    /// replaces them and with every node an inbound link reads them from.
    #[serde(skip)]
    stream_facts_of_output_ports: Arc<StreamFactsOfOutputPorts>,
    /// The upstream entries this node's inbound links reach.
    #[serde(skip)]
    upstream_stream_facts_by_inbound_link: Arc<UpstreamStreamFactsByInboundLink>,
    /// The pin groups the node's type declares, whose slots' ports its ports hold.
    #[serde(skip)]
    pub(super) pin_groups: Arc<[crate::core::descriptors::PinGroupDescriptor]>,
}

impl PartialEq for GraphNode {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.node_uuid == other.node_uuid
            && self.input_values == other.input_values
            && self.studio == other.studio
            && self.node_type == other.node_type
            && self.display_name == other.display_name
            && self.ports == other.ports
    }
}

impl Eq for GraphNode {}

impl std::fmt::Debug for GraphNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphNode")
            .field("id", &self.id)
            .field("node_uuid", &self.node_uuid)
            .field("node_type", &self.node_type)
            .field("display_name", &self.display_name)
            .field("input_values", &self.input_values)
            .field("ports", &self.ports)
            // Skip: components, component_renderers (runtime-only)
            .finish()
    }
}

impl GraphNode {
    /// Where a diagnostic of this node applies: the node, by its id and name.
    pub fn diagnostic_place(&self) -> DiagnosticPlaceOnTheWire {
        DiagnosticPlaceOnTheWire::Node {
            node_id: self.node_uuid.to_string(),
            node_name: self.display_name.clone(),
        }
    }

    /// The node's health and each diagnostic behind it, read from its state; `None` for a node
    /// that keeps no state.
    pub fn health_and_diagnostics(
        &self,
    ) -> Option<(NodeHealthOnTheWire, Vec<DiagnosticOnTheWire>)> {
        self.get::<StateComponent>()
            .map(|state| state.the_health_of_the_node(&self.diagnostic_place()))
    }

    pub fn new(
        node_type: NodeClassImportPath,
        node_kind: Option<NodeKind>,
        display_name: impl Into<String>,
        input_values: serde_json::Map<String, serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        let stream_facts_of_output_ports = Arc::new(StreamFactsOfOutputPorts::as_declared_by(
            &outputs,
            Some(&crate::core::graph::the_values_port_facts_read(
                &inputs,
                &input_values,
            )),
        ));
        let ports = GraphNodePorts { inputs, outputs }
            .resolved_against_the_input_values(&node_type, &input_values);
        Self {
            id: NodeRunId::new(),
            node_uuid: uuid::Uuid::new_v4(),
            input_values,
            studio: serde_json::Map::new(),
            node_type,
            display_name: display_name.into(),
            ports,
            node_kind,
            components: ComponentMap::new(),
            component_renderers: Vec::new(),
            stream_facts_of_output_ports,
            upstream_stream_facts_by_inbound_link: Arc::default(),
            pin_groups: Arc::from([]),
        }
    }

    pub(crate) fn runs_its_node(&self) -> bool {
        use super::super::components::{NodeInstanceComponent, OutOfProcessLinkWiringComponent};
        (self.has::<NodeInstanceComponent>() || self.has::<OutOfProcessLinkWiringComponent>())
            && self.stands()
    }

    /// Whether this node stands: it is not pending deletion.
    pub fn stands(&self) -> bool {
        !self.has::<super::super::components::PendingDeletionComponent>()
    }

    /// The entries this node's output ports carry now.
    pub(crate) fn stream_facts_of_output_ports(&self) -> &Arc<StreamFactsOfOutputPorts> {
        &self.stream_facts_of_output_ports
    }

    /// The upstream entries this node's inbound links reach.
    pub(crate) fn upstream_stream_facts_by_inbound_link(
        &self,
    ) -> &Arc<UpstreamStreamFactsByInboundLink> {
        &self.upstream_stream_facts_by_inbound_link
    }

    /// The values this node's port facts read: each it holds, and each input it is handed
    /// holding the value set on it or else its default.
    pub(crate) fn the_values_its_port_facts_read(&self) -> serde_json::Value {
        crate::core::graph::the_values_port_facts_read(&self.ports.inputs, &self.input_values)
    }

    /// Set the input value `input` to `value`, `None` taking it off; answers the value it held.
    /// A `GraphInput`'s `value` ports take what its settings declare of them again.
    pub(crate) fn set_input_value(
        &mut self,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let held = match value {
            Some(value) => self.input_values.insert(input.to_string(), value),
            None => self.input_values.remove(input),
        };
        self.resolve_the_ports_its_settings_declare();
        held
    }

    /// Hold `input_values` in place of every input value this node holds.
    pub(crate) fn hold_the_input_values(
        &mut self,
        input_values: serde_json::Map<String, serde_json::Value>,
    ) {
        self.input_values = input_values;
        self.resolve_the_ports_its_settings_declare();
    }

    /// Resolve this node's ports against what its input values declare of them: a
    /// `GraphInput`'s `value` ports take the pin type, the range and the lifetime its settings
    /// name, a `Constant`'s the pin type its `pin_type` chooses, and every output carries its
    /// stream facts on the pin type its settings read at setup choose. The ports of
    /// every other node type are as it declares them.
    pub(crate) fn resolve_the_ports_its_settings_declare(&mut self) {
        if self.node_type.carries_the_value_its_pin_type_chooses() {
            self.ports = std::mem::take(&mut self.ports)
                .resolved_against_the_input_values(&self.node_type, &self.input_values);
        }
        self.stream_facts_of_output_ports
            .take_the_pin_types_declared_by(
                &self.ports.outputs,
                Some(&self.the_values_its_port_facts_read()),
            );
    }

    /// Whether a port of this node takes its type from input `input`, which chooses it.
    pub(crate) fn a_port_takes_its_type_from(&self, input: &str) -> bool {
        self.ports
            .inputs
            .iter()
            .chain(&self.ports.outputs)
            .any(|port| {
                matches!(
                    port.declared_pin_type.as_ref().map(|declared| &declared.pin_type),
                    Some(crate::core::descriptors::DeclaredPinType::SetByInput { input_name })
                        if input_name == input
                )
            })
    }

    /// Each output of this node taking the type of its input `input`, as `SameAsInput` declares
    /// it.
    pub(crate) fn the_outputs_following_the_input<'node>(
        &'node self,
        input: &'node str,
    ) -> impl Iterator<Item = &'node PortInfo> + 'node {
        self.ports.outputs.iter().filter(move |output| {
            matches!(
                output.declared_pin_type.as_ref().map(|declared| &declared.pin_type),
                Some(crate::core::descriptors::DeclaredPinType::SameAsInput { input_port_name })
                    if input_port_name == input
            )
        })
    }

    /// Refuse a link into input `input` when it is a setting that takes no wire: one of a type
    /// no message carries, and every setting of a node type of the graph itself, which names a port of its stream or a variable rather than running on a value.
    pub(crate) fn refuse_a_link_into_a_setting(&self, input: &str) -> crate::core::Result<()> {
        refuse_a_link_into_a_port_taking_no_wire(
            self.input_port(input),
            &self.node_type,
            &self.display_name,
        )
    }

    /// Whether this node is a `GraphInput`; one in a graph that runs is at its stream's top
    /// level, since a fragment's own are flattened away.
    pub fn is_a_graph_input(&self) -> bool {
        self.node_type.is_the_graph_input_type()
    }

    pub fn node_type(&self) -> &NodeClassImportPath {
        &self.node_type
    }

    /// How the node's type runs; `None` when nothing registers its type.
    pub fn node_kind(&self) -> Option<NodeKind> {
        self.node_kind
    }

    /// The input port `name` finds: the input so named, else the one that had that name.
    pub fn input_port(&self, name: &str) -> Option<&PortInfo> {
        self.ports.input_named(name)
    }

    /// The output port `name` finds: the output so named, else the one that had that name.
    pub fn output_port(&self, name: &str) -> Option<&PortInfo> {
        self.ports.output_named(name)
    }

    pub fn has_input(&self, name: &str) -> bool {
        self.input_port(name).is_some()
    }

    pub fn has_output(&self, name: &str) -> bool {
        self.output_port(name).is_some()
    }

    /// Create a reference to an output port, under its current name. Panics if port doesn't
    /// exist.
    pub fn output(&self, port_name: &str) -> OutputLinkPortRef {
        self.try_output(port_name).unwrap_or_else(|| {
            panic!(
                "Node '{}' ({}) has no output port '{}'. Available outputs: {:?}",
                self.id,
                self.node_type,
                port_name,
                self.ports
                    .outputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            )
        })
    }

    /// Create a reference to an input port, under its current name. Panics if port doesn't
    /// exist.
    pub fn input(&self, port_name: &str) -> InputLinkPortRef {
        self.try_input(port_name).unwrap_or_else(|| {
            panic!(
                "Node '{}' ({}) has no input port '{}'. Available inputs: {:?}",
                self.id,
                self.node_type,
                port_name,
                self.ports
                    .inputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            )
        })
    }

    pub fn try_output(&self, port_name: &str) -> Option<OutputLinkPortRef> {
        self.output_port(port_name)
            .map(|port| OutputLinkPortRef::new(self.id.to_string(), &port.name))
    }

    pub fn try_input(&self, port_name: &str) -> Option<InputLinkPortRef> {
        self.input_port(port_name)
            .map(|port| InputLinkPortRef::new(self.id.to_string(), &port.name))
    }
}

impl GraphWeight for GraphNode {
    fn id(&self) -> &str {
        &self.id
    }
}

impl GraphNodeWithComponents for GraphNode {
    fn components(&self) -> &ComponentMap {
        &self.components
    }

    fn components_mut(&mut self) -> &mut ComponentMap {
        &mut self.components
    }

    fn component_renderers(
        &self,
    ) -> &[ComponentRenderer<crate::core::json_schema::GraphNodeComponentsOnTheWire>] {
        &self.component_renderers
    }

    fn component_renderers_mut(
        &mut self,
    ) -> &mut Vec<ComponentRenderer<crate::core::json_schema::GraphNodeComponentsOnTheWire>> {
        &mut self.component_renderers
    }
}

/// Refuse a link into `port`, an input of the node `node_name` of `node_type`, when it takes no
/// wire by the one rule, naming the port by its own name; no port, or one that takes a wire,
/// passes.
pub(crate) fn refuse_a_link_into_a_port_taking_no_wire(
    port: Option<&PortInfo>,
    node_type: &NodeClassImportPath,
    node_name: &str,
) -> crate::core::Result<()> {
    match port {
        Some(port) if !port.takes_a_wire_on_a_node_of(node_type) => {
            Err(the_refusal_of_a_link_into_a_setting(&port.name, node_name))
        }
        _ => Ok(()),
    }
}

/// The refusal of a link into input `input` of node `node`, a setting that takes no wire.
pub(crate) fn the_refusal_of_a_link_into_a_setting(input: &str, node: &str) -> crate::core::Error {
    crate::core::Error::PinLinkRefused(Box::new(
        crate::core::descriptors::PinLinkRefusal::input_takes_no_wire(node, input),
    ))
}
