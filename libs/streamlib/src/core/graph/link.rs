use crate::core::link_channel::LinkId;
use serde::{Deserialize, Serialize};

/// Direction of a port in a link endpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    /// Input port (receives data)
    Input,
    /// Output port (sends data)
    Output,
}

/// One endpoint of a link (source or target)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEndpoint {
    /// Node (processor) ID
    pub node: String,
    /// Port name on the node
    pub port: String,
    /// Direction of this port
    pub direction: LinkDirection,
}

impl LinkEndpoint {
    /// Create a new source endpoint (output port)
    pub fn source(node: impl Into<String>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Output,
        }
    }

    /// Create a new target endpoint (input port)
    pub fn target(node: impl Into<String>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Input,
        }
    }

    /// Convert to port address format "node.port"
    pub fn to_address(&self) -> String {
        format!("{}.{}", self.node, self.port)
    }
}

/// Link in the processor graph (connection between two ports)
///
/// Represents a connection between two processor ports. This is a pure data structure
/// that can be serialized, compared, and cloned for graph operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Unique link identifier
    pub id: LinkId,
    /// Source endpoint (output port)
    pub source: LinkEndpoint,
    /// Target endpoint (input port)
    pub target: LinkEndpoint,
}

impl Link {
    /// Create a new link from port addresses
    pub fn new(id: LinkId, from_port: &str, to_port: &str) -> Self {
        let (source_node, source_port) = from_port.split_once('.').unwrap_or((from_port, ""));
        let (target_node, target_port) = to_port.split_once('.').unwrap_or((to_port, ""));

        Self {
            id,
            source: LinkEndpoint::source(source_node, source_port),
            target: LinkEndpoint::target(target_node, target_port),
        }
    }

    /// Get source port address in "node.port" format
    pub fn from_port(&self) -> String {
        self.source.to_address()
    }

    /// Get target port address in "node.port" format
    pub fn to_port(&self) -> String {
        self.target.to_address()
    }
}
