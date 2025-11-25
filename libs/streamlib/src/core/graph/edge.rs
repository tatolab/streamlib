use crate::core::bus::{ConnectionId, PortType};
use serde::{Deserialize, Serialize};

/// Edge in the processor graph
///
/// Represents a connection between two processor ports. This is a pure data structure
/// that can be serialized, compared, and cloned for graph operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionEdge {
    /// Unique connection identifier
    pub id: ConnectionId,
    /// Source port address (e.g., "processor_0.video_out")
    pub from_port: String,
    /// Destination port address (e.g., "processor_1.video_in")
    pub to_port: String,
    /// Port type for type checking
    pub port_type: PortType,
}
