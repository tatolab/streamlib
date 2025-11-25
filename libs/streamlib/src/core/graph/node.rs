//! Processor node in the graph
//!
//! A ProcessorNode is pure serializable data describing a processor in the graph.
//! This is NOT an instance - the executor creates instances during compile.

use serde::{Deserialize, Serialize};

/// Unique identifier for a processor
pub type ProcessorId = String;

/// Node in the processor graph
///
/// Represents a processor in the graph topology. This is a pure data structure
/// that can be serialized, compared, and cloned for graph operations.
///
/// The executor converts these to actual processor instances during compile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessorNode {
    /// Unique processor identifier
    pub id: ProcessorId,
    /// Processor type name (e.g., "CameraProcessor")
    pub processor_type: String,
    /// Config checksum for cache invalidation
    pub config_checksum: u64,
}

impl ProcessorNode {
    /// Create a new processor node
    pub fn new(id: ProcessorId, processor_type: String) -> Self {
        Self {
            id,
            processor_type,
            config_checksum: 0,
        }
    }

    /// Create a new processor node with config checksum
    pub fn with_checksum(id: ProcessorId, processor_type: String, config_checksum: u64) -> Self {
        Self {
            id,
            processor_type,
            config_checksum,
        }
    }

    /// Get the processor ID
    pub fn id(&self) -> &ProcessorId {
        &self.id
    }

    /// Get the processor type
    pub fn processor_type(&self) -> &str {
        &self.processor_type
    }

    /// Create a port address string for an output port
    ///
    /// Returns format: "processor_id.port_name"
    pub fn output_port(&self, port_name: &str) -> String {
        format!("{}.{}", self.id, port_name)
    }

    /// Create a port address string for an input port
    ///
    /// Returns format: "processor_id.port_name"
    pub fn input_port(&self, port_name: &str) -> String {
        format!("{}.{}", self.id, port_name)
    }
}
