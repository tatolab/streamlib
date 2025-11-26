//! Processor node in the graph
//!
//! A ProcessorNode is pure serializable data describing a processor in the graph.
//! This is NOT an instance - the executor creates instances during compile.
//!
//! ProcessorNode contains:
//! - Processor type and ID
//! - Serialized config
//! - Port metadata (input/output names) for validation

use serde::{Deserialize, Serialize};

use super::link_port_ref::LinkPortRef;

/// Unique identifier for a processor
pub type ProcessorId = String;

/// The kind of port - determines how data flows
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// Data ports carry media frames (video, audio, etc.)
    #[default]
    Data,
    /// Event ports carry discrete signals/triggers
    Event,
    /// Control ports carry execution flow hints
    Control,
}

/// Metadata about a port (input or output)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    /// Port name
    pub name: String,
    /// Data type carried by this port (e.g., "VideoFrame", "AudioFrame")
    pub data_type: String,
    /// Kind of port (data, event, control)
    #[serde(default)]
    pub port_kind: PortKind,
}

/// Container for a node's input and output ports
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodePorts {
    /// Input ports
    pub inputs: Vec<PortInfo>,
    /// Output ports
    pub outputs: Vec<PortInfo>,
}

/// Node in the processor graph
///
/// Represents a processor in the graph topology. This is a pure data structure
/// that can be serialized, compared, and cloned for graph operations.
///
/// The executor converts these to actual processor instances during compile.
///
/// # Connection API
///
/// ProcessorNode provides `output()` and `input()` methods that return `LinkPortRef`
/// for use with `runtime.connect()`. These methods validate that the port exists.
///
/// ```ignore
/// let camera = runtime.add_processor::<CameraProcessor>(CameraConfig {
///     device_id: None,
/// })?;
/// let display = runtime.add_processor::<DisplayProcessor>(DisplayConfig {
///     width: 1920,
///     height: 1080,
///     ..Default::default()
/// })?;
///
/// runtime.connect(camera.output("video"), display.input("video"))?;
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessorNode {
    /// Unique processor identifier
    pub id: ProcessorId,
    /// Processor type name (e.g., "CameraProcessor")
    #[serde(rename = "type")]
    pub processor_type: String,
    /// Serialized config (JSON)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Port metadata (inputs and outputs)
    pub ports: NodePorts,
}

impl ProcessorNode {
    /// Create a new processor node with full metadata
    pub fn new(
        id: ProcessorId,
        processor_type: String,
        config: Option<serde_json::Value>,
        inputs: Vec<PortInfo>,
        outputs: Vec<PortInfo>,
    ) -> Self {
        Self {
            id,
            processor_type,
            config,
            ports: NodePorts { inputs, outputs },
        }
    }

    /// Get the processor type
    pub fn processor_type(&self) -> &str {
        &self.processor_type
    }

    /// Check if an input port exists
    pub fn has_input(&self, name: &str) -> bool {
        self.ports.inputs.iter().any(|p| p.name == name)
    }

    /// Check if an output port exists
    pub fn has_output(&self, name: &str) -> bool {
        self.ports.outputs.iter().any(|p| p.name == name)
    }

    /// Create a reference to an output port
    ///
    /// # Panics
    /// Panics if the port doesn't exist. Use `has_output()` to check first.
    ///
    /// Use with `runtime.connect()`:
    /// ```ignore
    /// runtime.connect(camera.output("video"), display.input("video"))?;
    /// ```
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

    /// Create a reference to an input port
    ///
    /// # Panics
    /// Panics if the port doesn't exist. Use `has_input()` to check first.
    ///
    /// Use with `runtime.connect()`:
    /// ```ignore
    /// runtime.connect(camera.output("video"), display.input("video"))?;
    /// ```
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

    /// Try to create a reference to an output port, returning None if it doesn't exist
    pub fn try_output(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_output(port_name) {
            Some(LinkPortRef::output(self.id.clone(), port_name))
        } else {
            None
        }
    }

    /// Try to create a reference to an input port, returning None if it doesn't exist
    pub fn try_input(&self, port_name: &str) -> Option<LinkPortRef> {
        if self.has_input(port_name) {
            Some(LinkPortRef::input(self.id.clone(), port_name))
        } else {
            None
        }
    }
}
