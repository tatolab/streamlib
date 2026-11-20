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
use crate::core::utils::compute_json_checksum;

#[derive(Serialize, Deserialize)]
pub struct GraphNode {
    /// This node's id for one run of its stream, which names its channels.
    pub id: NodeRunId,
    /// The node's UUID: its spec key in the graph document, kept across every run.
    #[serde(default = "uuid::Uuid::new_v4")]
    pub node_uuid: uuid::Uuid,
    /// The values the graph document sets on the node's unlinked inputs, by input name.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub pins: serde_json::Map<String, serde_json::Value>,
    /// The config fields the node takes as inputs too, each an input port of the field's name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_config_fields: Vec<String>,
    /// What the studio draws the node with.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub studio: serde_json::Map<String, serde_json::Value>,
    /// The import path of the class this node is an instance of.
    #[serde(rename = "type")]
    pub node_type: NodeClassImportPath,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
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
}

impl PartialEq for GraphNode {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.node_uuid == other.node_uuid
            && self.pins == other.pins
            && self.exposed_config_fields == other.exposed_config_fields
            && self.studio == other.studio
            && self.node_type == other.node_type
            && self.display_name == other.display_name
            && self.config == other.config
            && self.config_checksum == other.config_checksum
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
            .field("config", &self.config)
            .field("config_checksum", &self.config_checksum)
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
        config: Option<serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        let config_checksum = config.as_ref().map(compute_json_checksum).unwrap_or(0);
        let stream_facts_of_output_ports = Arc::new(StreamFactsOfOutputPorts::as_declared_by(
            &outputs,
            config.as_ref(),
        ));
        let ports = GraphNodePorts { inputs, outputs }
            .resolved_against_the_config(&node_type, config.as_ref());
        Self {
            id: NodeRunId::new(),
            node_uuid: uuid::Uuid::new_v4(),
            pins: serde_json::Map::new(),
            exposed_config_fields: Vec::new(),
            studio: serde_json::Map::new(),
            node_type,
            display_name: display_name.into(),
            config,
            config_checksum,
            ports,
            node_kind,
            components: ComponentMap::new(),
            component_renderers: Vec::new(),
            stream_facts_of_output_ports,
            upstream_stream_facts_by_inbound_link: Arc::default(),
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

    /// The values this node's port facts read: its config, each setting it reads at setup laid
    /// over it holding the value set on it or else its default.
    pub(crate) fn the_values_its_port_facts_read(&self) -> Option<serde_json::Value> {
        crate::core::graph::the_values_port_facts_read(
            self.config.as_ref(),
            &self.ports.inputs,
            &self.pins,
        )
    }

    /// Update config and recompute checksum.
    pub fn set_config(&mut self, config: serde_json::Value) {
        self.config_checksum = compute_json_checksum(&config);
        self.config = Some(config);
        self.resolve_the_ports_its_settings_declare();
    }

    /// Set the pin `input` to `value`, `None` taking it off; answers the pin it held. A
    /// `GraphInput`'s `value` ports take what its settings declare of them again.
    pub(crate) fn set_pin(
        &mut self,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let held = match value {
            Some(value) => self.pins.insert(input.to_string(), value),
            None => self.pins.remove(input),
        };
        self.resolve_the_ports_its_settings_declare();
        held
    }

    /// The one map of input values this node holds: its config and its pins, a pin winning over
    /// a config field of its name.
    pub(crate) fn input_values(&self) -> serde_json::Map<String, serde_json::Value> {
        crate::core::graph::legacy_config_of_a_node::input_values_of_a_config_and_pin_values(
            self.config.as_ref(),
            &self.pins,
        )
    }

    /// Resolve this node's ports against what its config and pins declare of them: a
    /// `GraphInput`'s `value` ports take the pin type, the family, the range and the lifetime
    /// its settings name. The ports of every other node type are as it declares them.
    pub(crate) fn resolve_the_ports_its_settings_declare(&mut self) {
        if !self.is_a_graph_input() {
            return;
        }
        let settings = serde_json::Value::Object(self.input_values());
        self.ports = std::mem::take(&mut self.ports)
            .resolved_against_the_config(&self.node_type, Some(&settings));
    }

    /// Refuse a link into input `input` when it is a setting, which takes a stored value and
    /// never a wire.
    pub(crate) fn refuse_a_link_into_a_setting(&self, input: &str) -> crate::core::Result<()> {
        match self.input_port(input) {
            Some(port) if !port.takes_a_wire() => Err(the_refusal_of_a_link_into_a_setting(
                &port.name,
                &self.display_name,
            )),
            _ => Ok(()),
        }
    }

    /// Whether this node is a `GraphInput`; one in a graph that runs is at its stream's top
    /// level, since a fragment's own are flattened away.
    pub fn is_a_graph_input(&self) -> bool {
        crate::core::graph::graph_interface_resolution::is_the_graph_input_type(&self.node_type)
    }

    pub fn node_type(&self) -> &NodeClassImportPath {
        &self.node_type
    }

    /// How the node's type runs; `None` when nothing registers its type.
    pub fn node_kind(&self) -> Option<NodeKind> {
        self.node_kind
    }

    /// Take `exposed_config_fields` as the config fields this node takes as inputs, each with
    /// its port of `exposed_config_field_input_ports`, in place of those it took before.
    pub(crate) fn replace_the_exposed_config_fields(
        &mut self,
        exposed_config_fields: Vec<String>,
        exposed_config_field_input_ports: Vec<PortInfo>,
    ) {
        let exposed_before =
            std::mem::replace(&mut self.exposed_config_fields, exposed_config_fields);
        self.ports
            .inputs
            .retain(|port| !exposed_before.contains(&port.name));
        self.ports.inputs.extend(exposed_config_field_input_ports);
    }

    /// Whether input `name` is a config field this node exposes as an input.
    pub fn is_an_exposed_config_field_input(&self, name: &str) -> bool {
        self.exposed_config_fields.iter().any(|field| field == name)
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

/// The refusal of a link into input `input` of node `node`, a setting, which takes no wire.
pub(crate) fn the_refusal_of_a_link_into_a_setting(input: &str, node: &str) -> crate::core::Error {
    crate::core::Error::PinLinkRefused(Box::new(
        crate::core::descriptors::PinLinkRefusal::input_takes_no_wire(node, input),
    ))
}
