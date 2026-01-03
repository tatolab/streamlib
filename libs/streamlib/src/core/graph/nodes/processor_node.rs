// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::super::components::{
    default_component_serializers, default_components, ComponentMap, ComponentSerializer,
};
use super::super::{GraphNodeWithComponents, GraphWeight, InputLinkPortRef, OutputLinkPortRef};
use super::{PortInfo, ProcessorNodePorts, ProcessorUniqueId};
use crate::core::utils::compute_json_checksum;

/// Node in the processor graph with embedded component storage.
#[derive(Serialize, Deserialize)]
pub struct ProcessorNode {
    pub id: ProcessorUniqueId,
    #[serde(rename = "type")]
    pub processor_type: String,
    /// Display name for UI. Defaults to processor_type if not overridden.
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    pub ports: ProcessorNodePorts,
    /// Runtime components (not serialized by derive, but via serialize_components).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
    /// Serializers for each inserted component type.
    #[serde(skip, default = "default_component_serializers")]
    component_serializers: Vec<ComponentSerializer>,
}

impl PartialEq for ProcessorNode {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
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
        processor_type: impl Into<String>,
        display_name: impl Into<String>,
        config: Option<serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        let processor_type = processor_type.into();

        let config_checksum = config.as_ref().map(compute_json_checksum).unwrap_or(0);
        Self {
            id: ProcessorUniqueId::new(),
            processor_type,
            display_name: display_name.into(),
            config,
            config_checksum,
            ports: ProcessorNodePorts { inputs, outputs },
            components: ComponentMap::new(),
            component_serializers: Vec::new(),
        }
    }

    /// Update config and recompute checksum.
    pub fn set_config(&mut self, config: serde_json::Value) {
        self.config_checksum = compute_json_checksum(&config);
        self.config = Some(config);
    }

    pub fn processor_type(&self) -> &str {
        &self.processor_type
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
