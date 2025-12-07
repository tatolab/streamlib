// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anymap2::Map;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

use super::super::{GraphNode, GraphWeight, LinkPortRef};

/// Compute a deterministic checksum from a JSON value.
fn compute_json_checksum(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Use canonical string representation for deterministic hashing
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Unique identifier for a processor node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessorId(String);

impl ProcessorId {
    /// Create a new ProcessorId from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the inner string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for ProcessorId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for ProcessorId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ProcessorId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProcessorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ProcessorId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ProcessorId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<ProcessorId> for String {
    fn from(id: ProcessorId) -> Self {
        id.0
    }
}

impl From<&ProcessorId> for String {
    fn from(id: &ProcessorId) -> Self {
        id.0.clone()
    }
}

impl From<&ProcessorId> for ProcessorId {
    fn from(id: &ProcessorId) -> Self {
        Self(id.0.clone())
    }
}

impl PartialEq<str> for ProcessorId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<ProcessorId> for str {
    fn eq(&self, other: &ProcessorId) -> bool {
        self == other.0
    }
}

impl PartialEq<&str> for ProcessorId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ProcessorId> for &str {
    fn eq(&self, other: &ProcessorId) -> bool {
        *self == other.0
    }
}

impl PartialEq<String> for ProcessorId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ProcessorId> for String {
    fn eq(&self, other: &ProcessorId) -> bool {
        *self == other.0
    }
}

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

/// TypeMap for component storage (Send + Sync).
type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

fn default_components() -> ComponentMap {
    ComponentMap::new()
}

/// Node in the processor graph with embedded component storage.
#[derive(Debug, Serialize, Deserialize)]
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
        let id = ProcessorId(cuid2::create_id());
        let config_checksum = config.as_ref().map(compute_json_checksum).unwrap_or(0);
        Self {
            id,
            processor_type,
            config,
            config_checksum,
            ports: NodePorts { inputs, outputs },
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
    fn insert<C: Send + Sync + 'static>(&mut self, component: C) {
        self.components.insert(component);
    }

    fn get<C: Send + Sync + 'static>(&self) -> Option<&C> {
        self.components.get::<C>()
    }

    fn get_mut<C: Send + Sync + 'static>(&mut self) -> Option<&mut C> {
        self.components.get_mut::<C>()
    }

    fn remove<C: Send + Sync + 'static>(&mut self) -> Option<C> {
        self.components.remove::<C>()
    }

    fn has<C: Send + Sync + 'static>(&self) -> bool {
        self.components.contains::<C>()
    }
}
