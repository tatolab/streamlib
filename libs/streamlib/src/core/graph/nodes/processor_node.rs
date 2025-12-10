// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::super::components::{default_components, Component, ComponentMap};
use super::super::{GraphNode, GraphWeight, LinkPortRef};
use super::{PortInfo, ProcessorNodePorts, ProcessorUniqueId};
use crate::core::utils::compute_json_checksum;

/// Node in the processor graph with embedded component storage.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessorNode {
    pub id: ProcessorUniqueId,
    #[serde(rename = "type")]
    pub processor_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    pub ports: ProcessorNodePorts,
    /// Runtime components (not serialized).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
}

impl PartialEq for ProcessorNode {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.processor_type == other.processor_type
            && self.config == other.config
            && self.config_checksum == other.config_checksum
            && self.ports == other.ports
    }
}

impl Eq for ProcessorNode {}

impl ProcessorNode {
    /// Create a new processor node. The ID is generated automatically using cuid2.
    pub fn new(
        processor_type: impl Into<String>,
        config: Option<serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        let processor_type = processor_type.into();

        let config_checksum = config.as_ref().map(compute_json_checksum).unwrap_or(0);
        Self {
            id: ProcessorUniqueId::new(),
            processor_type,
            config,
            config_checksum,
            ports: ProcessorNodePorts { inputs, outputs },
            components: ComponentMap::new(),
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
    pub fn output(&self, port_name: &str) -> LinkPortRef {
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
        LinkPortRef::output(self.id.to_string(), port_name)
    }

    /// Create a reference to an input port. Panics if port doesn't exist.
    pub fn input(&self, port_name: &str) -> LinkPortRef {
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
        LinkPortRef::input(self.id.to_string(), port_name)
    }

    pub fn try_output(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_output(port_name) {
            Some(LinkPortRef::output(self.id.to_string(), port_name))
        } else {
            None
        }
    }

    pub fn try_input(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_input(port_name) {
            Some(LinkPortRef::input(self.id.to_string(), port_name))
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

impl GraphNode for ProcessorNode {
    fn insert<C: Component>(&mut self, component: C) {
        self.components.insert(component);
    }

    fn get<C: Component>(&self) -> Option<&C> {
        self.components.get::<C>()
    }

    fn get_mut<C: Component>(&mut self) -> Option<&mut C> {
        self.components.get_mut::<C>()
    }

    fn remove<C: Component>(&mut self) -> Option<C> {
        self.components.remove::<C>()
    }

    fn has<C: Component>(&self) -> bool {
        self.components.contains::<C>()
    }
}
