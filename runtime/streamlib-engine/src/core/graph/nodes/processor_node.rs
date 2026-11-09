// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::super::components::{
    ComponentMap, ComponentSerializer, default_component_serializers, default_components,
};
use super::super::{GraphNodeWithComponents, GraphWeight, InputLinkPortRef, OutputLinkPortRef};
use super::{PortInfo, ProcessorNodePorts, ProcessorUniqueId};
use std::sync::Arc;

use crate::core::descriptors::{NodeKind, ProcessorClassImportPath};
use crate::core::output_port_stream_facts::{
    StreamFactsOfOutputPorts, UpstreamStreamFactsByInboundLink,
};
use crate::core::utils::compute_json_checksum;

/// Node in the processor graph with embedded component storage.
#[derive(Serialize, Deserialize)]
pub struct ProcessorNode {
    /// This node's id for one run of its stream, which names its channels.
    pub id: ProcessorUniqueId,
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
    pub processor_type: ProcessorClassImportPath,
    /// Display name for UI. Defaults to the processor's PascalCase short name if not overridden.
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    pub ports: ProcessorNodePorts,
    /// How the node's type runs, read off its registered descriptor; `None`
    /// on a node whose type nothing registers.
    #[serde(skip)]
    node_kind: Option<NodeKind>,
    /// Runtime components (not serialized by derive, but via serialize_components).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
    /// Serializers for each inserted component type.
    #[serde(skip, default = "default_component_serializers")]
    component_serializers: Vec<ComponentSerializer>,
    /// The entries this node's output ports carry now, shared with whatever
    /// replaces them and with every node an inbound link reads them from.
    #[serde(skip)]
    stream_facts_of_output_ports: Arc<StreamFactsOfOutputPorts>,
    /// The upstream entries this node's inbound links reach.
    #[serde(skip)]
    upstream_stream_facts_by_inbound_link: Arc<UpstreamStreamFactsByInboundLink>,
}

impl PartialEq for ProcessorNode {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.node_uuid == other.node_uuid
            && self.pins == other.pins
            && self.exposed_config_fields == other.exposed_config_fields
            && self.studio == other.studio
            && self.processor_type == other.processor_type
            && self.display_name == other.display_name
            && self.config == other.config
            && self.config_checksum == other.config_checksum
            && self.ports == other.ports
    }
}

impl Eq for ProcessorNode {}

impl std::fmt::Debug for ProcessorNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessorNode")
            .field("id", &self.id)
            .field("node_uuid", &self.node_uuid)
            .field("processor_type", &self.processor_type)
            .field("display_name", &self.display_name)
            .field("config", &self.config)
            .field("config_checksum", &self.config_checksum)
            .field("ports", &self.ports)
            // Skip: components, component_serializers (runtime-only)
            .finish()
    }
}

impl ProcessorNode {
    /// Create a new processor node. The ID is generated automatically using cuid2.
    pub fn new(
        processor_type: ProcessorClassImportPath,
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
        Self {
            id: ProcessorUniqueId::new(),
            node_uuid: uuid::Uuid::new_v4(),
            pins: serde_json::Map::new(),
            exposed_config_fields: Vec::new(),
            studio: serde_json::Map::new(),
            processor_type,
            display_name: display_name.into(),
            config,
            config_checksum,
            ports: ProcessorNodePorts { inputs, outputs },
            node_kind,
            components: ComponentMap::new(),
            component_serializers: Vec::new(),
            stream_facts_of_output_ports,
            upstream_stream_facts_by_inbound_link: Arc::default(),
        }
    }

    /// Whether this node's processor runs: an instance in this process, or a
    /// helper's wiring, and not being deleted.
    pub(crate) fn runs_its_processor(&self) -> bool {
        use super::super::components::{
            OutOfProcessLinkWiringComponent, PendingDeletionComponent, ProcessorInstanceComponent,
        };
        (self.has::<ProcessorInstanceComponent>() || self.has::<OutOfProcessLinkWiringComponent>())
            && !self.has::<PendingDeletionComponent>()
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

    /// Update config and recompute checksum.
    pub fn set_config(&mut self, config: serde_json::Value) {
        self.config_checksum = compute_json_checksum(&config);
        self.config = Some(config);
    }

    pub fn processor_type(&self) -> &ProcessorClassImportPath {
        &self.processor_type
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

    pub fn has_input(&self, name: &str) -> bool {
        self.ports.inputs.iter().any(|p| p.name == name)
    }

    pub fn has_output(&self, name: &str) -> bool {
        self.ports.outputs.iter().any(|p| p.name == name)
    }

    /// Create a reference to an output port. Panics if port doesn't exist.
    pub fn output(&self, port_name: &str) -> OutputLinkPortRef {
        if !self.has_output(port_name) {
            panic!(
                "Processor '{}' ({}) has no output port '{}'. Available outputs: {:?}",
                self.id,
                self.processor_type,
                port_name,
                self.ports
                    .outputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            );
        }
        OutputLinkPortRef::new(self.id.to_string(), port_name)
    }

    /// Create a reference to an input port. Panics if port doesn't exist.
    pub fn input(&self, port_name: &str) -> InputLinkPortRef {
        if !self.has_input(port_name) {
            panic!(
                "Processor '{}' ({}) has no input port '{}'. Available inputs: {:?}",
                self.id,
                self.processor_type,
                port_name,
                self.ports
                    .inputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            );
        }
        InputLinkPortRef::new(self.id.to_string(), port_name)
    }

    pub fn try_output(&self, port_name: &str) -> Option<OutputLinkPortRef> {
        if self.has_output(port_name) {
            Some(OutputLinkPortRef::new(self.id.to_string(), port_name))
        } else {
            None
        }
    }

    pub fn try_input(&self, port_name: &str) -> Option<InputLinkPortRef> {
        if self.has_input(port_name) {
            Some(InputLinkPortRef::new(self.id.to_string(), port_name))
        } else {
            None
        }
    }
}

impl GraphWeight for ProcessorNode {
    fn id(&self) -> &str {
        &self.id
    }
}

impl GraphNodeWithComponents for ProcessorNode {
    fn components(&self) -> &ComponentMap {
        &self.components
    }

    fn components_mut(&mut self) -> &mut ComponentMap {
        &mut self.components
    }

    fn component_serializers(&self) -> &[ComponentSerializer] {
        &self.component_serializers
    }

    fn component_serializers_mut(&mut self) -> &mut Vec<ComponentSerializer> {
        &mut self.component_serializers
    }
}
