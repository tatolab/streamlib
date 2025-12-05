// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::link_port_ref::LinkPortRef;

/// Compute a deterministic checksum from a JSON value.
fn compute_json_checksum(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Use canonical string representation for deterministic hashing
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Unique identifier for a processor
pub type ProcessorId = String;

/// The kind of port - determines how data flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    #[default]
    Data,
    Event,
    Control,
}

/// Metadata about a port (input or output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    pub name: String,
    pub data_type: String,
    #[serde(default)]
    pub port_kind: PortKind,
}

/// Container for a node's input and output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodePorts {
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

/// Node in the processor graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessorNode {
    pub id: ProcessorId,
    #[serde(rename = "type")]
    pub processor_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    pub ports: NodePorts,
}

impl ProcessorNode {
    pub fn new(
        id: ProcessorId,
        processor_type: String,
        config: Option<serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        let config_checksum = config.as_ref().map(compute_json_checksum).unwrap_or(0);
        Self {
            id,
            processor_type,
            config,
            config_checksum,
            ports: NodePorts { inputs, outputs },
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
        LinkPortRef::output(self.id.clone(), port_name)
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
        LinkPortRef::input(self.id.clone(), port_name)
    }

    pub fn try_output(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_output(port_name) {
            Some(LinkPortRef::output(self.id.clone(), port_name))
        } else {
            None
        }
    }

    pub fn try_input(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_input(port_name) {
            Some(LinkPortRef::input(self.id.clone(), port_name))
        } else {
            None
        }
    }
}
